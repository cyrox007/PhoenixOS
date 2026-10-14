from pathlib import Path
import re

p = Path('crates/phoenix-fs/src/lib.rs')
s = p.read_text()

# Дополняем редкие инициализаторы, где retired_count записан не сокращённой формой.
pattern = re.compile(r'DeepRecordUpdateResult \{.*?\n\s*\}', re.S)
parts = []
last = 0
for match in pattern.finditer(s):
    segment = match.group(0)
    if 'retired_extents:' not in segment:
        segment = re.sub(
            r'(\n\s*)(allocated:)',
            r'\1retired_extents: [FreeSpaceExtent::new(0, 0); MAX_OBJECT_TRANSACTION_RETIRED_EXTENTS],'
            r'\1retired_extent_count: 0,\1\2',
            segment,
            count=1,
        )
    parts.append(s[last:match.start()])
    parts.append(segment)
    last = match.end()
parts.append(s[last:])
s = ''.join(parts)

# Генератор вставляет тест перед существующим атрибутом; не допускаем повторных #[test].
while '    #[test]\n    #[test]\n' in s:
    s = s.replace('    #[test]\n    #[test]\n', '    #[test]\n')

p.write_text(s)
