from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
LIB = ROOT / "crates/phoenix-fs/src/lib.rs"
ROADMAP = ROOT / "ROADMAP.md"
STATUS = ROOT / "STATUS.md"


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise RuntimeError(f"{label}: ожидалось одно совпадение, найдено {count}")
    return text.replace(old, new, 1)


lib = LIB.read_text(encoding="utf-8")

commit_marker = """#[allow(clippy::too_many_arguments)]
fn commit_empty_leaf_prune<D: BlockDevice>(
"""

internal_rebalance = r'''fn combined_internal_record_after_child_delete(
    anchor: &[u8],
    sibling: &[u8],
    removed_child_index: usize,
    anchor_is_left: bool,
    merged_index: usize,
    anchor_item_count: usize,
    sibling_item_count: usize,
) -> Result<ObjectInternalRecord, PhoenixFsError> {
    if removed_child_index >= anchor_item_count || anchor_item_count == 0 {
        return Err(PhoenixFsError::InvalidObjectRewriteTarget);
    }

    let anchor_items = anchor_item_count - 1;
    let total_items = anchor_items
        .checked_add(sibling_item_count)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    if merged_index >= total_items {
        return Err(PhoenixFsError::InvalidObjectInternalRecordSize);
    }

    if anchor_is_left && merged_index < anchor_items {
        let source_index = if merged_index < removed_child_index {
            merged_index
        } else {
            merged_index + 1
        };
        return object_internal_record_at(anchor, source_index);
    }
    if anchor_is_left {
        return object_internal_record_at(sibling, merged_index - anchor_items);
    }
    if merged_index < sibling_item_count {
        return object_internal_record_at(sibling, merged_index);
    }

    let anchor_index = merged_index - sibling_item_count;
    let source_index = if anchor_index < removed_child_index {
        anchor_index
    } else {
        anchor_index + 1
    };
    object_internal_record_at(anchor, source_index)
}

fn materialize_internal_range_after_child_delete(
    anchor: &[u8],
    sibling: &[u8],
    destination: &mut [u8],
    new_generation: u64,
    new_block_number: u64,
    removed_child_index: usize,
    anchor_is_left: bool,
    range_start: usize,
    range_end: usize,
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let anchor_node = validate_object_internal(anchor)?;
    let sibling_node = validate_object_internal(sibling)?;
    if anchor_node.level != sibling_node.level
        || removed_child_index >= anchor_node.item_count as usize
        || new_generation <= anchor_node.metadata.generation
        || new_generation <= sibling_node.metadata.generation
        || new_block_number < SUPERBLOCK_COPY_COUNT
    {
        return Err(PhoenixFsError::InvalidObjectRewriteTarget);
    }

    let anchor_item_count = anchor_node.item_count as usize;
    let sibling_item_count = sibling_node.item_count as usize;
    let total_items = anchor_item_count
        .checked_sub(1)
        .and_then(|count| count.checked_add(sibling_item_count))
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let output_items = range_end
        .checked_sub(range_start)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    if range_start >= range_end
        || range_end > total_items
        || output_items > object_internal_capacity()
    {
        return Err(PhoenixFsError::BufferSize);
    }

    destination.fill(0);
    let start = TreeNodeHeader::entries_offset();
    for (output_index, merged_index) in (range_start..range_end).enumerate() {
        let record = combined_internal_record_after_child_delete(
            anchor,
            sibling,
            removed_child_index,
            anchor_is_left,
            merged_index,
            anchor_item_count,
            sibling_item_count,
        )?;
        let record_start = start
            .checked_add(
                output_index
                    .checked_mul(OBJECT_INTERNAL_RECORD_SIZE)
                    .ok_or(PhoenixFsError::ArithmeticOverflow)?,
            )
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        let record_end = record_start
            .checked_add(OBJECT_INTERNAL_RECORD_SIZE)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        record.encode(&mut destination[record_start..record_end])?;
    }

    let item_count =
        u32::try_from(output_items).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    let entries_bytes = output_items
        .checked_mul(OBJECT_INTERNAL_RECORD_SIZE)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let next = TreeNodeHeader::new(
        MetadataKind::ObjectTree,
        new_generation,
        new_block_number,
        anchor_node.level,
        item_count,
        entries_bytes as u32,
    )?;
    next.seal(destination)?;
    validate_object_internal(destination)?;
    Ok(next)
}

#[allow(clippy::too_many_arguments)]
fn try_commit_internal_rebalance_after_prune<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    path: ObjectTreePath,
    anchor_position: usize,
    object_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    object_copy_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    current_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    next_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    superblock_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<Option<DeepRecordUpdateResult>, PhoenixFsError> {
    if anchor_position == 0 {
        return Ok(None);
    }

    let anchor_step = path.parents[anchor_position];
    let grandparent_position = anchor_position - 1;
    let grandparent_step = path.parents[grandparent_position];

    read_filesystem_block(device, grandparent_step.block_number, object_buffer)?;
    let grandparent = validate_object_internal(object_buffer)?;
    if grandparent.metadata.generation > current.superblock.generation {
        return Err(PhoenixFsError::ObjectNodeGenerationAhead);
    }
    validate_path_pointer(object_buffer, grandparent_step)?;

    let mut grandparent_source = [0_u8; FILESYSTEM_BLOCK_SIZE];
    grandparent_source.copy_from_slice(object_buffer);
    let child_index = grandparent_step.child_index as usize;

    let mut anchor_source = [0_u8; FILESYSTEM_BLOCK_SIZE];
    read_filesystem_block(device, anchor_step.block_number, &mut anchor_source)?;
    let anchor_node = validate_object_internal(&anchor_source)?;
    if anchor_node.metadata.generation > current.superblock.generation {
        return Err(PhoenixFsError::ObjectNodeGenerationAhead);
    }
    validate_path_pointer(&anchor_source, anchor_step)?;

    let modified_count = anchor_node
        .item_count
        .checked_sub(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    if modified_count == 0 {
        return Ok(None);
    }

    let candidates = [
        (child_index + 1 < grandparent.item_count as usize).then_some(child_index + 1),
        child_index.checked_sub(1),
    ];
    let capacity = object_internal_capacity();
    let mut sibling_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
    let mut selected = None;

    for sibling_index in candidates.into_iter().flatten() {
        let sibling = object_internal_record_at(&grandparent_source, sibling_index)?;
        read_filesystem_block(device, sibling.child_block, &mut sibling_buffer)?;
        let sibling_node = validate_object_internal(&sibling_buffer)?;
        if sibling_node.metadata.generation > current.superblock.generation {
            return Err(PhoenixFsError::ObjectNodeGenerationAhead);
        }
        if sibling_node.level != anchor_node.level
            || sibling_node.metadata.block_number != sibling.child_block
            || first_object_node_key(&sibling_buffer, sibling_node)? != sibling.key
        {
            return Err(PhoenixFsError::ObjectTreePathMismatch);
        }

        let total_count = modified_count
            .checked_add(sibling_node.item_count)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        if total_count as usize <= capacity {
            continue;
        }
        if modified_count.checked_mul(2).unwrap_or(u32::MAX) >= sibling_node.item_count {
            continue;
        }

        selected = Some((sibling_index, sibling, total_count as usize));
        break;
    }

    let Some((sibling_index, sibling, total_items)) = selected else {
        return Ok(None);
    };
    let split_index = total_items / 2;
    if split_index == 0
        || split_index > capacity
        || total_items - split_index > capacity
    {
        return Err(PhoenixFsError::BufferSize);
    }

    let left_index = core::cmp::min(child_index, sibling_index);
    let anchor_is_left = child_index < sibling_index;
    let requested_blocks =
        u64::try_from(grandparent_position + 4).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    let allocation = plan_cow_allocation(
        current_free_space,
        current.superblock.total_blocks,
        requested_blocks,
    )?;
    let generation = current
        .superblock
        .generation
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    let left_block = allocation.allocated.start_block;
    let left = materialize_internal_range_after_child_delete(
        &anchor_source,
        &sibling_buffer,
        object_copy_buffer,
        generation,
        left_block,
        anchor_step.child_index as usize,
        anchor_is_left,
        0,
        split_index,
    )?;
    let left_key = first_object_node_key(object_copy_buffer, left)?;
    write_filesystem_block(device, left_block, object_copy_buffer)?;

    let right_block = left_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let right = materialize_internal_range_after_child_delete(
        &anchor_source,
        &sibling_buffer,
        object_copy_buffer,
        generation,
        right_block,
        anchor_step.child_index as usize,
        anchor_is_left,
        split_index,
        total_items,
    )?;
    let right_key = first_object_node_key(object_copy_buffer, right)?;
    write_filesystem_block(device, right_block, object_copy_buffer)?;

    let grandparent_block = right_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let rewrites = [
        ObjectChildRewrite::new(left_index as u32, left_block, left_key),
        ObjectChildRewrite::new((left_index + 1) as u32, right_block, right_key),
    ];
    let next_grandparent = materialize_object_internal_after_child_rewrites(
        &grandparent_source,
        object_copy_buffer,
        generation,
        grandparent_block,
        &rewrites,
    )?;
    write_filesystem_block(device, grandparent_block, object_copy_buffer)?;

    let grandparent_key = first_object_node_key(object_copy_buffer, next_grandparent)?;
    let mut next_block = grandparent_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let root = copy_object_parent_range(
        device,
        current,
        path,
        0,
        grandparent_position,
        grandparent_block,
        grandparent_key,
        generation,
        &mut next_block,
        object_buffer,
        object_copy_buffer,
    )?;

    let free_space_block = next_block;
    let allocation_end = allocation.allocated.end_block_exclusive()?;
    if free_space_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?
        != allocation_end
    {
        return Err(PhoenixFsError::InvalidTransactionRoots);
    }

    materialize_free_space_leaf_after_allocation(
        current_free_space,
        next_free_space,
        current.superblock.total_blocks,
        generation,
        free_space_block,
        allocation,
    )?;
    write_filesystem_block(device, free_space_block, next_free_space)?;

    let mut retired_blocks = [0_u64; MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS];
    let mut retired_count = 0_usize;
    push_unique_retired(&mut retired_blocks, &mut retired_count, path.leaf_block)?;
    for index in 0..path.parent_count {
        push_unique_retired(
            &mut retired_blocks,
            &mut retired_count,
            path.parents[index].block_number,
        )?;
    }
    push_unique_retired(&mut retired_blocks, &mut retired_count, sibling.child_block)?;
    push_unique_retired(
        &mut retired_blocks,
        &mut retired_count,
        current.superblock.roots.free_space_tree,
    )?;
    sort_u64_prefix(&mut retired_blocks, retired_count);

    let roots = TransactionRoots::new(root.0, free_space_block);
    let plan = TransactionCommitPlan::new(current, roots, &retired_blocks[..retired_count])?;
    let committed = commit_transaction(device, current, plan, superblock_buffer)?;

    Ok(Some(DeepRecordUpdateResult {
        active: committed.active,
        retired_blocks,
        retired_count,
        allocated: allocation.allocated,
    }))
}

'''

lib = replace_once(
    lib,
    commit_marker,
    internal_rebalance + commit_marker,
    "вставка реализации перераспределения",
)

merge_call = r'''    if let Some(result) = try_commit_internal_merge_after_prune(
        device,
        current,
        path,
        anchor_position,
        object_buffer,
        object_copy_buffer,
        current_free_space,
        next_free_space,
        superblock_buffer,
    )? {
        return Ok(result);
    }

    let requested_blocks =
'''

rebalance_call = r'''    if let Some(result) = try_commit_internal_merge_after_prune(
        device,
        current,
        path,
        anchor_position,
        object_buffer,
        object_copy_buffer,
        current_free_space,
        next_free_space,
        superblock_buffer,
    )? {
        return Ok(result);
    }

    if let Some(result) = try_commit_internal_rebalance_after_prune(
        device,
        current,
        path,
        anchor_position,
        object_buffer,
        object_copy_buffer,
        current_free_space,
        next_free_space,
        superblock_buffer,
    )? {
        return Ok(result);
    }

    let requested_blocks =
'''

lib = replace_once(lib, merge_call, rebalance_call, "подключение перераспределения")

test_marker = r'''    #[test]
    fn deleting_last_leaf_record_collapses_single_child_parent_chain() {
'''

test_code = r'''    #[test]
    fn pruning_leaf_rebalances_sparse_adjacent_internal_nodes() {
        let mut device = MemoryBlockDevice::<512, 4096>::new();
        let current = ActiveSuperblock {
            superblock: Superblock::new(5, 512, VOLUME_ID, TransactionRoots::new(8, 9)).unwrap(),
            slot: SuperblockSlot::First,
        };
        let start = TreeNodeHeader::entries_offset();
        let removed_key = ObjectTreeKey::new(1, ObjectRecordKind::Metadata, 0);
        let first_remaining_key = ObjectTreeKey::new(2, ObjectRecordKind::Metadata, 0);
        let second_remaining_key = ObjectTreeKey::new(3, ObjectRecordKind::Metadata, 0);
        let sibling_first_key = ObjectTreeKey::new(100, ObjectRecordKind::Metadata, 0);

        let mut removed_leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let removed_size = ObjectLeafRecordHeader::new(removed_key, 1)
            .unwrap()
            .encode_with_value(&mut removed_leaf[start..], b"a")
            .unwrap();
        TreeNodeHeader::new(MetadataKind::ObjectTree, 4, 12, 0, 1, removed_size as u32)
            .unwrap()
            .seal(&mut removed_leaf)
            .unwrap();

        let mut anchor = [0_u8; FILESYSTEM_BLOCK_SIZE];
        for (index, record) in [
            ObjectInternalRecord::new(removed_key, 12).unwrap(),
            ObjectInternalRecord::new(first_remaining_key, 13).unwrap(),
            ObjectInternalRecord::new(second_remaining_key, 14).unwrap(),
        ]
        .into_iter()
        .enumerate()
        {
            record
                .encode(
                    &mut anchor[start + index * OBJECT_INTERNAL_RECORD_SIZE
                        ..start + (index + 1) * OBJECT_INTERNAL_RECORD_SIZE],
                )
                .unwrap();
        }
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            4,
            10,
            1,
            3,
            (3 * OBJECT_INTERNAL_RECORD_SIZE) as u32,
        )
        .unwrap()
        .seal(&mut anchor)
        .unwrap();

        let sibling_items = object_internal_capacity();
        let mut sibling = [0_u8; FILESYSTEM_BLOCK_SIZE];
        for index in 0..sibling_items {
            let object_id = 100 + index as u64;
            ObjectInternalRecord::new(
                ObjectTreeKey::new(object_id, ObjectRecordKind::Metadata, 0),
                100 + index as u64,
            )
            .unwrap()
            .encode(
                &mut sibling[start + index * OBJECT_INTERNAL_RECORD_SIZE
                    ..start + (index + 1) * OBJECT_INTERNAL_RECORD_SIZE],
            )
            .unwrap();
        }
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            4,
            11,
            1,
            sibling_items as u32,
            (sibling_items * OBJECT_INTERNAL_RECORD_SIZE) as u32,
        )
        .unwrap()
        .seal(&mut sibling)
        .unwrap();

        let mut root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        for (index, record) in [
            ObjectInternalRecord::new(removed_key, 10).unwrap(),
            ObjectInternalRecord::new(sibling_first_key, 11).unwrap(),
        ]
        .into_iter()
        .enumerate()
        {
            record
                .encode(
                    &mut root[start + index * OBJECT_INTERNAL_RECORD_SIZE
                        ..start + (index + 1) * OBJECT_INTERNAL_RECORD_SIZE],
                )
                .unwrap();
        }
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            5,
            8,
            2,
            2,
            (2 * OBJECT_INTERNAL_RECORD_SIZE) as u32,
        )
        .unwrap()
        .seal(&mut root)
        .unwrap();

        let mut free_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        write_free_space_extent(&mut free_root, 0, FreeSpaceExtent::new(300, 100)).unwrap();
        TreeNodeHeader::new(
            MetadataKind::FreeSpaceTree,
            5,
            9,
            0,
            1,
            FREE_SPACE_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut free_root)
        .unwrap();

        write_filesystem_block(&mut device, 8, &root).unwrap();
        write_filesystem_block(&mut device, 10, &anchor).unwrap();
        write_filesystem_block(&mut device, 11, &sibling).unwrap();
        write_filesystem_block(&mut device, 12, &removed_leaf).unwrap();
        write_filesystem_block(&mut device, 9, &free_root).unwrap();

        let mut object_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut object_copy = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut current_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut next_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut superblock_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];

        let result = commit_object_record_delete(
            &mut device,
            current,
            removed_key,
            &mut object_buffer,
            &mut object_copy,
            &mut current_free,
            &mut next_free,
            &mut superblock_buffer,
        )
        .unwrap();

        assert_eq!(result.allocated.block_count, 4);
        assert!(result.retired_blocks().contains(&8));
        assert!(result.retired_blocks().contains(&10));
        assert!(result.retired_blocks().contains(&11));
        assert!(result.retired_blocks().contains(&12));

        let mut next_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(
            &mut device,
            result.active.superblock.roots.object_tree,
            &mut next_root,
        )
        .unwrap();
        let root_node = validate_object_internal(&next_root).unwrap();
        assert_eq!(root_node.item_count, 2);

        let left_pointer = object_internal_record_at(&next_root, 0).unwrap();
        let right_pointer = object_internal_record_at(&next_root, 1).unwrap();
        let mut left = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut right = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(&mut device, left_pointer.child_block, &mut left).unwrap();
        read_filesystem_block(&mut device, right_pointer.child_block, &mut right).unwrap();
        let left_node = validate_object_internal(&left).unwrap();
        let right_node = validate_object_internal(&right).unwrap();

        assert_eq!(left_node.item_count, 64);
        assert_eq!(right_node.item_count, 64);
        assert_eq!(
            object_internal_record_at(&left, 0).unwrap().key,
            first_remaining_key
        );
        assert_eq!(
            object_internal_record_at(&right, 0).unwrap().key,
            ObjectTreeKey::new(162, ObjectRecordKind::Metadata, 0)
        );
        assert_eq!(left_pointer.key, first_remaining_key);
        assert_eq!(
            right_pointer.key,
            ObjectTreeKey::new(162, ObjectRecordKind::Metadata, 0)
        );
    }

'''

lib = replace_once(lib, test_marker, test_code + test_marker, "вставка проверки перераспределения")
LIB.write_text(lib, encoding="utf-8")

roadmap = ROADMAP.read_text(encoding="utf-8")
roadmap = replace_once(
    roadmap,
    "- [ ] Реализовать перераспределение между соседними внутренними узлами.",
    "- [x] Реализовать перераспределение между соседними внутренними узлами.",
    "дорожная карта",
)
ROADMAP.write_text(roadmap, encoding="utf-8")

status = STATUS.read_text(encoding="utf-8")
status = replace_once(
    status,
    "Слияние распространено на соседние внутренние узлы одного уровня: после удаления пустой ветви два узла объединяются, если их дочерние указатели помещаются в один блок, а общий родитель сокращается на одну запись одной CoW-транзакцией. Следующий шаг — перераспределение дочерних указателей между разреженными соседними внутренними узлами. Подключение реального корневого тома ядра всё ещё ждёт системного блочного устройства.",
    "Слияние распространено на соседние внутренние узлы одного уровня: после удаления пустой ветви два узла объединяются, если их дочерние указатели помещаются в один блок, а общий родитель сокращается на одну запись одной CoW-транзакцией. Добавлено перераспределение дочерних указателей между разреженными соседними внутренними узлами: если после удаления объединение уже не помещается в один блок, а изменённый узел содержит меньше половины указателей соседа, оба узла материализуются заново и получают максимально равное число дочерних указателей. Общий родитель сохраняет два разделителя и одной CoW-транзакцией переводит их на новые узлы. Следующий проход должен закрыть краевой случай слияния внутренних узлов под двухдочерним корнем и после этого подтвердить общий этап балансировки при удалении. Подключение реального корневого тома ядра всё ещё ждёт системного блочного устройства.",
    "состояние проекта",
)
STATUS.write_text(status, encoding="utf-8")
