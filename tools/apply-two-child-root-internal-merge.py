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

old_guard = '''    validate_path_pointer(object_buffer, grandparent_step)?;
    if grandparent.item_count <= 2 {
        return Ok(None);
    }

    let mut grandparent_source = [0_u8; FILESYSTEM_BLOCK_SIZE];
'''
new_guard = '''    validate_path_pointer(object_buffer, grandparent_step)?;
    if grandparent.item_count < 2 {
        return Ok(None);
    }
    let collapse_two_child_root = grandparent_position == 0 && grandparent.item_count == 2;

    let mut grandparent_source = [0_u8; FILESYSTEM_BLOCK_SIZE];
'''
lib = replace_once(lib, old_guard, new_guard, "разрешение слияния под двухдочерним корнем")

old_allocation = '''    let left_index = core::cmp::min(child_index, sibling_index);
    let anchor_is_left = child_index < sibling_index;
    let requested_blocks =
        u64::try_from(grandparent_position + 3).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    let allocation = plan_cow_allocation(
'''
new_allocation = '''    let left_index = core::cmp::min(child_index, sibling_index);
    let anchor_is_left = child_index < sibling_index;
    let requested_blocks = if collapse_two_child_root {
        2
    } else {
        u64::try_from(grandparent_position + 3).map_err(|_| PhoenixFsError::ArithmeticOverflow)?
    };
    let allocation = plan_cow_allocation(
'''
lib = replace_once(lib, old_allocation, new_allocation, "расчёт блоков для схлопывания корня")

old_after_merge = '''    let merged_key = first_object_node_key(object_copy_buffer, merged)?;
    write_filesystem_block(device, merged_block, object_copy_buffer)?;

    let grandparent_block = merged_block
'''
new_after_merge = '''    let merged_key = first_object_node_key(object_copy_buffer, merged)?;
    write_filesystem_block(device, merged_block, object_copy_buffer)?;

    if collapse_two_child_root {
        let free_space_block = merged_block
            .checked_add(1)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
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

        let roots = TransactionRoots::new(merged_block, free_space_block);
        let plan = TransactionCommitPlan::new(current, roots, &retired_blocks[..retired_count])?;
        let committed = commit_transaction(device, current, plan, superblock_buffer)?;

        return Ok(Some(DeepRecordUpdateResult {
            active: committed.active,
            retired_blocks,
            retired_count,
            allocated: allocation.allocated,
        }));
    }

    let grandparent_block = merged_block
'''
lib = replace_once(lib, old_after_merge, new_after_merge, "схлопывание двухдочернего корня")

test_marker = '''    #[test]
    fn pruning_leaf_rebalances_sparse_adjacent_internal_nodes() {
'''

test_code = r'''    #[test]
    fn pruning_leaf_merges_internal_children_and_collapses_two_child_root() {
        let mut device = MemoryBlockDevice::<512, 1024>::new();
        let current = ActiveSuperblock {
            superblock: Superblock::new(5, 128, VOLUME_ID, TransactionRoots::new(8, 9)).unwrap(),
            slot: SuperblockSlot::First,
        };
        let start = TreeNodeHeader::entries_offset();
        let removed_key = ObjectTreeKey::new(1, ObjectRecordKind::Metadata, 0);
        let remaining_key = ObjectTreeKey::new(2, ObjectRecordKind::Metadata, 0);
        let sibling_key = ObjectTreeKey::new(3, ObjectRecordKind::Metadata, 0);

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
            ObjectInternalRecord::new(remaining_key, 13).unwrap(),
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
            2,
            (2 * OBJECT_INTERNAL_RECORD_SIZE) as u32,
        )
        .unwrap()
        .seal(&mut anchor)
        .unwrap();

        let mut sibling = [0_u8; FILESYSTEM_BLOCK_SIZE];
        ObjectInternalRecord::new(sibling_key, 15)
            .unwrap()
            .encode(&mut sibling[start..start + OBJECT_INTERNAL_RECORD_SIZE])
            .unwrap();
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            4,
            11,
            1,
            1,
            OBJECT_INTERNAL_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut sibling)
        .unwrap();

        let mut root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        for (index, record) in [
            ObjectInternalRecord::new(removed_key, 10).unwrap(),
            ObjectInternalRecord::new(sibling_key, 11).unwrap(),
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
        write_free_space_extent(&mut free_root, 0, FreeSpaceExtent::new(20, 80)).unwrap();
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

        assert_eq!(result.allocated.block_count, 2);
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
        assert_eq!(root_node.level, 1);
        assert_eq!(root_node.item_count, 2);
        assert_eq!(
            object_internal_record_at(&next_root, 0).unwrap().key,
            remaining_key
        );
        assert_eq!(
            object_internal_record_at(&next_root, 1).unwrap().key,
            sibling_key
        );
    }

'''
lib = replace_once(lib, test_marker, test_code + test_marker, "проверка схлопывания двухдочернего корня")
LIB.write_text(lib, encoding="utf-8")

roadmap = ROADMAP.read_text(encoding="utf-8")
roadmap = replace_once(
    roadmap,
    "- [ ] Реализовать слияние/перераспределение узлов при удалении.",
    "- [x] Реализовать слияние/перераспределение узлов при удалении.",
    "завершение балансировки при удалении",
)
ROADMAP.write_text(roadmap, encoding="utf-8")

status = STATUS.read_text(encoding="utf-8")
old_status = "Добавлено перераспределение дочерних указателей между разреженными соседними внутренними узлами: если после удаления объединение уже не помещается в один блок, а изменённый узел содержит меньше половины указателей соседа, оба узла материализуются заново и получают максимально равное число дочерних указателей. Общий родитель сохраняет два разделителя и одной CoW-транзакцией переводит их на новые узлы. Следующий проход должен закрыть краевой случай слияния внутренних узлов под двухдочерним корнем и после этого подтвердить общий этап балансировки при удалении. Подключение реального корневого тома ядра всё ещё ждёт системного блочного устройства."
new_status = "Добавлено перераспределение дочерних указателей между разреженными соседними внутренними узлами: если после удаления объединение уже не помещается в один блок, а изменённый узел содержит меньше половины указателей соседа, оба узла материализуются заново и получают максимально равное число дочерних указателей. Общий родитель сохраняет два разделителя и одной CoW-транзакцией переводит их на новые узлы. Закрыт краевой случай слияния внутренних узлов непосредственно под двухдочерним корнем: после удаления пустой ветви соседние внутренние узлы объединяются в новый CoW-узел, который сразу становится корнем дерева, поэтому лишний уровень не сохраняется. Общий этап слияния и перераспределения узлов при удалении завершён. Подключение реального корневого тома ядра всё ещё ждёт системного блочного устройства."
status = replace_once(status, old_status, new_status, "состояние PhoenixFS")
STATUS.write_text(status, encoding="utf-8")
