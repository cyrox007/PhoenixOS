from pathlib import Path

path = Path('crates/phoenix-fs/src/lib.rs')
s = path.read_text()
marker = 'fn pruning_leaf_rebalances_adjacent_internal_nodes_when_merge_does_not_fit()'
start = s.index(marker)
end = s.find('\n    #[test]', start + len(marker))
if end < 0:
    end = len(s)
segment = s[start:end]
segment = segment.replace('MemoryBlockDevice::<512, 2048>::new()', 'MemoryBlockDevice::<512, 1024>::new()', 1)
segment = segment.replace('Superblock::new(5, 256, VOLUME_ID', 'Superblock::new(5, 128, VOLUME_ID', 1)
segment = segment.replace('FreeSpaceExtent::new(300, 120)', 'FreeSpaceExtent::new(20, 80)', 1)
s = s[:start] + segment + s[end:]
path.write_text(s)
