from pathlib import Path

p = Path('crates/phoenix-fs/src/lib.rs')
s = p.read_text()

for name in [
    'phoenix_vfs_shrinks_existing_file_and_remounts',
    'phoenix_vfs_truncates_existing_file_to_zero_and_remounts',
]:
    plain = f'    fn {name}() {{'
    marked = f'    #[test]\n    fn {name}() {{'
    if marked not in s:
        assert plain in s
        s = s.replace(plain, marked, 1)

obsolete_shrink = '''        assert_eq!(
            filesystem.truncate_node(file, 6),
            Err(VfsError::InvalidOffset)
        );
'''
assert obsolete_shrink in s
s = s.replace(obsolete_shrink, '', 1)

obsolete_zero = '''        assert_eq!(
            filesystem.truncate_node(file, 1),
            Err(VfsError::InvalidOffset)
        );
'''
assert obsolete_zero in s
s = s.replace(obsolete_zero, '', 1)

p.write_text(s)
