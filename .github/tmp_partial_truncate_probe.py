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
    'commit_rewrite_single_extent_range',
    'commit_truncate_single_extent_file_to_zero',
    'truncate_node',
    'rewrite_data_extent_range',
]:
    print(f'===== {name} =====')
    print(extract(name))
