from pathlib import Path

path = Path('crates/phoenix-fs/src/lib.rs')
text = path.read_text()

old_error = '    NoValidSuperblock,\n    Device(BlockError),'
new_error = '    NoValidSuperblock,\n    NoRecoverableSnapshot,\n    Device(BlockError),'
if old_error not in text:
    raise SystemExit('Не найдено место для ошибки восстановления')
text = text.replace(old_error, new_error, 1)

old_mount = '        let active = read_active_superblock(&mut device, first_buffer, second_buffer)?;'
new_mount = '        let active = read_recoverable_superblock(&mut device, first_buffer, second_buffer)?;'
if old_mount not in text:
    raise SystemExit('Не найден путь подключения PhoenixVfs')
text = text.replace(old_mount, new_mount, 1)

marker = 'pub fn commit_next_superblock<D: BlockDevice>('
if marker not in text:
    raise SystemExit('Не найден commit_next_superblock')

helpers = r'''pub fn read_recoverable_superblock<D: BlockDevice>(
    device: &mut D,
    first_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    second_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<ActiveSuperblock, PhoenixFsError> {
    let preferred = read_active_superblock(device, first_buffer, second_buffer)?;
    if validate_superblock_snapshot(
        device,
        preferred.superblock,
        first_buffer,
        second_buffer,
    )
    .is_ok()
    {
        return Ok(preferred);
    }

    let total_blocks = filesystem_block_count(device)?;
    let fallback_slot = preferred.slot.other();
    let fallback = read_superblock_copy(
        device,
        fallback_slot.filesystem_block(),
        total_blocks,
        first_buffer,
    )
    .map_err(|_| PhoenixFsError::NoRecoverableSnapshot)?;

    validate_previous_snapshot(preferred.superblock, fallback)
        .map_err(|_| PhoenixFsError::NoRecoverableSnapshot)?;
    validate_superblock_snapshot(device, fallback, first_buffer, second_buffer)
        .map_err(|_| PhoenixFsError::NoRecoverableSnapshot)?;

    Ok(ActiveSuperblock {
        superblock: fallback,
        slot: fallback_slot,
    })
}

fn validate_previous_snapshot(
    newer: Superblock,
    older: Superblock,
) -> Result<(), PhoenixFsError> {
    if older.volume_id != newer.volume_id {
        return Err(PhoenixFsError::VolumeIdentityChanged);
    }
    if older.total_blocks != newer.total_blocks {
        return Err(PhoenixFsError::VolumeGeometryChanged);
    }
    if older.incompatible_features != newer.incompatible_features {
        return Err(PhoenixFsError::FeatureSetChanged);
    }
    if older.generation.checked_add(1) != Some(newer.generation) {
        return Err(PhoenixFsError::GenerationSequence);
    }
    Ok(())
}

fn validate_superblock_snapshot<D: BlockDevice>(
    device: &mut D,
    superblock: Superblock,
    node_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    child_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<(), PhoenixFsError> {
    let total_blocks = filesystem_block_count(device)?;
    if superblock.total_blocks != total_blocks {
        return Err(PhoenixFsError::InvalidVolumeGeometry);
    }

    validate_object_snapshot_node(
        device,
        superblock.roots.object_tree,
        superblock.generation,
        total_blocks,
        0,
        node_buffer,
        child_buffer,
    )?;

    read_filesystem_block(device, superblock.roots.free_space_tree, node_buffer)?;
    let free_space = validate_free_space_leaf(node_buffer, total_blocks)?;
    if free_space.metadata.block_number != superblock.roots.free_space_tree {
        return Err(PhoenixFsError::InvalidTransactionRoots);
    }
    if free_space.metadata.generation > superblock.generation {
        return Err(PhoenixFsError::GenerationSequence);
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_object_snapshot_node<D: BlockDevice>(
    device: &mut D,
    block_number: u64,
    maximum_generation: u64,
    total_blocks: u64,
    depth: usize,
    node_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    child_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<(), PhoenixFsError> {
    if depth > MAX_OBJECT_TREE_DEPTH {
        return Err(PhoenixFsError::ObjectTreeDepthExceeded);
    }
    if block_number < SUPERBLOCK_COPY_COUNT || block_number >= total_blocks {
        return Err(PhoenixFsError::InvalidObjectChildBlock(block_number));
    }

    read_filesystem_block(device, block_number, node_buffer)?;
    let node = TreeNodeHeader::decode(node_buffer)?;
    if node.metadata.kind != MetadataKind::ObjectTree {
        return Err(PhoenixFsError::ObjectTreePathMismatch);
    }
    if node.metadata.block_number != block_number {
        return Err(PhoenixFsError::InvalidMetadataBlock(node.metadata.block_number));
    }
    if node.metadata.generation > maximum_generation {
        return Err(PhoenixFsError::ObjectNodeGenerationAhead);
    }

    if node.level == 0 {
        validate_object_leaf(node_buffer)?;
        return Ok(());
    }

    let parent = validate_object_internal(node_buffer)?;
    let item_count = parent.item_count as usize;
    for child_index in 0..item_count {
        read_filesystem_block(device, block_number, node_buffer)?;
        let parent = validate_object_internal(node_buffer)?;
        if parent.metadata.block_number != block_number {
            return Err(PhoenixFsError::InvalidMetadataBlock(parent.metadata.block_number));
        }
        let record = object_internal_record_at(node_buffer, child_index)?;
        if record.child_block >= total_blocks {
            return Err(PhoenixFsError::InvalidObjectChildBlock(record.child_block));
        }

        read_filesystem_block(device, record.child_block, child_buffer)?;
        validate_object_tree_transition(node_buffer, child_index, child_buffer)?;
        let child = TreeNodeHeader::decode(child_buffer)?;
        if child.metadata.block_number != record.child_block {
            return Err(PhoenixFsError::InvalidMetadataBlock(child.metadata.block_number));
        }
        if child.metadata.generation > maximum_generation {
            return Err(PhoenixFsError::ObjectNodeGenerationAhead);
        }

        validate_object_snapshot_node(
            device,
            record.child_block,
            maximum_generation,
            total_blocks,
            depth + 1,
            child_buffer,
            node_buffer,
        )?;
    }

    Ok(())
}

'''
text = text.replace(marker, helpers + marker, 1)

# Добавляем регрессионные проверки в конец тестового модуля.
insert_at = text.rfind('\n}')
if insert_at == -1:
    raise SystemExit('Не найден конец тестового модуля')

tests = r'''

    #[test]
    fn recovery_falls_back_to_previous_generation_when_latest_object_tree_is_corrupted() {
        let mut device = MemoryBlockDevice::<512, 80>::new();
        let mut format_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let current = format_volume(&mut device, VOLUME_ID, 10, &mut format_buffer).unwrap();

        let mut object_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut object_copy = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut current_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut next_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut superblock_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let committed = commit_create_object(
            &mut device,
            current,
            ROOT_OBJECT_ID,
            1,
            2,
            "recover-me",
            ObjectType::File,
            20,
            &mut object_buffer,
            &mut object_copy,
            &mut current_free,
            &mut next_free,
            &mut superblock_buffer,
        )
        .unwrap();
        assert_eq!(committed.active.superblock.generation, 2);

        let latest_root = committed.active.superblock.roots.object_tree;
        let mut damaged = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(&mut device, latest_root, &mut damaged).unwrap();
        damaged[METADATA_HEADER_SIZE as usize + 3] ^= 0x5a;
        write_filesystem_block(&mut device, latest_root, &damaged).unwrap();
        device.flush().unwrap();

        let mut first = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let selected = read_active_superblock(&mut device, &mut first, &mut second).unwrap();
        assert_eq!(selected.superblock.generation, 2);

        let recovered = read_recoverable_superblock(&mut device, &mut first, &mut second).unwrap();
        assert_eq!(recovered.superblock.generation, 1);
        assert_eq!(recovered.superblock.roots, current.superblock.roots);
    }

    #[test]
    fn recovery_falls_back_to_previous_generation_when_latest_free_space_root_is_corrupted() {
        let mut device = MemoryBlockDevice::<512, 80>::new();
        let mut format_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let current = format_volume(&mut device, VOLUME_ID, 10, &mut format_buffer).unwrap();

        let mut object_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut object_copy = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut current_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut next_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut superblock_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let committed = commit_create_object(
            &mut device,
            current,
            ROOT_OBJECT_ID,
            1,
            2,
            "recover-free-space",
            ObjectType::File,
            20,
            &mut object_buffer,
            &mut object_copy,
            &mut current_free,
            &mut next_free,
            &mut superblock_buffer,
        )
        .unwrap();

        let latest_root = committed.active.superblock.roots.free_space_tree;
        let mut damaged = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(&mut device, latest_root, &mut damaged).unwrap();
        damaged[METADATA_HEADER_SIZE as usize + 5] ^= 0xa5;
        write_filesystem_block(&mut device, latest_root, &damaged).unwrap();
        device.flush().unwrap();

        let mut first = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let recovered = read_recoverable_superblock(&mut device, &mut first, &mut second).unwrap();
        assert_eq!(recovered.superblock.generation, 1);
        assert_eq!(recovered.superblock.roots, current.superblock.roots);
    }

    #[test]
    fn recovery_rejects_non_adjacent_snapshot_as_unsafe_fallback() {
        let mut device = MemoryBlockDevice::<512, 64>::new();
        let older = Superblock::new(2, 8, VOLUME_ID, ROOTS).unwrap();
        let newer = Superblock::new(5, 8, VOLUME_ID, ROOTS).unwrap();
        let mut first = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second = [0_u8; FILESYSTEM_BLOCK_SIZE];
        older.encode(&mut first).unwrap();
        newer.encode(&mut second).unwrap();
        device.write_blocks(0, &first).unwrap();
        device.write_blocks(8, &second).unwrap();
        device.flush().unwrap();

        assert_eq!(
            read_recoverable_superblock(&mut device, &mut first, &mut second),
            Err(PhoenixFsError::NoRecoverableSnapshot)
        );
    }
'''
text = text[:insert_at] + tests + text[insert_at:]
path.write_text(text)

status = Path('STATUS.md')
status_text = status.read_text().rstrip() + r'''


## PhoenixFS — восстановление предыдущего устойчивого поколения

Подключение PhoenixFS теперь проверяет не только контрольную сумму выбранной копии суперблока, но и весь достижимый снимок дерева объектов вместе с корнем дерева свободного места. Обход дерева объектов выполняется без динамического выделения памяти, повторно проверяет типы и номера блоков, уровни, ключи-разделители и ограничение поколений. Если новейший суперблок цел, но его снимок метаданных повреждён или не завершён, подключение автоматически пробует предыдущую копию суперблока.

Резервным состоянием допускается только непосредственно предыдущее поколение того же тома с той же геометрией и набором несовместимых признаков. Более старую копию нельзя считать безопасным снимком: её блоки уже могли быть возвращены в свободное пространство после последующих фиксаций. Поэтому разрыв поколений отклоняется вместо произвольного отката. Добавлены проверки повреждения нового корня дерева объектов, повреждения нового корня свободного места и запрета небезопасного отката через несколько поколений.
'''
status.write_text(status_text + '\n')
