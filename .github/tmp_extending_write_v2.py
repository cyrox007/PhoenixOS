from pathlib import Path

path = Path("crates/phoenix-fs/src/lib.rs")
text = path.read_text()


def replace_once(old: str, new: str) -> None:
    global text
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"ожидалось одно совпадение, найдено {count}: {old[:160]!r}")
    text = text.replace(old, new, 1)


anchor = "fn copy_truncated_data_extent<D: BlockDevice>(\n"
helper = r'''fn overlay_data_range<D: BlockDevice>(
    device: &mut D,
    start_block: u64,
    extent_data_bytes: u64,
    offset: u64,
    data: &[u8],
    block_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<(), PhoenixFsError> {
    if data.is_empty() {
        return Ok(());
    }

    let data_len = u64::try_from(data.len()).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    let write_end = offset
        .checked_add(data_len)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    if write_end > extent_data_bytes {
        return Err(PhoenixFsError::InvalidExtent);
    }

    let block_size = FILESYSTEM_BLOCK_SIZE as u64;
    let first_block = offset / block_size;
    let last_block = write_end
        .checked_sub(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?
        / block_size;

    for block_index in first_block..=last_block {
        let block_number = start_block
            .checked_add(block_index)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        read_filesystem_block(device, block_number, block_buffer)?;

        let block_start = block_index
            .checked_mul(block_size)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        let block_end = block_start
            .checked_add(block_size)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        let overlap_start = core::cmp::max(block_start, offset);
        let overlap_end = core::cmp::min(block_end, write_end);
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
        write_filesystem_block(device, block_number, block_buffer)?;
    }

    Ok(())
}

'''
replace_once(anchor, helper + anchor)

replace_once(
    "pub fn commit_extend_single_extent_file<D: BlockDevice>(\n    device: &mut D,\n    current: ActiveSuperblock,\n    object_id: u64,\n    new_length: u64,\n    object_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],",
    "pub fn commit_extend_single_extent_file<D: BlockDevice>(\n    device: &mut D,\n    current: ActiveSuperblock,\n    object_id: u64,\n    new_length: u64,\n    write_offset: u64,\n    data: &[u8],\n    object_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],",
)
replace_once(
    r'''    let written_blocks = copy_extended_data_extent(
        device,
        old_extent,
        data_start_block,
        new_length,
        data_buffer,
    )?;
    if written_blocks != data_blocks {
        return Err(PhoenixFsError::InvalidExtent);
    }
''',
    r'''    let written_blocks = copy_extended_data_extent(
        device,
        old_extent,
        data_start_block,
        new_length,
        data_buffer,
    )?;
    if written_blocks != data_blocks {
        return Err(PhoenixFsError::InvalidExtent);
    }
    overlay_data_range(
        device,
        data_start_block,
        new_length,
        write_offset,
        data,
        data_buffer,
    )?;
''',
)

replace_once(
    "pub fn commit_extend_empty_file<D: BlockDevice>(\n    device: &mut D,\n    current: ActiveSuperblock,\n    object_id: u64,\n    new_length: u64,\n    object_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],",
    "pub fn commit_extend_empty_file<D: BlockDevice>(\n    device: &mut D,\n    current: ActiveSuperblock,\n    object_id: u64,\n    new_length: u64,\n    write_offset: u64,\n    data: &[u8],\n    object_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],",
)
replace_once(
    "    let written_blocks = write_zero_data_extent(device, data_start_block, new_length, data_buffer)?;\n    if written_blocks != data_blocks {\n        return Err(PhoenixFsError::InvalidExtent);\n    }\n",
    "    let written_blocks = write_zero_data_extent(device, data_start_block, new_length, data_buffer)?;\n    if written_blocks != data_blocks {\n        return Err(PhoenixFsError::InvalidExtent);\n    }\n    overlay_data_range(\n        device,\n        data_start_block,\n        new_length,\n        write_offset,\n        data,\n        data_buffer,\n    )?;\n",
)

old_write = r'''        let result = if metadata.length == 0 && offset == 0 {
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
        } else if metadata.length != 0 && write_end <= metadata.length {
            commit_rewrite_single_extent_range(
                self.device.get_mut(),
                self.active,
                node.0,
                offset,
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
'''
new_write = r'''        let result = if metadata.length == 0 && offset == 0 {
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
        } else if metadata.length == 0 {
            commit_extend_empty_file(
                self.device.get_mut(),
                self.active,
                node.0,
                write_end,
                offset,
                data,
                self.node_buffer.get_mut(),
                &mut self.object_copy_buffer,
                self.data_buffer.get_mut(),
                &mut self.current_free_space_buffer,
                &mut self.next_free_space_buffer,
                &mut self.superblock_buffer,
            )
        } else if write_end <= metadata.length {
            commit_rewrite_single_extent_range(
                self.device.get_mut(),
                self.active,
                node.0,
                offset,
                data,
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
                write_end,
                offset,
                data,
                self.node_buffer.get_mut(),
                &mut self.object_copy_buffer,
                self.data_buffer.get_mut(),
                &mut self.current_free_space_buffer,
                &mut self.next_free_space_buffer,
                &mut self.superblock_buffer,
            )
        }
'''
replace_once(old_write, new_write)

replace_once(
    r'''            commit_extend_empty_file(
                self.device.get_mut(),
                self.active,
                node.0,
                length,
                self.node_buffer.get_mut(),''',
    r'''            commit_extend_empty_file(
                self.device.get_mut(),
                self.active,
                node.0,
                length,
                0,
                &[],
                self.node_buffer.get_mut(),''',
)
replace_once(
    r'''            commit_extend_single_extent_file(
                self.device.get_mut(),
                self.active,
                node.0,
                length,
                self.node_buffer.get_mut(),''',
    r'''            commit_extend_single_extent_file(
                self.device.get_mut(),
                self.active,
                node.0,
                length,
                0,
                &[],
                self.node_buffer.get_mut(),''',
)

replace_once(
    r'''        assert_eq!(filesystem.write_node(file, 0, data), Ok(data.len()));
        assert_eq!(filesystem.metadata(file).unwrap().length, data.len() as u64);
        assert_eq!(
            filesystem.write_node(file, data.len() as u64, b"second"),
            Err(VfsError::InvalidOffset)
        );

        let mut output = [0_u8; 64];
        assert_eq!(filesystem.read_node(file, 0, &mut output), Ok(data.len()));
        assert_eq!(&output[..data.len()], data);
''',
    r'''        assert_eq!(filesystem.write_node(file, 0, data), Ok(data.len()));
        assert_eq!(filesystem.metadata(file).unwrap().length, data.len() as u64);
        let generation_before_append = filesystem.active_superblock().superblock.generation;
        assert_eq!(
            filesystem.write_node(file, data.len() as u64, b"second"),
            Ok(6)
        );
        assert_eq!(
            filesystem.active_superblock().superblock.generation,
            generation_before_append + 1
        );

        let expected_len = data.len() + 6;
        let mut output = [0_u8; 64];
        assert_eq!(filesystem.read_node(file, 0, &mut output), Ok(expected_len));
        assert_eq!(&output[..data.len()], data);
        assert_eq!(&output[data.len()..expected_len], b"second");
''',
)
replace_once(
    "            remounted.read_node(file, 0, &mut remounted_output),\n            Ok(data.len())\n        );\n        assert_eq!(&remounted_output[..data.len()], data);",
    "            remounted.read_node(file, 0, &mut remounted_output),\n            Ok(expected_len)\n        );\n        assert_eq!(&remounted_output[..data.len()], data);\n        assert_eq!(&remounted_output[data.len()..expected_len], b\"second\");",
)

test_anchor = "    #[test]\n    fn phoenix_vfs_creates_and_removes_empty_nodes() {"
extra_test = r'''    #[test]
    fn phoenix_vfs_extending_write_zero_fills_gap_and_crosses_block_boundary() {
        use phoenix_vfs::{FileSystem, NodeKind};

        let device = MemoryBlockDevice::<512, 256>::new();
        let mut format_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut filesystem =
            PhoenixVfs::format_new(device, VOLUME_ID, 77, &mut format_buffer).unwrap();
        let root = filesystem.root_node().unwrap();
        let file = filesystem
            .create_node(root, b"range.bin", NodeKind::File)
            .unwrap();
        let initial = [0xAB_u8; FILESYSTEM_BLOCK_SIZE - 6];
        assert_eq!(filesystem.write_node(file, 0, &initial), Ok(initial.len()));

        let generation_before = filesystem.active_superblock().superblock.generation;
        assert_eq!(
            filesystem.write_node(file, FILESYSTEM_BLOCK_SIZE as u64 - 2, b"ABCDEF"),
            Ok(6)
        );
        assert_eq!(
            filesystem.active_superblock().superblock.generation,
            generation_before + 1
        );
        assert_eq!(
            filesystem.metadata(file).unwrap().length,
            FILESYSTEM_BLOCK_SIZE as u64 + 4
        );

        let mut boundary = [0_u8; 12];
        assert_eq!(
            filesystem.read_node(
                file,
                FILESYSTEM_BLOCK_SIZE as u64 - 8,
                &mut boundary,
            ),
            Ok(12)
        );
        assert_eq!(&boundary[..2], &[0xAB; 2]);
        assert_eq!(&boundary[2..6], &[0; 4]);
        assert_eq!(&boundary[6..], b"ABCDEF");

        let empty = filesystem
            .create_node(root, b"sparse.bin", NodeKind::File)
            .unwrap();
        let empty_generation = filesystem.active_superblock().superblock.generation;
        assert_eq!(filesystem.write_node(empty, 4, b"ok"), Ok(2));
        assert_eq!(
            filesystem.active_superblock().superblock.generation,
            empty_generation + 1
        );
        let mut sparse = [0xFF_u8; 6];
        assert_eq!(filesystem.read_node(empty, 0, &mut sparse), Ok(6));
        assert_eq!(&sparse, b"\0\0\0\0ok");

        let mut device = filesystem.into_device();
        let mut first = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let active = read_active_superblock(&mut device, &mut first, &mut second).unwrap();
        let remounted = PhoenixVfs::new(device, active).unwrap();
        let mut remounted_boundary = [0_u8; 12];
        assert_eq!(
            remounted.read_node(
                file,
                FILESYSTEM_BLOCK_SIZE as u64 - 8,
                &mut remounted_boundary,
            ),
            Ok(12)
        );
        assert_eq!(remounted_boundary, boundary);
        let mut remounted_sparse = [0xFF_u8; 6];
        assert_eq!(remounted.read_node(empty, 0, &mut remounted_sparse), Ok(6));
        assert_eq!(&remounted_sparse, b"\0\0\0\0ok");
    }

'''
replace_once(test_anchor, extra_test + test_anchor)
path.write_text(text)

roadmap = Path("ROADMAP.md")
roadmap_text = roadmap.read_text()
roadmap_old = "  - [x] атомарные операции изменения экстентов, расширения файла и частичного усечения через ВФС для одноэкстентного файла (создание объектов, первая запись, запись диапазона без изменения размера, расширение с нулевым заполнением, частичное усечение и усечение до нуля);"
roadmap_new = "  - [x] атомарные операции изменения экстентов, расширения файла и частичного усечения через ВФС для одноэкстентного файла (создание объектов, первая запись, запись диапазона без изменения размера, запись с увеличением размера и нулевым заполнением разрыва, расширение через изменение длины, частичное усечение и усечение до нуля);"
if roadmap_text.count(roadmap_old) != 1:
    raise SystemExit("не найдено ожидаемое описание PhoenixFS в ROADMAP.md")
roadmap.write_text(roadmap_text.replace(roadmap_old, roadmap_new, 1))

status = Path("STATUS.md")
status_text = status.read_text()
status_old = "Расширение пока ограничено текущей одноэкстентной моделью. Следующий шаг — разрешить `write_node` атомарно увеличивать логический размер файла при записи за прежний конец."
status_new = "Расширение пока ограничено текущей одноэкстентной моделью. `write_node` теперь также может атомарно увеличивать логический размер файла при записи за прежний конец.\n\n## PhoenixFS — запись с увеличением размера\n\nВФС поддерживает запись, пересекающую прежний конец файла или начинающуюся после него. PhoenixFS в одной транзакции выделяет новый экстент, сохраняет прежние байты, обнуляет логический промежуток до новой записи и накладывает новые данные до переключения поколения суперблока. Это же правило действует для пустого файла при записи с ненулевого смещения: образовавшийся префикс читается как нули.\n\nПроверки покрывают добавление в конец файла, единственное увеличение поколения, запись с разрывом, переход через границу 4 КиБ, запись с ненулевого смещения в пустой файл и сохранение результата после повторного подключения. Следующая задача этапа хранения — перечисление каталога через общий интерфейс ВФС."
if status_text.count(status_old) != 1:
    raise SystemExit("не найдено ожидаемое описание расширения в STATUS.md")
status.write_text(status_text.replace(status_old, status_new, 1))
