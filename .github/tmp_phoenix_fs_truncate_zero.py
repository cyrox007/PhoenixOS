from pathlib import Path

p = Path('crates/phoenix-fs/src/lib.rs')
s = p.read_text()

helper_anchor = 'fn materialize_object_leaf_with_two_replacements(\n'
helper = r'''fn materialize_object_leaf_with_replacement_and_deletion(
    current: &[u8],
    destination: &mut [u8],
    replacement_key: ObjectTreeKey,
    replacement_value: &[u8],
    deletion_key: ObjectTreeKey,
    new_generation: u64,
    new_block_number: u64,
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let current_node = validate_object_leaf(current)?;
    validate_rewrite_target(current_node, new_generation, new_block_number)?;
    if replacement_key == deletion_key || current_node.item_count <= 1 {
        return Err(PhoenixFsError::InvalidObjectMutationBatch);
    }

    let entries_start = TreeNodeHeader::entries_offset();
    let entries_end = entries_start
        .checked_add(current_node.entries_bytes as usize)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    destination.fill(0);
    let mut source_cursor = entries_start;
    let mut destination_cursor = entries_start;
    let mut replaced = false;
    let mut deleted = false;

    for _ in 0..current_node.item_count {
        let (record, old_value, source_size) =
            ObjectLeafRecordHeader::decode_with_value(&current[source_cursor..entries_end])?;
        source_cursor = source_cursor
            .checked_add(source_size)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        if record.key == deletion_key {
            deleted = true;
            continue;
        }
        let value = if record.key == replacement_key {
            replaced = true;
            replacement_value
        } else {
            old_value
        };
        let value_bytes =
            u32::try_from(value.len()).map_err(|_| PhoenixFsError::InvalidObjectRecordSize)?;
        destination_cursor = encode_leaf_record_at(
            destination,
            destination_cursor,
            ObjectLeafRecordHeader::new(record.key, value_bytes)?,
            value,
        )?;
    }

    if !replaced || !deleted {
        return Err(PhoenixFsError::ObjectRecordNotFound);
    }
    seal_object_leaf_rewrite(
        destination,
        current_node,
        new_generation,
        new_block_number,
        current_node.item_count - 1,
        destination_cursor,
    )
}

'''
if helper not in s:
    if helper_anchor not in s:
        raise SystemExit('helper anchor missing')
    s = s.replace(helper_anchor, helper + helper_anchor, 1)

commit_anchor = 'fn write_data_extent<D: BlockDevice>(\n'
commit = r'''#[allow(clippy::too_many_arguments)]
pub fn commit_truncate_single_extent_file_to_zero<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    object_id: u64,
    object_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    object_copy_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    current_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    next_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    superblock_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<DeepRecordUpdateResult, PhoenixFsError> {
    if object_id == 0 {
        return Err(PhoenixFsError::InvalidObjectId);
    }

    let mut metadata_bytes = [0_u8; OBJECT_METADATA_VALUE_SIZE];
    let metadata = read_object_metadata(
        device,
        current.superblock.roots.object_tree,
        object_id,
        current.superblock.generation,
        object_buffer,
        &mut metadata_bytes,
    )?;
    if metadata.object_type != ObjectType::File || metadata.size_bytes == 0 {
        return Err(PhoenixFsError::InvalidObjectMutationBatch);
    }

    let (extent_key, old_extent) =
        read_single_file_extent(device, current, object_id, object_buffer)?;
    if extent_key.offset != 0 || old_extent.data_bytes != metadata.size_bytes {
        return Err(PhoenixFsError::InvalidObjectMutationBatch);
    }

    let metadata_key = ObjectTreeKey::new(object_id, ObjectRecordKind::Metadata, 0);
    let metadata_path = find_object_tree_path(
        device,
        current.superblock.roots.object_tree,
        metadata_key,
        current.superblock.generation,
        object_buffer,
    )?;
    let extent_path = find_object_tree_path(
        device,
        current.superblock.roots.object_tree,
        extent_key,
        current.superblock.generation,
        object_buffer,
    )?;
    if metadata_path != extent_path {
        return Err(PhoenixFsError::ObjectMutationBatchSpansLeaves);
    }

    let metadata_retired = metadata_path.node_count() + 1;
    let data_retired =
        usize::try_from(old_extent.block_count).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    if metadata_retired
        .checked_add(data_retired)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?
        > MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS
    {
        return Err(PhoenixFsError::BufferSize);
    }

    let total_blocks = current.superblock.total_blocks;
    read_filesystem_block(
        device,
        current.superblock.roots.free_space_tree,
        current_free_space,
    )?;
    let free_node = validate_free_space_leaf(current_free_space, total_blocks)?;
    if free_node.metadata.block_number != current.superblock.roots.free_space_tree {
        return Err(PhoenixFsError::InvalidTransactionRoots);
    }

    let requested_blocks = u64::try_from(metadata_path.node_count() + 1)
        .map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    let allocation = plan_cow_allocation(current_free_space, total_blocks, requested_blocks)?;
    let generation = current
        .superblock
        .generation
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    ObjectMetadataValue::new(
        ObjectType::File,
        0,
        metadata.created_ns,
        metadata.modified_ns,
        metadata.changed_ns,
    )
    .encode(&mut metadata_bytes)?;
    validate_object_record_value(metadata_key, &metadata_bytes, total_blocks)?;

    read_filesystem_block(device, metadata_path.leaf_block, object_buffer)?;
    let object_block = allocation.allocated.start_block;
    let leaf = materialize_object_leaf_with_replacement_and_deletion(
        object_buffer,
        object_copy_buffer,
        metadata_key,
        &metadata_bytes,
        extent_key,
        generation,
        object_block,
    )?;
    write_filesystem_block(device, object_block, object_copy_buffer)?;

    let mut new_child_block = object_block;
    let mut new_child_first_key = first_object_node_key(object_copy_buffer, leaf)?;
    let mut next_block = object_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    for parent_position in (0..metadata_path.parent_count).rev() {
        let step = metadata_path.parents[parent_position];
        read_filesystem_block(device, step.block_number, object_buffer)?;
        validate_path_pointer(object_buffer, step)?;
        materialize_object_internal_after_child_copy(
            object_buffer,
            object_copy_buffer,
            generation,
            next_block,
            step.child_index as usize,
            new_child_block,
            new_child_first_key,
        )?;
        write_filesystem_block(device, next_block, object_copy_buffer)?;
        let copied_parent = TreeNodeHeader::decode(object_copy_buffer)?;
        new_child_first_key = first_object_node_key(object_copy_buffer, copied_parent)?;
        new_child_block = next_block;
        next_block = next_block
            .checked_add(1)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    }

    let free_space_block = next_block;
    if free_space_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?
        != allocation.allocated.end_block_exclusive()?
    {
        return Err(PhoenixFsError::InvalidTransactionRoots);
    }
    materialize_free_space_leaf_after_allocation(
        current_free_space,
        next_free_space,
        total_blocks,
        generation,
        free_space_block,
        allocation,
    )?;
    write_filesystem_block(device, free_space_block, next_free_space)?;

    let mut retired_blocks = [0_u64; MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS];
    let mut retired_count = 0_usize;
    push_unique_retired(&mut retired_blocks, &mut retired_count, metadata_path.leaf_block)?;
    for index in 0..metadata_path.parent_count {
        push_unique_retired(
            &mut retired_blocks,
            &mut retired_count,
            metadata_path.parents[index].block_number,
        )?;
    }
    push_unique_retired(
        &mut retired_blocks,
        &mut retired_count,
        current.superblock.roots.free_space_tree,
    )?;
    append_retired_extent_blocks(&mut retired_blocks, &mut retired_count, old_extent)?;
    sort_u64_prefix(&mut retired_blocks, retired_count);

    let roots = TransactionRoots::new(new_child_block, free_space_block);
    let plan = TransactionCommitPlan::new(current, roots, &retired_blocks[..retired_count])?;
    let committed = commit_transaction(device, current, plan, superblock_buffer)?;
    Ok(DeepRecordUpdateResult {
        active: committed.active,
        retired_blocks,
        retired_count,
        allocated: allocation.allocated,
    })
}

'''
if commit not in s:
    if commit_anchor not in s:
        raise SystemExit('commit anchor missing')
    s = s.replace(commit_anchor, commit + commit_anchor, 1)

old = '''    fn truncate_node(&mut self, _node: NodeId, _length: u64) -> Result<(), VfsError> {\n        Err(VfsError::ReadOnly)\n    }\n'''
new = '''    fn truncate_node(&mut self, node: NodeId, length: u64) -> Result<(), VfsError> {\n        let metadata = self.metadata(node)?;\n        if metadata.kind != NodeKind::File {\n            return Err(VfsError::IsDirectory);\n        }\n        if length == metadata.length {\n            return Ok(());\n        }\n        if length != 0 || metadata.length == 0 {\n            return Err(VfsError::InvalidOffset);\n        }\n\n        let result = commit_truncate_single_extent_file_to_zero(\n            self.device.get_mut(),\n            self.active,\n            node.0,\n            self.node_buffer.get_mut(),\n            &mut self.object_copy_buffer,\n            &mut self.current_free_space_buffer,\n            &mut self.next_free_space_buffer,\n            &mut self.superblock_buffer,\n        )\n        .map_err(map_phoenix_fs_to_vfs)?;\n        self.active = result.active;\n        Ok(())\n    }\n'''
if old not in s:
    raise SystemExit('truncate anchor missing')
s = s.replace(old, new, 1)

end_anchor = '''    #[test]\n    fn rejects_roots_outside_data_area() {\n'''
test = r'''    #[test]
    fn phoenix_vfs_truncates_existing_file_to_zero_and_remounts() {
        use phoenix_vfs::{FileSystem, NodeId, NodeKind};

        let device = MemoryBlockDevice::<512, 256>::new();
        let mut buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut filesystem = PhoenixVfs::format_new(device, VOLUME_ID, 1, &mut buffer).unwrap();
        let file = filesystem
            .create_node(NodeId(ROOT_OBJECT_ID), b"truncate.txt", NodeKind::File)
            .unwrap();
        assert_eq!(filesystem.write_node(file, 0, b"hello").unwrap(), 5);

        let before = filesystem.active_superblock().superblock.generation;
        filesystem.truncate_node(file, 0).unwrap();
        assert_eq!(filesystem.metadata(file).unwrap().length, 0);
        assert_eq!(filesystem.active_superblock().superblock.generation, before + 1);
        assert_eq!(filesystem.truncate_node(file, 0), Ok(()));
        assert_eq!(filesystem.truncate_node(file, 1), Err(VfsError::InvalidOffset));

        let device = filesystem.into_device();
        let mut first = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let remounted = PhoenixVfs::mount(device, &mut first, &mut second).unwrap();
        assert_eq!(remounted.metadata(file).unwrap().length, 0);
        let mut output = [0_u8; 5];
        assert_eq!(remounted.read_node(file, 0, &mut output).unwrap(), 0);
    }

'''
if test not in s:
    if end_anchor not in s:
        raise SystemExit('test anchor missing')
    s = s.replace(end_anchor, test + end_anchor, 1)

p.write_text(s)

roadmap = Path('ROADMAP.md')
r = roadmap.read_text()
line = '- PhoenixFS: через общий ВФС поддержано атомарное усечение одноэкстентного файла до нулевой длины; частичное усечение и расширение остаются следующими шагами.\n'
if line not in r:
    roadmap.write_text(r + '\n' + line)

status = Path('STATUS.md')
t = status.read_text()
line = '- PhoenixFS: ВФС поддерживает атомарное усечение существующего одноэкстентного файла до нуля; метаданные и удаление экстента публикуются одним поколением, старые блоки данных переходят в отложенное освобождение. Частичное усечение и расширение пока не реализованы.\n'
if line not in t:
    status.write_text(t + '\n' + line)
