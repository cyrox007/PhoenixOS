from pathlib import Path
import re

path = Path('crates/phoenix-fs/src/lib.rs')
text = path.read_text()
pattern = re.compile(r'''\n    let metadata_retired = metadata_path(?:\n        \.node_count\(\)\n        \.checked_add\(1\)\n        \.ok_or\(PhoenixFsError::ArithmeticOverflow\)\?|\.node_count\(\) \+ 1);\n    let data_retired =\n        usize::try_from\(old_extent\.block_count\)\.map_err\(\|_\| PhoenixFsError::ArithmeticOverflow\)\?;\n    if metadata_retired\n        \.checked_add\(data_retired\)\n        \.ok_or\(PhoenixFsError::ArithmeticOverflow\)\?\n        > MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS\n    \{\n        return Err\(PhoenixFsError::BufferSize\);\n    \}\n''')
replacement = '''\n    let metadata_retired = metadata_path\n        .node_count()\n        .checked_add(1)\n        .ok_or(PhoenixFsError::ArithmeticOverflow)?;\n    if metadata_retired > MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS {\n        return Err(PhoenixFsError::BufferSize);\n    }\n'''
text, count = pattern.subn(replacement, text)
if count != 4:
    raise SystemExit(f'expected 4 obsolete retirement limit checks, replaced {count}')

tests_marker = '''#[cfg(test)]\nmod tests {\n    use super::*;'''
if tests_marker not in text:
    raise SystemExit('tests module marker not found')
text = text.replace(
    tests_marker,
    '''#[cfg(test)]\nmod tests {\n    extern crate std;\n\n    use super::*;''',
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

        let device = MemoryBlockDevice::<512, 4096>::new();
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
