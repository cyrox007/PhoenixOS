from pathlib import Path

path = Path("crates/phoenix-fs/src/lib.rs")
text = path.read_text()

repls = [
('pub const EXTENT_VALUE_SIZE: usize = 24;\n', 'pub const EXTENT_VALUE_SIZE: usize = 32;\nconst LEGACY_EXTENT_VALUE_SIZE: usize = 24;\nconst EXTENT_CHECKSUM_KIND_NONE: u32 = 0;\nconst EXTENT_CHECKSUM_KIND_CRC32C: u32 = 1;\n'),
('    InvalidExtent,\n    InvalidReadRange,', '    InvalidExtent,\n    InvalidExtentChecksumKind(u32),\n    DataChecksumMismatch,\n    InvalidReadRange,'),
]
for old,new in repls:
    if old not in text:
        raise SystemExit(f"Не найден фрагмент: {old!r}")
    text = text.replace(old,new,1)

start = text.index("#[derive(Debug, Clone, Copy, PartialEq, Eq)]\npub struct ExtentValue")
end = text.index("\npub fn validate_object_record_value", start)
extent_impl = r'''#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtentValue {
    pub physical_start_block: u64,
    pub block_count: u64,
    pub data_bytes: u64,
    pub data_checksum: Option<u32>,
}

impl ExtentValue {
    pub const fn new(physical_start_block: u64, block_count: u64, data_bytes: u64) -> Self {
        Self {
            physical_start_block,
            block_count,
            data_bytes,
            data_checksum: None,
        }
    }

    pub const fn with_data_checksum(
        physical_start_block: u64,
        block_count: u64,
        data_bytes: u64,
        data_checksum: u32,
    ) -> Self {
        Self {
            physical_start_block,
            block_count,
            data_bytes,
            data_checksum: Some(data_checksum),
        }
    }

    pub fn encode(self, destination: &mut [u8]) -> Result<(), PhoenixFsError> {
        if destination.len() != EXTENT_VALUE_SIZE {
            return Err(PhoenixFsError::InvalidObjectValueSize);
        }
        if self.block_count == 0 || self.data_bytes == 0 {
            return Err(PhoenixFsError::InvalidExtent);
        }

        destination.fill(0);
        write_u64(destination, 0, self.physical_start_block);
        write_u64(destination, 8, self.block_count);
        write_u64(destination, 16, self.data_bytes);
        if let Some(checksum) = self.data_checksum {
            write_u32(destination, 24, EXTENT_CHECKSUM_KIND_CRC32C);
            write_u32(destination, 28, checksum);
        }
        Ok(())
    }

    pub fn decode(source: &[u8]) -> Result<Self, PhoenixFsError> {
        if source.len() != LEGACY_EXTENT_VALUE_SIZE && source.len() != EXTENT_VALUE_SIZE {
            return Err(PhoenixFsError::InvalidObjectValueSize);
        }

        let data_checksum = if source.len() == LEGACY_EXTENT_VALUE_SIZE {
            None
        } else {
            let kind = read_u32(source, 24);
            let checksum = read_u32(source, 28);
            match kind {
                EXTENT_CHECKSUM_KIND_NONE => {
                    if checksum != 0 {
                        return Err(PhoenixFsError::InvalidReservedField);
                    }
                    None
                }
                EXTENT_CHECKSUM_KIND_CRC32C => Some(checksum),
                other => return Err(PhoenixFsError::InvalidExtentChecksumKind(other)),
            }
        };

        let extent = Self {
            physical_start_block: read_u64(source, 0),
            block_count: read_u64(source, 8),
            data_bytes: read_u64(source, 16),
            data_checksum,
        };
        if extent.block_count == 0 || extent.data_bytes == 0 {
            return Err(PhoenixFsError::InvalidExtent);
        }
        Ok(extent)
    }

    pub fn validate_for_key(
        self,
        key: ObjectTreeKey,
        total_blocks: u64,
    ) -> Result<(), PhoenixFsError> {
        if key.kind != ObjectRecordKind::Extent
            || !key.offset.is_multiple_of(FILESYSTEM_BLOCK_SIZE as u64)
            || self.physical_start_block < SUPERBLOCK_COPY_COUNT
        {
            return Err(PhoenixFsError::InvalidExtent);
        }

        let end_block = self
            .physical_start_block
            .checked_add(self.block_count)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        if end_block > total_blocks {
            return Err(PhoenixFsError::InvalidExtent);
        }

        let capacity = self
            .block_count
            .checked_mul(FILESYSTEM_BLOCK_SIZE as u64)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        if self.data_bytes > capacity {
            return Err(PhoenixFsError::InvalidExtent);
        }
        Ok(())
    }
}
'''
text = text[:start] + extent_impl + text[end:]

marker = "fn read_extent_bytes<D: BlockDevice>(\n"
idx = text.index(marker)
helpers = r'''fn checksum_extent_data<D: BlockDevice>(
    device: &mut D,
    start_block: u64,
    data_bytes: u64,
    block_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<u32, PhoenixFsError> {
    if data_bytes == 0 {
        return Err(PhoenixFsError::InvalidExtent);
    }

    let block_size = FILESYSTEM_BLOCK_SIZE as u64;
    let block_count = data_bytes
        .checked_add(block_size - 1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?
        / block_size;
    let mut remaining = data_bytes;
    let mut crc = !0_u32;

    for block_index in 0..block_count {
        let block_number = start_block
            .checked_add(block_index)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        read_filesystem_block(device, block_number, block_buffer)?;

        let valid_bytes = core::cmp::min(remaining, block_size);
        let valid_bytes =
            usize::try_from(valid_bytes).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
        crc = crc32c_update(crc, &block_buffer[..valid_bytes]);
        remaining -= valid_bytes as u64;
    }

    if remaining != 0 {
        return Err(PhoenixFsError::InvalidExtent);
    }
    Ok(!crc)
}

fn extent_with_data_checksum<D: BlockDevice>(
    device: &mut D,
    start_block: u64,
    block_count: u64,
    data_bytes: u64,
    block_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<ExtentValue, PhoenixFsError> {
    let extent = ExtentValue::new(start_block, block_count, data_bytes);
    extent.validate_for_key(
        ObjectTreeKey::new(1, ObjectRecordKind::Extent, 0),
        filesystem_block_count(device)?,
    )?;
    let checksum = checksum_extent_data(device, start_block, data_bytes, block_buffer)?;
    Ok(ExtentValue::with_data_checksum(
        start_block,
        block_count,
        data_bytes,
        checksum,
    ))
}

fn verify_extent_data_checksum<D: BlockDevice>(
    device: &mut D,
    extent: ExtentValue,
    block_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<(), PhoenixFsError> {
    let Some(expected) = extent.data_checksum else {
        return Ok(());
    };

    let actual = checksum_extent_data(
        device,
        extent.physical_start_block,
        extent.data_bytes,
        block_buffer,
    )?;
    if actual != expected {
        return Err(PhoenixFsError::DataChecksumMismatch);
    }
    Ok(())
}

'''
text = text[:idx] + helpers + text[idx:]

read_old = '''    if end > extent.data_bytes {
        return Err(PhoenixFsError::InvalidReadRange);
    }

    let mut copied = 0_usize;'''
read_new = '''    if end > extent.data_bytes {
        return Err(PhoenixFsError::InvalidReadRange);
    }

    verify_extent_data_checksum(device, extent, block_buffer)?;

    let mut copied = 0_usize;'''
if read_old not in text:
    raise SystemExit("Не найден read_extent_bytes")
text = text.replace(read_old, read_new, 1)

production_repls = [
('let next_extent = ExtentValue::new(data_start_block, data_blocks, old_extent.data_bytes);',
 '''let next_extent = extent_with_data_checksum(
        device,
        data_start_block,
        data_blocks,
        old_extent.data_bytes,
        data_buffer,
    )?;'''),
('let next_extent = ExtentValue::new(data_start_block, data_blocks, new_length);',
 '''let next_extent = extent_with_data_checksum(
        device,
        data_start_block,
        data_blocks,
        new_length,
        data_buffer,
    )?;'''),
('let extent = ExtentValue::new(data_start_block, data_blocks, data.len() as u64);',
 '''let extent = extent_with_data_checksum(
        device,
        data_start_block,
        data_blocks,
        data.len() as u64,
        data_buffer,
    )?;'''),
('let extent = ExtentValue::new(data_start_block, data_blocks, new_length);',
 '''let extent = extent_with_data_checksum(
        device,
        data_start_block,
        data_blocks,
        new_length,
        data_buffer,
    )?;'''),
]
counts = [1,2,1,1]
for (old,new), expected in zip(production_repls, counts):
    actual = text.count(old)
    if actual != expected:
        raise SystemExit(f"Ожидалось {expected} совпадений {old!r}, найдено {actual}")
    text = text.replace(old,new)

crc_marker = "fn crc32c_with_zeroed_range(bytes: &[u8], zero_start: usize, zero_end: usize) -> u32 {"
crc_idx = text.index(crc_marker)
crc_helper = r'''fn crc32c_update(mut crc: u32, bytes: &[u8]) -> u32 {
    for byte in bytes.iter().copied() {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0x82f6_3b78 & mask);
        }
    }
    crc
}

'''
text = text[:crc_idx] + crc_helper + text[crc_idx:]

insert_at = text.rfind("\n}")
tests = r'''

    #[test]
    fn extent_value_round_trip_preserves_crc32c_checksum_and_reads_legacy_value() {
        let extent = ExtentValue::with_data_checksum(20, 2, 7000, 0x1234_5678);
        let mut encoded = [0_u8; EXTENT_VALUE_SIZE];
        extent.encode(&mut encoded).unwrap();
        assert_eq!(ExtentValue::decode(&encoded), Ok(extent));

        let mut legacy = [0_u8; LEGACY_EXTENT_VALUE_SIZE];
        write_u64(&mut legacy, 0, 20);
        write_u64(&mut legacy, 8, 2);
        write_u64(&mut legacy, 16, 7000);
        assert_eq!(
            ExtentValue::decode(&legacy),
            Ok(ExtentValue::new(20, 2, 7000))
        );
    }

    #[test]
    fn phoenix_vfs_detects_corrupted_user_data_on_read() {
        use phoenix_vfs::{FileSystem, NodeKind};

        let device = MemoryBlockDevice::<512, 160>::new();
        let mut format_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut filesystem =
            PhoenixVfs::format_new(device, VOLUME_ID, 1, &mut format_buffer).unwrap();
        let root = filesystem.root_node().unwrap();
        let file = filesystem.create_node(root, b"checked.bin", NodeKind::File).unwrap();
        assert_eq!(filesystem.write_node(file, 0, b"checksum-protected"), Ok(18));

        let active = filesystem.active_superblock();
        let mut device = filesystem.into_device();
        let mut node_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let (_, extent) = read_single_file_extent(&mut device, active, file.0, &mut node_buffer)
            .unwrap();
        assert!(extent.data_checksum.is_some());

        let mut data_block = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(&mut device, extent.physical_start_block, &mut data_block).unwrap();
        data_block[3] ^= 0x5a;
        write_filesystem_block(&mut device, extent.physical_start_block, &data_block).unwrap();
        device.flush().unwrap();

        let mut first = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let remounted = PhoenixVfs::mount(device, &mut first, &mut second).unwrap();
        let mut output = [0_u8; 32];
        assert_eq!(remounted.read_node(file, 0, &mut output), Err(VfsError::Storage));
    }

    #[test]
    fn partial_read_checks_entire_extent_for_corruption() {
        use phoenix_vfs::{FileSystem, NodeKind};

        let device = MemoryBlockDevice::<512, 256>::new();
        let mut format_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut filesystem =
            PhoenixVfs::format_new(device, VOLUME_ID, 1, &mut format_buffer).unwrap();
        let root = filesystem.root_node().unwrap();
        let file = filesystem.create_node(root, b"large-checked.bin", NodeKind::File).unwrap();
        let payload = std::vec![0x41_u8; FILESYSTEM_BLOCK_SIZE + 128];
        assert_eq!(filesystem.write_node(file, 0, &payload), Ok(payload.len()));

        let active = filesystem.active_superblock();
        let mut device = filesystem.into_device();
        let mut node_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let (_, extent) = read_single_file_extent(&mut device, active, file.0, &mut node_buffer)
            .unwrap();

        let second_block = extent.physical_start_block + 1;
        let mut data_block = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(&mut device, second_block, &mut data_block).unwrap();
        data_block[64] ^= 1;
        write_filesystem_block(&mut device, second_block, &data_block).unwrap();
        device.flush().unwrap();

        let mut first = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let remounted = PhoenixVfs::mount(device, &mut first, &mut second).unwrap();
        let mut prefix = [0_u8; 4];
        assert_eq!(remounted.read_node(file, 0, &mut prefix), Err(VfsError::Storage));
    }
'''
text = text[:insert_at] + tests + text[insert_at:]

path.write_text(text)

status = Path("STATUS.md")
status_text = status.read_text().rstrip() + r'''

## PhoenixFS — контрольные суммы пользовательских данных

Новые пользовательские экстенты теперь получают контрольную сумму CRC32C по всем логически значимым байтам. Контрольная сумма хранится вместе с описанием экстента и пересчитывается после первой записи, частичной перезаписи, усечения и расширения до публикации нового поколения. При чтении PhoenixFS проверяет весь экстент, поэтому повреждение за пределами запрошенного фрагмента также обнаруживается до возврата данных вызывающей стороне.

Формат описания экстента расширен с сохранением чтения прежнего 24-байтового представления: старый экстент без контрольной суммы остаётся читаемым, а все новые изменения переводят данные в защищённое представление. Несовпадение контрольной суммы возвращается как ошибка хранилища через общий ВФС. Проверки покрывают повреждение данных после успешной фиксации, обнаружение повреждения при частичном чтении и совместимость с прежним форматом экстента.
'''
status.write_text(status_text + "\n")
