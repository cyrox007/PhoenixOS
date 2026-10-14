from pathlib import Path

p = Path('crates/phoenix-fs/src/lib.rs')
s = p.read_text()
anchor = '''    #[test]
    fn phoenix_vfs_extends_existing_file_with_zeroes_and_remounts() {'''
assert anchor in s

test = r'''    #[test]
    fn phoenix_vfs_rewrites_file_larger_than_retired_block_capacity() {
        use phoenix_vfs::{FileSystem, NodeKind};

        let device = MemoryBlockDevice::<512, 2048>::new();
        let mut buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut filesystem = PhoenixVfs::format_new(device, VOLUME_ID, 1, &mut buffer).unwrap();
        let root = filesystem.root_node().unwrap();
        let file = filesystem
            .create_node(root, b"large-retired.bin", NodeKind::File)
            .unwrap();

        let old_blocks = MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS as u64 + 8;
        let length = old_blocks * FILESYSTEM_BLOCK_SIZE as u64;
        filesystem.truncate_node(file, length).unwrap();
        let before = filesystem.active_superblock().superblock.generation;

        assert_eq!(filesystem.write_node(file, length - 1, &[0xA5]).unwrap(), 1);
        assert_eq!(filesystem.active_superblock().superblock.generation, before + 1);
        let mut last = [0_u8; 1];
        assert_eq!(filesystem.read_node(file, length - 1, &mut last).unwrap(), 1);
        assert_eq!(last, [0xA5]);

        let device = filesystem.into_device();
        let mut first = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let remounted = PhoenixVfs::mount(device, &mut first, &mut second).unwrap();
        assert_eq!(remounted.metadata(file).unwrap().length, length);
        let mut last = [0_u8; 1];
        assert_eq!(remounted.read_node(file, length - 1, &mut last).unwrap(), 1);
        assert_eq!(last, [0xA5]);
    }

'''
s = s.replace(anchor, test + anchor, 1)
p.write_text(s)
