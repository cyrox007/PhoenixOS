from pathlib import Path
import re

path = Path('crates/phoenix-fs/src/lib.rs')
text = path.read_text()
pattern = re.compile(r'''\n    let metadata_retired = metadata_path(?:\n        \.node_count\(\)\n        \.checked_add\(1\)\n        \.ok_or\(PhoenixFsError::ArithmeticOverflow\)\?|\.node_count\(\) \+ 1);\n    let data_retired =\n        usize::try_from\(old_extent\.block_count\)\.map_err\(\|_\| PhoenixFsError::ArithmeticOverflow\)\?;\n    if metadata_retired\n        \.checked_add\(data_retired\)\n        \.ok_or\(PhoenixFsError::ArithmeticOverflow\)\?\n        > MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS\n    \{\n        return Err\(PhoenixFsError::BufferSize\);\n    \}\n''')
replacement = '''\n    let metadata_retired = metadata_path\n        .node_count()\n        .checked_add(1)\n        .ok_or(PhoenixFsError::ArithmeticOverflow)?;\n    if metadata_retired > MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS {\n        return Err(PhoenixFsError::BufferSize);\n    }\n'''
text, count = pattern.subn(replacement, text)
if count != 4:
    raise SystemExit(f'expected 4 obsolete retirement limit checks, replaced {count}')

tests_marker = '''#[cfg(test)]\nmod tests {\n    use super::*;\n    use phoenix_block::MemoryBlockDevice;\n\n    const VOLUME_ID: [u8; 16] = *b"phoenix-volume-1";'''
if tests_marker not in text:
    raise SystemExit('tests module marker not found')
text = text.replace(
    tests_marker,
    '''#[cfg(test)]\nmod tests {\n    extern crate std;\n\n    use super::*;\n    use phoenix_block::{BlockDevice, BlockError, BlockGeometry, MemoryBlockDevice};\n\n    struct HeapBlockDevice {\n        bytes: std::vec::Vec<u8>,\n        block_size: usize,\n        block_count: u64,\n        dirty: bool,\n    }\n\n    impl HeapBlockDevice {\n        fn new(block_size: usize, block_count: u64) -> Self {\n            let byte_count = block_size\n                .checked_mul(usize::try_from(block_count).unwrap())\n                .unwrap();\n            Self {\n                bytes: std::vec![0; byte_count],\n                block_size,\n                block_count,\n                dirty: false,\n            }\n        }\n\n        fn checked_range(\n            &self,\n            first_block: u64,\n            byte_length: usize,\n        ) -> Result<core::ops::Range<usize>, BlockError> {\n            phoenix_block::validate_transfer(self.geometry(), first_block, byte_length)?;\n            let start = usize::try_from(first_block)\n                .map_err(|_| BlockError::ArithmeticOverflow)?\n                .checked_mul(self.block_size)\n                .ok_or(BlockError::ArithmeticOverflow)?;\n            let end = start\n                .checked_add(byte_length)\n                .ok_or(BlockError::ArithmeticOverflow)?;\n            Ok(start..end)\n        }\n    }\n\n    impl BlockDevice for HeapBlockDevice {\n        fn geometry(&self) -> BlockGeometry {\n            BlockGeometry {\n                block_size: self.block_size,\n                block_count: self.block_count,\n            }\n        }\n\n        fn read_blocks(\n            &mut self,\n            first_block: u64,\n            destination: &mut [u8],\n        ) -> Result<(), BlockError> {\n            let range = self.checked_range(first_block, destination.len())?;\n            destination.copy_from_slice(&self.bytes[range]);\n            Ok(())\n        }\n\n        fn write_blocks(&mut self, first_block: u64, source: &[u8]) -> Result<(), BlockError> {\n            let range = self.checked_range(first_block, source.len())?;\n            self.bytes[range].copy_from_slice(source);\n            self.dirty = true;\n            Ok(())\n        }\n\n        fn flush(&mut self) -> Result<(), BlockError> {\n            self.dirty = false;\n            Ok(())\n        }\n    }\n\n    const VOLUME_ID: [u8; 16] = *b"phoenix-volume-1";''',
    1,
)

marker = '''    #[test]\n    fn phoenix_vfs_rewrites_range_inside_existing_file_atomically() {'''
if marker not in text:
    raise SystemExit('test insertion marker not found')

test = r'''    #[test]
    fn phoenix_vfs_rewrites_large_extent_without_per_block_retirement_limit() {
        use phoenix_vfs::{FileSystem, NodeId, NodeKind};

        const LARGE_BLOCKS: usize = MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS + 2;
        const LARGE_BYTES: usize = FILESYSTEM_BLOCK_SIZE * LARGE_BLOCKS;

        let device = HeapBlockDevice::new(512, 1024);
        let mut buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut filesystem = PhoenixVfs::format_new(device, VOLUME_ID, 1, &mut buffer).unwrap();
        let root = filesystem.root_node().unwrap();
        let file = filesystem
            .create_node(root, b"large.bin", NodeKind::File)
            .unwrap();

        let payload = std::vec![0x41_u8; LARGE_BYTES];
        assert_eq!(filesystem.write_node(file, 0, &payload).unwrap(), payload.len());
        let generation_before = filesystem.active_superblock().superblock.generation;

        let rewrite_offset = (FILESYSTEM_BLOCK_SIZE * (LARGE_BLOCKS / 2)) as u64;
        assert_eq!(filesystem.write_node(file, rewrite_offset, b"Z").unwrap(), 1);
        assert_eq!(
            filesystem.active_superblock().superblock.generation,
            generation_before + 1
        );

        let mut byte = [0_u8; 1];
        assert_eq!(filesystem.read_node(file, rewrite_offset, &mut byte).unwrap(), 1);
        assert_eq!(byte, [b'Z']);

        let device = filesystem.into_device();
        let mut first_superblock_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second_superblock_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let remounted = PhoenixVfs::mount(
            device,
            &mut first_superblock_buffer,
            &mut second_superblock_buffer,
        )
        .unwrap();
        assert_eq!(remounted.metadata(NodeId(file.0)).unwrap().length, LARGE_BYTES as u64);
        assert_eq!(remounted.read_node(file, rewrite_offset, &mut byte).unwrap(), 1);
        assert_eq!(byte, [b'Z']);
    }

'''
text = text.replace(marker, test + marker, 1)
path.write_text(text)

status = Path('STATUS.md')
status_text = status.read_text()
status_text += '''\n\n## PhoenixFS — снятие остаточного лимита диапазонного возврата\n\nПосле перехода на возврат пользовательских экстентов диапазонами удалена устаревшая проверка, которая всё ещё прибавляла каждый блок старого экстента к лимиту массива служебных блоков. Перезапись, усечение и расширение одноэкстентных файлов больше не получают `BufferSize` только из-за размера старого пользовательского экстента. Ограниченный массив сохраняется исключительно для CoW-блоков метаданных; пользовательские данные возвращаются через `FreeSpaceExtent`. Добавлена сквозная проверка крупного файла, размер которого превышает прежний поштучный предел, включая повторное подключение тома.\n'''
status.write_text(status_text)
