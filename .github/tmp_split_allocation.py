from pathlib import Path
import re

path = Path('crates/phoenix-fs/src/lib.rs')
text = path.read_text()

marker = '''pub fn materialize_free_space_leaf_after_allocation(
'''
if marker not in text:
    raise SystemExit('allocation materializer marker not found')

split_support = r'''#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitCowAllocationPlan {
    pub data: FreeSpaceExtent,
    pub metadata: FreeSpaceExtent,
}

pub fn plan_split_cow_allocation(
    free_space_leaf: &[u8],
    total_blocks: u64,
    data_blocks: u64,
    metadata_blocks: u64,
) -> Result<SplitCowAllocationPlan, PhoenixFsError> {
    if data_blocks == 0 || metadata_blocks == 0 {
        return Err(PhoenixFsError::InvalidFreeSpaceRange);
    }

    let node = validate_free_space_leaf(free_space_leaf, total_blocks)?;
    for data_index in 0..node.item_count as usize {
        let data_source = free_space_extent_at(free_space_leaf, data_index)?;
        if data_source.block_count < data_blocks {
            continue;
        }

        let data = FreeSpaceExtent::new(data_source.start_block, data_blocks);
        let data_end = data.end_block_exclusive()?;
        let same_extent_remaining = data_source.block_count - data_blocks;
        if same_extent_remaining >= metadata_blocks {
            return Ok(SplitCowAllocationPlan {
                data,
                metadata: FreeSpaceExtent::new(data_end, metadata_blocks),
            });
        }

        for metadata_index in 0..node.item_count as usize {
            if metadata_index == data_index {
                continue;
            }

            let metadata_source = free_space_extent_at(free_space_leaf, metadata_index)?;
            if metadata_source.block_count < metadata_blocks {
                continue;
            }

            return Ok(SplitCowAllocationPlan {
                data,
                metadata: FreeSpaceExtent::new(metadata_source.start_block, metadata_blocks),
            });
        }
    }

    Err(PhoenixFsError::NoFreeSpace)
}

pub fn materialize_free_space_leaf_after_split_allocation(
    current: &[u8],
    destination: &mut [u8],
    total_blocks: u64,
    new_generation: u64,
    new_block_number: u64,
    plan: SplitCowAllocationPlan,
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let current_node = validate_free_space_leaf(current, total_blocks)?;
    if destination.len() != FILESYSTEM_BLOCK_SIZE {
        return Err(PhoenixFsError::BufferSize);
    }
    if new_generation <= current_node.metadata.generation {
        return Err(PhoenixFsError::GenerationSequence);
    }
    if !plan.metadata.contains(new_block_number)? {
        return Err(PhoenixFsError::AllocationTargetOutsideRange);
    }

    let expected = plan_split_cow_allocation(
        current,
        total_blocks,
        plan.data.block_count,
        plan.metadata.block_count,
    )?;
    if expected != plan {
        return Err(PhoenixFsError::StaleAllocationPlan);
    }

    let allocations = if plan.data.start_block <= plan.metadata.start_block {
        [plan.data, plan.metadata]
    } else {
        [plan.metadata, plan.data]
    };

    destination.fill(0);
    let entries_start = TreeNodeHeader::entries_offset();
    let mut write_index = 0_usize;

    for index in 0..current_node.item_count as usize {
        let source = free_space_extent_at(current, index)?;
        let source_end = source.end_block_exclusive()?;
        let mut cursor = source.start_block;

        for allocation in allocations {
            let allocation_end = allocation.end_block_exclusive()?;
            if allocation_end <= source.start_block || allocation.start_block >= source_end {
                continue;
            }
            if allocation.start_block < cursor || allocation_end > source_end {
                return Err(PhoenixFsError::StaleAllocationPlan);
            }

            if allocation.start_block > cursor {
                write_free_space_extent(
                    destination,
                    write_index,
                    FreeSpaceExtent::new(cursor, allocation.start_block - cursor),
                )?;
                write_index += 1;
            }
            cursor = allocation_end;
        }

        if cursor < source_end {
            write_free_space_extent(
                destination,
                write_index,
                FreeSpaceExtent::new(cursor, source_end - cursor),
            )?;
            write_index += 1;
        }
    }

    let entries_bytes = write_index
        .checked_mul(FREE_SPACE_RECORD_SIZE)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let node = TreeNodeHeader::new(
        MetadataKind::FreeSpaceTree,
        new_generation,
        new_block_number,
        0,
        write_index as u32,
        entries_bytes as u32,
    )?;
    node.seal(destination)?;
    validate_free_space_leaf(destination, total_blocks)?;

    let data_end = entries_start
        .checked_add(entries_bytes)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    if destination[data_end..].iter().any(|byte| *byte != 0) {
        return Err(PhoenixFsError::InvalidReservedField);
    }

    Ok(node)
}

'''
text = text.replace(marker, split_support + marker, 1)

allocation_pattern = re.compile(r'''    let requested_blocks = data_blocks\n        \.checked_add\(metadata_blocks\)\n        \.ok_or\(PhoenixFsError::ArithmeticOverflow\)\?;\n    let allocation = plan_cow_allocation\(current_free_space, total_blocks, requested_blocks\)\?;''')
allocation_replacement = '''    let allocation = plan_split_cow_allocation(\n        current_free_space,\n        total_blocks,\n        data_blocks,\n        metadata_blocks,\n    )?;'''


def transform_function(source: str, name: str, end_marker: str) -> str:
    start = source.find(f'pub fn {name}')
    if start < 0:
        raise SystemExit(f'{name}: start not found')
    end = source.find(end_marker, start)
    if end < 0:
        raise SystemExit(f'{name}: end marker not found')
    block = source[start:end]

    block, count = allocation_pattern.subn(allocation_replacement, block)
    if count != 1:
        raise SystemExit(f'{name}: expected one combined allocation, got {count}')

    old = '    let data_start_block = allocation.allocated.start_block;'
    if block.count(old) != 1:
        raise SystemExit(f'{name}: data allocation marker mismatch')
    block = block.replace(old, '    let data_start_block = allocation.data.start_block;', 1)

    object_pattern = re.compile(r'''    let object_block = data_start_block\n        \.checked_add\(data_blocks\)\n        \.ok_or\(PhoenixFsError::ArithmeticOverflow\)\?;''')
    block, count = object_pattern.subn('    let object_block = allocation.metadata.start_block;', block)
    if count != 1:
        raise SystemExit(f'{name}: object block marker mismatch ({count})')

    old_end = '        != allocation.allocated.end_block_exclusive()?'
    if block.count(old_end) != 1:
        raise SystemExit(f'{name}: allocation end marker mismatch')
    block = block.replace(old_end, '        != allocation.metadata.end_block_exclusive()?', 1)

    old_materializer = '    materialize_free_space_leaf_after_allocation('
    if block.count(old_materializer) != 1:
        raise SystemExit(f'{name}: free-space materializer marker mismatch')
    block = block.replace(
        old_materializer,
        '    materialize_free_space_leaf_after_split_allocation(',
        1,
    )

    old_result = '        allocated: allocation.allocated,'
    if block.count(old_result) != 1:
        raise SystemExit(f'{name}: result allocation marker mismatch')
    block = block.replace(old_result, '        allocated: allocation.metadata,', 1)

    return source[:start] + block + source[end:]

for function_name, end_marker in [
    ('commit_rewrite_single_extent_range', '\n#[allow(clippy::too_many_arguments)]\npub fn commit_shrink_single_extent_file'),
    ('commit_shrink_single_extent_file', '\npub fn commit_extend_single_extent_file'),
    ('commit_extend_single_extent_file', '\n#[allow(clippy::too_many_arguments)]\npub fn commit_truncate_single_extent_file_to_zero'),
    ('commit_write_empty_file', '\npub fn commit_extend_empty_file'),
    ('commit_extend_empty_file', '\nimpl<D: BlockDevice> FileSystem for PhoenixVfs<D>'),
]:
    text = transform_function(text, function_name, end_marker)

# Regression: the old combined allocator cannot satisfy this fragmented leaf,
# while the split planner can reserve data and metadata independently.
test_marker = '''    #[test]\n    fn cow_allocation_uses_first_sufficient_extent() {'''
if test_marker not in text:
    raise SystemExit('cow allocation test marker not found')

new_test = r'''    #[test]
    fn split_cow_allocation_uses_fragmented_ranges_for_data_and_metadata() {
        let mut block = [0_u8; FILESYSTEM_BLOCK_SIZE];
        write_free_space_extent(&mut block, 0, FreeSpaceExtent::new(10, 8)).unwrap();
        write_free_space_extent(&mut block, 1, FreeSpaceExtent::new(30, 3)).unwrap();
        TreeNodeHeader::new(
            MetadataKind::FreeSpaceTree,
            5,
            9,
            0,
            2,
            (2 * FREE_SPACE_RECORD_SIZE) as u32,
        )
        .unwrap()
        .seal(&mut block)
        .unwrap();

        assert_eq!(
            plan_cow_allocation(&block, 64, 9),
            Err(PhoenixFsError::NoFreeSpace)
        );

        let plan = plan_split_cow_allocation(&block, 64, 6, 3).unwrap();
        assert_eq!(plan.data, FreeSpaceExtent::new(10, 6));
        assert_eq!(plan.metadata, FreeSpaceExtent::new(30, 3));

        let mut next = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let node = materialize_free_space_leaf_after_split_allocation(
            &block,
            &mut next,
            64,
            6,
            32,
            plan,
        )
        .unwrap();
        assert_eq!(node.item_count, 1);
        assert_eq!(free_space_extent_at(&next, 0).unwrap(), FreeSpaceExtent::new(16, 2));
    }

'''
text = text.replace(test_marker, new_test + test_marker, 1)
path.write_text(text)

status = Path('STATUS.md')
status_text = status.read_text()
status_text += '''\n\n## PhoenixFS — раздельное резервирование данных и служебных блоков\n\nПути записи, перезаписи, усечения и расширения одноэкстентных файлов больше не требуют одного непрерывного свободного диапазона одновременно для пользовательских данных и CoW-метаданных. Данные и служебная часть транзакции планируются независимо, но публикуются одной сменой поколения. Это позволяет использовать фрагментированное свободное место без ложного `NoFreeSpace`, сохраняя непрерывность каждого пользовательского экстента и служебного CoW-диапазона по отдельности. Добавлена регрессия, где прежний объединённый планировщик не может выделить девять блоков одним диапазоном, а новый план успешно размещает шесть блоков данных и три служебных блока в разных свободных диапазонах.\n'''
status.write_text(status_text)
