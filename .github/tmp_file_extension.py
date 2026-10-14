from pathlib import Path

LIB = Path('crates/phoenix-fs/src/lib.rs')
ROADMAP = Path('ROADMAP.md')
STATUS = Path('STATUS.md')
text = LIB.read_text()


def function_span(source: str, name: str):
    marker = f'fn {name}'
    marker_pos = source.index(marker)
    line_start = source.rfind('\n', 0, marker_pos) + 1
    brace = source.index('{', marker_pos)
    depth = 0
    for index in range(brace, len(source)):
        char = source[index]
        if char == '{':
            depth += 1
        elif char == '}':
            depth -= 1
            if depth == 0:
                return line_start, index + 1, source[line_start:index + 1]
    raise RuntimeError(name)


def replace_once(source: str, old: str, new: str, label: str) -> str:
    count = source.count(old)
    if count != 1:
        raise RuntimeError(f'{label}: ожидалось одно совпадение, найдено {count}')
    return source.replace(old, new, 1)


helpers = r'''fn write_zero_data_extent<D: BlockDevice>(
    device: &mut D,
    start_block: u64,
    data_bytes: u64,
    block_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<u64, PhoenixFsError> {
    if data_bytes == 0 {
        return Err(PhoenixFsError::InvalidExtent);
    }

    let block_size = FILESYSTEM_BLOCK_SIZE as u64;
    let block_count = data_bytes
        .checked_add(block_size - 1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?
        / block_size;
    block_buffer.fill(0);
    for block_index in 0..block_count {
        let block_number = start_block
            .checked_add(block_index)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        write_filesystem_block(device, block_number, block_buffer)?;
    }
    Ok(block_count)
}

fn copy_extended_data_extent<D: BlockDevice>(
    device: &mut D,
    old_extent: ExtentValue,
    new_start_block: u64,
    new_data_bytes: u64,
    block_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<u64, PhoenixFsError> {
    if old_extent.data_bytes == 0 || new_data_bytes <= old_extent.data_bytes {
        return Err(PhoenixFsError::InvalidExtent);
    }

    let block_size = FILESYSTEM_BLOCK_SIZE as u64;
    let new_block_count = new_data_bytes
        .checked_add(block_size - 1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?
        / block_size;
    if new_block_count < old_extent.block_count {
        return Err(PhoenixFsError::InvalidExtent);
    }

    for block_index in 0..new_block_count {
        if block_index < old_extent.block_count {
            let old_block = old_extent
                .physical_start_block
                .checked_add(block_index)
                .ok_or(PhoenixFsError::ArithmeticOverflow)?;
            read_filesystem_block(device, old_block, block_buffer)?;

            if block_index + 1 == old_extent.block_count {
                let block_start = block_index
                    .checked_mul(block_size)
                    .ok_or(PhoenixFsError::ArithmeticOverflow)?;
                let retained = old_extent
                    .data_bytes
                    .checked_sub(block_start)
                    .ok_or(PhoenixFsError::InvalidExtent)?;
                let retained = usize::try_from(retained)
                    .map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
                if retained < FILESYSTEM_BLOCK_SIZE {
                    block_buffer[retained..].fill(0);
                }
            }
        } else {
            block_buffer.fill(0);
        }

        let new_block = new_start_block
            .checked_add(block_index)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        write_filesystem_block(device, new_block, block_buffer)?;
    }

    Ok(new_block_count)
}

'''
anchor = 'fn copy_truncated_data_extent<D: BlockDevice>('
if 'fn write_zero_data_extent<D: BlockDevice>(' not in text:
    text = replace_once(text, anchor, helpers + anchor, 'вставка помощников расширения')

# Расширение непустого одноэкстентного файла строим из уже проверенного пути частичного усечения.
_, _, shrink_source = function_span(text, 'commit_shrink_single_extent_file')
extend_source = shrink_source
extend_source = replace_once(
    extend_source,
    'pub fn commit_shrink_single_extent_file<D: BlockDevice>(',
    'pub fn commit_extend_single_extent_file<D: BlockDevice>(',
    'имя функции расширения',
)
extend_source = replace_once(
    extend_source,
    '        || new_length >= metadata.size_bytes\n',
    '        || new_length <= metadata.size_bytes\n',
    'условие расширения',
)
extend_source = replace_once(
    extend_source,
    '    let written_blocks = copy_truncated_data_extent(\n',
    '    let written_blocks = copy_extended_data_extent(\n',
    'копирование расширенного экстента',
)
if 'pub fn commit_extend_single_extent_file<D: BlockDevice>(' not in text:
    insertion_point = '#[allow(clippy::too_many_arguments)]\npub fn commit_truncate_single_extent_file_to_zero<D: BlockDevice>('
    text = replace_once(text, insertion_point, extend_source + '\n\n' + insertion_point, 'вставка расширения непустого файла')

# Расширение пустого файла строим из существующего атомарного первого записывающего пути.
_, _, empty_write_source = function_span(text, 'commit_write_empty_file')
empty_extend = empty_write_source
empty_extend = replace_once(
    empty_extend,
    'pub fn commit_write_empty_file<D: BlockDevice>(',
    'pub fn commit_extend_empty_file<D: BlockDevice>(',
    'имя расширения пустого файла',
)
empty_extend = replace_once(
    empty_extend,
    '    data: &[u8],\n',
    '    new_length: u64,\n',
    'аргумент длины пустого файла',
)
empty_extend = replace_once(
    empty_extend,
    '    if object_id == 0 || data.is_empty() {\n',
    '    if object_id == 0 || new_length == 0 {\n',
    'проверка длины пустого файла',
)
old_blocks = '''    let data_blocks = data
        .len()
        .checked_add(FILESYSTEM_BLOCK_SIZE - 1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?
        / FILESYSTEM_BLOCK_SIZE;
    let data_blocks = u64::try_from(data_blocks).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
'''
new_blocks = '''    let block_size = FILESYSTEM_BLOCK_SIZE as u64;
    let data_blocks = new_length
        .checked_add(block_size - 1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?
        / block_size;
'''
empty_extend = replace_once(empty_extend, old_blocks, new_blocks, 'расчёт блоков пустого расширения')
empty_extend = replace_once(
    empty_extend,
    '    let written_blocks = write_data_extent(device, data_start_block, data, data_buffer)?;\n',
    '    let written_blocks = write_zero_data_extent(device, data_start_block, new_length, data_buffer)?;\n',
    'нулевые блоки пустого расширения',
)
empty_extend = replace_once(empty_extend, '        data.len() as u64,\n', '        new_length,\n', 'размер метаданных пустого расширения')
empty_extend = replace_once(
    empty_extend,
    '    let extent = ExtentValue::new(data_start_block, data_blocks, data.len() as u64);\n',
    '    let extent = ExtentValue::new(data_start_block, data_blocks, new_length);\n',
    'размер экстента пустого расширения',
)
if 'pub fn commit_extend_empty_file<D: BlockDevice>(' not in text:
    marker = 'impl<D: BlockDevice> FileSystem for PhoenixVfs<D> {'
    text = replace_once(text, marker, empty_extend + '\n\n' + marker, 'вставка расширения пустого файла')

start, end, _ = function_span(text, 'truncate_node')
new_truncate = r'''    fn truncate_node(&mut self, node: NodeId, length: u64) -> Result<(), VfsError> {
        let metadata = self.metadata(node)?;
        if metadata.kind != NodeKind::File {
            return Err(VfsError::IsDirectory);
        }
        if length == metadata.length {
            return Ok(());
        }

        let result = if length == 0 {
            commit_truncate_single_extent_file_to_zero(
                self.device.get_mut(),
                self.active,
                node.0,
                self.node_buffer.get_mut(),
                &mut self.object_copy_buffer,
                &mut self.current_free_space_buffer,
                &mut self.next_free_space_buffer,
                &mut self.superblock_buffer,
            )
        } else if metadata.length == 0 {
            commit_extend_empty_file(
                self.device.get_mut(),
                self.active,
                node.0,
                length,
                self.node_buffer.get_mut(),
                &mut self.object_copy_buffer,
                self.data_buffer.get_mut(),
                &mut self.current_free_space_buffer,
                &mut self.next_free_space_buffer,
                &mut self.superblock_buffer,
            )
        } else if length < metadata.length {
            commit_shrink_single_extent_file(
                self.device.get_mut(),
                self.active,
                node.0,
                length,
                self.node_buffer.get_mut(),
                &mut self.object_copy_buffer,
                self.data_buffer.get_mut(),
                &mut self.current_free_space_buffer,
                &mut self.next_free_space_buffer,
                &mut self.superblock_buffer,
            )
        } else {
            commit_extend_single_extent_file(
                self.device.get_mut(),
                self.active,
                node.0,
                length,
                self.node_buffer.get_mut(),
                &mut self.object_copy_buffer,
                self.data_buffer.get_mut(),
                &mut self.current_free_space_buffer,
                &mut self.next_free_space_buffer,
                &mut self.superblock_buffer,
            )
        }
        .map_err(map_phoenix_fs_to_vfs)?;

        self.active = result.active;
        Ok(())
    }'''
text = text[:start] + new_truncate + text[end:]

if 'fn phoenix_vfs_extends_existing_file_with_zeroes_and_remounts()' not in text:
    test_anchor = '    fn phoenix_vfs_shrinks_existing_file_and_remounts() {'
    tests = r'''    #[test]
    fn phoenix_vfs_extends_existing_file_with_zeroes_and_remounts() {
        use phoenix_vfs::{FileSystem, NodeId, NodeKind};

        let device = MemoryBlockDevice::<512, 256>::new();
        let mut buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut filesystem = PhoenixVfs::format_new(device, VOLUME_ID, 1, &mut buffer).unwrap();
        let file = filesystem
            .create_node(NodeId(ROOT_OBJECT_ID), b"extend.txt", NodeKind::File)
            .unwrap();
        assert_eq!(filesystem.write_node(file, 0, b"hello").unwrap(), 5);

        let before = filesystem.active_superblock().superblock.generation;
        let new_length = (FILESYSTEM_BLOCK_SIZE + 17) as u64;
        filesystem.truncate_node(file, new_length).unwrap();
        assert_eq!(filesystem.metadata(file).unwrap().length, new_length);
        assert_eq!(
            filesystem.active_superblock().superblock.generation,
            before + 1
        );

        let mut output = [0xFF_u8; FILESYSTEM_BLOCK_SIZE + 17];
        assert_eq!(filesystem.read_node(file, 0, &mut output).unwrap(), output.len());
        assert_eq!(&output[..5], b"hello");
        assert!(output[5..].iter().all(|byte| *byte == 0));

        let stable_generation = filesystem.active_superblock().superblock.generation;
        filesystem.truncate_node(file, new_length).unwrap();
        assert_eq!(filesystem.active_superblock().superblock.generation, stable_generation);

        let device = filesystem.into_device();
        let mut first = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let remounted = PhoenixVfs::mount(device, &mut first, &mut second).unwrap();
        assert_eq!(remounted.metadata(file).unwrap().length, new_length);
        let mut output = [0xFF_u8; FILESYSTEM_BLOCK_SIZE + 17];
        assert_eq!(remounted.read_node(file, 0, &mut output).unwrap(), output.len());
        assert_eq!(&output[..5], b"hello");
        assert!(output[5..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn phoenix_vfs_extends_empty_file_with_zeroes() {
        use phoenix_vfs::{FileSystem, NodeId, NodeKind};

        let device = MemoryBlockDevice::<512, 256>::new();
        let mut buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut filesystem = PhoenixVfs::format_new(device, VOLUME_ID, 1, &mut buffer).unwrap();
        let file = filesystem
            .create_node(NodeId(ROOT_OBJECT_ID), b"empty-grow.bin", NodeKind::File)
            .unwrap();

        let before = filesystem.active_superblock().superblock.generation;
        let new_length = (FILESYSTEM_BLOCK_SIZE * 2 + 3) as u64;
        filesystem.truncate_node(file, new_length).unwrap();
        assert_eq!(filesystem.metadata(file).unwrap().length, new_length);
        assert_eq!(filesystem.active_superblock().superblock.generation, before + 1);

        let mut output = [0xA5_u8; FILESYSTEM_BLOCK_SIZE * 2 + 3];
        assert_eq!(filesystem.read_node(file, 0, &mut output).unwrap(), output.len());
        assert!(output.iter().all(|byte| *byte == 0));
    }

'''
    text = replace_once(text, test_anchor, tests + test_anchor, 'вставка тестов расширения')

LIB.write_text(text)

roadmap = ROADMAP.read_text()
old = '  - атомарные операции изменения экстентов, расширения файла и частичного усечения через ВФС (создание объектов, запись нового пустого файла, запись произвольного диапазона внутри существующего одноэкстентного файла, частичное усечение до меньшего ненулевого размера и усечение до нуля уже реализованы; расширение файла остаётся открытым);'
new = '  - [x] атомарные операции изменения экстентов, расширения файла и частичного усечения через ВФС для одноэкстентного файла (создание объектов, первая запись, запись диапазона без изменения размера, расширение с нулевым заполнением, частичное усечение и усечение до нуля);'
if old in roadmap:
    roadmap = roadmap.replace(old, new, 1)
elif new not in roadmap:
    raise RuntimeError('Не найден пункт ROADMAP для расширения файла')
ROADMAP.write_text(roadmap)

status = STATUS.read_text()
section = '''\n## PhoenixFS — расширение файлов\n\nВФС теперь умеет атомарно увеличивать размер одноэкстентного файла через `truncate_node`. Для непустого файла PhoenixFS выделяет новый экстент, переносит прежние данные и физически обнуляет весь добавленный диапазон, включая хвост последнего старого блока. Пустой файл также можно сразу расширить до ненулевого размера: создаётся новый нулевой экстент. Метаданные, экстент и дерево свободного места переключаются одним новым поколением суперблока, поэтому старое состояние остаётся целостным до фиксации транзакции.\n\nРасширение пока ограничено текущей одноэкстентной моделью. Следующий шаг — разрешить `write_node` атомарно увеличивать логический размер файла при записи за прежний конец.\n'''
if '## PhoenixFS — расширение файлов' not in status:
    status += section
STATUS.write_text(status)
