from pathlib import Path

LIB = Path('crates/phoenix-fs/src/lib.rs')
ROADMAP = Path('ROADMAP.md')
STATUS = Path('STATUS.md')
text = LIB.read_text()


def function_span(source: str, name: str) -> tuple[int, int, str]:
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
    raise RuntimeError(f'Не удалось выделить функцию {name}')


def replace_once(source: str, old: str, new: str, label: str) -> str:
    count = source.count(old)
    if count != 1:
        raise RuntimeError(f'{label}: ожидалось одно совпадение, найдено {count}')
    return source.replace(old, new, 1)


copy_helper = r'''fn copy_truncated_data_extent<D: BlockDevice>(
    device: &mut D,
    old_extent: ExtentValue,
    new_start_block: u64,
    new_data_bytes: u64,
    block_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<u64, PhoenixFsError> {
    if new_data_bytes == 0 || new_data_bytes > old_extent.data_bytes {
        return Err(PhoenixFsError::InvalidExtent);
    }

    let block_size = FILESYSTEM_BLOCK_SIZE as u64;
    let rounded_bytes = new_data_bytes
        .checked_add(block_size - 1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let new_block_count = rounded_bytes / block_size;
    if new_block_count == 0 || new_block_count > old_extent.block_count {
        return Err(PhoenixFsError::InvalidExtent);
    }

    for block_index in 0..new_block_count {
        let old_block = old_extent
            .physical_start_block
            .checked_add(block_index)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        read_filesystem_block(device, old_block, block_buffer)?;

        if block_index + 1 == new_block_count {
            let block_start = block_index
                .checked_mul(block_size)
                .ok_or(PhoenixFsError::ArithmeticOverflow)?;
            let retained = new_data_bytes
                .checked_sub(block_start)
                .ok_or(PhoenixFsError::InvalidExtent)?;
            let retained = usize::try_from(retained)
                .map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
            if retained < FILESYSTEM_BLOCK_SIZE {
                block_buffer[retained..].fill(0);
            }
        }

        let new_block = new_start_block
            .checked_add(block_index)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        write_filesystem_block(device, new_block, block_buffer)?;
    }

    Ok(new_block_count)
}

'''

anchor = 'fn rewrite_data_extent_range<D: BlockDevice>('
if 'fn copy_truncated_data_extent<D: BlockDevice>(' not in text:
    text = replace_once(text, anchor, copy_helper + anchor, 'вставка копирования усечённого экстента')

_, _, rewrite_source = function_span(text, 'commit_rewrite_single_extent_range')
shrink_source = rewrite_source
shrink_source = replace_once(
    shrink_source,
    'fn commit_rewrite_single_extent_range<D: BlockDevice>(',
    'fn commit_shrink_single_extent_file<D: BlockDevice>(',
    'имя функции частичного усечения',
)
shrink_source = replace_once(
    shrink_source,
    '    offset: u64,\n    data: &[u8],\n',
    '    new_length: u64,\n',
    'аргументы частичного усечения',
)
old_validation = '''    if object_id == 0 || data.is_empty() {
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
    let write_end = offset
        .checked_add(data_len)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    if metadata.object_type != ObjectType::File
        || metadata.size_bytes == 0
        || write_end > metadata.size_bytes
    {
        return Err(PhoenixFsError::InvalidObjectMutationBatch);
    }
'''
new_validation = '''    if object_id == 0 || new_length == 0 {
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
    if metadata.object_type != ObjectType::File
        || metadata.size_bytes == 0
        || new_length >= metadata.size_bytes
    {
        return Err(PhoenixFsError::InvalidObjectMutationBatch);
    }
'''
shrink_source = replace_once(
    shrink_source,
    old_validation,
    new_validation,
    'проверка параметров частичного усечения',
)
shrink_source = replace_once(
    shrink_source,
    '    let data_blocks = old_extent.block_count;\n',
    '''    let block_size = FILESYSTEM_BLOCK_SIZE as u64;
    let rounded_bytes = new_length
        .checked_add(block_size - 1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let data_blocks = rounded_bytes / block_size;
''',
    'расчёт числа блоков после усечения',
)
old_copy = '''    let written_blocks = rewrite_data_extent_range(
        device,
        old_extent,
        data_start_block,
        offset,
        data,
        data_buffer,
    )?;
'''
new_copy = '''    let written_blocks = copy_truncated_data_extent(
        device,
        old_extent,
        data_start_block,
        new_length,
        data_buffer,
    )?;
'''
shrink_source = replace_once(shrink_source, old_copy, new_copy, 'копирование сохранённого префикса')
shrink_source = replace_once(
    shrink_source,
    '''    let next_metadata = ObjectMetadataValue::new(
        ObjectType::File,
        metadata.size_bytes,
''',
    '''    let next_metadata = ObjectMetadataValue::new(
        ObjectType::File,
        new_length,
''',
    'новый размер в метаданных',
)
shrink_source = replace_once(
    shrink_source,
    '    let next_extent = ExtentValue::new(data_start_block, data_blocks, old_extent.data_bytes);\n',
    '    let next_extent = ExtentValue::new(data_start_block, data_blocks, new_length);\n',
    'новый размер экстента',
)

if 'fn commit_shrink_single_extent_file<D: BlockDevice>(' not in text:
    truncate_anchor = '#[allow(clippy::too_many_arguments)]\npub fn commit_truncate_single_extent_file_to_zero<D: BlockDevice>('
    insertion = '#[allow(clippy::too_many_arguments)]\npub ' + shrink_source + '\n\n'
    text = replace_once(text, truncate_anchor, insertion + truncate_anchor, 'вставка транзакции частичного усечения')

truncate_start, truncate_end, _ = function_span(text, 'truncate_node')
new_truncate = r'''    fn truncate_node(&mut self, node: NodeId, length: u64) -> Result<(), VfsError> {
        let metadata = self.metadata(node)?;
        if metadata.kind != NodeKind::File {
            return Err(VfsError::IsDirectory);
        }
        if length == metadata.length {
            return Ok(());
        }
        if length > metadata.length {
            return Err(VfsError::InvalidOffset);
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
        } else {
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
        }
        .map_err(map_phoenix_fs_to_vfs)?;

        self.active = result.active;
        Ok(())
    }'''
text = text[:truncate_start] + new_truncate + text[truncate_end:]

if 'fn phoenix_vfs_shrinks_existing_file_and_remounts()' not in text:
    test_anchor = '    fn phoenix_vfs_truncates_existing_file_to_zero_and_remounts() {'
    tests = r'''    #[test]
    fn phoenix_vfs_shrinks_existing_file_and_remounts() {
        use phoenix_vfs::{FileSystem, NodeId, NodeKind};

        let device = MemoryBlockDevice::<512, 256>::new();
        let mut buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut filesystem = PhoenixVfs::format_new(device, VOLUME_ID, 1, &mut buffer).unwrap();
        let file = filesystem
            .create_node(NodeId(ROOT_OBJECT_ID), b"shrink.txt", NodeKind::File)
            .unwrap();
        assert_eq!(filesystem.write_node(file, 0, b"hello world").unwrap(), 11);

        let before = filesystem.active_superblock().superblock.generation;
        filesystem.truncate_node(file, 5).unwrap();
        assert_eq!(filesystem.metadata(file).unwrap().length, 5);
        assert_eq!(
            filesystem.active_superblock().superblock.generation,
            before + 1
        );

        let stable_generation = filesystem.active_superblock().superblock.generation;
        assert_eq!(filesystem.truncate_node(file, 5), Ok(()));
        assert_eq!(
            filesystem.active_superblock().superblock.generation,
            stable_generation
        );
        assert_eq!(
            filesystem.truncate_node(file, 6),
            Err(VfsError::InvalidOffset)
        );

        let mut output = [0_u8; 11];
        assert_eq!(filesystem.read_node(file, 0, &mut output).unwrap(), 5);
        assert_eq!(&output[..5], b"hello");

        let device = filesystem.into_device();
        let mut first = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let remounted = PhoenixVfs::mount(device, &mut first, &mut second).unwrap();
        assert_eq!(remounted.metadata(file).unwrap().length, 5);
        let mut output = [0_u8; 11];
        assert_eq!(remounted.read_node(file, 0, &mut output).unwrap(), 5);
        assert_eq!(&output[..5], b"hello");
    }

    #[test]
    fn phoenix_vfs_shrink_zeroes_tail_of_last_retained_block() {
        use phoenix_vfs::{FileSystem, NodeId, NodeKind};

        let device = MemoryBlockDevice::<512, 256>::new();
        let mut buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut filesystem = PhoenixVfs::format_new(device, VOLUME_ID, 1, &mut buffer).unwrap();
        let file = filesystem
            .create_node(NodeId(ROOT_OBJECT_ID), b"blocks.bin", NodeKind::File)
            .unwrap();
        let input = [0xA5_u8; FILESYSTEM_BLOCK_SIZE + 37];
        assert_eq!(filesystem.write_node(file, 0, &input).unwrap(), input.len());

        let new_length = (FILESYSTEM_BLOCK_SIZE + 1) as u64;
        filesystem.truncate_node(file, new_length).unwrap();
        let active = filesystem.active_superblock();
        let mut device = filesystem.into_device();

        let mut object_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let (_, extent) = read_single_file_extent(
            &mut device,
            active,
            file.0,
            &mut object_buffer,
        )
        .unwrap();
        assert_eq!(extent.block_count, 2);
        assert_eq!(extent.data_bytes, new_length);

        let final_block = extent.physical_start_block + 1;
        let mut data_block = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(&mut device, final_block, &mut data_block).unwrap();
        assert_eq!(data_block[0], 0xA5);
        assert!(data_block[1..].iter().all(|byte| *byte == 0));

        let mut first = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let remounted = PhoenixVfs::mount(device, &mut first, &mut second).unwrap();
        let mut output = [0_u8; FILESYSTEM_BLOCK_SIZE + 37];
        assert_eq!(
            remounted.read_node(file, 0, &mut output).unwrap(),
            FILESYSTEM_BLOCK_SIZE + 1
        );
        assert_eq!(&output[..FILESYSTEM_BLOCK_SIZE + 1], &input[..FILESYSTEM_BLOCK_SIZE + 1]);
    }

'''
    text = replace_once(text, test_anchor, tests + test_anchor, 'вставка тестов частичного усечения')

LIB.write_text(text)

roadmap = ROADMAP.read_text()
old_roadmap = '  - атомарные операции изменения экстентов, расширения файла и частичного усечения через ВФС (создание объектов, запись нового пустого файла, запись произвольного диапазона внутри существующего одноэкстентного файла и усечение до нуля уже реализованы);'
new_roadmap = '  - атомарные операции изменения экстентов, расширения файла и частичного усечения через ВФС (создание объектов, запись нового пустого файла, запись произвольного диапазона внутри существующего одноэкстентного файла, частичное усечение до меньшего ненулевого размера и усечение до нуля уже реализованы; расширение файла остаётся открытым);'
if old_roadmap in roadmap:
    roadmap = roadmap.replace(old_roadmap, new_roadmap, 1)
elif new_roadmap not in roadmap:
    raise RuntimeError('Не найден пункт PhoenixFS в ROADMAP.md')
ROADMAP.write_text(roadmap)

status = STATUS.read_text()
status_section = '''
## PhoenixFS — частичное усечение файлов

ВФС теперь умеет атомарно уменьшать существующий одноэкстентный файл до меньшего ненулевого размера. PhoenixFS выделяет новый экстент, копирует только сохраняемый префикс, обнуляет хвост последнего сохранённого блока, обновляет размер объекта и экстента в одном новом поколении и только после переключения суперблока считает старые блоки выбывшими. Усечение до нуля продолжает использовать отдельный путь удаления экстента; увеличение размера файла пока не поддерживается.

Проверки покрывают сохранение префикса после повторного подключения, единственное увеличение поколения при фактическом усечении, отсутствие нового поколения при усечении до текущего размера, отказ от увеличения через `truncate_node` и обнуление неиспользуемого хвоста последнего блока.
'''
if '## PhoenixFS — частичное усечение файлов' not in status:
    status = status.rstrip() + '\n\n' + status_section.lstrip()
STATUS.write_text(status)
