from pathlib import Path

path = Path('crates/phoenix-fs/src/lib.rs')
text = path.read_text()


def extract(name: str) -> str:
    marker = f'fn {name}'
    start = text.index(marker)
    brace = text.index('{', start)
    depth = 0
    for i in range(brace, len(text)):
        ch = text[i]
        if ch == '{':
            depth += 1
        elif ch == '}':
            depth -= 1
            if depth == 0:
                return text[start:i + 1]
    raise RuntimeError(name)

for name in [
    'write_node',
    'truncate_node',
    'phoenix_vfs_truncates_existing_file_to_zero_and_remounts',
    'phoenix_vfs_rewrites_range_inside_existing_file_atomically',
]:
    print(f'===== {name} =====')
    print(extract(name))
