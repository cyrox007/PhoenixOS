from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: ожидалось одно совпадение, найдено {count}")
    return text.replace(old, new, 1)


path = Path("crates/phoenix-fs/src/lib.rs")
text = path.read_text()

old = '''#[allow(clippy::too_many_arguments)]
pub fn commit_rewrite_single_extent_file<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    object_id: u64,
    data: &[u8],
'''
new = '''#[allow(clippy::too_many_arguments)]
fn rewrite_data_extent_range<D: BlockDevice>(
    device: &mut D,
    old_extent: ExtentValue,
    new_start_block: u64,
    offset: u64,
    data: &[u8],
    block_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<u64, PhoenixFsError> {
    if data.is_empty() {
        return Err(PhoenixFsError::InvalidExtent);
    }

    let data_len = u64::try_from(data.len()).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    let write_end = offset
        .checked_add(data_len)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    if write_end > old_extent.data_bytes {
        return Err(PhoenixFsError::InvalidExtent);
    }

    let block_size = FILESYSTEM_BLOCK_SIZE as u64;
    for block_index in 0..old_extent.block_count {
        let old_block = old_extent
            .physical_start_block
            .checked_add(block_index)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        read_filesystem_block(device, old_block, block_buffer)?;

        let block_start = block_index
            .checked_mul(block_size)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        let block_end = block_start
            .checked_add(block_size)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        let overlap_start = core::cmp::max(block_start, offset);
        let overlap_end = core::cmp::min(block_end, write_end);

        if overlap_start < overlap_end {
            let destination_start = usize::try_from(overlap_start - block_start)
                .map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
            let source_start = usize::try_from(overlap_start - offset)
                .map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
            let copy_len = usize::try_from(overlap_end - overlap_start)
                .map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
            let destination_end = destination_start
                .checked_add(copy_len)
                .ok_or(PhoenixFsError::ArithmeticOverflow)?;
            let source_end = source_start
                .checked_add(copy_len)
                .ok_or(PhoenixFsError::ArithmeticOverflow)?;
            block_buffer[destination_start..destination_end]
                .copy_from_slice(&data[source_start..source_end]);
        }

        let new_block = new_start_block
            .checked_add(block_index)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        write_filesystem_block(device, new_block, block_buffer)?;
    }

    Ok(old_extent.block_count)
}

pub fn commit_rewrite_single_extent_range<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    object_id: u64,
    offset: u64,
    data: &[u8],
'''
text = replace_once(text, old, new, "добавление диапазонной перезаписи")

old = '''    let data_len = u64::try_from(data.len()).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    if metadata.object_type != ObjectType::File
        || metadata.size_bytes == 0
        || metadata.size_bytes != data_len
    {
'''
new = '''    let data_len = u64::try_from(data.len()).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    let write_end = offset
        .checked_add(data_len)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    if metadata.object_type != ObjectType::File
        || metadata.size_bytes == 0
        || write_end > metadata.size_bytes
    {
'''
text = replace_once(text, old, new, "проверка границ диапазона")

old = '''    let data_blocks = old_extent.block_count;
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
'''
new = '''    let data_blocks = old_extent.block_count;
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
    let written_blocks = rewrite_data_extent_range(
        device,
        old_extent,
        data_start_block,
        offset,
        data,
        data_buffer,
    )?;
'''
text = replace_once(text, old, new, "копирование старого экстента")

text = replace_once(
    text,
    "    let next_extent = ExtentValue::new(data_start_block, data_blocks, data_len);\n",
    "    let next_extent = ExtentValue::new(data_start_block, data_blocks, old_extent.data_bytes);\n",
    "сохранение размера экстента",
)

old = '''        if offset != 0 {
            return Err(VfsError::InvalidOffset);
        }

        let result = if metadata.length == 0 {
'''
new = '''        let data_len = u64::try_from(data.len()).map_err(|_| VfsError::InvalidOffset)?;
        let write_end = offset
            .checked_add(data_len)
            .ok_or(VfsError::InvalidOffset)?;

        let result = if metadata.length == 0 && offset == 0 {
'''
text = replace_once(text, old, new, "VFS проверка смещения")

old = '''        } else if metadata.length == data.len() as u64 {
            commit_rewrite_single_extent_file(
                self.device.get_mut(),
                self.active,
                node.0,
                data,
'''
new = '''        } else if metadata.length != 0 && write_end <= metadata.length {
            commit_rewrite_single_extent_range(
                self.device.get_mut(),
                self.active,
                node.0,
                offset,
                data,
'''
text = replace_once(text, old, new, "VFS диапазонная запись")

text = replace_once(
    text,
    '            filesystem.write_node(file, 0, b"second"),\n',
    '            filesystem.write_node(file, data.len() as u64, b"second"),\n',
    "проверка записи за концом файла",
)

test_anchor = '''    #[test]
    fn rejects_roots_outside_data_area() {
'''
test = '''    #[test]
    fn phoenix_vfs_rewrites_range_inside_existing_file_atomically() {
        use phoenix_vfs::{FileSystem, NodeId, NodeKind};

        let device = MemoryBlockDevice::<512, 128>::new();
        let mut buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut filesystem = PhoenixVfs::format_new(device, VOLUME_ID, 1, &mut buffer).unwrap();
        let file = filesystem
            .create_node(NodeId(ROOT_OBJECT_ID), b"note.txt", NodeKind::File)
            .unwrap();

        assert_eq!(filesystem.write_node(file, 0, b"hello world").unwrap(), 11);
        let first_generation = filesystem.active_superblock().superblock.generation;
        assert_eq!(filesystem.write_node(file, 6, b"there").unwrap(), 5);
        assert_eq!(
            filesystem.active_superblock().superblock.generation,
            first_generation + 1
        );

        let mut output = [0_u8; 11];
        assert_eq!(filesystem.read_node(file, 0, &mut output).unwrap(), 11);
        assert_eq!(&output, b"hello there");

        let device = filesystem.into_device();
        let mut first = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let remounted = PhoenixVfs::mount(device, &mut first, &mut second).unwrap();
        let mut output = [0_u8; 11];
        assert_eq!(remounted.read_node(file, 0, &mut output).unwrap(), 11);
        assert_eq!(&output, b"hello there");
    }

'''
text = replace_once(text, test_anchor, test + test_anchor, "сквозная проверка диапазонной записи")

path.write_text(text)

roadmap = Path("ROADMAP.md")
roadmap_text = roadmap.read_text()
roadmap_text = replace_once(
    roadmap_text,
    "  - атомарные операции изменения экстентов, записи произвольных диапазонов и усечения через ВФС (создание объектов, запись нового пустого файла и полная перезапись одноэкстентного файла того же размера уже реализованы);",
    "  - атомарные операции изменения экстентов, расширения файла и частичного усечения через ВФС (создание объектов, запись нового пустого файла, запись произвольного диапазона внутри существующего одноэкстентного файла и усечение до нуля уже реализованы);",
    "ROADMAP",
)
roadmap.write_text(roadmap_text)

status = Path("STATUS.md")
status_text = status.read_text()
needle = "PhoenixFS теперь также умеет атомарно перезаписывать существующий одноэкстентный файл целиком без изменения размера: новые блоки данных и метаданные публикуются новым поколением, а старые блоки данных включаются в отложенное освобождение только после переключения суперблока."
replacement = "PhoenixFS теперь также умеет атомарно записывать произвольный диапазон внутри существующего одноэкстентного файла без изменения его размера: незатронутые блоки копируются в новый физический экстент, изменяемый диапазон накладывается поверх копии, а старые блоки данных включаются в отложенное освобождение только после переключения суперблока."
status_text = replace_once(status_text, needle, replacement, "STATUS")
status.write_text(status_text)
