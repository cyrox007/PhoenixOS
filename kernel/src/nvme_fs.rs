//! Проверка PhoenixFS на обособленном участке временного NVMe QEMU.
//!
//! Файловая система получает только ограниченное представление BlockDevice.
//! Испытание не форматирует всё пространство имён и не обращается к SSD хоста.

use phoenix_block::{BlockDevice, BlockError, BlockGeometry, BlockRegion};
use phoenix_fs::{FILESYSTEM_BLOCK_SIZE, PhoenixFsError, PhoenixVfs};
use phoenix_vfs::{FileSystem, NodeKind, VfsError};

pub const TEST_REGION_START_LBA: u64 = 1024;
pub const TEST_REGION_BLOCKS: u64 = 2048;
const EXPECTED_GEOMETRY: BlockGeometry = BlockGeometry {
    block_size: 512,
    block_count: 32_768,
};
const VOLUME_ID: [u8; 16] = *b"PhoenixNVMeTest1";
const FILE_CONTENT: &[u8] = b"PhoenixOS: PhoenixFS na NVMe";
const FILE_APPEND: &[u8] = b" - proverka zapisi";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NvmeFsError {
    UnsafeDevice,
    Region(BlockError),
    Filesystem(PhoenixFsError),
    File(VfsError),
    DataMismatch,
}

impl From<BlockError> for NvmeFsError {
    fn from(error: BlockError) -> Self {
        Self::Region(error)
    }
}

impl From<PhoenixFsError> for NvmeFsError {
    fn from(error: PhoenixFsError) -> Self {
        Self::Filesystem(error)
    }
}

impl From<VfsError> for NvmeFsError {
    fn from(error: VfsError) -> Self {
        Self::File(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NvmeFsReport {
    pub generation: u64,
    pub verified_bytes: u16,
    pub region_start_lba: u64,
    pub region_blocks: u64,
}

/// Два независимых чтения проверяют совпадение исходного содержимого файла.
fn verify_contents<D: BlockDevice>(
    filesystem: &PhoenixVfs<D>,
    file: phoenix_vfs::NodeId,
) -> Result<u16, NvmeFsError> {
    let mut observed = [0_u8; 128];
    let expected_length = FILE_CONTENT.len() + FILE_APPEND.len();
    let length = filesystem.read_node(file, 0, &mut observed)?;
    if length != expected_length
        || &observed[..FILE_CONTENT.len()] != FILE_CONTENT
        || &observed[FILE_CONTENT.len()..length] != FILE_APPEND
        || filesystem.metadata(file)?.length != expected_length as u64
    {
        return Err(NvmeFsError::DataMismatch);
    }
    Ok(length as u16)
}

/// Опасное форматирование возможно только после строгой проверки геометрии
/// временного NVMe. Соседние секторы читаются до и после форматирования
/// для подтверждения изоляции тестовой области.
pub fn exercise<D: BlockDevice>(disk: &mut D) -> Result<NvmeFsReport, NvmeFsError> {
    if disk.geometry() != EXPECTED_GEOMETRY {
        return Err(NvmeFsError::UnsafeDevice);
    }

    let before_lba = TEST_REGION_START_LBA - 1;
    let after_lba = TEST_REGION_START_LBA + TEST_REGION_BLOCKS;
    let mut before_guard = [0_u8; 512];
    let mut after_guard = [0_u8; 512];
    disk.read_blocks(before_lba, &mut before_guard)?;
    disk.read_blocks(after_lba, &mut after_guard)?;

    let region = BlockRegion::new(disk, TEST_REGION_START_LBA, TEST_REGION_BLOCKS)?;
    let mut format_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
    let mut filesystem = PhoenixVfs::format_new(region, VOLUME_ID, 1, &mut format_buffer)?;
    let root = filesystem.root_node()?;
    let directory = filesystem.create_node(root, b"diagnostics", NodeKind::Directory)?;
    let file = filesystem.create_node(directory, b"nvme.txt", NodeKind::File)?;
    if filesystem.write_node(file, 0, FILE_CONTENT)? != FILE_CONTENT.len() {
        return Err(NvmeFsError::DataMismatch);
    }
    if filesystem.write_node(file, FILE_CONTENT.len() as u64, FILE_APPEND)?
        != FILE_APPEND.len()
    {
        return Err(NvmeFsError::DataMismatch);
    }
    let first_verified = verify_contents(&filesystem, file)?;
    let generation = filesystem.active_superblock().superblock.generation;
    if generation <= 1 {
        return Err(NvmeFsError::DataMismatch);
    }

    let mut region = filesystem.into_device();
    region.flush()?;
    let mut superblock_a = [0_u8; FILESYSTEM_BLOCK_SIZE];
    let mut superblock_b = [0_u8; FILESYSTEM_BLOCK_SIZE];
    let remounted = PhoenixVfs::mount(region, &mut superblock_a, &mut superblock_b)?;
    if remounted.active_superblock().superblock.generation != generation {
        return Err(NvmeFsError::DataMismatch);
    }
    let mounted_root = remounted.root_node()?;
    let mounted_directory = remounted.lookup_child(mounted_root, b"diagnostics")?;
    let mounted_file = remounted.lookup_child(mounted_directory, b"nvme.txt")?;
    if mounted_directory != directory || mounted_file != file {
        return Err(NvmeFsError::DataMismatch);
    }
    let second_verified = verify_contents(&remounted, mounted_file)?;

    let mut region = remounted.into_device();
    region.flush()?;
    let disk = region.into_inner();
    let mut current = [0_u8; 512];
    disk.read_blocks(before_lba, &mut current)?;
    if current != before_guard {
        return Err(NvmeFsError::DataMismatch);
    }
    disk.read_blocks(after_lba, &mut current)?;
    if current != after_guard {
        return Err(NvmeFsError::DataMismatch);
    }

    Ok(NvmeFsReport {
        generation,
        verified_bytes: first_verified + second_verified,
        region_start_lba: TEST_REGION_START_LBA,
        region_blocks: TEST_REGION_BLOCKS,
    })
}
