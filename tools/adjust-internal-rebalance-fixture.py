from pathlib import Path


path = Path(__file__).with_name("apply-internal-rebalance.py")
text = path.read_text(encoding="utf-8")

replacements = [
    (
        "MemoryBlockDevice::<512, 4096>::new()",
        "MemoryBlockDevice::<512, 1024>::new()",
    ),
    (
        "Superblock::new(5, 512, VOLUME_ID, TransactionRoots::new(8, 9))",
        "Superblock::new(5, 128, VOLUME_ID, TransactionRoots::new(8, 9))",
    ),
    (
        "ObjectTreeKey::new(object_id, ObjectRecordKind::Metadata, 0),\n                100 + index as u64,",
        "ObjectTreeKey::new(object_id, ObjectRecordKind::Metadata, 0),\n                15,",
    ),
    (
        "FreeSpaceExtent::new(300, 100)",
        "FreeSpaceExtent::new(20, 80)",
    ),
]

for old, new in replacements:
    count = text.count(old)
    if count != 1:
        raise RuntimeError(f"ожидалось одно совпадение для {old!r}, найдено {count}")
    text = text.replace(old, new, 1)

path.write_text(text, encoding="utf-8")
