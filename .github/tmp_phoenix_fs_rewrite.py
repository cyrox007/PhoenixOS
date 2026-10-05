from pathlib import Path

path = Path('crates/phoenix-fs/src/lib.rs')
text = path.read_text()

anchor = '''fn write_data_extent<D: BlockDevice>(
'''
addition = r'''fn materialize_object_leaf_with_two_replacements(
    current: &[u8],
    destination: &mut [u8],
    first_key: ObjectTreeKey,
    first_value: &[u8],
    second_key: ObjectTreeKey,
    second_value: &[u8],
    new_generation: u64,
    new_block_number: u64,
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let current_node = validate_object_leaf(current)?;
    validate_rewrite_target(current_node, new_generation, new_block_number)?;
    first_key.validate()?;
    second_key.validate()?;
    if first_key == second_key {
        return Err(PhoenixFsError::InvalidObjectMutationBatch);
    }

    let entries_start = TreeNodeHeader::entries_offset();
    let entries_end = entries_start
        .checked_add(current_node.entries_bytes as usize)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    destination.fill(0);

    let mut source_cursor = entries_start;
    let mut destination_cursor = entries_start;
    let mut first_replaced = false;
    let mut second_replaced = false;

    for _ in 0..current_node.item_count {
        let (record, old_value, source_size) =
            ObjectLeafRecordHeader::decode_with_value(&current[source_cursor..entries_end])?;
        let value = if record.key == first_key {
            first_replaced = true;
            first_value
        } else if record.key == second_key {
            second_replaced = true;
            second_value
        } else {
            old_value
        };
        let value_bytes =
            u32::try_from(value.len()).map_err(|_| PhoenixFsError::InvalidObjectRecordSize)?;
        let next_record = ObjectLeafRecordHeader::new(record.key, value_bytes)?;
        destination_cursor =
            encode_leaf_record_at(destination, destination_cursor, next_record, value)?;
        source_cursor = source_cursor
            .checked_add(source_size)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    }

    if !first_replaced || !second_replaced {
        return Err(PhoenixFsError::ObjectRecordNotFound);
    }

    seal_object_leaf_rewrite(
        destination,
        current_node,
        new_generation,
        new_block_number,
        current_node.item_count,
        destination_cursor,
    )
}

fn read_single_file_extent<D: BlockDevice>(
    device: &mut D,
    active: ActiveSuperblock,
    object_id: u64,
    node_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<(ObjectTreeKey, ExtentValue), PhoenixFsError> {
    let total_blocks = active.superblock.total_blocks;
    let mut found = None;

    scan_object_tree_records(
        device,
        active.superblock.roots.object_tree,
        active.superblock.generation,
        node_buffer,
        |key, value| {
            if key.object_id != object_id || key.kind != ObjectRecordKind::Extent {
                return Ok(false);
            }
            if found.is_some() {
                return Err(PhoenixFsError::InvalidObjectMutationBatch);
            }

            let extent = ExtentValue::decode(value)?;
            extent.validate_for_key(key, total_blocks)?;
            found = Some((key, extent));
            Ok(false)
        },
    )?;

    found.ok_or(PhoenixFsError::ObjectRecordNotFound)
}

fn append_retired_extent_blocks(
    retired_blocks: &mut [u64; MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS],
    retired_count: &mut usize,
    extent: ExtentValue,
) -> Result<(), PhoenixFsError> {
    for delta in 0..extent.block_count {
        let block = extent
            .physical_start_block
            .checked_add(delta)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        push_unique_retired(retired_blocks, retired_count, block)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn commit_rewrite_single_extent_file<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    object_id: u64,
    data: &[u8],
    object_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    object_copy_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    data_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    current_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    next_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    superblock_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<DeepRecordUpdateResult, PhoenixFsError> {
    if object_id == 0 || data.is_empty() {
        return Err(PhoenixFsError::InvalidObjectMutationBatch);
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
    let data_len = u64::try_from(data.len()).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    if metadata.object_type != ObjectType::File
        || metadata.size_bytes == 0
        || metadata.size_bytes != data_len
    {
        return Err(PhoenixFsError::InvalidObjectMutationBatch);
    }

    let (extent_key, old_extent) = read_single_file_extent(device, current, object_id, object_buffer)?;
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

    let metadata_retired = metadata_path
        .node_count()
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let data_retired = usize::try_from(old_extent.block_count)
        .map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
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
    if free_node.metadata.generation > current.superblock.generation {
        return Err(PhoenixFsError::ObjectNodeGenerationAhead);
    }

    let data_blocks = old_extent.block_count;
    let metadata_blocks = u64::try_from(metadata_path.node_count() + 1)
        .map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    let requested_blocks = data_blocks
        .checked_add(metadata_blocks)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let allocation = plan_cow_allocation(current_free_space, total_blocks, requested_blocks)?;
    let generation = current
        .superblock
        .generation
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    let data_start_block = allocation.allocated.start_block;
    let written_blocks = write_data_extent(device, data_start_block, data, data_buffer)?;
    if written_blocks != data_blocks {
        return Err(PhoenixFsError::InvalidExtent);
    }

    let next_metadata = ObjectMetadataValue::new(
        ObjectType::File,
        metadata.size_bytes,
        metadata.created_ns,
        metadata.modified_ns,
        metadata.changed_ns,
    );
    next_metadata.encode(&mut metadata_bytes)?;
    validate_object_record_value(metadata_key, &metadata_bytes, total_blocks)?;

    let next_extent = ExtentValue::new(data_start_block, data_blocks, data_len);
    let mut extent_bytes = [0_u8; EXTENT_VALUE_SIZE];
    next_extent.encode(&mut extent_bytes)?;
    validate_object_record_value(extent_key, &extent_bytes, total_blocks)?;

    read_filesystem_block(device, metadata_path.leaf_block, object_buffer)?;
    let object_block = data_start_block
        .checked_add(data_blocks)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let leaf = materialize_object_leaf_with_two_replacements(
        object_buffer,
        object_copy_buffer,
        metadata_key,
        &metadata_bytes,
        extent_key,
        &extent_bytes,
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
        let parent = validate_object_internal(object_buffer)?;
        if parent.metadata.generation > current.superblock.generation {
            return Err(PhoenixFsError::ObjectNodeGenerationAhead);
        }
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
    push_unique_retired(
        &mut retired_blocks,
        &mut retired_count,
        metadata_path.leaf_block,
    )?;
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
if addition not in text:
    text = text.replace(anchor, addition + anchor, 1)

old_write = '''        if offset != 0 || metadata.length != 0 {
            return Err(VfsError::InvalidOffset);
        }

        let result = commit_write_empty_file(
            self.device.get_mut(),
            self.active,
            node.0,
            data,
            self.node_buffer.get_mut(),
            &mut self.object_copy_buffer,
            self.data_buffer.get_mut(),
            &mut self.current_free_space_buffer,
            &mut self.next_free_space_buffer,
            &mut self.superblock_buffer,
        )
        .map_err(map_phoenix_fs_to_vfs)?;
'''
new_write = '''        if offset != 0 {
            return Err(VfsError::InvalidOffset);
        }

        let result = if metadata.length == 0 {
            commit_write_empty_file(
                self.device.get_mut(),
                self.active,
                node.0,
                data,
                self.node_buffer.get_mut(),
                &mut self.object_copy_buffer,
                self.data_buffer.get_mut(),
                &mut self.current_free_space_buffer,
                &mut self.next_free_space_buffer,
                &mut self.superblock_buffer,
            )
        } else if metadata.length == data.len() as u64 {
            commit_rewrite_single_extent_file(
                self.device.get_mut(),
                self.active,
                node.0,
                data,
                self.node_buffer.get_mut(),
                &mut self.object_copy_buffer,
                self.data_buffer.get_mut(),
                &mut self.current_free_space_buffer,
                &mut self.next_free_space_buffer,
                &mut self.superblock_buffer,
            )
        } else {
            return Err(VfsError::InvalidOffset);
        }
        .map_err(map_phoenix_fs_to_vfs)?;
'''
if old_write not in text:
    raise SystemExit('write_node anchor not found')
text = text.replace(old_write, new_write, 1)

end_anchor = '''    #[test]
    fn rejects_roots_outside_data_area() {
'''
test = r'''    #[test]
    fn phoenix_vfs_rewrites_existing_file_atomically() {
        use phoenix_vfs::{FileSystem, NodeId, NodeKind};

        let device = MemoryBlockDevice::<512, 1024>::new();
        let mut buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut filesystem = PhoenixVfs::format_new(device, VOLUME_ID, 1, &mut buffer).unwrap();
        let file = filesystem
            .create_node(NodeId(ROOT_OBJECT_ID), b"note.txt", NodeKind::File)
            .unwrap();

        assert_eq!(filesystem.write_node(file, 0, b"hello").unwrap(), 5);
        let first_generation = filesystem.active_superblock().superblock.generation;
        assert_eq!(filesystem.write_node(file, 0, b"world").unwrap(), 5);
        assert_eq!(
            filesystem.active_superblock().superblock.generation,
            first_generation + 1
        );

        let mut output = [0_u8; 5];
        assert_eq!(filesystem.read_node(file, 0, &mut output).unwrap(), 5);
        assert_eq!(&output, b"world");

        let device = filesystem.into_device();
        let mut first = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let remounted = PhoenixVfs::mount(device, &mut first, &mut second).unwrap();
        let mut output = [0_u8; 5];
        assert_eq!(remounted.read_node(file, 0, &mut output).unwrap(), 5);
        assert_eq!(&output, b"world");
    }

'''
if test not in text:
    text = text.replace(end_anchor, test + end_anchor, 1)

path.write_text(text)

roadmap = Path('ROADMAP.md')
roadmap_text = roadmap.read_text()
old = '  - атомарные операции создания объектов, изменения экстентов, записи и усечения через ВФС;'
new = '  - атомарные операции изменения экстентов, записи произвольных диапазонов и усечения через ВФС (создание объектов, запись нового пустого файла и полная перезапись одноэкстентного файла того же размера уже реализованы);'
if old in roadmap_text:
    roadmap_text = roadmap_text.replace(old, new, 1)
roadmap.write_text(roadmap_text)

status = Path('STATUS.md')
status_text = status.read_text()
marker = 'Подключение реального корневого тома ядра всё ещё ждёт системного блочного устройства.'
addition_status = ' PhoenixFS теперь также умеет атомарно перезаписывать существующий одноэкстентный файл целиком без изменения размера: новые блоки данных и метаданные публикуются новым поколением, а старые блоки данных включаются в отложенное освобождение только после переключения суперблока.'
if addition_status.strip() not in status_text:
    status_text = status_text.replace(marker, addition_status.strip() + ' ' + marker, 1)
status.write_text(status_text)
