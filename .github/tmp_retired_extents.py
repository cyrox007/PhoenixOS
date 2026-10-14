from pathlib import Path
import re

p = Path('crates/phoenix-fs/src/lib.rs')
s = p.read_text()

s = s.replace(
    'pub const MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS: usize = MAX_OBJECT_TREE_DEPTH * 2 + 3;\n',
    'pub const MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS: usize = MAX_OBJECT_TREE_DEPTH * 2 + 3;\n'
    'pub const MAX_OBJECT_TRANSACTION_RETIRED_EXTENTS: usize = 4;\n'
)

old = '''pub struct DeepRecordUpdateResult {
    pub active: ActiveSuperblock,
    pub retired_blocks: [u64; MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS],
    pub retired_count: usize,
    pub allocated: FreeSpaceExtent,
}

impl DeepRecordUpdateResult {
    pub fn retired_blocks(&self) -> &[u64] {
        &self.retired_blocks[..self.retired_count]
    }
}'''
new = '''pub struct DeepRecordUpdateResult {
    pub active: ActiveSuperblock,
    pub retired_blocks: [u64; MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS],
    pub retired_count: usize,
    pub retired_extents: [FreeSpaceExtent; MAX_OBJECT_TRANSACTION_RETIRED_EXTENTS],
    pub retired_extent_count: usize,
    pub allocated: FreeSpaceExtent,
}

impl DeepRecordUpdateResult {
    pub fn retired_blocks(&self) -> &[u64] {
        &self.retired_blocks[..self.retired_count]
    }

    pub fn retired_extents(&self) -> &[FreeSpaceExtent] {
        &self.retired_extents[..self.retired_extent_count]
    }
}'''
assert old in s
s = s.replace(old, new)

# Все существующие результаты по умолчанию не имеют диапазонов данных.
s = re.sub(
    r'(retired_count,\n)(\s+)(allocated:)',
    r'\1\2retired_extents: [FreeSpaceExtent::new(0, 0); MAX_OBJECT_TRANSACTION_RETIRED_EXTENTS],\n\2retired_extent_count: 0,\n\2\3',
    s,
)

# Старый путь разворачивал каждый блок пользовательского экстента в фиксированный массив.
fn_pat = re.compile(r'''\nfn append_retired_extent_blocks\(.*?\n\}\n''', re.S)
m = fn_pat.search(s)
assert m, 'append_retired_extent_blocks not found'
s = s[:m.start()] + '\n' + s[m.end():]

call = 'append_retired_extent_blocks(&mut retired_blocks, &mut retired_count, old_extent)?;'
replacement = '''let retired_data_extent = FreeSpaceExtent::new(
        old_extent.physical_start_block,
        old_extent.block_count,
    );'''
assert s.count(call) == 4, s.count(call)
s = s.replace(call, replacement)

# В четырёх транзакциях переписывания файла возвращаем старый пользовательский экстент диапазоном.
needle = '''retired_extents: [FreeSpaceExtent::new(0, 0); MAX_OBJECT_TRANSACTION_RETIRED_EXTENTS],
        retired_extent_count: 0,'''
pos = 0
changed = 0
while True:
    marker = s.find('let retired_data_extent =', pos)
    if marker < 0:
        break
    result = s.find('DeepRecordUpdateResult {', marker)
    assert result >= 0
    end = s.find('}', result)
    segment = s[result:end]
    assert needle in segment
    segment = segment.replace(
        needle,
        '''retired_extents: {
            let mut extents = [FreeSpaceExtent::new(0, 0); MAX_OBJECT_TRANSACTION_RETIRED_EXTENTS];
            extents[0] = retired_data_extent;
            extents
        },
        retired_extent_count: 1,''',
        1,
    )
    s = s[:result] + segment + s[end:]
    pos = result + len(segment)
    changed += 1
assert changed == 4, changed

# Диапазонный возврат свободного места. Старый интерфейс блоков сохраняется для совместимости.
anchor = 'pub fn materialize_free_space_leaf_with_reclaimed_blocks('
idx = s.index(anchor)
insert = r'''pub fn materialize_free_space_leaf_with_reclaimed_extents(
    current: &[u8],
    destination: &mut [u8],
    total_blocks: u64,
    new_generation: u64,
    new_block_number: u64,
    reclaimed_extents: &[FreeSpaceExtent],
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let current_node = validate_free_space_leaf(current, total_blocks)?;
    if destination.len() != FILESYSTEM_BLOCK_SIZE {
        return Err(PhoenixFsError::BufferSize);
    }
    if new_generation <= current_node.metadata.generation {
        return Err(PhoenixFsError::GenerationSequence);
    }
    if new_block_number < SUPERBLOCK_COPY_COUNT || new_block_number >= total_blocks {
        return Err(PhoenixFsError::InvalidMetadataBlock(new_block_number));
    }

    validate_reclaimed_extents_input(reclaimed_extents, total_blocks, new_block_number)?;

    destination.fill(0);
    let entries_start = TreeNodeHeader::entries_offset();
    let entries_bytes = current_node.entries_bytes as usize;
    let entries_end = entries_start
        .checked_add(entries_bytes)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    destination[entries_start..entries_end].copy_from_slice(&current[entries_start..entries_end]);

    let mut item_count = current_node.item_count as usize;
    for extent in reclaimed_extents.iter().copied() {
        insert_reclaimed_extent(destination, &mut item_count, extent)?;
    }

    let entries_bytes = item_count
        .checked_mul(FREE_SPACE_RECORD_SIZE)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let next = TreeNodeHeader::new(
        MetadataKind::FreeSpaceTree,
        new_generation,
        new_block_number,
        0,
        item_count as u32,
        entries_bytes as u32,
    )?;
    next.seal(destination)?;
    validate_free_space_leaf(destination, total_blocks)?;
    Ok(next)
}

fn validate_reclaimed_extents_input(
    reclaimed_extents: &[FreeSpaceExtent],
    total_blocks: u64,
    new_block_number: u64,
) -> Result<(), PhoenixFsError> {
    let mut previous_end = None;
    for extent in reclaimed_extents.iter().copied() {
        extent.validate(total_blocks)?;
        let end = extent.end_block_exclusive()?;
        if extent.contains(new_block_number)? {
            return Err(PhoenixFsError::InvalidReclaimedBlock(new_block_number));
        }
        if previous_end.is_some_and(|previous| previous > extent.start_block) {
            return Err(PhoenixFsError::InvalidReclaimedBlock(extent.start_block));
        }
        previous_end = Some(end);
    }
    Ok(())
}

fn insert_reclaimed_extent(
    block: &mut [u8],
    item_count: &mut usize,
    reclaimed: FreeSpaceExtent,
) -> Result<(), PhoenixFsError> {
    let reclaimed_end = reclaimed.end_block_exclusive()?;
    let mut index = 0_usize;
    while index < *item_count {
        let extent = free_space_extent_at(block, index)?;
        if reclaimed.start_block < extent.end_block_exclusive()? {
            break;
        }
        index += 1;
    }

    let previous = if index == 0 {
        None
    } else {
        Some(free_space_extent_at(block, index - 1)?)
    };
    let next = if index == *item_count {
        None
    } else {
        Some(free_space_extent_at(block, index)?)
    };

    if previous.is_some_and(|extent| reclaimed.start_block < extent.end_block_exclusive().unwrap_or(u64::MAX))
        || next.is_some_and(|extent| reclaimed_end > extent.start_block)
    {
        return Err(PhoenixFsError::ReclaimedBlockAlreadyFree(reclaimed.start_block));
    }

    let joins_previous = previous
        .map(|extent| extent.end_block_exclusive() == Ok(reclaimed.start_block))
        .unwrap_or(false);
    let joins_next = next
        .map(|extent| reclaimed_end == extent.start_block)
        .unwrap_or(false);

    if joins_previous && joins_next {
        let previous = previous.ok_or(PhoenixFsError::InvalidFreeSpaceRange)?;
        let next = next.ok_or(PhoenixFsError::InvalidFreeSpaceRange)?;
        let next_end = next.end_block_exclusive()?;
        let merged = FreeSpaceExtent::new(
            previous.start_block,
            next_end
                .checked_sub(previous.start_block)
                .ok_or(PhoenixFsError::ArithmeticOverflow)?,
        );
        write_free_space_extent(block, index - 1, merged)?;
        remove_free_space_extent(block, item_count, index)?;
        return Ok(());
    }

    if joins_previous {
        let previous = previous.ok_or(PhoenixFsError::InvalidFreeSpaceRange)?;
        let merged = FreeSpaceExtent::new(
            previous.start_block,
            reclaimed_end
                .checked_sub(previous.start_block)
                .ok_or(PhoenixFsError::ArithmeticOverflow)?,
        );
        return write_free_space_extent(block, index - 1, merged);
    }

    if joins_next {
        let next = next.ok_or(PhoenixFsError::InvalidFreeSpaceRange)?;
        let next_end = next.end_block_exclusive()?;
        let merged = FreeSpaceExtent::new(
            reclaimed.start_block,
            next_end
                .checked_sub(reclaimed.start_block)
                .ok_or(PhoenixFsError::ArithmeticOverflow)?,
        );
        return write_free_space_extent(block, index, merged);
    }

    insert_free_space_extent(block, item_count, index, reclaimed)
}

'''
s = s[:idx] + insert + s[idx:]

# Документация.
roadmap = Path('ROADMAP.md')
r = roadmap.read_text()
old_line = '  - [x] атомарные операции изменения экстентов, расширения файла и частичного усечения через ВФС для одноэкстентного файла (создание объектов, первая запись, запись диапазона без изменения размера, запись с увеличением размера и нулевым заполнением разрыва, расширение через изменение длины, частичное усечение и усечение до нуля);'
assert old_line in r
r = r.replace(old_line, old_line + '\n  - [x] возврат освобождённых пользовательских блоков диапазонами экстентов без зависимости от размера файла;')
roadmap.write_text(r)

status = Path('STATUS.md')
st = status.read_text()
st += '''\n\n## PhoenixFS — диапазонный возврат освобождённых блоков\n\nСтарые пользовательские экстенты больше не разворачиваются в фиксированный массив отдельных номеров блоков. Результат транзакции хранит освобождённые пользовательские области как диапазоны `FreeSpaceExtent`, поэтому размер файла больше не ограничен вместимостью списка `MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS`. Метаданные по-прежнему учитываются отдельными блоками, а пользовательские данные возвращаются диапазонами только после успешного переключения поколения суперблока. Добавлен прямой диапазонный путь объединения освобождённых областей с деревом свободного места.\n'''
status.write_text(st)

# Тест диапазона, который заведомо больше старой вместимости списка блоков.
test_anchor = '\n    #[test]\n    fn reclaimed_blocks_merge_adjacent_free_ranges()'
tidx = s.index(test_anchor)
test = r'''
    #[test]
    fn reclaimed_large_extent_merges_without_block_enumeration() {
        let mut current = [0_u8; FILESYSTEM_BLOCK_SIZE];
        write_free_space_extent(&mut current, 0, FreeSpaceExtent::new(10, 2)).unwrap();
        write_free_space_extent(&mut current, 1, FreeSpaceExtent::new(60, 2)).unwrap();
        TreeNodeHeader::new(
            MetadataKind::FreeSpaceTree,
            7,
            9,
            0,
            2,
            (2 * FREE_SPACE_RECORD_SIZE) as u32,
        )
        .unwrap()
        .seal(&mut current)
        .unwrap();

        let reclaimed = [FreeSpaceExtent::new(
            12,
            MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS as u64 + 10,
        )];
        let mut next = [0_u8; FILESYSTEM_BLOCK_SIZE];
        materialize_free_space_leaf_with_reclaimed_extents(
            &current,
            &mut next,
            128,
            8,
            100,
            &reclaimed,
        )
        .unwrap();

        assert_eq!(free_space_extent_at(&next, 0).unwrap(), FreeSpaceExtent::new(10, 2 + MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS as u64 + 10));
        assert_eq!(free_space_extent_at(&next, 1).unwrap(), FreeSpaceExtent::new(60, 2));
    }
'''
s = s[:tidx] + test + s[tidx:]

p.write_text(s)
