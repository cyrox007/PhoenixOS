#![no_std]

use core::cell::RefCell;
use phoenix_block::{BlockDevice, BlockError};
use phoenix_vfs::{FileSystem, NodeId, NodeKind, NodeMetadata, VfsError};

pub const FILESYSTEM_BLOCK_SIZE: usize = 4096;
pub const SUPERBLOCK_COPY_COUNT: u64 = 2;
pub const FORMAT_VERSION: u32 = 1;

const SUPERBLOCK_MAGIC: [u8; 8] = *b"PHXFS\0\x01\0";
const SUPERBLOCK_HEADER_SIZE: u32 = 88;
const SUPERBLOCK_CHECKSUM_OFFSET: usize = 64;
const SUPERBLOCK_CHECKSUM_END: usize = 68;
const METADATA_MAGIC: [u8; 8] = *b"PHXMETA\0";
const METADATA_HEADER_SIZE: u32 = 48;
const METADATA_CHECKSUM_OFFSET: usize = 44;
const METADATA_CHECKSUM_END: usize = 48;
const TREE_NODE_HEADER_SIZE: u32 = 64;
const TREE_NODE_PREFIX_SIZE: u32 = TREE_NODE_HEADER_SIZE - METADATA_HEADER_SIZE;
pub const OBJECT_LEAF_RECORD_HEADER_SIZE: usize = 32;
pub const OBJECT_INTERNAL_RECORD_SIZE: usize = 32;
pub const FREE_SPACE_RECORD_SIZE: usize = 16;
pub const MAX_OBJECT_TREE_DEPTH: usize = 16;
pub const MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS: usize = MAX_OBJECT_TREE_DEPTH * 2 + 3;
pub const ROOT_OBJECT_ID: u64 = 1;
pub const INITIAL_OBJECT_TREE_BLOCK: u64 = 2;
pub const INITIAL_FREE_SPACE_TREE_BLOCK: u64 = 3;
pub const MINIMUM_FILESYSTEM_BLOCKS: u64 = 5;
pub const OBJECT_METADATA_VALUE_SIZE: usize = 48;
pub const DIRECTORY_ENTRY_VALUE_HEADER_SIZE: usize = 16;
pub const MAX_DIRECTORY_NAME_BYTES: usize = 1024;
pub const EXTENT_VALUE_SIZE: usize = 24;
const OBJECT_LEAF_RECORD_ALIGNMENT: usize = 8;
const SUPPORTED_INCOMPATIBLE_FEATURES: u64 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhoenixFsError {
    BufferSize,
    InvalidMagic,
    UnsupportedVersion(u32),
    InvalidHeaderSize,
    InvalidFilesystemBlockSize,
    UnsupportedDeviceBlockSize,
    InvalidDeviceGeometry,
    InvalidGeneration,
    InvalidVolumeId,
    InvalidVolumeGeometry,
    InvalidRootBlock(u64),
    DuplicateRootBlock(u64),
    InvalidMetadataMagic,
    InvalidMetadataHeaderSize,
    InvalidMetadataKind(u32),
    InvalidMetadataGeneration,
    InvalidMetadataBlock(u64),
    InvalidMetadataPayloadSize(u32),
    MetadataChecksumMismatch,
    InvalidTreeNodePayloadSize(u32),
    InvalidTreeNodeItemCount,
    InvalidObjectId,
    InvalidObjectRecordKind(u32),
    InvalidObjectRecordFlags(u32),
    InvalidObjectRecordSize,
    InvalidObjectLeafNode,
    InvalidObjectInternalNode,
    InvalidObjectInternalRecordSize,
    InvalidObjectChildBlock(u64),
    InvalidObjectTreeLevel,
    ObjectChildGenerationAhead,
    ObjectChildKeyMismatch,
    ObjectRecordsOutOfOrder,
    InvalidFreeSpaceLeafNode,
    InvalidFreeSpaceRecordSize,
    InvalidFreeSpaceRange,
    FreeSpaceRangesNotCanonical,
    NoFreeSpace,
    StaleAllocationPlan,
    AllocationTargetOutsideRange,
    InvalidObjectRewriteTarget,
    InvalidTransactionRoots,
    InvalidRetiredBlock(u64),
    DuplicateRetiredBlock(u64),
    RetiredBlockStillReferenced(u64),
    ObjectRecordNotFound,
    ObjectRecordAlreadyExists,
    ObjectLeafWouldBecomeEmpty,
    InvalidObjectMutationBatch,
    ObjectMutationBatchSpansLeaves,
    InvalidReclaimedBlock(u64),
    ReclaimedBlockAlreadyFree(u64),
    ObjectTreeDepthExceeded,
    ObjectNodeGenerationAhead,
    ObjectTreePathMismatch,
    InvalidObjectValueSize,
    InvalidObjectType(u32),
    InvalidObjectFlags(u32),
    InvalidDirectoryName,
    InvalidExtent,
    InvalidReadRange,
    DirectoryEntryNotFound,
    ParentNotDirectory,
    ObjectNotEmpty,
    CannotRemoveRootObject,
    DirectoryEntryTargetMismatch,
    UnsupportedFeatures(u64),
    InvalidReservedField,
    ChecksumMismatch,
    ArithmeticOverflow,
    ConflictingGeneration(u64),
    GenerationSequence,
    VolumeIdentityChanged,
    VolumeGeometryChanged,
    FeatureSetChanged,
    NoValidSuperblock,
    Device(BlockError),
}

impl From<BlockError> for PhoenixFsError {
    fn from(error: BlockError) -> Self {
        Self::Device(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuperblockSlot {
    First,
    Second,
}

impl SuperblockSlot {
    pub const fn filesystem_block(self) -> u64 {
        match self {
            Self::First => 0,
            Self::Second => 1,
        }
    }

    pub const fn other(self) -> Self {
        match self {
            Self::First => Self::Second,
            Self::Second => Self::First,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveSuperblock {
    pub superblock: Superblock,
    pub slot: SuperblockSlot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransactionRoots {
    pub object_tree: u64,
    pub free_space_tree: u64,
}

impl TransactionRoots {
    pub const fn new(object_tree: u64, free_space_tree: u64) -> Self {
        Self {
            object_tree,
            free_space_tree,
        }
    }

    fn validate(self, total_blocks: u64) -> Result<(), PhoenixFsError> {
        for block in [self.object_tree, self.free_space_tree] {
            if block < SUPERBLOCK_COPY_COUNT || block >= total_blocks {
                return Err(PhoenixFsError::InvalidRootBlock(block));
            }
        }
        if self.object_tree == self.free_space_tree {
            return Err(PhoenixFsError::DuplicateRootBlock(self.object_tree));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum MetadataKind {
    ObjectTree = 1,
    FreeSpaceTree = 2,
}

impl MetadataKind {
    fn from_raw(value: u32) -> Result<Self, PhoenixFsError> {
        match value {
            1 => Ok(Self::ObjectTree),
            2 => Ok(Self::FreeSpaceTree),
            other => Err(PhoenixFsError::InvalidMetadataKind(other)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetadataBlockHeader {
    pub kind: MetadataKind,
    pub generation: u64,
    pub block_number: u64,
    pub payload_bytes: u32,
}

impl MetadataBlockHeader {
    pub const fn new(
        kind: MetadataKind,
        generation: u64,
        block_number: u64,
        payload_bytes: u32,
    ) -> Self {
        Self {
            kind,
            generation,
            block_number,
            payload_bytes,
        }
    }

    pub fn seal(&self, block: &mut [u8]) -> Result<(), PhoenixFsError> {
        self.validate()?;
        if block.len() != FILESYSTEM_BLOCK_SIZE {
            return Err(PhoenixFsError::BufferSize);
        }

        self.write_prefix(block)?;
        let checksum = metadata_crc32c(block);
        write_u32(block, METADATA_CHECKSUM_OFFSET, checksum);
        Ok(())
    }

    pub fn decode(block: &[u8]) -> Result<Self, PhoenixFsError> {
        if block.len() != FILESYSTEM_BLOCK_SIZE {
            return Err(PhoenixFsError::BufferSize);
        }
        if block[..8] != METADATA_MAGIC {
            return Err(PhoenixFsError::InvalidMetadataMagic);
        }

        let version = read_u32(block, 8);
        if version != FORMAT_VERSION {
            return Err(PhoenixFsError::UnsupportedVersion(version));
        }
        if read_u32(block, 12) != METADATA_HEADER_SIZE {
            return Err(PhoenixFsError::InvalidMetadataHeaderSize);
        }
        if read_u32(block, 20) != 0 {
            return Err(PhoenixFsError::InvalidReservedField);
        }

        let expected = read_u32(block, METADATA_CHECKSUM_OFFSET);
        if expected != metadata_crc32c(block) {
            return Err(PhoenixFsError::MetadataChecksumMismatch);
        }

        let header = Self {
            kind: MetadataKind::from_raw(read_u32(block, 16))?,
            generation: read_u64(block, 24),
            block_number: read_u64(block, 32),
            payload_bytes: read_u32(block, 40),
        };
        header.validate()?;
        Ok(header)
    }

    pub fn validate_for_volume(&self, total_blocks: u64) -> Result<(), PhoenixFsError> {
        self.validate()?;
        if self.block_number >= total_blocks {
            return Err(PhoenixFsError::InvalidMetadataBlock(self.block_number));
        }
        Ok(())
    }

    fn write_prefix(&self, block: &mut [u8]) -> Result<(), PhoenixFsError> {
        self.validate()?;
        if block.len() != FILESYSTEM_BLOCK_SIZE {
            return Err(PhoenixFsError::BufferSize);
        }

        block[..METADATA_HEADER_SIZE as usize].fill(0);
        block[..8].copy_from_slice(&METADATA_MAGIC);
        write_u32(block, 8, FORMAT_VERSION);
        write_u32(block, 12, METADATA_HEADER_SIZE);
        write_u32(block, 16, self.kind as u32);
        write_u64(block, 24, self.generation);
        write_u64(block, 32, self.block_number);
        write_u32(block, 40, self.payload_bytes);
        Ok(())
    }

    fn validate(&self) -> Result<(), PhoenixFsError> {
        if self.generation == 0 {
            return Err(PhoenixFsError::InvalidMetadataGeneration);
        }
        if self.block_number < SUPERBLOCK_COPY_COUNT {
            return Err(PhoenixFsError::InvalidMetadataBlock(self.block_number));
        }

        let capacity = FILESYSTEM_BLOCK_SIZE - METADATA_HEADER_SIZE as usize;
        let payload_size = usize::try_from(self.payload_bytes)
            .map_err(|_| PhoenixFsError::InvalidMetadataPayloadSize(self.payload_bytes))?;
        if payload_size > capacity {
            return Err(PhoenixFsError::InvalidMetadataPayloadSize(
                self.payload_bytes,
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeNodeHeader {
    pub metadata: MetadataBlockHeader,
    pub level: u32,
    pub item_count: u32,
    pub entries_bytes: u32,
}

impl TreeNodeHeader {
    pub fn new(
        kind: MetadataKind,
        generation: u64,
        block_number: u64,
        level: u32,
        item_count: u32,
        entries_bytes: u32,
    ) -> Result<Self, PhoenixFsError> {
        let payload_bytes = TREE_NODE_PREFIX_SIZE
            .checked_add(entries_bytes)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        let node = Self {
            metadata: MetadataBlockHeader::new(kind, generation, block_number, payload_bytes),
            level,
            item_count,
            entries_bytes,
        };
        node.validate()?;
        Ok(node)
    }

    pub fn seal(&self, block: &mut [u8]) -> Result<(), PhoenixFsError> {
        self.validate()?;
        if block.len() != FILESYSTEM_BLOCK_SIZE {
            return Err(PhoenixFsError::BufferSize);
        }

        self.metadata.write_prefix(block)?;
        block[METADATA_HEADER_SIZE as usize..TREE_NODE_HEADER_SIZE as usize].fill(0);
        write_u32(block, 48, self.level);
        write_u32(block, 52, self.item_count);
        write_u32(block, 56, self.entries_bytes);

        let checksum = metadata_crc32c(block);
        write_u32(block, METADATA_CHECKSUM_OFFSET, checksum);
        Ok(())
    }

    pub fn decode(block: &[u8]) -> Result<Self, PhoenixFsError> {
        let metadata = MetadataBlockHeader::decode(block)?;
        let node = Self {
            metadata,
            level: read_u32(block, 48),
            item_count: read_u32(block, 52),
            entries_bytes: read_u32(block, 56),
        };

        if read_u32(block, 60) != 0 {
            return Err(PhoenixFsError::InvalidReservedField);
        }

        node.validate()?;
        Ok(node)
    }

    pub const fn entries_offset() -> usize {
        TREE_NODE_HEADER_SIZE as usize
    }

    fn validate(&self) -> Result<(), PhoenixFsError> {
        self.metadata.validate()?;

        let expected_payload = TREE_NODE_PREFIX_SIZE
            .checked_add(self.entries_bytes)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        if self.metadata.payload_bytes != expected_payload {
            return Err(PhoenixFsError::InvalidTreeNodePayloadSize(
                self.metadata.payload_bytes,
            ));
        }

        let entries_capacity = FILESYSTEM_BLOCK_SIZE - TREE_NODE_HEADER_SIZE as usize;
        let entries_size = usize::try_from(self.entries_bytes)
            .map_err(|_| PhoenixFsError::InvalidTreeNodePayloadSize(self.entries_bytes))?;
        if entries_size > entries_capacity {
            return Err(PhoenixFsError::InvalidTreeNodePayloadSize(
                self.entries_bytes,
            ));
        }

        if self.item_count == 0 && self.entries_bytes != 0 {
            return Err(PhoenixFsError::InvalidTreeNodeItemCount);
        }
        if self.item_count != 0 && self.entries_bytes == 0 {
            return Err(PhoenixFsError::InvalidTreeNodeItemCount);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ObjectRecordKind {
    Metadata = 1,
    DirectoryEntry = 2,
    Extent = 3,
}

impl ObjectRecordKind {
    fn from_raw(value: u32) -> Result<Self, PhoenixFsError> {
        match value {
            1 => Ok(Self::Metadata),
            2 => Ok(Self::DirectoryEntry),
            3 => Ok(Self::Extent),
            other => Err(PhoenixFsError::InvalidObjectRecordKind(other)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ObjectTreeKey {
    pub object_id: u64,
    pub kind: ObjectRecordKind,
    pub offset: u64,
}

impl ObjectTreeKey {
    pub const fn new(object_id: u64, kind: ObjectRecordKind, offset: u64) -> Self {
        Self {
            object_id,
            kind,
            offset,
        }
    }

    fn validate(self) -> Result<(), PhoenixFsError> {
        if self.object_id == 0 {
            return Err(PhoenixFsError::InvalidObjectId);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ObjectType {
    File = 1,
    Directory = 2,
}

impl ObjectType {
    fn from_raw(value: u32) -> Result<Self, PhoenixFsError> {
        match value {
            1 => Ok(Self::File),
            2 => Ok(Self::Directory),
            other => Err(PhoenixFsError::InvalidObjectType(other)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectMetadataValue {
    pub object_type: ObjectType,
    pub size_bytes: u64,
    pub created_ns: u64,
    pub modified_ns: u64,
    pub changed_ns: u64,
}

impl ObjectMetadataValue {
    pub const fn new(
        object_type: ObjectType,
        size_bytes: u64,
        created_ns: u64,
        modified_ns: u64,
        changed_ns: u64,
    ) -> Self {
        Self {
            object_type,
            size_bytes,
            created_ns,
            modified_ns,
            changed_ns,
        }
    }

    pub fn encode(self, destination: &mut [u8]) -> Result<(), PhoenixFsError> {
        if destination.len() != OBJECT_METADATA_VALUE_SIZE {
            return Err(PhoenixFsError::InvalidObjectValueSize);
        }

        destination.fill(0);
        write_u32(destination, 0, self.object_type as u32);
        write_u64(destination, 8, self.size_bytes);
        write_u64(destination, 16, self.created_ns);
        write_u64(destination, 24, self.modified_ns);
        write_u64(destination, 32, self.changed_ns);
        Ok(())
    }

    pub fn decode(source: &[u8]) -> Result<Self, PhoenixFsError> {
        if source.len() != OBJECT_METADATA_VALUE_SIZE {
            return Err(PhoenixFsError::InvalidObjectValueSize);
        }
        let flags = read_u32(source, 4);
        if flags != 0 {
            return Err(PhoenixFsError::InvalidObjectFlags(flags));
        }
        if read_u64(source, 40) != 0 {
            return Err(PhoenixFsError::InvalidReservedField);
        }

        Ok(Self {
            object_type: ObjectType::from_raw(read_u32(source, 0))?,
            size_bytes: read_u64(source, 8),
            created_ns: read_u64(source, 16),
            modified_ns: read_u64(source, 24),
            changed_ns: read_u64(source, 32),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectoryEntryValue {
    pub target_object_id: u64,
    pub target_type: ObjectType,
}

impl DirectoryEntryValue {
    pub fn new(target_object_id: u64, target_type: ObjectType) -> Result<Self, PhoenixFsError> {
        if target_object_id == 0 {
            return Err(PhoenixFsError::InvalidObjectId);
        }
        Ok(Self {
            target_object_id,
            target_type,
        })
    }

    pub fn encoded_size(name: &str) -> Result<usize, PhoenixFsError> {
        validate_directory_name(name)?;
        DIRECTORY_ENTRY_VALUE_HEADER_SIZE
            .checked_add(name.len())
            .ok_or(PhoenixFsError::ArithmeticOverflow)
    }

    pub fn encode(self, name: &str, destination: &mut [u8]) -> Result<usize, PhoenixFsError> {
        if self.target_object_id == 0 {
            return Err(PhoenixFsError::InvalidObjectId);
        }
        let encoded_size = Self::encoded_size(name)?;
        if destination.len() < encoded_size {
            return Err(PhoenixFsError::BufferSize);
        }

        destination[..encoded_size].fill(0);
        write_u64(destination, 0, self.target_object_id);
        write_u32(destination, 8, self.target_type as u32);
        let name_bytes =
            u32::try_from(name.len()).map_err(|_| PhoenixFsError::InvalidDirectoryName)?;
        write_u32(destination, 12, name_bytes);
        destination[DIRECTORY_ENTRY_VALUE_HEADER_SIZE..encoded_size]
            .copy_from_slice(name.as_bytes());
        Ok(encoded_size)
    }

    pub fn decode(source: &[u8]) -> Result<(Self, &str), PhoenixFsError> {
        if source.len() < DIRECTORY_ENTRY_VALUE_HEADER_SIZE {
            return Err(PhoenixFsError::InvalidObjectValueSize);
        }

        let target_object_id = read_u64(source, 0);
        if target_object_id == 0 {
            return Err(PhoenixFsError::InvalidObjectId);
        }
        let target_type = ObjectType::from_raw(read_u32(source, 8))?;
        let name_bytes = read_u32(source, 12) as usize;
        let expected_size = DIRECTORY_ENTRY_VALUE_HEADER_SIZE
            .checked_add(name_bytes)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        if source.len() != expected_size {
            return Err(PhoenixFsError::InvalidObjectValueSize);
        }

        let name = core::str::from_utf8(&source[DIRECTORY_ENTRY_VALUE_HEADER_SIZE..])
            .map_err(|_| PhoenixFsError::InvalidDirectoryName)?;
        validate_directory_name(name)?;
        Ok((
            Self {
                target_object_id,
                target_type,
            },
            name,
        ))
    }
}

fn validate_directory_name(name: &str) -> Result<(), PhoenixFsError> {
    if name.is_empty()
        || name.len() > MAX_DIRECTORY_NAME_BYTES
        || name == "."
        || name == ".."
        || name.bytes().any(|byte| byte == 0 || byte == b'/')
    {
        return Err(PhoenixFsError::InvalidDirectoryName);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtentValue {
    pub physical_start_block: u64,
    pub block_count: u64,
    pub data_bytes: u64,
}

impl ExtentValue {
    pub const fn new(physical_start_block: u64, block_count: u64, data_bytes: u64) -> Self {
        Self {
            physical_start_block,
            block_count,
            data_bytes,
        }
    }

    pub fn encode(self, destination: &mut [u8]) -> Result<(), PhoenixFsError> {
        if destination.len() != EXTENT_VALUE_SIZE {
            return Err(PhoenixFsError::InvalidObjectValueSize);
        }
        if self.block_count == 0 || self.data_bytes == 0 {
            return Err(PhoenixFsError::InvalidExtent);
        }

        write_u64(destination, 0, self.physical_start_block);
        write_u64(destination, 8, self.block_count);
        write_u64(destination, 16, self.data_bytes);
        Ok(())
    }

    pub fn decode(source: &[u8]) -> Result<Self, PhoenixFsError> {
        if source.len() != EXTENT_VALUE_SIZE {
            return Err(PhoenixFsError::InvalidObjectValueSize);
        }

        let extent = Self {
            physical_start_block: read_u64(source, 0),
            block_count: read_u64(source, 8),
            data_bytes: read_u64(source, 16),
        };
        if extent.block_count == 0 || extent.data_bytes == 0 {
            return Err(PhoenixFsError::InvalidExtent);
        }
        Ok(extent)
    }

    pub fn validate_for_key(
        self,
        key: ObjectTreeKey,
        total_blocks: u64,
    ) -> Result<(), PhoenixFsError> {
        if key.kind != ObjectRecordKind::Extent
            || !key.offset.is_multiple_of(FILESYSTEM_BLOCK_SIZE as u64)
            || self.physical_start_block < SUPERBLOCK_COPY_COUNT
        {
            return Err(PhoenixFsError::InvalidExtent);
        }

        let end_block = self
            .physical_start_block
            .checked_add(self.block_count)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        if end_block > total_blocks {
            return Err(PhoenixFsError::InvalidExtent);
        }

        let capacity = self
            .block_count
            .checked_mul(FILESYSTEM_BLOCK_SIZE as u64)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        if self.data_bytes > capacity {
            return Err(PhoenixFsError::InvalidExtent);
        }
        Ok(())
    }
}

pub fn validate_object_record_value(
    key: ObjectTreeKey,
    value: &[u8],
    total_blocks: u64,
) -> Result<(), PhoenixFsError> {
    match key.kind {
        ObjectRecordKind::Metadata => {
            if key.offset != 0 {
                return Err(PhoenixFsError::InvalidObjectValueSize);
            }
            ObjectMetadataValue::decode(value)?;
            Ok(())
        }
        ObjectRecordKind::DirectoryEntry => {
            DirectoryEntryValue::decode(value)?;
            Ok(())
        }
        ObjectRecordKind::Extent => {
            let extent = ExtentValue::decode(value)?;
            extent.validate_for_key(key, total_blocks)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectoryLookupResult {
    pub entry_id: u64,
    pub target_object_id: u64,
    pub target_type: ObjectType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObjectScanFrame {
    block_number: u64,
    next_child_index: u32,
}

const EMPTY_OBJECT_SCAN_FRAME: ObjectScanFrame = ObjectScanFrame {
    block_number: 0,
    next_child_index: 0,
};

pub fn read_object_record_value<D: BlockDevice>(
    device: &mut D,
    root_block: u64,
    key: ObjectTreeKey,
    maximum_generation: u64,
    node_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    value_buffer: &mut [u8],
) -> Result<usize, PhoenixFsError> {
    let path = find_object_tree_path(device, root_block, key, maximum_generation, node_buffer)?;
    read_filesystem_block(device, path.leaf_block, node_buffer)?;

    let node = validate_object_leaf(node_buffer)?;
    let entries_start = TreeNodeHeader::entries_offset();
    let entries_end = entries_start
        .checked_add(node.entries_bytes as usize)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let mut cursor = entries_start;

    for _ in 0..node.item_count {
        let (record, value, encoded_size) =
            ObjectLeafRecordHeader::decode_with_value(&node_buffer[cursor..entries_end])?;
        if record.key == key {
            if value_buffer.len() < value.len() {
                return Err(PhoenixFsError::BufferSize);
            }
            value_buffer[..value.len()].copy_from_slice(value);
            return Ok(value.len());
        }
        if record.key > key {
            break;
        }
        cursor = cursor
            .checked_add(encoded_size)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    }

    Err(PhoenixFsError::ObjectRecordNotFound)
}

pub fn read_object_metadata<D: BlockDevice>(
    device: &mut D,
    root_block: u64,
    object_id: u64,
    maximum_generation: u64,
    node_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    value_buffer: &mut [u8; OBJECT_METADATA_VALUE_SIZE],
) -> Result<ObjectMetadataValue, PhoenixFsError> {
    let key = ObjectTreeKey::new(object_id, ObjectRecordKind::Metadata, 0);
    let size = read_object_record_value(
        device,
        root_block,
        key,
        maximum_generation,
        node_buffer,
        value_buffer,
    )?;
    if size != OBJECT_METADATA_VALUE_SIZE {
        return Err(PhoenixFsError::InvalidObjectValueSize);
    }
    ObjectMetadataValue::decode(value_buffer)
}

pub fn lookup_directory_entry<D: BlockDevice>(
    device: &mut D,
    root_block: u64,
    directory_object_id: u64,
    name: &str,
    maximum_generation: u64,
    node_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<DirectoryLookupResult, PhoenixFsError> {
    validate_directory_name(name)?;
    if directory_object_id == 0 {
        return Err(PhoenixFsError::InvalidObjectId);
    }

    let mut result = None;
    scan_object_tree_records(
        device,
        root_block,
        maximum_generation,
        node_buffer,
        |key, value| {
            if key.object_id != directory_object_id || key.kind != ObjectRecordKind::DirectoryEntry
            {
                return Ok(false);
            }

            let (entry, entry_name) = DirectoryEntryValue::decode(value)?;
            if entry_name != name {
                return Ok(false);
            }

            result = Some(DirectoryLookupResult {
                entry_id: key.offset,
                target_object_id: entry.target_object_id,
                target_type: entry.target_type,
            });
            Ok(true)
        },
    )?;

    result.ok_or(PhoenixFsError::DirectoryEntryNotFound)
}

fn ensure_object_has_no_payload_records<D: BlockDevice>(
    device: &mut D,
    root_block: u64,
    object_id: u64,
    maximum_generation: u64,
    node_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<(), PhoenixFsError> {
    if object_id == 0 {
        return Err(PhoenixFsError::InvalidObjectId);
    }

    let mut has_payload = false;
    scan_object_tree_records(
        device,
        root_block,
        maximum_generation,
        node_buffer,
        |key, _| {
            if key.object_id == object_id && key.kind != ObjectRecordKind::Metadata {
                has_payload = true;
                return Ok(true);
            }
            Ok(false)
        },
    )?;

    if has_payload {
        return Err(PhoenixFsError::ObjectNotEmpty);
    }
    Ok(())
}

pub fn read_file_range<D: BlockDevice>(
    device: &mut D,
    root_block: u64,
    object_id: u64,
    file_offset: u64,
    destination: &mut [u8],
    maximum_generation: u64,
    node_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    data_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<usize, PhoenixFsError> {
    if object_id == 0 {
        return Err(PhoenixFsError::InvalidObjectId);
    }
    if destination.is_empty() {
        return Ok(0);
    }

    let requested_end = file_offset
        .checked_add(destination.len() as u64)
        .ok_or(PhoenixFsError::InvalidReadRange)?;
    let total_blocks = filesystem_block_count(device)?;
    let mut written = 0_usize;
    let mut next_logical_offset = file_offset;

    while next_logical_offset < requested_end {
        let Some((extent_key, extent)) = find_next_extent(
            device,
            root_block,
            object_id,
            next_logical_offset,
            maximum_generation,
            total_blocks,
            node_buffer,
        )?
        else {
            break;
        };

        let extent_start = extent_key.offset;
        let extent_end = extent_start
            .checked_add(extent.data_bytes)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        if next_logical_offset < extent_start {
            break;
        }
        if next_logical_offset >= extent_end {
            next_logical_offset = extent_end;
            continue;
        }

        let copy_end = core::cmp::min(extent_end, requested_end);
        let bytes_to_copy = usize::try_from(copy_end - next_logical_offset)
            .map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
        read_extent_bytes(
            device,
            extent,
            next_logical_offset - extent_start,
            &mut destination[written..written + bytes_to_copy],
            data_buffer,
        )?;

        written += bytes_to_copy;
        next_logical_offset = copy_end;
    }

    Ok(written)
}

fn find_next_extent<D: BlockDevice>(
    device: &mut D,
    root_block: u64,
    object_id: u64,
    logical_offset: u64,
    maximum_generation: u64,
    total_blocks: u64,
    node_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<Option<(ObjectTreeKey, ExtentValue)>, PhoenixFsError> {
    let mut best: Option<(ObjectTreeKey, ExtentValue)> = None;

    scan_object_tree_records(
        device,
        root_block,
        maximum_generation,
        node_buffer,
        |key, value| {
            if key.object_id != object_id || key.kind != ObjectRecordKind::Extent {
                return Ok(false);
            }

            let extent = ExtentValue::decode(value)?;
            extent.validate_for_key(key, total_blocks)?;
            if key.offset <= logical_offset {
                let end = key
                    .offset
                    .checked_add(extent.data_bytes)
                    .ok_or(PhoenixFsError::ArithmeticOverflow)?;
                if logical_offset < end {
                    best = Some((key, extent));
                    return Ok(true);
                }
                return Ok(false);
            }

            if best.is_none() {
                best = Some((key, extent));
            }
            Ok(false)
        },
    )?;

    Ok(best)
}

fn read_extent_bytes<D: BlockDevice>(
    device: &mut D,
    extent: ExtentValue,
    extent_offset: u64,
    destination: &mut [u8],
    block_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<(), PhoenixFsError> {
    let end = extent_offset
        .checked_add(destination.len() as u64)
        .ok_or(PhoenixFsError::InvalidReadRange)?;
    if end > extent.data_bytes {
        return Err(PhoenixFsError::InvalidReadRange);
    }

    let mut copied = 0_usize;
    let mut current_offset = extent_offset;
    while copied < destination.len() {
        let block_delta = current_offset / FILESYSTEM_BLOCK_SIZE as u64;
        let block_number = extent
            .physical_start_block
            .checked_add(block_delta)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        read_filesystem_block(device, block_number, block_buffer)?;

        let in_block = (current_offset % FILESYSTEM_BLOCK_SIZE as u64) as usize;
        let available = FILESYSTEM_BLOCK_SIZE - in_block;
        let remaining = destination.len() - copied;
        let copy_bytes = core::cmp::min(available, remaining);
        destination[copied..copied + copy_bytes]
            .copy_from_slice(&block_buffer[in_block..in_block + copy_bytes]);

        copied += copy_bytes;
        current_offset = current_offset
            .checked_add(copy_bytes as u64)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    }

    Ok(())
}

fn scan_object_tree_records<D, F>(
    device: &mut D,
    root_block: u64,
    maximum_generation: u64,
    node_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    mut visitor: F,
) -> Result<(), PhoenixFsError>
where
    D: BlockDevice,
    F: FnMut(ObjectTreeKey, &[u8]) -> Result<bool, PhoenixFsError>,
{
    let mut stack = [EMPTY_OBJECT_SCAN_FRAME; MAX_OBJECT_TREE_DEPTH];
    let mut depth = 0_usize;
    let mut current_block = root_block;

    loop {
        read_filesystem_block(device, current_block, node_buffer)?;
        let node = TreeNodeHeader::decode(node_buffer)?;
        validate_scanned_node(node, current_block, maximum_generation)?;

        if node.level == 0 {
            validate_object_leaf(node_buffer)?;
            if visit_leaf_records(node_buffer, &mut visitor)? {
                return Ok(());
            }

            let Some(next_block) = advance_object_scan(
                device,
                &mut stack,
                &mut depth,
                maximum_generation,
                node_buffer,
            )?
            else {
                return Ok(());
            };
            current_block = next_block;
            continue;
        }

        validate_object_internal(node_buffer)?;
        if depth == MAX_OBJECT_TREE_DEPTH {
            return Err(PhoenixFsError::ObjectTreeDepthExceeded);
        }

        let first = object_internal_record_at(node_buffer, 0)?;
        stack[depth] = ObjectScanFrame {
            block_number: current_block,
            next_child_index: 1,
        };
        depth += 1;
        current_block = first.child_block;
    }
}

fn validate_scanned_node(
    node: TreeNodeHeader,
    expected_block: u64,
    maximum_generation: u64,
) -> Result<(), PhoenixFsError> {
    if node.metadata.kind != MetadataKind::ObjectTree
        || node.metadata.block_number != expected_block
    {
        return Err(PhoenixFsError::ObjectTreePathMismatch);
    }
    if node.metadata.generation > maximum_generation {
        return Err(PhoenixFsError::ObjectNodeGenerationAhead);
    }
    Ok(())
}

fn visit_leaf_records<F>(block: &[u8], visitor: &mut F) -> Result<bool, PhoenixFsError>
where
    F: FnMut(ObjectTreeKey, &[u8]) -> Result<bool, PhoenixFsError>,
{
    let node = validate_object_leaf(block)?;
    let entries_start = TreeNodeHeader::entries_offset();
    let entries_end = entries_start
        .checked_add(node.entries_bytes as usize)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let mut cursor = entries_start;

    for _ in 0..node.item_count {
        let (record, value, encoded_size) =
            ObjectLeafRecordHeader::decode_with_value(&block[cursor..entries_end])?;
        if visitor(record.key, value)? {
            return Ok(true);
        }
        cursor = cursor
            .checked_add(encoded_size)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    }

    Ok(false)
}

fn advance_object_scan<D: BlockDevice>(
    device: &mut D,
    stack: &mut [ObjectScanFrame; MAX_OBJECT_TREE_DEPTH],
    depth: &mut usize,
    maximum_generation: u64,
    node_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<Option<u64>, PhoenixFsError> {
    while *depth > 0 {
        let frame_index = *depth - 1;
        let frame = stack[frame_index];

        read_filesystem_block(device, frame.block_number, node_buffer)?;
        let parent = validate_object_internal(node_buffer)?;
        validate_scanned_node(parent, frame.block_number, maximum_generation)?;

        if frame.next_child_index < parent.item_count {
            let record = object_internal_record_at(node_buffer, frame.next_child_index as usize)?;
            stack[frame_index].next_child_index += 1;
            return Ok(Some(record.child_block));
        }

        *depth -= 1;
    }

    Ok(None)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectLeafRecordHeader {
    pub key: ObjectTreeKey,
    pub value_bytes: u32,
}

impl ObjectLeafRecordHeader {
    pub fn new(key: ObjectTreeKey, value_bytes: u32) -> Result<Self, PhoenixFsError> {
        key.validate()?;
        let header = Self { key, value_bytes };
        header.encoded_size()?;
        Ok(header)
    }

    pub fn encode_with_value(
        &self,
        destination: &mut [u8],
        value: &[u8],
    ) -> Result<usize, PhoenixFsError> {
        self.key.validate()?;
        if value.len() != self.value_bytes as usize {
            return Err(PhoenixFsError::InvalidObjectRecordSize);
        }

        let encoded_size = self.encoded_size()?;
        if destination.len() < encoded_size {
            return Err(PhoenixFsError::BufferSize);
        }

        destination[..encoded_size].fill(0);
        write_u64(destination, 0, self.key.object_id);
        write_u32(destination, 8, self.key.kind as u32);
        write_u64(destination, 16, self.key.offset);
        write_u32(destination, 24, self.value_bytes);
        destination[OBJECT_LEAF_RECORD_HEADER_SIZE..OBJECT_LEAF_RECORD_HEADER_SIZE + value.len()]
            .copy_from_slice(value);
        Ok(encoded_size)
    }

    pub fn decode_with_value(source: &[u8]) -> Result<(Self, &[u8], usize), PhoenixFsError> {
        if source.len() < OBJECT_LEAF_RECORD_HEADER_SIZE {
            return Err(PhoenixFsError::InvalidObjectRecordSize);
        }

        let flags = read_u32(source, 12);
        let reserved = read_u32(source, 28);
        if flags != 0 {
            return Err(PhoenixFsError::InvalidObjectRecordFlags(flags));
        }
        if reserved != 0 {
            return Err(PhoenixFsError::InvalidReservedField);
        }

        let header = Self {
            key: ObjectTreeKey {
                object_id: read_u64(source, 0),
                kind: ObjectRecordKind::from_raw(read_u32(source, 8))?,
                offset: read_u64(source, 16),
            },
            value_bytes: read_u32(source, 24),
        };
        header.key.validate()?;

        let encoded_size = header.encoded_size()?;
        if source.len() < encoded_size {
            return Err(PhoenixFsError::InvalidObjectRecordSize);
        }

        let value_end = OBJECT_LEAF_RECORD_HEADER_SIZE
            .checked_add(header.value_bytes as usize)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        let value = &source[OBJECT_LEAF_RECORD_HEADER_SIZE..value_end];

        if source[value_end..encoded_size]
            .iter()
            .any(|byte| *byte != 0)
        {
            return Err(PhoenixFsError::InvalidReservedField);
        }

        Ok((header, value, encoded_size))
    }

    pub fn encoded_size(&self) -> Result<usize, PhoenixFsError> {
        let raw_size = OBJECT_LEAF_RECORD_HEADER_SIZE
            .checked_add(self.value_bytes as usize)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        align_up(raw_size, OBJECT_LEAF_RECORD_ALIGNMENT)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectInternalRecord {
    pub key: ObjectTreeKey,
    pub child_block: u64,
}

impl ObjectInternalRecord {
    pub fn new(key: ObjectTreeKey, child_block: u64) -> Result<Self, PhoenixFsError> {
        key.validate()?;
        validate_object_child_block(child_block)?;
        Ok(Self { key, child_block })
    }

    pub fn encode(&self, destination: &mut [u8]) -> Result<(), PhoenixFsError> {
        self.key.validate()?;
        validate_object_child_block(self.child_block)?;
        if destination.len() < OBJECT_INTERNAL_RECORD_SIZE {
            return Err(PhoenixFsError::BufferSize);
        }

        destination[..OBJECT_INTERNAL_RECORD_SIZE].fill(0);
        write_u64(destination, 0, self.key.object_id);
        write_u32(destination, 8, self.key.kind as u32);
        write_u64(destination, 16, self.key.offset);
        write_u64(destination, 24, self.child_block);
        Ok(())
    }

    pub fn decode(source: &[u8]) -> Result<Self, PhoenixFsError> {
        if source.len() < OBJECT_INTERNAL_RECORD_SIZE {
            return Err(PhoenixFsError::InvalidObjectInternalRecordSize);
        }
        if read_u32(source, 12) != 0 {
            return Err(PhoenixFsError::InvalidReservedField);
        }

        let record = Self {
            key: ObjectTreeKey {
                object_id: read_u64(source, 0),
                kind: ObjectRecordKind::from_raw(read_u32(source, 8))?,
                offset: read_u64(source, 16),
            },
            child_block: read_u64(source, 24),
        };
        record.key.validate()?;
        validate_object_child_block(record.child_block)?;
        Ok(record)
    }
}

pub fn validate_object_internal(block: &[u8]) -> Result<TreeNodeHeader, PhoenixFsError> {
    let node = TreeNodeHeader::decode(block)?;
    validate_object_internal_header(node)?;

    let entries_start = TreeNodeHeader::entries_offset();
    let entries_size = usize::try_from(node.entries_bytes)
        .map_err(|_| PhoenixFsError::InvalidTreeNodePayloadSize(node.entries_bytes))?;
    let expected_size = (node.item_count as usize)
        .checked_mul(OBJECT_INTERNAL_RECORD_SIZE)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    if entries_size != expected_size {
        return Err(PhoenixFsError::InvalidObjectInternalRecordSize);
    }

    let entries_end = entries_start
        .checked_add(entries_size)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    if entries_end > block.len() {
        return Err(PhoenixFsError::InvalidObjectInternalRecordSize);
    }

    let mut previous_key: Option<ObjectTreeKey> = None;
    for index in 0..node.item_count as usize {
        let offset = entries_start + index * OBJECT_INTERNAL_RECORD_SIZE;
        let record =
            ObjectInternalRecord::decode(&block[offset..offset + OBJECT_INTERNAL_RECORD_SIZE])?;
        if record.child_block == node.metadata.block_number {
            return Err(PhoenixFsError::InvalidObjectChildBlock(record.child_block));
        }
        if previous_key.is_some_and(|previous| previous >= record.key) {
            return Err(PhoenixFsError::ObjectRecordsOutOfOrder);
        }
        previous_key = Some(record.key);
    }

    Ok(node)
}

pub fn validate_object_tree_transition(
    parent_block: &[u8],
    child_index: usize,
    child_block: &[u8],
) -> Result<ObjectInternalRecord, PhoenixFsError> {
    let parent = validate_object_internal(parent_block)?;
    if child_index >= parent.item_count as usize {
        return Err(PhoenixFsError::InvalidObjectInternalRecordSize);
    }

    let record_offset = TreeNodeHeader::entries_offset()
        .checked_add(
            child_index
                .checked_mul(OBJECT_INTERNAL_RECORD_SIZE)
                .ok_or(PhoenixFsError::ArithmeticOverflow)?,
        )
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let record = ObjectInternalRecord::decode(
        &parent_block[record_offset..record_offset + OBJECT_INTERNAL_RECORD_SIZE],
    )?;

    let child = TreeNodeHeader::decode(child_block)?;
    if child.metadata.kind != MetadataKind::ObjectTree {
        return Err(PhoenixFsError::InvalidObjectInternalNode);
    }
    if record.child_block != child.metadata.block_number {
        return Err(PhoenixFsError::InvalidObjectChildBlock(
            child.metadata.block_number,
        ));
    }
    if child.level.checked_add(1) != Some(parent.level) {
        return Err(PhoenixFsError::InvalidObjectTreeLevel);
    }
    if child.metadata.generation > parent.metadata.generation {
        return Err(PhoenixFsError::ObjectChildGenerationAhead);
    }

    let child_first_key = first_object_node_key(child_block, child)?;
    if child_first_key != record.key {
        return Err(PhoenixFsError::ObjectChildKeyMismatch);
    }

    Ok(record)
}

fn validate_object_internal_header(node: TreeNodeHeader) -> Result<(), PhoenixFsError> {
    if node.metadata.kind != MetadataKind::ObjectTree || node.level == 0 || node.item_count == 0 {
        return Err(PhoenixFsError::InvalidObjectInternalNode);
    }
    Ok(())
}

fn validate_object_child_block(block: u64) -> Result<(), PhoenixFsError> {
    if block < SUPERBLOCK_COPY_COUNT {
        return Err(PhoenixFsError::InvalidObjectChildBlock(block));
    }
    Ok(())
}

fn first_object_node_key(
    block: &[u8],
    node: TreeNodeHeader,
) -> Result<ObjectTreeKey, PhoenixFsError> {
    if node.item_count == 0 {
        return Err(PhoenixFsError::ObjectChildKeyMismatch);
    }

    let entries = &block[TreeNodeHeader::entries_offset()..];
    if node.level == 0 {
        let (record, _, _) = ObjectLeafRecordHeader::decode_with_value(entries)?;
        return Ok(record.key);
    }

    let record = ObjectInternalRecord::decode(entries)?;
    Ok(record.key)
}

pub fn validate_object_leaf(block: &[u8]) -> Result<TreeNodeHeader, PhoenixFsError> {
    let node = TreeNodeHeader::decode(block)?;
    if node.metadata.kind != MetadataKind::ObjectTree || node.level != 0 {
        return Err(PhoenixFsError::InvalidObjectLeafNode);
    }

    let entries_start = TreeNodeHeader::entries_offset();
    let entries_size = usize::try_from(node.entries_bytes)
        .map_err(|_| PhoenixFsError::InvalidTreeNodePayloadSize(node.entries_bytes))?;
    let entries_end = entries_start
        .checked_add(entries_size)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    if entries_end > block.len() {
        return Err(PhoenixFsError::InvalidTreeNodePayloadSize(
            node.entries_bytes,
        ));
    }

    let mut cursor = entries_start;
    let mut previous_key: Option<ObjectTreeKey> = None;

    for _ in 0..node.item_count {
        if cursor >= entries_end {
            return Err(PhoenixFsError::InvalidObjectRecordSize);
        }

        let (record, _, encoded_size) =
            ObjectLeafRecordHeader::decode_with_value(&block[cursor..entries_end])?;

        if previous_key.is_some_and(|previous| previous >= record.key) {
            return Err(PhoenixFsError::ObjectRecordsOutOfOrder);
        }

        previous_key = Some(record.key);
        cursor = cursor
            .checked_add(encoded_size)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    }

    if cursor != entries_end {
        return Err(PhoenixFsError::InvalidObjectRecordSize);
    }

    Ok(node)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FreeSpaceExtent {
    pub start_block: u64,
    pub block_count: u64,
}

impl FreeSpaceExtent {
    pub const fn new(start_block: u64, block_count: u64) -> Self {
        Self {
            start_block,
            block_count,
        }
    }

    pub fn end_block_exclusive(self) -> Result<u64, PhoenixFsError> {
        self.start_block
            .checked_add(self.block_count)
            .ok_or(PhoenixFsError::ArithmeticOverflow)
    }

    pub fn contains(self, block: u64) -> Result<bool, PhoenixFsError> {
        Ok(block >= self.start_block && block < self.end_block_exclusive()?)
    }

    fn validate(self, total_blocks: u64) -> Result<(), PhoenixFsError> {
        if self.block_count == 0 || self.start_block < SUPERBLOCK_COPY_COUNT {
            return Err(PhoenixFsError::InvalidFreeSpaceRange);
        }
        if self.end_block_exclusive()? > total_blocks {
            return Err(PhoenixFsError::InvalidFreeSpaceRange);
        }
        Ok(())
    }

    pub fn encode(self, destination: &mut [u8]) -> Result<(), PhoenixFsError> {
        if destination.len() < FREE_SPACE_RECORD_SIZE {
            return Err(PhoenixFsError::BufferSize);
        }
        if self.block_count == 0 {
            return Err(PhoenixFsError::InvalidFreeSpaceRange);
        }

        write_u64(destination, 0, self.start_block);
        write_u64(destination, 8, self.block_count);
        Ok(())
    }

    pub fn decode(source: &[u8]) -> Result<Self, PhoenixFsError> {
        if source.len() < FREE_SPACE_RECORD_SIZE {
            return Err(PhoenixFsError::InvalidFreeSpaceRecordSize);
        }

        let extent = Self {
            start_block: read_u64(source, 0),
            block_count: read_u64(source, 8),
        };
        if extent.block_count == 0 {
            return Err(PhoenixFsError::InvalidFreeSpaceRange);
        }
        Ok(extent)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CowAllocationPlan {
    pub record_index: u32,
    pub source_extent: FreeSpaceExtent,
    pub allocated: FreeSpaceExtent,
    pub remaining: Option<FreeSpaceExtent>,
}

pub fn validate_free_space_leaf(
    block: &[u8],
    total_blocks: u64,
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let node = TreeNodeHeader::decode(block)?;
    if node.metadata.kind != MetadataKind::FreeSpaceTree || node.level != 0 {
        return Err(PhoenixFsError::InvalidFreeSpaceLeafNode);
    }

    let expected_bytes = (node.item_count as usize)
        .checked_mul(FREE_SPACE_RECORD_SIZE)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    if node.entries_bytes as usize != expected_bytes {
        return Err(PhoenixFsError::InvalidFreeSpaceRecordSize);
    }

    let mut previous_end = None;
    for index in 0..node.item_count as usize {
        let extent = free_space_extent_at(block, index)?;
        extent.validate(total_blocks)?;
        if previous_end.is_some_and(|end| extent.start_block <= end) {
            return Err(PhoenixFsError::FreeSpaceRangesNotCanonical);
        }
        previous_end = Some(extent.end_block_exclusive()?);
    }

    Ok(node)
}

pub fn plan_cow_allocation(
    free_space_leaf: &[u8],
    total_blocks: u64,
    requested_blocks: u64,
) -> Result<CowAllocationPlan, PhoenixFsError> {
    if requested_blocks == 0 {
        return Err(PhoenixFsError::InvalidFreeSpaceRange);
    }

    let node = validate_free_space_leaf(free_space_leaf, total_blocks)?;
    for index in 0..node.item_count as usize {
        let extent = free_space_extent_at(free_space_leaf, index)?;
        if extent.block_count < requested_blocks {
            continue;
        }

        let allocated = FreeSpaceExtent::new(extent.start_block, requested_blocks);
        let remaining_count = extent.block_count - requested_blocks;
        let remaining = if remaining_count == 0 {
            None
        } else {
            Some(FreeSpaceExtent::new(
                extent.start_block + requested_blocks,
                remaining_count,
            ))
        };

        return Ok(CowAllocationPlan {
            record_index: index as u32,
            source_extent: extent,
            allocated,
            remaining,
        });
    }

    Err(PhoenixFsError::NoFreeSpace)
}

pub fn materialize_free_space_leaf_after_allocation(
    current: &[u8],
    destination: &mut [u8],
    total_blocks: u64,
    new_generation: u64,
    new_block_number: u64,
    plan: CowAllocationPlan,
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let current_node = validate_free_space_leaf(current, total_blocks)?;
    if destination.len() != FILESYSTEM_BLOCK_SIZE {
        return Err(PhoenixFsError::BufferSize);
    }
    if new_generation <= current_node.metadata.generation {
        return Err(PhoenixFsError::GenerationSequence);
    }
    if !plan.allocated.contains(new_block_number)? {
        return Err(PhoenixFsError::AllocationTargetOutsideRange);
    }

    let selected = free_space_extent_at(current, plan.record_index as usize)?;
    if selected != plan.source_extent {
        return Err(PhoenixFsError::StaleAllocationPlan);
    }

    destination.fill(0);
    let entries_start = TreeNodeHeader::entries_offset();
    let mut write_index = 0_usize;

    for index in 0..current_node.item_count as usize {
        if index != plan.record_index as usize {
            let extent = free_space_extent_at(current, index)?;
            write_free_space_extent(destination, write_index, extent)?;
            write_index += 1;
            continue;
        }

        if let Some(remaining) = plan.remaining {
            remaining.validate(total_blocks)?;
            write_free_space_extent(destination, write_index, remaining)?;
            write_index += 1;
        }
    }

    let entries_bytes = write_index
        .checked_mul(FREE_SPACE_RECORD_SIZE)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let node = TreeNodeHeader::new(
        MetadataKind::FreeSpaceTree,
        new_generation,
        new_block_number,
        0,
        write_index as u32,
        entries_bytes as u32,
    )?;
    node.seal(destination)?;
    validate_free_space_leaf(destination, total_blocks)?;

    let data_end = entries_start
        .checked_add(entries_bytes)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    if destination[data_end..].iter().any(|byte| *byte != 0) {
        return Err(PhoenixFsError::InvalidReservedField);
    }

    Ok(node)
}

fn free_space_extent_at(block: &[u8], index: usize) -> Result<FreeSpaceExtent, PhoenixFsError> {
    let offset = TreeNodeHeader::entries_offset()
        .checked_add(
            index
                .checked_mul(FREE_SPACE_RECORD_SIZE)
                .ok_or(PhoenixFsError::ArithmeticOverflow)?,
        )
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let end = offset
        .checked_add(FREE_SPACE_RECORD_SIZE)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    if end > block.len() {
        return Err(PhoenixFsError::InvalidFreeSpaceRecordSize);
    }
    FreeSpaceExtent::decode(&block[offset..end])
}

fn write_free_space_extent(
    block: &mut [u8],
    index: usize,
    extent: FreeSpaceExtent,
) -> Result<(), PhoenixFsError> {
    let offset = TreeNodeHeader::entries_offset()
        .checked_add(
            index
                .checked_mul(FREE_SPACE_RECORD_SIZE)
                .ok_or(PhoenixFsError::ArithmeticOverflow)?,
        )
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let end = offset
        .checked_add(FREE_SPACE_RECORD_SIZE)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    if end > block.len() {
        return Err(PhoenixFsError::BufferSize);
    }
    extent.encode(&mut block[offset..end])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectPathStep {
    pub block_number: u64,
    pub child_index: u32,
    pub child_key: ObjectTreeKey,
}

const EMPTY_OBJECT_PATH_STEP: ObjectPathStep = ObjectPathStep {
    block_number: 0,
    child_index: 0,
    child_key: ObjectTreeKey::new(1, ObjectRecordKind::Metadata, 0),
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectTreePath {
    pub leaf_block: u64,
    pub parent_count: usize,
    pub parents: [ObjectPathStep; MAX_OBJECT_TREE_DEPTH],
}

impl ObjectTreePath {
    pub const fn new() -> Self {
        Self {
            leaf_block: 0,
            parent_count: 0,
            parents: [EMPTY_OBJECT_PATH_STEP; MAX_OBJECT_TREE_DEPTH],
        }
    }

    pub const fn node_count(&self) -> usize {
        self.parent_count + 1
    }
}

impl Default for ObjectTreePath {
    fn default() -> Self {
        Self::new()
    }
}

pub fn find_object_tree_path<D: BlockDevice>(
    device: &mut D,
    root_block: u64,
    target_key: ObjectTreeKey,
    maximum_generation: u64,
    buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<ObjectTreePath, PhoenixFsError> {
    find_object_tree_path_with_mode(
        device,
        root_block,
        target_key,
        maximum_generation,
        buffer,
        false,
    )
}

fn find_object_tree_path_for_insertion<D: BlockDevice>(
    device: &mut D,
    root_block: u64,
    target_key: ObjectTreeKey,
    maximum_generation: u64,
    buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<ObjectTreePath, PhoenixFsError> {
    find_object_tree_path_with_mode(
        device,
        root_block,
        target_key,
        maximum_generation,
        buffer,
        true,
    )
}

fn find_object_tree_path_with_mode<D: BlockDevice>(
    device: &mut D,
    root_block: u64,
    target_key: ObjectTreeKey,
    maximum_generation: u64,
    buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    insertion: bool,
) -> Result<ObjectTreePath, PhoenixFsError> {
    target_key.validate()?;
    let mut path = ObjectTreePath::new();
    let mut current_block = root_block;
    let mut expected_key = None;
    let mut expected_parent_level = None;
    let mut expected_parent_generation = None;

    loop {
        read_filesystem_block(device, current_block, buffer)?;
        let node = TreeNodeHeader::decode(buffer)?;
        if node.metadata.kind != MetadataKind::ObjectTree {
            return Err(PhoenixFsError::ObjectTreePathMismatch);
        }
        if node.metadata.block_number != current_block {
            return Err(PhoenixFsError::ObjectTreePathMismatch);
        }
        if node.metadata.generation > maximum_generation {
            return Err(PhoenixFsError::ObjectNodeGenerationAhead);
        }

        if let Some(parent_level) = expected_parent_level {
            if node.level.checked_add(1) != Some(parent_level) {
                return Err(PhoenixFsError::ObjectTreePathMismatch);
            }
        }
        if let Some(parent_generation) = expected_parent_generation {
            if node.metadata.generation > parent_generation {
                return Err(PhoenixFsError::ObjectNodeGenerationAhead);
            }
        }
        if let Some(key) = expected_key {
            if first_object_node_key(buffer, node)? != key {
                return Err(PhoenixFsError::ObjectTreePathMismatch);
            }
        }

        if node.level == 0 {
            validate_object_leaf(buffer)?;
            if !insertion {
                find_object_leaf_record(buffer, target_key)?;
            }
            path.leaf_block = current_block;
            return Ok(path);
        }

        validate_object_internal(buffer)?;
        let (child_index, child_record) = select_object_child(buffer, node, target_key, insertion)?;
        if path.parent_count == MAX_OBJECT_TREE_DEPTH {
            return Err(PhoenixFsError::ObjectTreeDepthExceeded);
        }

        path.parents[path.parent_count] = ObjectPathStep {
            block_number: current_block,
            child_index: child_index as u32,
            child_key: child_record.key,
        };
        path.parent_count += 1;
        expected_key = Some(child_record.key);
        expected_parent_level = Some(node.level);
        expected_parent_generation = Some(node.metadata.generation);
        current_block = child_record.child_block;
    }
}

fn select_object_child(
    block: &[u8],
    node: TreeNodeHeader,
    target_key: ObjectTreeKey,
    insertion: bool,
) -> Result<(usize, ObjectInternalRecord), PhoenixFsError> {
    let mut selected = None;

    for index in 0..node.item_count as usize {
        let offset = TreeNodeHeader::entries_offset()
            .checked_add(
                index
                    .checked_mul(OBJECT_INTERNAL_RECORD_SIZE)
                    .ok_or(PhoenixFsError::ArithmeticOverflow)?,
            )
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        let record =
            ObjectInternalRecord::decode(&block[offset..offset + OBJECT_INTERNAL_RECORD_SIZE])?;
        if record.key > target_key {
            break;
        }
        selected = Some((index, record));
    }

    if let Some(selected) = selected {
        return Ok(selected);
    }
    if insertion {
        return Ok((0, object_internal_record_at(block, 0)?));
    }
    Err(PhoenixFsError::ObjectRecordNotFound)
}

fn find_object_leaf_record(block: &[u8], target_key: ObjectTreeKey) -> Result<(), PhoenixFsError> {
    let node = validate_object_leaf(block)?;
    let entries_start = TreeNodeHeader::entries_offset();
    let entries_end = entries_start
        .checked_add(node.entries_bytes as usize)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let mut cursor = entries_start;

    for _ in 0..node.item_count {
        let (record, _, encoded_size) =
            ObjectLeafRecordHeader::decode_with_value(&block[cursor..entries_end])?;
        if record.key == target_key {
            return Ok(());
        }
        if record.key > target_key {
            break;
        }
        cursor = cursor
            .checked_add(encoded_size)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    }

    Err(PhoenixFsError::ObjectRecordNotFound)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeepRecordUpdateResult {
    pub active: ActiveSuperblock,
    pub retired_blocks: [u64; MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS],
    pub retired_count: usize,
    pub allocated: FreeSpaceExtent,
}

impl DeepRecordUpdateResult {
    pub fn retired_blocks(&self) -> &[u64] {
        &self.retired_blocks[..self.retired_count]
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ObjectLeafInsert<'a> {
    pub key: ObjectTreeKey,
    pub value: &'a [u8],
}

impl<'a> ObjectLeafInsert<'a> {
    pub const fn new(key: ObjectTreeKey, value: &'a [u8]) -> Self {
        Self { key, value }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectLeafDelete {
    pub key: ObjectTreeKey,
}

impl ObjectLeafDelete {
    pub const fn new(key: ObjectTreeKey) -> Self {
        Self { key }
    }
}

#[derive(Debug, Clone, Copy)]
enum ObjectLeafMutation<'a> {
    Replace {
        key: ObjectTreeKey,
        value: &'a [u8],
    },
    Insert {
        key: ObjectTreeKey,
        value: &'a [u8],
    },
    InsertBatch {
        insertions: &'a [ObjectLeafInsert<'a>],
    },
    Delete {
        key: ObjectTreeKey,
    },
    DeleteBatch {
        deletions: &'a [ObjectLeafDelete],
    },
}

impl ObjectLeafMutation<'_> {
    fn key(self) -> Result<ObjectTreeKey, PhoenixFsError> {
        match self {
            Self::Replace { key, .. } | Self::Insert { key, .. } | Self::Delete { key } => Ok(key),
            Self::InsertBatch { insertions } => insertions
                .first()
                .map(|insertion| insertion.key)
                .ok_or(PhoenixFsError::InvalidObjectMutationBatch),
            Self::DeleteBatch { deletions } => deletions
                .first()
                .map(|deletion| deletion.key)
                .ok_or(PhoenixFsError::InvalidObjectMutationBatch),
        }
    }

    const fn is_insertion(self) -> bool {
        matches!(self, Self::Insert { .. } | Self::InsertBatch { .. })
    }
}

pub fn commit_object_record_update<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    target_key: ObjectTreeKey,
    new_value: &[u8],
    object_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    object_copy_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    current_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    next_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    superblock_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<DeepRecordUpdateResult, PhoenixFsError> {
    commit_object_leaf_mutation(
        device,
        current,
        ObjectLeafMutation::Replace {
            key: target_key,
            value: new_value,
        },
        object_buffer,
        object_copy_buffer,
        current_free_space,
        next_free_space,
        superblock_buffer,
    )
}

pub fn commit_object_record_insert<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    key: ObjectTreeKey,
    value: &[u8],
    object_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    object_copy_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    current_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    next_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    superblock_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<DeepRecordUpdateResult, PhoenixFsError> {
    commit_object_leaf_mutation(
        device,
        current,
        ObjectLeafMutation::Insert { key, value },
        object_buffer,
        object_copy_buffer,
        current_free_space,
        next_free_space,
        superblock_buffer,
    )
}

pub fn commit_object_record_insertions<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    insertions: &[ObjectLeafInsert<'_>],
    object_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    object_copy_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    current_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    next_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    superblock_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<DeepRecordUpdateResult, PhoenixFsError> {
    validate_object_insertions(insertions)?;
    let paths = classify_insertion_paths(
        device,
        current.superblock.roots.object_tree,
        insertions,
        current.superblock.generation,
        object_buffer,
    )?;

    let Some(second_path) = paths.second else {
        return commit_object_leaf_mutation(
            device,
            current,
            ObjectLeafMutation::InsertBatch { insertions },
            object_buffer,
            object_copy_buffer,
            current_free_space,
            next_free_space,
            superblock_buffer,
        );
    };

    commit_two_leaf_insertions(
        device,
        current,
        paths.first,
        second_path,
        &insertions[..paths.split_index],
        &insertions[paths.split_index..],
        object_buffer,
        object_copy_buffer,
        current_free_space,
        next_free_space,
        superblock_buffer,
    )
}

pub fn commit_create_object<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    parent_object_id: u64,
    entry_id: u64,
    new_object_id: u64,
    name: &str,
    object_type: ObjectType,
    timestamp_ns: u64,
    object_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    object_copy_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    current_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    next_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    superblock_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<DeepRecordUpdateResult, PhoenixFsError> {
    validate_directory_name(name)?;
    if parent_object_id == 0 || new_object_id == 0 {
        return Err(PhoenixFsError::InvalidObjectId);
    }

    let mut metadata_buffer = [0_u8; OBJECT_METADATA_VALUE_SIZE];
    let parent_metadata = read_object_metadata(
        device,
        current.superblock.roots.object_tree,
        parent_object_id,
        current.superblock.generation,
        object_buffer,
        &mut metadata_buffer,
    )?;
    if parent_metadata.object_type != ObjectType::Directory {
        return Err(PhoenixFsError::ParentNotDirectory);
    }

    match read_object_metadata(
        device,
        current.superblock.roots.object_tree,
        new_object_id,
        current.superblock.generation,
        object_buffer,
        &mut metadata_buffer,
    ) {
        Ok(_) => return Err(PhoenixFsError::ObjectRecordAlreadyExists),
        Err(PhoenixFsError::ObjectRecordNotFound) => {}
        Err(error) => return Err(error),
    }

    match lookup_directory_entry(
        device,
        current.superblock.roots.object_tree,
        parent_object_id,
        name,
        current.superblock.generation,
        object_buffer,
    ) {
        Ok(_) => return Err(PhoenixFsError::ObjectRecordAlreadyExists),
        Err(PhoenixFsError::DirectoryEntryNotFound) => {}
        Err(error) => return Err(error),
    }

    let metadata =
        ObjectMetadataValue::new(object_type, 0, timestamp_ns, timestamp_ns, timestamp_ns);
    metadata.encode(&mut metadata_buffer)?;

    let directory_entry = DirectoryEntryValue::new(new_object_id, object_type)?;
    let mut directory_buffer = [0_u8; DIRECTORY_ENTRY_VALUE_HEADER_SIZE + MAX_DIRECTORY_NAME_BYTES];
    let directory_size = directory_entry.encode(name, &mut directory_buffer)?;

    let metadata_key = ObjectTreeKey::new(new_object_id, ObjectRecordKind::Metadata, 0);
    let directory_key =
        ObjectTreeKey::new(parent_object_id, ObjectRecordKind::DirectoryEntry, entry_id);

    let metadata_insert = ObjectLeafInsert::new(metadata_key, &metadata_buffer);
    let directory_insert =
        ObjectLeafInsert::new(directory_key, &directory_buffer[..directory_size]);
    let insertions = if metadata_key < directory_key {
        [metadata_insert, directory_insert]
    } else {
        [directory_insert, metadata_insert]
    };

    commit_object_record_insertions(
        device,
        current,
        &insertions,
        object_buffer,
        object_copy_buffer,
        current_free_space,
        next_free_space,
        superblock_buffer,
    )
}

pub fn commit_remove_object<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    parent_object_id: u64,
    name: &str,
    object_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    object_copy_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    current_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    next_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    superblock_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<DeepRecordUpdateResult, PhoenixFsError> {
    validate_directory_name(name)?;
    if parent_object_id == 0 {
        return Err(PhoenixFsError::InvalidObjectId);
    }

    let mut metadata_buffer = [0_u8; OBJECT_METADATA_VALUE_SIZE];
    let parent_metadata = read_object_metadata(
        device,
        current.superblock.roots.object_tree,
        parent_object_id,
        current.superblock.generation,
        object_buffer,
        &mut metadata_buffer,
    )?;
    if parent_metadata.object_type != ObjectType::Directory {
        return Err(PhoenixFsError::ParentNotDirectory);
    }

    let entry = lookup_directory_entry(
        device,
        current.superblock.roots.object_tree,
        parent_object_id,
        name,
        current.superblock.generation,
        object_buffer,
    )?;
    if entry.target_object_id == ROOT_OBJECT_ID {
        return Err(PhoenixFsError::CannotRemoveRootObject);
    }

    let target_metadata = read_object_metadata(
        device,
        current.superblock.roots.object_tree,
        entry.target_object_id,
        current.superblock.generation,
        object_buffer,
        &mut metadata_buffer,
    )?;
    if target_metadata.object_type != entry.target_type {
        return Err(PhoenixFsError::DirectoryEntryTargetMismatch);
    }

    ensure_object_has_no_payload_records(
        device,
        current.superblock.roots.object_tree,
        entry.target_object_id,
        current.superblock.generation,
        object_buffer,
    )?;

    let metadata_key = ObjectTreeKey::new(entry.target_object_id, ObjectRecordKind::Metadata, 0);
    let directory_key = ObjectTreeKey::new(
        parent_object_id,
        ObjectRecordKind::DirectoryEntry,
        entry.entry_id,
    );
    let metadata_delete = ObjectLeafDelete::new(metadata_key);
    let directory_delete = ObjectLeafDelete::new(directory_key);
    let deletions = if metadata_key < directory_key {
        [metadata_delete, directory_delete]
    } else {
        [directory_delete, metadata_delete]
    };

    commit_object_record_deletions(
        device,
        current,
        &deletions,
        object_buffer,
        object_copy_buffer,
        current_free_space,
        next_free_space,
        superblock_buffer,
    )
}

pub fn commit_object_record_deletions<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    deletions: &[ObjectLeafDelete],
    object_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    object_copy_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    current_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    next_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    superblock_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<DeepRecordUpdateResult, PhoenixFsError> {
    validate_object_deletions(deletions)?;
    let paths = classify_deletion_paths(
        device,
        current.superblock.roots.object_tree,
        deletions,
        current.superblock.generation,
        object_buffer,
    )?;

    let Some(second_path) = paths.second else {
        return commit_object_leaf_mutation(
            device,
            current,
            ObjectLeafMutation::DeleteBatch { deletions },
            object_buffer,
            object_copy_buffer,
            current_free_space,
            next_free_space,
            superblock_buffer,
        );
    };

    commit_two_leaf_deletions(
        device,
        current,
        paths.first,
        second_path,
        &deletions[..paths.split_index],
        &deletions[paths.split_index..],
        object_buffer,
        object_copy_buffer,
        current_free_space,
        next_free_space,
        superblock_buffer,
    )
}

pub fn commit_object_record_delete<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    key: ObjectTreeKey,
    object_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    object_copy_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    current_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    next_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    superblock_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<DeepRecordUpdateResult, PhoenixFsError> {
    commit_object_leaf_mutation(
        device,
        current,
        ObjectLeafMutation::Delete { key },
        object_buffer,
        object_copy_buffer,
        current_free_space,
        next_free_space,
        superblock_buffer,
    )
}

#[allow(clippy::too_many_arguments)]
fn find_empty_branch_prune_anchor<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    path: ObjectTreePath,
    buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<(usize, TreeNodeHeader), PhoenixFsError> {
    for parent_position in (0..path.parent_count).rev() {
        let step = path.parents[parent_position];
        read_filesystem_block(device, step.block_number, buffer)?;
        let parent = validate_object_internal(buffer)?;
        if parent.metadata.generation > current.superblock.generation {
            return Err(PhoenixFsError::ObjectNodeGenerationAhead);
        }
        validate_path_pointer(buffer, step)?;

        if parent.item_count > 1 {
            return Ok((parent_position, parent));
        }
    }

    Err(PhoenixFsError::ObjectLeafWouldBecomeEmpty)
}

fn materialize_merged_internal_after_child_delete(
    anchor: &[u8],
    sibling: &[u8],
    destination: &mut [u8],
    new_generation: u64,
    new_block_number: u64,
    removed_child_index: usize,
    anchor_is_left: bool,
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let anchor_node = validate_object_internal(anchor)?;
    let sibling_node = validate_object_internal(sibling)?;
    if anchor_node.level != sibling_node.level
        || removed_child_index >= anchor_node.item_count as usize
        || new_generation <= anchor_node.metadata.generation
        || new_generation <= sibling_node.metadata.generation
        || new_block_number < SUPERBLOCK_COPY_COUNT
    {
        return Err(PhoenixFsError::InvalidObjectRewriteTarget);
    }

    let anchor_items = anchor_node
        .item_count
        .checked_sub(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let item_count = anchor_items
        .checked_add(sibling_node.item_count)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    if item_count == 0 || item_count as usize > object_internal_capacity() {
        return Err(PhoenixFsError::BufferSize);
    }

    destination.fill(0);
    let start = TreeNodeHeader::entries_offset();
    let mut output_index = 0_usize;

    let mut copy_node = |source: &[u8], skip: Option<usize>| -> Result<(), PhoenixFsError> {
        let node = validate_object_internal(source)?;
        for input_index in 0..node.item_count as usize {
            if skip == Some(input_index) {
                continue;
            }
            let record = object_internal_record_at(source, input_index)?;
            record.encode(
                &mut destination[start + output_index * OBJECT_INTERNAL_RECORD_SIZE
                    ..start + (output_index + 1) * OBJECT_INTERNAL_RECORD_SIZE],
            )?;
            output_index += 1;
        }
        Ok(())
    };

    if anchor_is_left {
        copy_node(anchor, Some(removed_child_index))?;
        copy_node(sibling, None)?;
    } else {
        copy_node(sibling, None)?;
        copy_node(anchor, Some(removed_child_index))?;
    }

    let entries_bytes = output_index
        .checked_mul(OBJECT_INTERNAL_RECORD_SIZE)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let next = TreeNodeHeader::new(
        MetadataKind::ObjectTree,
        new_generation,
        new_block_number,
        anchor_node.level,
        item_count,
        entries_bytes as u32,
    )?;
    next.seal(destination)?;
    validate_object_internal(destination)?;
    Ok(next)
}

#[allow(clippy::too_many_arguments)]
fn try_commit_internal_merge_after_prune<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    path: ObjectTreePath,
    anchor_position: usize,
    object_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    object_copy_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    current_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    next_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    superblock_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<Option<DeepRecordUpdateResult>, PhoenixFsError> {
    if anchor_position == 0 {
        return Ok(None);
    }

    let anchor_step = path.parents[anchor_position];
    let grandparent_position = anchor_position - 1;
    let grandparent_step = path.parents[grandparent_position];

    read_filesystem_block(device, grandparent_step.block_number, object_buffer)?;
    let grandparent = validate_object_internal(object_buffer)?;
    if grandparent.metadata.generation > current.superblock.generation {
        return Err(PhoenixFsError::ObjectNodeGenerationAhead);
    }
    validate_path_pointer(object_buffer, grandparent_step)?;
    if grandparent.item_count < 2 {
        return Ok(None);
    }
    let collapse_two_child_root = grandparent_position == 0 && grandparent.item_count == 2;

    let mut grandparent_source = [0_u8; FILESYSTEM_BLOCK_SIZE];
    grandparent_source.copy_from_slice(object_buffer);
    let child_index = grandparent_step.child_index as usize;

    let mut anchor_source = [0_u8; FILESYSTEM_BLOCK_SIZE];
    read_filesystem_block(device, anchor_step.block_number, &mut anchor_source)?;
    let anchor_node = validate_object_internal(&anchor_source)?;
    if anchor_node.metadata.generation > current.superblock.generation {
        return Err(PhoenixFsError::ObjectNodeGenerationAhead);
    }
    validate_path_pointer(&anchor_source, anchor_step)?;

    let modified_count = anchor_node
        .item_count
        .checked_sub(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    if modified_count == 0 {
        return Ok(None);
    }

    let candidates = [
        (child_index + 1 < grandparent.item_count as usize).then_some(child_index + 1),
        child_index.checked_sub(1),
    ];
    let mut sibling_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
    let mut selected = None;

    for sibling_index in candidates.into_iter().flatten() {
        let sibling = object_internal_record_at(&grandparent_source, sibling_index)?;
        read_filesystem_block(device, sibling.child_block, &mut sibling_buffer)?;
        let sibling_node = validate_object_internal(&sibling_buffer)?;
        if sibling_node.metadata.generation > current.superblock.generation {
            return Err(PhoenixFsError::ObjectNodeGenerationAhead);
        }
        if sibling_node.level != anchor_node.level
            || sibling_node.metadata.block_number != sibling.child_block
            || first_object_node_key(&sibling_buffer, sibling_node)? != sibling.key
        {
            return Err(PhoenixFsError::ObjectTreePathMismatch);
        }

        let merged_count = modified_count
            .checked_add(sibling_node.item_count)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        if merged_count as usize <= object_internal_capacity() {
            selected = Some((sibling_index, sibling));
            break;
        }
    }

    let Some((sibling_index, sibling)) = selected else {
        return Ok(None);
    };
    let left_index = core::cmp::min(child_index, sibling_index);
    let anchor_is_left = child_index < sibling_index;
    let requested_blocks = if collapse_two_child_root {
        2
    } else {
        u64::try_from(grandparent_position + 3).map_err(|_| PhoenixFsError::ArithmeticOverflow)?
    };
    let allocation = plan_cow_allocation(
        current_free_space,
        current.superblock.total_blocks,
        requested_blocks,
    )?;
    let generation = current
        .superblock
        .generation
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    let merged_block = allocation.allocated.start_block;
    let merged = materialize_merged_internal_after_child_delete(
        &anchor_source,
        &sibling_buffer,
        object_copy_buffer,
        generation,
        merged_block,
        anchor_step.child_index as usize,
        anchor_is_left,
    )?;
    let merged_key = first_object_node_key(object_copy_buffer, merged)?;
    write_filesystem_block(device, merged_block, object_copy_buffer)?;

    if collapse_two_child_root {
        let free_space_block = merged_block
            .checked_add(1)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        let allocation_end = allocation.allocated.end_block_exclusive()?;
        if free_space_block
            .checked_add(1)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?
            != allocation_end
        {
            return Err(PhoenixFsError::InvalidTransactionRoots);
        }

        materialize_free_space_leaf_after_allocation(
            current_free_space,
            next_free_space,
            current.superblock.total_blocks,
            generation,
            free_space_block,
            allocation,
        )?;
        write_filesystem_block(device, free_space_block, next_free_space)?;

        let mut retired_blocks = [0_u64; MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS];
        let mut retired_count = 0_usize;
        push_unique_retired(&mut retired_blocks, &mut retired_count, path.leaf_block)?;
        for index in 0..path.parent_count {
            push_unique_retired(
                &mut retired_blocks,
                &mut retired_count,
                path.parents[index].block_number,
            )?;
        }
        push_unique_retired(&mut retired_blocks, &mut retired_count, sibling.child_block)?;
        push_unique_retired(
            &mut retired_blocks,
            &mut retired_count,
            current.superblock.roots.free_space_tree,
        )?;
        sort_u64_prefix(&mut retired_blocks, retired_count);

        let roots = TransactionRoots::new(merged_block, free_space_block);
        let plan = TransactionCommitPlan::new(current, roots, &retired_blocks[..retired_count])?;
        let committed = commit_transaction(device, current, plan, superblock_buffer)?;

        return Ok(Some(DeepRecordUpdateResult {
            active: committed.active,
            retired_blocks,
            retired_count,
            allocated: allocation.allocated,
        }));
    }

    let grandparent_block = merged_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let next_grandparent = materialize_object_internal_after_children_merge(
        &grandparent_source,
        object_copy_buffer,
        generation,
        grandparent_block,
        left_index,
        merged_block,
        merged_key,
    )?;
    write_filesystem_block(device, grandparent_block, object_copy_buffer)?;

    let grandparent_key = first_object_node_key(object_copy_buffer, next_grandparent)?;
    let mut next_block = grandparent_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let root = copy_object_parent_range(
        device,
        current,
        path,
        0,
        grandparent_position,
        grandparent_block,
        grandparent_key,
        generation,
        &mut next_block,
        object_buffer,
        object_copy_buffer,
    )?;

    let free_space_block = next_block;
    let allocation_end = allocation.allocated.end_block_exclusive()?;
    if free_space_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?
        != allocation_end
    {
        return Err(PhoenixFsError::InvalidTransactionRoots);
    }

    materialize_free_space_leaf_after_allocation(
        current_free_space,
        next_free_space,
        current.superblock.total_blocks,
        generation,
        free_space_block,
        allocation,
    )?;
    write_filesystem_block(device, free_space_block, next_free_space)?;

    let mut retired_blocks = [0_u64; MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS];
    let mut retired_count = 0_usize;
    push_unique_retired(&mut retired_blocks, &mut retired_count, path.leaf_block)?;
    for index in 0..path.parent_count {
        push_unique_retired(
            &mut retired_blocks,
            &mut retired_count,
            path.parents[index].block_number,
        )?;
    }
    push_unique_retired(&mut retired_blocks, &mut retired_count, sibling.child_block)?;
    push_unique_retired(
        &mut retired_blocks,
        &mut retired_count,
        current.superblock.roots.free_space_tree,
    )?;
    sort_u64_prefix(&mut retired_blocks, retired_count);

    let roots = TransactionRoots::new(root.0, free_space_block);
    let plan = TransactionCommitPlan::new(current, roots, &retired_blocks[..retired_count])?;
    let committed = commit_transaction(device, current, plan, superblock_buffer)?;

    Ok(Some(DeepRecordUpdateResult {
        active: committed.active,
        retired_blocks,
        retired_count,
        allocated: allocation.allocated,
    }))
}

fn combined_internal_record_after_child_delete(
    anchor: &[u8],
    sibling: &[u8],
    removed_child_index: usize,
    anchor_is_left: bool,
    merged_index: usize,
    anchor_item_count: usize,
    sibling_item_count: usize,
) -> Result<ObjectInternalRecord, PhoenixFsError> {
    if removed_child_index >= anchor_item_count || anchor_item_count == 0 {
        return Err(PhoenixFsError::InvalidObjectRewriteTarget);
    }

    let anchor_items = anchor_item_count - 1;
    let total_items = anchor_items
        .checked_add(sibling_item_count)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    if merged_index >= total_items {
        return Err(PhoenixFsError::InvalidObjectInternalRecordSize);
    }

    if anchor_is_left && merged_index < anchor_items {
        let source_index = if merged_index < removed_child_index {
            merged_index
        } else {
            merged_index + 1
        };
        return object_internal_record_at(anchor, source_index);
    }
    if anchor_is_left {
        return object_internal_record_at(sibling, merged_index - anchor_items);
    }
    if merged_index < sibling_item_count {
        return object_internal_record_at(sibling, merged_index);
    }

    let anchor_index = merged_index - sibling_item_count;
    let source_index = if anchor_index < removed_child_index {
        anchor_index
    } else {
        anchor_index + 1
    };
    object_internal_record_at(anchor, source_index)
}

fn materialize_internal_range_after_child_delete(
    anchor: &[u8],
    sibling: &[u8],
    destination: &mut [u8],
    new_generation: u64,
    new_block_number: u64,
    removed_child_index: usize,
    anchor_is_left: bool,
    range_start: usize,
    range_end: usize,
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let anchor_node = validate_object_internal(anchor)?;
    let sibling_node = validate_object_internal(sibling)?;
    if anchor_node.level != sibling_node.level
        || removed_child_index >= anchor_node.item_count as usize
        || new_generation <= anchor_node.metadata.generation
        || new_generation <= sibling_node.metadata.generation
        || new_block_number < SUPERBLOCK_COPY_COUNT
    {
        return Err(PhoenixFsError::InvalidObjectRewriteTarget);
    }

    let anchor_item_count = anchor_node.item_count as usize;
    let sibling_item_count = sibling_node.item_count as usize;
    let total_items = anchor_item_count
        .checked_sub(1)
        .and_then(|count| count.checked_add(sibling_item_count))
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let output_items = range_end
        .checked_sub(range_start)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    if range_start >= range_end
        || range_end > total_items
        || output_items > object_internal_capacity()
    {
        return Err(PhoenixFsError::BufferSize);
    }

    destination.fill(0);
    let start = TreeNodeHeader::entries_offset();
    for (output_index, merged_index) in (range_start..range_end).enumerate() {
        let record = combined_internal_record_after_child_delete(
            anchor,
            sibling,
            removed_child_index,
            anchor_is_left,
            merged_index,
            anchor_item_count,
            sibling_item_count,
        )?;
        let record_start = start
            .checked_add(
                output_index
                    .checked_mul(OBJECT_INTERNAL_RECORD_SIZE)
                    .ok_or(PhoenixFsError::ArithmeticOverflow)?,
            )
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        let record_end = record_start
            .checked_add(OBJECT_INTERNAL_RECORD_SIZE)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        record.encode(&mut destination[record_start..record_end])?;
    }

    let item_count = u32::try_from(output_items).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    let entries_bytes = output_items
        .checked_mul(OBJECT_INTERNAL_RECORD_SIZE)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let next = TreeNodeHeader::new(
        MetadataKind::ObjectTree,
        new_generation,
        new_block_number,
        anchor_node.level,
        item_count,
        entries_bytes as u32,
    )?;
    next.seal(destination)?;
    validate_object_internal(destination)?;
    Ok(next)
}

#[allow(clippy::too_many_arguments)]
fn try_commit_internal_rebalance_after_prune<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    path: ObjectTreePath,
    anchor_position: usize,
    object_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    object_copy_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    current_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    next_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    superblock_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<Option<DeepRecordUpdateResult>, PhoenixFsError> {
    if anchor_position == 0 {
        return Ok(None);
    }

    let anchor_step = path.parents[anchor_position];
    let grandparent_position = anchor_position - 1;
    let grandparent_step = path.parents[grandparent_position];

    read_filesystem_block(device, grandparent_step.block_number, object_buffer)?;
    let grandparent = validate_object_internal(object_buffer)?;
    if grandparent.metadata.generation > current.superblock.generation {
        return Err(PhoenixFsError::ObjectNodeGenerationAhead);
    }
    validate_path_pointer(object_buffer, grandparent_step)?;

    let mut grandparent_source = [0_u8; FILESYSTEM_BLOCK_SIZE];
    grandparent_source.copy_from_slice(object_buffer);
    let child_index = grandparent_step.child_index as usize;

    let mut anchor_source = [0_u8; FILESYSTEM_BLOCK_SIZE];
    read_filesystem_block(device, anchor_step.block_number, &mut anchor_source)?;
    let anchor_node = validate_object_internal(&anchor_source)?;
    if anchor_node.metadata.generation > current.superblock.generation {
        return Err(PhoenixFsError::ObjectNodeGenerationAhead);
    }
    validate_path_pointer(&anchor_source, anchor_step)?;

    let modified_count = anchor_node
        .item_count
        .checked_sub(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    if modified_count == 0 {
        return Ok(None);
    }

    let candidates = [
        (child_index + 1 < grandparent.item_count as usize).then_some(child_index + 1),
        child_index.checked_sub(1),
    ];
    let capacity = object_internal_capacity();
    let mut sibling_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
    let mut selected = None;

    for sibling_index in candidates.into_iter().flatten() {
        let sibling = object_internal_record_at(&grandparent_source, sibling_index)?;
        read_filesystem_block(device, sibling.child_block, &mut sibling_buffer)?;
        let sibling_node = validate_object_internal(&sibling_buffer)?;
        if sibling_node.metadata.generation > current.superblock.generation {
            return Err(PhoenixFsError::ObjectNodeGenerationAhead);
        }
        if sibling_node.level != anchor_node.level
            || sibling_node.metadata.block_number != sibling.child_block
            || first_object_node_key(&sibling_buffer, sibling_node)? != sibling.key
        {
            return Err(PhoenixFsError::ObjectTreePathMismatch);
        }

        let total_count = modified_count
            .checked_add(sibling_node.item_count)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        if total_count as usize <= capacity {
            continue;
        }
        if modified_count.checked_mul(2).unwrap_or(u32::MAX) >= sibling_node.item_count {
            continue;
        }

        selected = Some((sibling_index, sibling, total_count as usize));
        break;
    }

    let Some((sibling_index, sibling, total_items)) = selected else {
        return Ok(None);
    };
    let split_index = total_items / 2;
    if split_index == 0 || split_index > capacity || total_items - split_index > capacity {
        return Err(PhoenixFsError::BufferSize);
    }

    let left_index = core::cmp::min(child_index, sibling_index);
    let anchor_is_left = child_index < sibling_index;
    let requested_blocks =
        u64::try_from(grandparent_position + 4).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    let allocation = plan_cow_allocation(
        current_free_space,
        current.superblock.total_blocks,
        requested_blocks,
    )?;
    let generation = current
        .superblock
        .generation
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    let left_block = allocation.allocated.start_block;
    let left = materialize_internal_range_after_child_delete(
        &anchor_source,
        &sibling_buffer,
        object_copy_buffer,
        generation,
        left_block,
        anchor_step.child_index as usize,
        anchor_is_left,
        0,
        split_index,
    )?;
    let left_key = first_object_node_key(object_copy_buffer, left)?;
    write_filesystem_block(device, left_block, object_copy_buffer)?;

    let right_block = left_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let right = materialize_internal_range_after_child_delete(
        &anchor_source,
        &sibling_buffer,
        object_copy_buffer,
        generation,
        right_block,
        anchor_step.child_index as usize,
        anchor_is_left,
        split_index,
        total_items,
    )?;
    let right_key = first_object_node_key(object_copy_buffer, right)?;
    write_filesystem_block(device, right_block, object_copy_buffer)?;

    let grandparent_block = right_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let rewrites = [
        ObjectChildRewrite::new(left_index as u32, left_block, left_key),
        ObjectChildRewrite::new((left_index + 1) as u32, right_block, right_key),
    ];
    let next_grandparent = materialize_object_internal_after_child_rewrites(
        &grandparent_source,
        object_copy_buffer,
        generation,
        grandparent_block,
        &rewrites,
    )?;
    write_filesystem_block(device, grandparent_block, object_copy_buffer)?;

    let grandparent_key = first_object_node_key(object_copy_buffer, next_grandparent)?;
    let mut next_block = grandparent_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let root = copy_object_parent_range(
        device,
        current,
        path,
        0,
        grandparent_position,
        grandparent_block,
        grandparent_key,
        generation,
        &mut next_block,
        object_buffer,
        object_copy_buffer,
    )?;

    let free_space_block = next_block;
    let allocation_end = allocation.allocated.end_block_exclusive()?;
    if free_space_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?
        != allocation_end
    {
        return Err(PhoenixFsError::InvalidTransactionRoots);
    }

    materialize_free_space_leaf_after_allocation(
        current_free_space,
        next_free_space,
        current.superblock.total_blocks,
        generation,
        free_space_block,
        allocation,
    )?;
    write_filesystem_block(device, free_space_block, next_free_space)?;

    let mut retired_blocks = [0_u64; MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS];
    let mut retired_count = 0_usize;
    push_unique_retired(&mut retired_blocks, &mut retired_count, path.leaf_block)?;
    for index in 0..path.parent_count {
        push_unique_retired(
            &mut retired_blocks,
            &mut retired_count,
            path.parents[index].block_number,
        )?;
    }
    push_unique_retired(&mut retired_blocks, &mut retired_count, sibling.child_block)?;
    push_unique_retired(
        &mut retired_blocks,
        &mut retired_count,
        current.superblock.roots.free_space_tree,
    )?;
    sort_u64_prefix(&mut retired_blocks, retired_count);

    let roots = TransactionRoots::new(root.0, free_space_block);
    let plan = TransactionCommitPlan::new(current, roots, &retired_blocks[..retired_count])?;
    let committed = commit_transaction(device, current, plan, superblock_buffer)?;

    Ok(Some(DeepRecordUpdateResult {
        active: committed.active,
        retired_blocks,
        retired_count,
        allocated: allocation.allocated,
    }))
}

#[allow(clippy::too_many_arguments)]
fn commit_empty_leaf_prune<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    path: ObjectTreePath,
    object_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    object_copy_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    current_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    next_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    superblock_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<DeepRecordUpdateResult, PhoenixFsError> {
    if path.parent_count == 0 {
        return Err(PhoenixFsError::ObjectTreePathMismatch);
    }

    let generation = current
        .superblock
        .generation
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let (anchor_position, anchor) =
        find_empty_branch_prune_anchor(device, current, path, object_buffer)?;
    let anchor_step = path.parents[anchor_position];

    let mut retired_blocks = [0_u64; MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS];
    retired_blocks[0] = path.leaf_block;
    for index in 0..path.parent_count {
        retired_blocks[index + 1] = path.parents[index].block_number;
    }
    let retired_count = path.parent_count + 2;
    retired_blocks[path.parent_count + 1] = current.superblock.roots.free_space_tree;

    if anchor_position == 0 && anchor.item_count == 2 {
        let sibling_index = if anchor_step.child_index == 0 { 1 } else { 0 };
        let sibling = object_internal_record_at(object_buffer, sibling_index)?;
        let allocation =
            plan_cow_allocation(current_free_space, current.superblock.total_blocks, 1)?;
        let free_space_block = allocation.allocated.start_block;

        materialize_free_space_leaf_after_allocation(
            current_free_space,
            next_free_space,
            current.superblock.total_blocks,
            generation,
            free_space_block,
            allocation,
        )?;
        write_filesystem_block(device, free_space_block, next_free_space)?;

        sort_u64_prefix(&mut retired_blocks, retired_count);
        let roots = TransactionRoots::new(sibling.child_block, free_space_block);
        let plan = TransactionCommitPlan::new(current, roots, &retired_blocks[..retired_count])?;
        let committed = commit_transaction(device, current, plan, superblock_buffer)?;

        return Ok(DeepRecordUpdateResult {
            active: committed.active,
            retired_blocks,
            retired_count,
            allocated: allocation.allocated,
        });
    }

    if let Some(result) = try_commit_internal_merge_after_prune(
        device,
        current,
        path,
        anchor_position,
        object_buffer,
        object_copy_buffer,
        current_free_space,
        next_free_space,
        superblock_buffer,
    )? {
        return Ok(result);
    }

    if let Some(result) = try_commit_internal_rebalance_after_prune(
        device,
        current,
        path,
        anchor_position,
        object_buffer,
        object_copy_buffer,
        current_free_space,
        next_free_space,
        superblock_buffer,
    )? {
        return Ok(result);
    }

    let requested_blocks =
        u64::try_from(anchor_position + 2).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    let allocation = plan_cow_allocation(
        current_free_space,
        current.superblock.total_blocks,
        requested_blocks,
    )?;
    let mut next_block = allocation.allocated.start_block;

    let mut anchor_source = [0_u8; FILESYSTEM_BLOCK_SIZE];
    anchor_source.copy_from_slice(object_buffer);
    let new_anchor = materialize_object_internal_without_child(
        &anchor_source,
        object_copy_buffer,
        generation,
        next_block,
        anchor_step.child_index as usize,
    )?;
    write_filesystem_block(device, next_block, object_copy_buffer)?;
    let anchor_key = first_object_node_key(object_copy_buffer, new_anchor)?;
    let anchor_block = next_block;
    next_block = next_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    let root = copy_object_parent_range(
        device,
        current,
        path,
        0,
        anchor_position,
        anchor_block,
        anchor_key,
        generation,
        &mut next_block,
        object_buffer,
        object_copy_buffer,
    )?;

    let free_space_block = next_block;
    let allocation_end = allocation.allocated.end_block_exclusive()?;
    if free_space_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?
        != allocation_end
    {
        return Err(PhoenixFsError::InvalidTransactionRoots);
    }

    materialize_free_space_leaf_after_allocation(
        current_free_space,
        next_free_space,
        current.superblock.total_blocks,
        generation,
        free_space_block,
        allocation,
    )?;
    write_filesystem_block(device, free_space_block, next_free_space)?;

    sort_u64_prefix(&mut retired_blocks, retired_count);
    let roots = TransactionRoots::new(root.0, free_space_block);
    let plan = TransactionCommitPlan::new(current, roots, &retired_blocks[..retired_count])?;
    let committed = commit_transaction(device, current, plan, superblock_buffer)?;

    Ok(DeepRecordUpdateResult {
        active: committed.active,
        retired_blocks,
        retired_count,
        allocated: allocation.allocated,
    })
}

fn last_object_leaf_key(block: &[u8]) -> Result<ObjectTreeKey, PhoenixFsError> {
    let node = validate_object_leaf(block)?;
    if node.item_count == 0 {
        return Err(PhoenixFsError::InvalidObjectLeafNode);
    }

    let start = TreeNodeHeader::entries_offset();
    let end = start
        .checked_add(node.entries_bytes as usize)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let mut cursor = start;
    let mut last = None;

    for _ in 0..node.item_count {
        let (record, _, size) = ObjectLeafRecordHeader::decode_with_value(&block[cursor..end])?;
        last = Some(record.key);
        cursor = cursor
            .checked_add(size)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    }

    last.ok_or(PhoenixFsError::InvalidObjectLeafNode)
}

fn read_validated_leaf_child<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    parent: &[u8],
    child_index: usize,
    buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<(ObjectInternalRecord, TreeNodeHeader), PhoenixFsError> {
    let record = object_internal_record_at(parent, child_index)?;
    read_filesystem_block(device, record.child_block, buffer)?;
    let node = validate_object_leaf(buffer)?;
    if node.metadata.generation > current.superblock.generation {
        return Err(PhoenixFsError::ObjectNodeGenerationAhead);
    }
    if node.metadata.block_number != record.child_block
        || first_object_node_key(buffer, node)? != record.key
    {
        return Err(PhoenixFsError::ObjectTreePathMismatch);
    }
    Ok((record, node))
}

fn materialize_merged_object_leaves(
    left: &[u8],
    right: &[u8],
    destination: &mut [u8],
    new_generation: u64,
    new_block_number: u64,
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let left_node = validate_object_leaf(left)?;
    let right_node = validate_object_leaf(right)?;
    if new_generation < left_node.metadata.generation
        || new_generation < right_node.metadata.generation
        || new_block_number < SUPERBLOCK_COPY_COUNT
    {
        return Err(PhoenixFsError::InvalidObjectRewriteTarget);
    }

    let left_last = last_object_leaf_key(left)?;
    let right_first = first_object_node_key(right, right_node)?;
    if left_last >= right_first {
        return Err(PhoenixFsError::ObjectRecordsOutOfOrder);
    }

    let entries_bytes = (left_node.entries_bytes as usize)
        .checked_add(right_node.entries_bytes as usize)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let start = TreeNodeHeader::entries_offset();
    let end = start
        .checked_add(entries_bytes)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    if end > destination.len() {
        return Err(PhoenixFsError::BufferSize);
    }

    destination.fill(0);
    let left_end = start + left_node.entries_bytes as usize;
    destination[start..left_end].copy_from_slice(&left[start..left_end]);
    let right_start = TreeNodeHeader::entries_offset();
    let right_end = right_start + right_node.entries_bytes as usize;
    destination[left_end..end].copy_from_slice(&right[right_start..right_end]);

    let item_count = left_node
        .item_count
        .checked_add(right_node.item_count)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let next = TreeNodeHeader::new(
        MetadataKind::ObjectTree,
        new_generation,
        new_block_number,
        0,
        item_count,
        entries_bytes as u32,
    )?;
    next.seal(destination)?;
    validate_object_leaf(destination)?;
    Ok(next)
}

fn materialize_object_internal_after_children_merge(
    current: &[u8],
    destination: &mut [u8],
    new_generation: u64,
    new_block_number: u64,
    left_index: usize,
    merged_block: u64,
    merged_first_key: ObjectTreeKey,
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let current_node = validate_object_internal(current)?;
    validate_rewrite_target(current_node, new_generation, new_block_number)?;

    let right_index = left_index
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    if right_index >= current_node.item_count as usize || current_node.item_count <= 2 {
        return Err(PhoenixFsError::InvalidObjectRewriteTarget);
    }
    validate_object_child_block(merged_block)?;
    merged_first_key.validate()?;

    destination.fill(0);
    let start = TreeNodeHeader::entries_offset();
    let mut output_index = 0_usize;

    for input_index in 0..current_node.item_count as usize {
        if input_index == right_index {
            continue;
        }

        let record = if input_index == left_index {
            ObjectInternalRecord::new(merged_first_key, merged_block)?
        } else {
            object_internal_record_at(current, input_index)?
        };
        record.encode(
            &mut destination[start + output_index * OBJECT_INTERNAL_RECORD_SIZE
                ..start + (output_index + 1) * OBJECT_INTERNAL_RECORD_SIZE],
        )?;
        output_index += 1;
    }

    let item_count = current_node.item_count - 1;
    let entries_bytes = output_index
        .checked_mul(OBJECT_INTERNAL_RECORD_SIZE)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let next = TreeNodeHeader::new(
        MetadataKind::ObjectTree,
        new_generation,
        new_block_number,
        current_node.level,
        item_count,
        entries_bytes as u32,
    )?;
    next.seal(destination)?;
    validate_object_internal(destination)?;
    Ok(next)
}

#[allow(clippy::too_many_arguments)]
fn try_commit_leaf_merge_after_delete<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    path: ObjectTreePath,
    modified_leaf: &[u8; FILESYSTEM_BLOCK_SIZE],
    allocation: CowAllocationPlan,
    object_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    object_copy_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    current_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    next_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    superblock_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<Option<DeepRecordUpdateResult>, PhoenixFsError> {
    if path.parent_count == 0 {
        return Ok(None);
    }

    let modified_node = validate_object_leaf(modified_leaf)?;
    if modified_node.item_count == 0 {
        return Ok(None);
    }

    let parent_position = path.parent_count - 1;
    let step = path.parents[parent_position];
    read_filesystem_block(device, step.block_number, object_buffer)?;
    let parent = validate_object_internal(object_buffer)?;
    validate_path_pointer(object_buffer, step)?;
    if parent.item_count <= 2 {
        return Ok(None);
    }

    let child_index = step.child_index as usize;
    let mut parent_source = [0_u8; FILESYSTEM_BLOCK_SIZE];
    parent_source.copy_from_slice(object_buffer);

    let capacity = FILESYSTEM_BLOCK_SIZE
        .checked_sub(TreeNodeHeader::entries_offset())
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let mut sibling_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
    let mut selected_sibling = None;
    let candidates = [
        (child_index + 1 < parent.item_count as usize).then_some(child_index + 1),
        child_index.checked_sub(1),
    ];

    for sibling_index in candidates.into_iter().flatten() {
        let (sibling, sibling_node) = read_validated_leaf_child(
            device,
            current,
            &parent_source,
            sibling_index,
            &mut sibling_buffer,
        )?;
        let combined_bytes = (modified_node.entries_bytes as usize)
            .checked_add(sibling_node.entries_bytes as usize)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        if combined_bytes <= capacity {
            selected_sibling = Some((sibling_index, sibling));
            break;
        }
    }

    let Some((sibling_index, sibling)) = selected_sibling else {
        return Ok(None);
    };
    let left_index = core::cmp::min(child_index, sibling_index);

    let generation = current
        .superblock
        .generation
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let merged_block = allocation.allocated.start_block;
    let (left, right): (&[u8], &[u8]) = if child_index < sibling_index {
        (&modified_leaf[..], &sibling_buffer[..])
    } else {
        (&sibling_buffer[..], &modified_leaf[..])
    };
    let merged = materialize_merged_object_leaves(
        left,
        right,
        object_copy_buffer,
        generation,
        merged_block,
    )?;
    let merged_key = first_object_node_key(object_copy_buffer, merged)?;
    write_filesystem_block(device, merged_block, object_copy_buffer)?;

    let parent_block = merged_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let next_parent = materialize_object_internal_after_children_merge(
        &parent_source,
        object_copy_buffer,
        generation,
        parent_block,
        left_index,
        merged_block,
        merged_key,
    )?;
    write_filesystem_block(device, parent_block, object_copy_buffer)?;

    let parent_key = first_object_node_key(object_copy_buffer, next_parent)?;
    let mut next_block = parent_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let root = copy_object_parent_range(
        device,
        current,
        path,
        0,
        parent_position,
        parent_block,
        parent_key,
        generation,
        &mut next_block,
        object_buffer,
        object_copy_buffer,
    )?;

    let free_space_block = next_block;
    let allocation_end = allocation.allocated.end_block_exclusive()?;
    if free_space_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?
        != allocation_end
    {
        return Err(PhoenixFsError::InvalidTransactionRoots);
    }

    materialize_free_space_leaf_after_allocation(
        current_free_space,
        next_free_space,
        current.superblock.total_blocks,
        generation,
        free_space_block,
        allocation,
    )?;
    write_filesystem_block(device, free_space_block, next_free_space)?;

    let mut retired_blocks = [0_u64; MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS];
    let mut retired_count = 0_usize;
    push_unique_retired(&mut retired_blocks, &mut retired_count, path.leaf_block)?;
    push_unique_retired(&mut retired_blocks, &mut retired_count, sibling.child_block)?;
    for index in 0..path.parent_count {
        push_unique_retired(
            &mut retired_blocks,
            &mut retired_count,
            path.parents[index].block_number,
        )?;
    }
    push_unique_retired(
        &mut retired_blocks,
        &mut retired_count,
        current.superblock.roots.free_space_tree,
    )?;
    sort_u64_prefix(&mut retired_blocks, retired_count);

    let roots = TransactionRoots::new(root.0, free_space_block);
    let plan = TransactionCommitPlan::new(current, roots, &retired_blocks[..retired_count])?;
    let committed = commit_transaction(device, current, plan, superblock_buffer)?;

    Ok(Some(DeepRecordUpdateResult {
        active: committed.active,
        retired_blocks,
        retired_count,
        allocated: allocation.allocated,
    }))
}

fn choose_two_leaf_rebalance_index(left: &[u8], right: &[u8]) -> Result<usize, PhoenixFsError> {
    let left_node = validate_object_leaf(left)?;
    let right_node = validate_object_leaf(right)?;
    let capacity = FILESYSTEM_BLOCK_SIZE
        .checked_sub(TreeNodeHeader::entries_offset())
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let total_bytes = (left_node.entries_bytes as usize)
        .checked_add(right_node.entries_bytes as usize)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    if total_bytes <= capacity {
        return Err(PhoenixFsError::BufferSize);
    }

    let total_items = left_node.item_count as usize + right_node.item_count as usize;
    if total_items < 2 {
        return Err(PhoenixFsError::BufferSize);
    }

    let mut best_index = None;
    let mut best_distance = usize::MAX;
    let mut cumulative = 0_usize;
    let mut merged_index = 0_usize;

    for source in [left, right] {
        let node = validate_object_leaf(source)?;
        let start = TreeNodeHeader::entries_offset();
        let end = start
            .checked_add(node.entries_bytes as usize)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        let mut cursor = start;

        for _ in 0..node.item_count {
            let (_, _, size) = ObjectLeafRecordHeader::decode_with_value(&source[cursor..end])?;
            cumulative = cumulative
                .checked_add(size)
                .ok_or(PhoenixFsError::ArithmeticOverflow)?;
            merged_index += 1;
            cursor = cursor
                .checked_add(size)
                .ok_or(PhoenixFsError::ArithmeticOverflow)?;

            if merged_index == total_items {
                continue;
            }

            let right_bytes = total_bytes - cumulative;
            if cumulative > capacity || right_bytes > capacity {
                continue;
            }

            let distance = cumulative.abs_diff(right_bytes);
            if distance < best_distance {
                best_distance = distance;
                best_index = Some(merged_index);
            }
        }
    }

    best_index.ok_or(PhoenixFsError::BufferSize)
}

fn materialize_two_leaf_range(
    left: &[u8],
    right: &[u8],
    destination: &mut [u8],
    new_generation: u64,
    new_block_number: u64,
    range_start: usize,
    range_end: usize,
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let left_node = validate_object_leaf(left)?;
    let right_node = validate_object_leaf(right)?;
    if new_generation < left_node.metadata.generation
        || new_generation < right_node.metadata.generation
        || new_block_number < SUPERBLOCK_COPY_COUNT
    {
        return Err(PhoenixFsError::InvalidObjectRewriteTarget);
    }

    let total_items = left_node.item_count as usize + right_node.item_count as usize;
    if range_start >= range_end || range_end > total_items {
        return Err(PhoenixFsError::InvalidObjectMutationBatch);
    }

    destination.fill(0);
    let destination_start = TreeNodeHeader::entries_offset();
    let mut destination_cursor = destination_start;
    let mut merged_index = 0_usize;

    for source in [left, right] {
        let node = validate_object_leaf(source)?;
        let start = TreeNodeHeader::entries_offset();
        let end = start
            .checked_add(node.entries_bytes as usize)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        let mut cursor = start;

        for _ in 0..node.item_count {
            let (record, value, size) =
                ObjectLeafRecordHeader::decode_with_value(&source[cursor..end])?;
            if (range_start..range_end).contains(&merged_index) {
                destination_cursor =
                    encode_leaf_record_at(destination, destination_cursor, record, value)?;
            }
            merged_index += 1;
            cursor = cursor
                .checked_add(size)
                .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        }
    }

    let item_count =
        u32::try_from(range_end - range_start).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    let entries_bytes = destination_cursor
        .checked_sub(destination_start)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let next = TreeNodeHeader::new(
        MetadataKind::ObjectTree,
        new_generation,
        new_block_number,
        0,
        item_count,
        entries_bytes as u32,
    )?;
    next.seal(destination)?;
    validate_object_leaf(destination)?;
    Ok(next)
}

#[allow(clippy::too_many_arguments)]
fn try_commit_leaf_rebalance_after_delete<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    path: ObjectTreePath,
    modified_leaf: &[u8; FILESYSTEM_BLOCK_SIZE],
    object_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    object_copy_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    current_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    next_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    superblock_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<Option<DeepRecordUpdateResult>, PhoenixFsError> {
    if path.parent_count == 0 {
        return Ok(None);
    }

    let modified_node = validate_object_leaf(modified_leaf)?;
    if modified_node.item_count == 0 {
        return Ok(None);
    }

    let parent_position = path.parent_count - 1;
    let step = path.parents[parent_position];
    read_filesystem_block(device, step.block_number, object_buffer)?;
    let parent = validate_object_internal(object_buffer)?;
    validate_path_pointer(object_buffer, step)?;

    let child_index = step.child_index as usize;
    let mut parent_source = [0_u8; FILESYSTEM_BLOCK_SIZE];
    parent_source.copy_from_slice(object_buffer);

    let capacity = FILESYSTEM_BLOCK_SIZE
        .checked_sub(TreeNodeHeader::entries_offset())
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let modified_bytes = modified_node.entries_bytes as usize;
    let mut sibling_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
    let mut selected_sibling = None;
    let candidates = [
        (child_index + 1 < parent.item_count as usize).then_some(child_index + 1),
        child_index.checked_sub(1),
    ];

    for sibling_index in candidates.into_iter().flatten() {
        let (sibling, sibling_node) = read_validated_leaf_child(
            device,
            current,
            &parent_source,
            sibling_index,
            &mut sibling_buffer,
        )?;
        let sibling_bytes = sibling_node.entries_bytes as usize;
        let combined_bytes = modified_bytes
            .checked_add(sibling_bytes)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        if combined_bytes <= capacity {
            continue;
        }
        if modified_bytes.checked_mul(2).unwrap_or(usize::MAX) >= sibling_bytes {
            continue;
        }

        selected_sibling = Some((sibling_index, sibling));
        break;
    }

    let Some((sibling_index, sibling)) = selected_sibling else {
        return Ok(None);
    };

    let (left, right, left_index): (&[u8], &[u8], usize) = if child_index < sibling_index {
        (&modified_leaf[..], &sibling_buffer[..], child_index)
    } else {
        (&sibling_buffer[..], &modified_leaf[..], sibling_index)
    };
    let split_index = choose_two_leaf_rebalance_index(left, right)?;
    let left_node = validate_object_leaf(left)?;
    let right_node = validate_object_leaf(right)?;
    let total_items = left_node.item_count as usize + right_node.item_count as usize;

    let requested_blocks =
        u64::try_from(path.node_count() + 2).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    let allocation = plan_cow_allocation(
        current_free_space,
        current.superblock.total_blocks,
        requested_blocks,
    )?;
    let generation = current
        .superblock
        .generation
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    let left_block = allocation.allocated.start_block;
    let right_block = left_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let parent_block = right_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    let next_left = materialize_two_leaf_range(
        left,
        right,
        object_copy_buffer,
        generation,
        left_block,
        0,
        split_index,
    )?;
    let left_key = first_object_node_key(object_copy_buffer, next_left)?;
    write_filesystem_block(device, left_block, object_copy_buffer)?;

    let next_right = materialize_two_leaf_range(
        left,
        right,
        object_copy_buffer,
        generation,
        right_block,
        split_index,
        total_items,
    )?;
    let right_key = first_object_node_key(object_copy_buffer, next_right)?;
    write_filesystem_block(device, right_block, object_copy_buffer)?;

    let rewrites = [
        ObjectChildRewrite::new(left_index as u32, left_block, left_key),
        ObjectChildRewrite::new((left_index + 1) as u32, right_block, right_key),
    ];
    let next_parent = materialize_object_internal_after_child_rewrites(
        &parent_source,
        object_copy_buffer,
        generation,
        parent_block,
        &rewrites,
    )?;
    write_filesystem_block(device, parent_block, object_copy_buffer)?;

    let parent_key = first_object_node_key(object_copy_buffer, next_parent)?;
    let mut next_block = parent_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let root = copy_object_parent_range(
        device,
        current,
        path,
        0,
        parent_position,
        parent_block,
        parent_key,
        generation,
        &mut next_block,
        object_buffer,
        object_copy_buffer,
    )?;

    let free_space_block = next_block;
    let allocation_end = allocation.allocated.end_block_exclusive()?;
    if free_space_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?
        != allocation_end
    {
        return Err(PhoenixFsError::InvalidTransactionRoots);
    }

    materialize_free_space_leaf_after_allocation(
        current_free_space,
        next_free_space,
        current.superblock.total_blocks,
        generation,
        free_space_block,
        allocation,
    )?;
    write_filesystem_block(device, free_space_block, next_free_space)?;

    let mut retired_blocks = [0_u64; MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS];
    let mut retired_count = 0_usize;
    push_unique_retired(&mut retired_blocks, &mut retired_count, path.leaf_block)?;
    push_unique_retired(&mut retired_blocks, &mut retired_count, sibling.child_block)?;
    for index in 0..path.parent_count {
        push_unique_retired(
            &mut retired_blocks,
            &mut retired_count,
            path.parents[index].block_number,
        )?;
    }
    push_unique_retired(
        &mut retired_blocks,
        &mut retired_count,
        current.superblock.roots.free_space_tree,
    )?;
    sort_u64_prefix(&mut retired_blocks, retired_count);

    let roots = TransactionRoots::new(root.0, free_space_block);
    let plan = TransactionCommitPlan::new(current, roots, &retired_blocks[..retired_count])?;
    let committed = commit_transaction(device, current, plan, superblock_buffer)?;

    Ok(Some(DeepRecordUpdateResult {
        active: committed.active,
        retired_blocks,
        retired_count,
        allocated: allocation.allocated,
    }))
}

fn commit_object_leaf_mutation<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    mutation: ObjectLeafMutation<'_>,
    object_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    object_copy_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    current_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    next_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    superblock_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<DeepRecordUpdateResult, PhoenixFsError> {
    let total_blocks = current.superblock.total_blocks;
    let generation = current
        .superblock
        .generation
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let key = mutation.key()?;

    let path = if let ObjectLeafMutation::InsertBatch { insertions } = mutation {
        find_shared_insertion_path(
            device,
            current.superblock.roots.object_tree,
            insertions,
            current.superblock.generation,
            object_buffer,
        )?
    } else if let ObjectLeafMutation::DeleteBatch { deletions } = mutation {
        find_shared_deletion_path(
            device,
            current.superblock.roots.object_tree,
            deletions,
            current.superblock.generation,
            object_buffer,
        )?
    } else if mutation.is_insertion() {
        find_object_tree_path_for_insertion(
            device,
            current.superblock.roots.object_tree,
            key,
            current.superblock.generation,
            object_buffer,
        )?
    } else {
        find_object_tree_path(
            device,
            current.superblock.roots.object_tree,
            key,
            current.superblock.generation,
            object_buffer,
        )?
    };

    read_filesystem_block(
        device,
        current.superblock.roots.free_space_tree,
        current_free_space,
    )?;
    let free_node = validate_free_space_leaf(current_free_space, total_blocks)?;
    if free_node.metadata.block_number != current.superblock.roots.free_space_tree {
        return Err(PhoenixFsError::InvalidTransactionRoots);
    }
    if free_node.metadata.generation > current.superblock.generation {
        return Err(PhoenixFsError::ObjectNodeGenerationAhead);
    }

    read_filesystem_block(device, path.leaf_block, object_buffer)?;
    let leaf = validate_object_leaf(object_buffer)?;
    let deleted_items = match mutation {
        ObjectLeafMutation::Delete { .. } => 1,
        ObjectLeafMutation::DeleteBatch { deletions } => deletions.len(),
        _ => 0,
    };
    if path.parent_count > 0 && deleted_items != 0 && deleted_items == leaf.item_count as usize {
        return commit_empty_leaf_prune(
            device,
            current,
            path,
            object_buffer,
            object_copy_buffer,
            current_free_space,
            next_free_space,
            superblock_buffer,
        );
    }

    let requested_blocks =
        u64::try_from(path.node_count() + 1).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    let allocation = plan_cow_allocation(current_free_space, total_blocks, requested_blocks)?;
    let first_new_block = allocation.allocated.start_block;

    match mutation {
        ObjectLeafMutation::Replace { key, value } => {
            materialize_object_leaf_with_replaced_value(
                object_buffer,
                object_copy_buffer,
                key,
                value,
                generation,
                first_new_block,
            )?;
        }
        ObjectLeafMutation::Insert { key, value } => {
            let insertion = [ObjectLeafInsert::new(key, value)];
            match materialize_object_leaf_with_inserted_value(
                object_buffer,
                object_copy_buffer,
                key,
                value,
                generation,
                first_new_block,
            ) {
                Ok(_) => {}
                Err(PhoenixFsError::BufferSize) => {
                    if path.parent_count == 0 {
                        return commit_root_leaf_split_insertions(
                            device,
                            current,
                            &insertion,
                            object_buffer,
                            object_copy_buffer,
                            current_free_space,
                            next_free_space,
                            superblock_buffer,
                        );
                    }
                    return commit_nonroot_leaf_split_insertions(
                        device,
                        current,
                        path,
                        &insertion,
                        object_buffer,
                        object_copy_buffer,
                        current_free_space,
                        next_free_space,
                        superblock_buffer,
                    );
                }
                Err(error) => return Err(error),
            }
        }
        ObjectLeafMutation::InsertBatch { insertions } => {
            match materialize_object_leaf_with_inserted_values(
                object_buffer,
                object_copy_buffer,
                insertions,
                generation,
                first_new_block,
            ) {
                Ok(_) => {}
                Err(PhoenixFsError::BufferSize) => {
                    if path.parent_count == 0 {
                        return commit_root_leaf_split_insertions(
                            device,
                            current,
                            insertions,
                            object_buffer,
                            object_copy_buffer,
                            current_free_space,
                            next_free_space,
                            superblock_buffer,
                        );
                    }
                    return commit_nonroot_leaf_split_insertions(
                        device,
                        current,
                        path,
                        insertions,
                        object_buffer,
                        object_copy_buffer,
                        current_free_space,
                        next_free_space,
                        superblock_buffer,
                    );
                }
                Err(error) => return Err(error),
            }
        }
        ObjectLeafMutation::Delete { key } => {
            materialize_object_leaf_without_key(
                object_buffer,
                object_copy_buffer,
                key,
                generation,
                first_new_block,
            )?;
        }
        ObjectLeafMutation::DeleteBatch { deletions } => {
            materialize_object_leaf_without_keys(
                object_buffer,
                object_copy_buffer,
                deletions,
                generation,
                first_new_block,
            )?;
        }
    }
    let leaf_node = TreeNodeHeader::decode(object_copy_buffer)?;
    if deleted_items != 0 && leaf_node.item_count > 0 && path.parent_count > 0 {
        let mut modified_leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        modified_leaf.copy_from_slice(object_copy_buffer);
        if let Some(result) = try_commit_leaf_merge_after_delete(
            device,
            current,
            path,
            &modified_leaf,
            allocation,
            object_buffer,
            object_copy_buffer,
            current_free_space,
            next_free_space,
            superblock_buffer,
        )? {
            return Ok(result);
        }

        if let Some(result) = try_commit_leaf_rebalance_after_delete(
            device,
            current,
            path,
            &modified_leaf,
            object_buffer,
            object_copy_buffer,
            current_free_space,
            next_free_space,
            superblock_buffer,
        )? {
            return Ok(result);
        }
    }

    write_filesystem_block(device, first_new_block, object_copy_buffer)?;

    let mut new_child_block = first_new_block;
    let mut new_child_first_key = if path.parent_count == 0 && leaf_node.item_count == 0 {
        None
    } else {
        Some(first_object_node_key(object_copy_buffer, leaf_node)?)
    };
    let mut next_block = first_new_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    for parent_position in (0..path.parent_count).rev() {
        let step = path.parents[parent_position];
        read_filesystem_block(device, step.block_number, object_buffer)?;
        let current_parent = validate_object_internal(object_buffer)?;
        if current_parent.metadata.generation > current.superblock.generation {
            return Err(PhoenixFsError::ObjectNodeGenerationAhead);
        }

        let old_pointer = object_internal_record_at(object_buffer, step.child_index as usize)?;
        if old_pointer.key != step.child_key {
            return Err(PhoenixFsError::ObjectTreePathMismatch);
        }

        let child_first_key =
            new_child_first_key.ok_or(PhoenixFsError::ObjectLeafWouldBecomeEmpty)?;
        materialize_object_internal_after_child_copy(
            object_buffer,
            object_copy_buffer,
            generation,
            next_block,
            step.child_index as usize,
            new_child_block,
            child_first_key,
        )?;
        write_filesystem_block(device, next_block, object_copy_buffer)?;

        let copied_parent = TreeNodeHeader::decode(object_copy_buffer)?;
        new_child_first_key = Some(first_object_node_key(object_copy_buffer, copied_parent)?);
        new_child_block = next_block;
        next_block = next_block
            .checked_add(1)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    }

    let free_space_block = next_block;
    materialize_free_space_leaf_after_allocation(
        current_free_space,
        next_free_space,
        total_blocks,
        generation,
        free_space_block,
        allocation,
    )?;
    write_filesystem_block(device, free_space_block, next_free_space)?;

    let mut retired_blocks = [0_u64; MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS];
    retired_blocks[0] = path.leaf_block;
    for index in 0..path.parent_count {
        retired_blocks[index + 1] = path.parents[path.parent_count - index - 1].block_number;
    }
    let retired_count = path.node_count() + 1;
    retired_blocks[path.node_count()] = current.superblock.roots.free_space_tree;
    sort_u64_prefix(&mut retired_blocks, retired_count);

    let roots = TransactionRoots::new(new_child_block, free_space_block);
    let plan = TransactionCommitPlan::new(current, roots, &retired_blocks[..retired_count])?;
    let committed = commit_transaction(device, current, plan, superblock_buffer)?;

    Ok(DeepRecordUpdateResult {
        active: committed.active,
        retired_blocks,
        retired_count,
        allocated: allocation.allocated,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObjectDeletionPaths {
    first: ObjectTreePath,
    split_index: usize,
    second: Option<ObjectTreePath>,
}

fn classify_deletion_paths<D: BlockDevice>(
    device: &mut D,
    root_block: u64,
    deletions: &[ObjectLeafDelete],
    maximum_generation: u64,
    buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<ObjectDeletionPaths, PhoenixFsError> {
    validate_object_deletions(deletions)?;

    let first = find_object_tree_path(
        device,
        root_block,
        deletions[0].key,
        maximum_generation,
        buffer,
    )?;
    let mut second = None;
    let mut split_index = deletions.len();

    for (index, deletion) in deletions.iter().enumerate().skip(1) {
        let candidate =
            find_object_tree_path(device, root_block, deletion.key, maximum_generation, buffer)?;

        match second {
            None if candidate == first => {}
            None => {
                second = Some(candidate);
                split_index = index;
            }
            Some(existing) if candidate == existing => {}
            Some(_) => return Err(PhoenixFsError::ObjectMutationBatchSpansLeaves),
        }
    }

    Ok(ObjectDeletionPaths {
        first,
        split_index,
        second,
    })
}

fn find_shared_deletion_path<D: BlockDevice>(
    device: &mut D,
    root_block: u64,
    deletions: &[ObjectLeafDelete],
    maximum_generation: u64,
    buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<ObjectTreePath, PhoenixFsError> {
    let paths = classify_deletion_paths(device, root_block, deletions, maximum_generation, buffer)?;
    if paths.second.is_some() {
        return Err(PhoenixFsError::ObjectMutationBatchSpansLeaves);
    }
    Ok(paths.first)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObjectInsertionPaths {
    first: ObjectTreePath,
    split_index: usize,
    second: Option<ObjectTreePath>,
}

fn classify_insertion_paths<D: BlockDevice>(
    device: &mut D,
    root_block: u64,
    insertions: &[ObjectLeafInsert<'_>],
    maximum_generation: u64,
    buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<ObjectInsertionPaths, PhoenixFsError> {
    validate_object_insertions(insertions)?;

    let first = find_object_tree_path_for_insertion(
        device,
        root_block,
        insertions[0].key,
        maximum_generation,
        buffer,
    )?;
    let mut second = None;
    let mut split_index = insertions.len();

    for (index, insertion) in insertions.iter().enumerate().skip(1) {
        let candidate = find_object_tree_path_for_insertion(
            device,
            root_block,
            insertion.key,
            maximum_generation,
            buffer,
        )?;

        match second {
            None if candidate == first => {}
            None => {
                second = Some(candidate);
                split_index = index;
            }
            Some(existing) if candidate == existing => {}
            Some(_) => return Err(PhoenixFsError::ObjectMutationBatchSpansLeaves),
        }
    }

    Ok(ObjectInsertionPaths {
        first,
        split_index,
        second,
    })
}

fn commit_two_leaf_insertions<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    first_path: ObjectTreePath,
    second_path: ObjectTreePath,
    first_insertions: &[ObjectLeafInsert<'_>],
    second_insertions: &[ObjectLeafInsert<'_>],
    object_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    object_copy_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    current_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    next_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    superblock_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<DeepRecordUpdateResult, PhoenixFsError> {
    if first_insertions.is_empty() || second_insertions.is_empty() {
        return Err(PhoenixFsError::InvalidObjectMutationBatch);
    }
    let divergence = object_path_divergence(first_path, second_path)?;

    let mut retired_blocks = [0_u64; MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS];
    let retired_count = collect_two_path_retired_blocks(
        first_path,
        second_path,
        current.superblock.roots.free_space_tree,
        &mut retired_blocks,
    )?;
    let requested_blocks =
        u64::try_from(retired_count).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;

    let total_blocks = current.superblock.total_blocks;
    read_filesystem_block(
        device,
        current.superblock.roots.free_space_tree,
        current_free_space,
    )?;
    let free_node = validate_free_space_leaf(current_free_space, total_blocks)?;
    if free_node.metadata.block_number != current.superblock.roots.free_space_tree {
        return Err(PhoenixFsError::InvalidTransactionRoots);
    }
    if free_node.metadata.generation > current.superblock.generation {
        return Err(PhoenixFsError::ObjectNodeGenerationAhead);
    }

    let allocation = plan_cow_allocation(current_free_space, total_blocks, requested_blocks)?;
    let generation = current
        .superblock
        .generation
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let mut next_block = allocation.allocated.start_block;

    let first_leaf = copy_inserted_leaf(
        device,
        first_path.leaf_block,
        first_insertions,
        generation,
        next_block,
        object_buffer,
        object_copy_buffer,
    )?;
    next_block = next_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    let second_leaf = copy_inserted_leaf(
        device,
        second_path.leaf_block,
        second_insertions,
        generation,
        next_block,
        object_buffer,
        object_copy_buffer,
    )?;
    next_block = next_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    let first_branch = copy_object_parent_range(
        device,
        current,
        first_path,
        divergence + 1,
        first_path.parent_count,
        first_leaf.0,
        first_leaf.1,
        generation,
        &mut next_block,
        object_buffer,
        object_copy_buffer,
    )?;
    let second_branch = copy_object_parent_range(
        device,
        current,
        second_path,
        divergence + 1,
        second_path.parent_count,
        second_leaf.0,
        second_leaf.1,
        generation,
        &mut next_block,
        object_buffer,
        object_copy_buffer,
    )?;

    let divergence_step = first_path.parents[divergence];
    let second_divergence_step = second_path.parents[divergence];
    if divergence_step.block_number != second_divergence_step.block_number {
        return Err(PhoenixFsError::ObjectTreePathMismatch);
    }

    read_filesystem_block(device, divergence_step.block_number, object_buffer)?;
    let parent = validate_object_internal(object_buffer)?;
    if parent.metadata.generation > current.superblock.generation {
        return Err(PhoenixFsError::ObjectNodeGenerationAhead);
    }
    validate_path_pointer(object_buffer, divergence_step)?;
    validate_path_pointer(object_buffer, second_divergence_step)?;

    let mut rewrites = [
        ObjectChildRewrite::new(divergence_step.child_index, first_branch.0, first_branch.1),
        ObjectChildRewrite::new(
            second_divergence_step.child_index,
            second_branch.0,
            second_branch.1,
        ),
    ];
    if rewrites[0].child_index > rewrites[1].child_index {
        rewrites.swap(0, 1);
    }

    materialize_object_internal_after_child_rewrites(
        object_buffer,
        object_copy_buffer,
        generation,
        next_block,
        &rewrites,
    )?;
    write_filesystem_block(device, next_block, object_copy_buffer)?;
    let divergence_node = TreeNodeHeader::decode(object_copy_buffer)?;
    let common_key = first_object_node_key(object_copy_buffer, divergence_node)?;
    let common_block = next_block;
    next_block = next_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    let root = copy_object_parent_range(
        device,
        current,
        first_path,
        0,
        divergence,
        common_block,
        common_key,
        generation,
        &mut next_block,
        object_buffer,
        object_copy_buffer,
    )?;

    let free_space_block = next_block;
    let allocation_end = allocation.allocated.end_block_exclusive()?;
    if free_space_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?
        != allocation_end
    {
        return Err(PhoenixFsError::InvalidTransactionRoots);
    }

    materialize_free_space_leaf_after_allocation(
        current_free_space,
        next_free_space,
        total_blocks,
        generation,
        free_space_block,
        allocation,
    )?;
    write_filesystem_block(device, free_space_block, next_free_space)?;

    sort_u64_prefix(&mut retired_blocks, retired_count);
    let roots = TransactionRoots::new(root.0, free_space_block);
    let plan = TransactionCommitPlan::new(current, roots, &retired_blocks[..retired_count])?;
    let committed = commit_transaction(device, current, plan, superblock_buffer)?;

    Ok(DeepRecordUpdateResult {
        active: committed.active,
        retired_blocks,
        retired_count,
        allocated: allocation.allocated,
    })
}

fn commit_two_leaf_deletions<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    first_path: ObjectTreePath,
    second_path: ObjectTreePath,
    first_deletions: &[ObjectLeafDelete],
    second_deletions: &[ObjectLeafDelete],
    object_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    object_copy_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    current_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    next_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    superblock_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<DeepRecordUpdateResult, PhoenixFsError> {
    if first_deletions.is_empty() || second_deletions.is_empty() {
        return Err(PhoenixFsError::InvalidObjectMutationBatch);
    }
    let divergence = object_path_divergence(first_path, second_path)?;

    let mut retired_blocks = [0_u64; MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS];
    let retired_count = collect_two_path_retired_blocks(
        first_path,
        second_path,
        current.superblock.roots.free_space_tree,
        &mut retired_blocks,
    )?;
    let requested_blocks =
        u64::try_from(retired_count).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;

    let total_blocks = current.superblock.total_blocks;
    read_filesystem_block(
        device,
        current.superblock.roots.free_space_tree,
        current_free_space,
    )?;
    let free_node = validate_free_space_leaf(current_free_space, total_blocks)?;
    if free_node.metadata.block_number != current.superblock.roots.free_space_tree {
        return Err(PhoenixFsError::InvalidTransactionRoots);
    }
    if free_node.metadata.generation > current.superblock.generation {
        return Err(PhoenixFsError::ObjectNodeGenerationAhead);
    }

    let allocation = plan_cow_allocation(current_free_space, total_blocks, requested_blocks)?;
    let generation = current
        .superblock
        .generation
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let mut next_block = allocation.allocated.start_block;

    let first_leaf = copy_deleted_leaf(
        device,
        first_path.leaf_block,
        first_deletions,
        generation,
        next_block,
        object_buffer,
        object_copy_buffer,
    )?;
    next_block = next_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    let second_leaf = copy_deleted_leaf(
        device,
        second_path.leaf_block,
        second_deletions,
        generation,
        next_block,
        object_buffer,
        object_copy_buffer,
    )?;
    next_block = next_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    let first_branch = copy_object_parent_range(
        device,
        current,
        first_path,
        divergence + 1,
        first_path.parent_count,
        first_leaf.0,
        first_leaf.1,
        generation,
        &mut next_block,
        object_buffer,
        object_copy_buffer,
    )?;
    let second_branch = copy_object_parent_range(
        device,
        current,
        second_path,
        divergence + 1,
        second_path.parent_count,
        second_leaf.0,
        second_leaf.1,
        generation,
        &mut next_block,
        object_buffer,
        object_copy_buffer,
    )?;

    let first_step = first_path.parents[divergence];
    let second_step = second_path.parents[divergence];
    if first_step.block_number != second_step.block_number {
        return Err(PhoenixFsError::ObjectTreePathMismatch);
    }

    read_filesystem_block(device, first_step.block_number, object_buffer)?;
    let parent = validate_object_internal(object_buffer)?;
    if parent.metadata.generation > current.superblock.generation {
        return Err(PhoenixFsError::ObjectNodeGenerationAhead);
    }
    validate_path_pointer(object_buffer, first_step)?;
    validate_path_pointer(object_buffer, second_step)?;

    let mut rewrites = [
        ObjectChildRewrite::new(first_step.child_index, first_branch.0, first_branch.1),
        ObjectChildRewrite::new(second_step.child_index, second_branch.0, second_branch.1),
    ];
    if rewrites[0].child_index > rewrites[1].child_index {
        rewrites.swap(0, 1);
    }

    materialize_object_internal_after_child_rewrites(
        object_buffer,
        object_copy_buffer,
        generation,
        next_block,
        &rewrites,
    )?;
    write_filesystem_block(device, next_block, object_copy_buffer)?;
    let divergence_node = TreeNodeHeader::decode(object_copy_buffer)?;
    let common_key = first_object_node_key(object_copy_buffer, divergence_node)?;
    let common_block = next_block;
    next_block = next_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    let root = copy_object_parent_range(
        device,
        current,
        first_path,
        0,
        divergence,
        common_block,
        common_key,
        generation,
        &mut next_block,
        object_buffer,
        object_copy_buffer,
    )?;

    let free_space_block = next_block;
    let allocation_end = allocation.allocated.end_block_exclusive()?;
    if free_space_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?
        != allocation_end
    {
        return Err(PhoenixFsError::InvalidTransactionRoots);
    }

    materialize_free_space_leaf_after_allocation(
        current_free_space,
        next_free_space,
        total_blocks,
        generation,
        free_space_block,
        allocation,
    )?;
    write_filesystem_block(device, free_space_block, next_free_space)?;

    sort_u64_prefix(&mut retired_blocks, retired_count);
    let roots = TransactionRoots::new(root.0, free_space_block);
    let plan = TransactionCommitPlan::new(current, roots, &retired_blocks[..retired_count])?;
    let committed = commit_transaction(device, current, plan, superblock_buffer)?;

    Ok(DeepRecordUpdateResult {
        active: committed.active,
        retired_blocks,
        retired_count,
        allocated: allocation.allocated,
    })
}

fn copy_deleted_leaf<D: BlockDevice>(
    device: &mut D,
    source_block: u64,
    deletions: &[ObjectLeafDelete],
    generation: u64,
    destination_block: u64,
    object_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    object_copy_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<(u64, ObjectTreeKey), PhoenixFsError> {
    read_filesystem_block(device, source_block, object_buffer)?;
    materialize_object_leaf_without_keys(
        object_buffer,
        object_copy_buffer,
        deletions,
        generation,
        destination_block,
    )?;
    let node = TreeNodeHeader::decode(object_copy_buffer)?;
    if node.item_count == 0 {
        return Err(PhoenixFsError::ObjectLeafWouldBecomeEmpty);
    }

    write_filesystem_block(device, destination_block, object_copy_buffer)?;
    let first_key = first_object_node_key(object_copy_buffer, node)?;
    Ok((destination_block, first_key))
}

fn copy_inserted_leaf<D: BlockDevice>(
    device: &mut D,
    source_block: u64,
    insertions: &[ObjectLeafInsert<'_>],
    generation: u64,
    destination_block: u64,
    object_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    object_copy_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<(u64, ObjectTreeKey), PhoenixFsError> {
    read_filesystem_block(device, source_block, object_buffer)?;
    materialize_object_leaf_with_inserted_values(
        object_buffer,
        object_copy_buffer,
        insertions,
        generation,
        destination_block,
    )?;
    write_filesystem_block(device, destination_block, object_copy_buffer)?;
    let node = TreeNodeHeader::decode(object_copy_buffer)?;
    let first_key = first_object_node_key(object_copy_buffer, node)?;
    Ok((destination_block, first_key))
}

#[allow(clippy::too_many_arguments)]
fn copy_object_parent_range<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    path: ObjectTreePath,
    range_start: usize,
    range_end: usize,
    mut child_block: u64,
    mut child_first_key: ObjectTreeKey,
    generation: u64,
    next_block: &mut u64,
    object_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    object_copy_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<(u64, ObjectTreeKey), PhoenixFsError> {
    if range_start > range_end || range_end > path.parent_count {
        return Err(PhoenixFsError::ObjectTreePathMismatch);
    }

    for parent_position in (range_start..range_end).rev() {
        let step = path.parents[parent_position];
        read_filesystem_block(device, step.block_number, object_buffer)?;
        let parent = validate_object_internal(object_buffer)?;
        if parent.metadata.generation > current.superblock.generation {
            return Err(PhoenixFsError::ObjectNodeGenerationAhead);
        }
        validate_path_pointer(object_buffer, step)?;

        materialize_object_internal_after_child_copy(
            object_buffer,
            object_copy_buffer,
            generation,
            *next_block,
            step.child_index as usize,
            child_block,
            child_first_key,
        )?;
        write_filesystem_block(device, *next_block, object_copy_buffer)?;

        let copied_parent = TreeNodeHeader::decode(object_copy_buffer)?;
        child_first_key = first_object_node_key(object_copy_buffer, copied_parent)?;
        child_block = *next_block;
        *next_block = (*next_block)
            .checked_add(1)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    }

    Ok((child_block, child_first_key))
}

fn validate_path_pointer(block: &[u8], step: ObjectPathStep) -> Result<(), PhoenixFsError> {
    let pointer = object_internal_record_at(block, step.child_index as usize)?;
    if pointer.key != step.child_key {
        return Err(PhoenixFsError::ObjectTreePathMismatch);
    }
    Ok(())
}

fn object_path_divergence(
    first: ObjectTreePath,
    second: ObjectTreePath,
) -> Result<usize, PhoenixFsError> {
    if first.leaf_block == second.leaf_block
        || first.parent_count == 0
        || first.parent_count != second.parent_count
    {
        return Err(PhoenixFsError::ObjectTreePathMismatch);
    }

    for index in 0..first.parent_count {
        let first_step = first.parents[index];
        let second_step = second.parents[index];
        if first_step.block_number != second_step.block_number {
            return Err(PhoenixFsError::ObjectTreePathMismatch);
        }
        if first_step.child_index != second_step.child_index {
            return Ok(index);
        }
        if first_step.child_key != second_step.child_key {
            return Err(PhoenixFsError::ObjectTreePathMismatch);
        }
    }

    Err(PhoenixFsError::ObjectTreePathMismatch)
}

fn collect_two_path_retired_blocks(
    first: ObjectTreePath,
    second: ObjectTreePath,
    free_space_root: u64,
    retired: &mut [u64; MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS],
) -> Result<usize, PhoenixFsError> {
    let mut count = 0_usize;
    push_unique_retired(retired, &mut count, first.leaf_block)?;
    push_unique_retired(retired, &mut count, second.leaf_block)?;

    for index in 0..first.parent_count {
        push_unique_retired(retired, &mut count, first.parents[index].block_number)?;
    }
    for index in 0..second.parent_count {
        push_unique_retired(retired, &mut count, second.parents[index].block_number)?;
    }
    push_unique_retired(retired, &mut count, free_space_root)?;
    Ok(count)
}

fn push_unique_retired(
    retired: &mut [u64; MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS],
    count: &mut usize,
    block: u64,
) -> Result<(), PhoenixFsError> {
    if retired[..*count].contains(&block) {
        return Ok(());
    }
    if *count == retired.len() {
        return Err(PhoenixFsError::ObjectTreeDepthExceeded);
    }
    retired[*count] = block;
    *count += 1;
    Ok(())
}

fn validate_object_insertions(insertions: &[ObjectLeafInsert<'_>]) -> Result<(), PhoenixFsError> {
    if insertions.is_empty() {
        return Err(PhoenixFsError::InvalidObjectMutationBatch);
    }

    let mut previous_key = None;
    for insertion in insertions {
        insertion.key.validate()?;
        let value_bytes = u32::try_from(insertion.value.len())
            .map_err(|_| PhoenixFsError::InvalidObjectRecordSize)?;
        ObjectLeafRecordHeader::new(insertion.key, value_bytes)?;

        if previous_key.is_some_and(|previous| previous >= insertion.key) {
            return Err(PhoenixFsError::InvalidObjectMutationBatch);
        }
        previous_key = Some(insertion.key);
    }
    Ok(())
}

fn find_shared_insertion_path<D: BlockDevice>(
    device: &mut D,
    root_block: u64,
    insertions: &[ObjectLeafInsert<'_>],
    maximum_generation: u64,
    buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<ObjectTreePath, PhoenixFsError> {
    validate_object_insertions(insertions)?;

    let first = find_object_tree_path_for_insertion(
        device,
        root_block,
        insertions[0].key,
        maximum_generation,
        buffer,
    )?;

    for insertion in &insertions[1..] {
        let candidate = find_object_tree_path_for_insertion(
            device,
            root_block,
            insertion.key,
            maximum_generation,
            buffer,
        )?;
        if candidate != first {
            return Err(PhoenixFsError::ObjectMutationBatchSpansLeaves);
        }
    }
    Ok(first)
}

fn object_internal_record_at(
    block: &[u8],
    index: usize,
) -> Result<ObjectInternalRecord, PhoenixFsError> {
    let node = validate_object_internal(block)?;
    if index >= node.item_count as usize {
        return Err(PhoenixFsError::InvalidObjectInternalRecordSize);
    }

    let offset = TreeNodeHeader::entries_offset()
        .checked_add(
            index
                .checked_mul(OBJECT_INTERNAL_RECORD_SIZE)
                .ok_or(PhoenixFsError::ArithmeticOverflow)?,
        )
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    ObjectInternalRecord::decode(&block[offset..offset + OBJECT_INTERNAL_RECORD_SIZE])
}

fn sort_u64_prefix(values: &mut [u64], length: usize) {
    let mut index = 1_usize;
    while index < length {
        let value = values[index];
        let mut position = index;
        while position > 0 && values[position - 1] > value {
            values[position] = values[position - 1];
            position -= 1;
        }
        values[position] = value;
        index += 1;
    }
}

fn choose_object_leaf_split_index(
    current: &[u8],
    insertions: &[ObjectLeafInsert<'_>],
) -> Result<usize, PhoenixFsError> {
    let node = validate_object_leaf(current)?;
    validate_object_insertions(insertions)?;

    let capacity = FILESYSTEM_BLOCK_SIZE
        .checked_sub(TreeNodeHeader::entries_offset())
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let inserted_bytes = insertions.iter().try_fold(0_usize, |total, insertion| {
        let value_bytes = u32::try_from(insertion.value.len())
            .map_err(|_| PhoenixFsError::InvalidObjectRecordSize)?;
        let record = ObjectLeafRecordHeader::new(insertion.key, value_bytes)?;
        total
            .checked_add(record.encoded_size()?)
            .ok_or(PhoenixFsError::ArithmeticOverflow)
    })?;
    let total_bytes = (node.entries_bytes as usize)
        .checked_add(inserted_bytes)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    if total_bytes <= capacity || total_bytes > capacity.saturating_mul(2) {
        return Err(PhoenixFsError::BufferSize);
    }

    let total_items = node.item_count as usize + insertions.len();
    if total_items < 2 {
        return Err(PhoenixFsError::BufferSize);
    }

    let entries_start = TreeNodeHeader::entries_offset();
    let entries_end = entries_start
        .checked_add(node.entries_bytes as usize)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let mut source_cursor = entries_start;
    let mut source_remaining = node.item_count as usize;
    let mut insertion_index = 0_usize;
    let mut cumulative = 0_usize;
    let mut best_index = None;
    let mut best_distance = usize::MAX;

    for merged_index in 0..total_items {
        let encoded_size = if source_remaining == 0 {
            let insertion = insertions[insertion_index];
            insertion_index += 1;
            let value_bytes = u32::try_from(insertion.value.len())
                .map_err(|_| PhoenixFsError::InvalidObjectRecordSize)?;
            ObjectLeafRecordHeader::new(insertion.key, value_bytes)?.encoded_size()?
        } else {
            let (record, _, source_size) =
                ObjectLeafRecordHeader::decode_with_value(&current[source_cursor..entries_end])?;
            let use_insertion =
                insertion_index < insertions.len() && insertions[insertion_index].key < record.key;

            if insertion_index < insertions.len() && insertions[insertion_index].key == record.key {
                return Err(PhoenixFsError::ObjectRecordAlreadyExists);
            }

            if use_insertion {
                let insertion = insertions[insertion_index];
                insertion_index += 1;
                let value_bytes = u32::try_from(insertion.value.len())
                    .map_err(|_| PhoenixFsError::InvalidObjectRecordSize)?;
                ObjectLeafRecordHeader::new(insertion.key, value_bytes)?.encoded_size()?
            } else {
                source_cursor = source_cursor
                    .checked_add(source_size)
                    .ok_or(PhoenixFsError::ArithmeticOverflow)?;
                source_remaining -= 1;
                source_size
            }
        };

        cumulative = cumulative
            .checked_add(encoded_size)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        let split_index = merged_index + 1;
        if split_index == total_items {
            continue;
        }

        let right_bytes = total_bytes - cumulative;
        if cumulative > capacity || right_bytes > capacity {
            continue;
        }

        let distance = cumulative.abs_diff(right_bytes);
        if distance < best_distance {
            best_distance = distance;
            best_index = Some(split_index);
        }
    }

    best_index.ok_or(PhoenixFsError::BufferSize)
}

fn materialize_object_leaf_insert_split_side(
    current: &[u8],
    destination: &mut [u8],
    insertions: &[ObjectLeafInsert<'_>],
    new_generation: u64,
    new_block_number: u64,
    range_start: usize,
    range_end: usize,
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let current_node = validate_object_leaf(current)?;
    validate_rewrite_target(current_node, new_generation, new_block_number)?;
    validate_object_insertions(insertions)?;

    let total_items = current_node.item_count as usize + insertions.len();
    if range_start >= range_end || range_end > total_items {
        return Err(PhoenixFsError::InvalidObjectMutationBatch);
    }

    destination.fill(0);
    let entries_start = TreeNodeHeader::entries_offset();
    let entries_end = entries_start
        .checked_add(current_node.entries_bytes as usize)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let mut source_cursor = entries_start;
    let mut source_remaining = current_node.item_count as usize;
    let mut insertion_index = 0_usize;
    let mut destination_cursor = entries_start;

    for merged_index in 0..total_items {
        let (record, value, source_size, consumed_source) = if source_remaining == 0 {
            let insertion = insertions[insertion_index];
            insertion_index += 1;
            let value_bytes = u32::try_from(insertion.value.len())
                .map_err(|_| PhoenixFsError::InvalidObjectRecordSize)?;
            (
                ObjectLeafRecordHeader::new(insertion.key, value_bytes)?,
                insertion.value,
                0_usize,
                false,
            )
        } else {
            let (source_record, source_value, source_size) =
                ObjectLeafRecordHeader::decode_with_value(&current[source_cursor..entries_end])?;

            if insertion_index < insertions.len()
                && insertions[insertion_index].key == source_record.key
            {
                return Err(PhoenixFsError::ObjectRecordAlreadyExists);
            }

            if insertion_index < insertions.len()
                && insertions[insertion_index].key < source_record.key
            {
                let insertion = insertions[insertion_index];
                insertion_index += 1;
                let value_bytes = u32::try_from(insertion.value.len())
                    .map_err(|_| PhoenixFsError::InvalidObjectRecordSize)?;
                (
                    ObjectLeafRecordHeader::new(insertion.key, value_bytes)?,
                    insertion.value,
                    0_usize,
                    false,
                )
            } else {
                (source_record, source_value, source_size, true)
            }
        };

        if (range_start..range_end).contains(&merged_index) {
            destination_cursor =
                encode_leaf_record_at(destination, destination_cursor, record, value)?;
        }

        if consumed_source {
            source_cursor = source_cursor
                .checked_add(source_size)
                .ok_or(PhoenixFsError::ArithmeticOverflow)?;
            source_remaining -= 1;
        }
    }

    let item_count =
        u32::try_from(range_end - range_start).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    seal_object_leaf_rewrite(
        destination,
        current_node,
        new_generation,
        new_block_number,
        item_count,
        destination_cursor,
    )
}

#[allow(clippy::too_many_arguments)]
fn count_full_parent_chain<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    path: ObjectTreePath,
    buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<usize, PhoenixFsError> {
    let mut full = 0_usize;

    for parent_position in (0..path.parent_count).rev() {
        let step = path.parents[parent_position];
        read_filesystem_block(device, step.block_number, buffer)?;
        let parent = validate_object_internal(buffer)?;
        if parent.metadata.generation > current.superblock.generation {
            return Err(PhoenixFsError::ObjectNodeGenerationAhead);
        }
        validate_path_pointer(buffer, step)?;

        if object_internal_can_accept_split(buffer)? {
            break;
        }
        full += 1;
    }

    Ok(full)
}

#[allow(clippy::too_many_arguments)]
fn commit_nonroot_leaf_split_insertions<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    path: ObjectTreePath,
    insertions: &[ObjectLeafInsert<'_>],
    current_leaf: &[u8; FILESYSTEM_BLOCK_SIZE],
    object_copy_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    current_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    next_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    superblock_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<DeepRecordUpdateResult, PhoenixFsError> {
    if path.parent_count == 0 {
        return Err(PhoenixFsError::ObjectTreePathMismatch);
    }

    let leaf = validate_object_leaf(current_leaf)?;
    if leaf.metadata.block_number != path.leaf_block {
        return Err(PhoenixFsError::ObjectTreePathMismatch);
    }

    let full_parent_count = count_full_parent_chain(device, current, path, object_copy_buffer)?;
    let split_index = choose_object_leaf_split_index(current_leaf, insertions)?;
    let total_items = leaf.item_count as usize + insertions.len();
    let generation = current
        .superblock
        .generation
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    let requested_blocks = if full_parent_count == path.parent_count {
        2_usize
            .checked_add(
                full_parent_count
                    .checked_mul(2)
                    .ok_or(PhoenixFsError::ArithmeticOverflow)?,
            )
            .and_then(|value| value.checked_add(2))
            .ok_or(PhoenixFsError::ArithmeticOverflow)?
    } else {
        path.parent_count
            .checked_add(full_parent_count)
            .and_then(|value| value.checked_add(3))
            .ok_or(PhoenixFsError::ArithmeticOverflow)?
    };
    let requested_blocks =
        u64::try_from(requested_blocks).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    let allocation = plan_cow_allocation(
        current_free_space,
        current.superblock.total_blocks,
        requested_blocks,
    )?;

    let mut next_block = allocation.allocated.start_block;
    let left_block = next_block;
    next_block = next_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let right_block = next_block;
    next_block = next_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    let left = materialize_object_leaf_insert_split_side(
        current_leaf,
        object_copy_buffer,
        insertions,
        generation,
        left_block,
        0,
        split_index,
    )?;
    let mut left_key = first_object_node_key(object_copy_buffer, left)?;
    write_filesystem_block(device, left_block, object_copy_buffer)?;

    let right = materialize_object_leaf_insert_split_side(
        current_leaf,
        object_copy_buffer,
        insertions,
        generation,
        right_block,
        split_index,
        total_items,
    )?;
    let mut right_key = first_object_node_key(object_copy_buffer, right)?;
    write_filesystem_block(device, right_block, object_copy_buffer)?;

    let mut left_child_block = left_block;
    let mut right_child_block = right_block;
    let mut first_single_parent = None;
    let mut top_split_level = None;

    for parent_position in (0..path.parent_count).rev() {
        let step = path.parents[parent_position];
        read_filesystem_block(device, step.block_number, object_copy_buffer)?;
        let parent = validate_object_internal(object_copy_buffer)?;
        if parent.metadata.generation > current.superblock.generation {
            return Err(PhoenixFsError::ObjectNodeGenerationAhead);
        }
        validate_path_pointer(object_copy_buffer, step)?;

        let mut parent_source = [0_u8; FILESYSTEM_BLOCK_SIZE];
        parent_source.copy_from_slice(object_copy_buffer);

        if object_internal_can_accept_split(&parent_source)? {
            materialize_object_internal_after_child_split(
                &parent_source,
                object_copy_buffer,
                generation,
                next_block,
                step.child_index as usize,
                left_child_block,
                left_key,
                right_child_block,
                right_key,
            )?;
            write_filesystem_block(device, next_block, object_copy_buffer)?;

            let node = TreeNodeHeader::decode(object_copy_buffer)?;
            let key = first_object_node_key(object_copy_buffer, node)?;
            first_single_parent = Some((parent_position, next_block, key));
            next_block = next_block
                .checked_add(1)
                .ok_or(PhoenixFsError::ArithmeticOverflow)?;
            break;
        }

        let total_records = parent.item_count as usize + 1;
        let split_at = total_records / 2;
        let left_parent_block = next_block;
        next_block = next_block
            .checked_add(1)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        let right_parent_block = next_block;
        next_block = next_block
            .checked_add(1)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;

        let source_left_block = left_child_block;
        let source_left_key = left_key;
        let source_right_block = right_child_block;
        let source_right_key = right_key;

        let left_parent = materialize_object_internal_child_split_side(
            &parent_source,
            object_copy_buffer,
            generation,
            left_parent_block,
            step.child_index as usize,
            source_left_block,
            source_left_key,
            source_right_block,
            source_right_key,
            0,
            split_at,
        )?;
        let next_left_key = first_object_node_key(object_copy_buffer, left_parent)?;
        write_filesystem_block(device, left_parent_block, object_copy_buffer)?;

        let right_parent = materialize_object_internal_child_split_side(
            &parent_source,
            object_copy_buffer,
            generation,
            right_parent_block,
            step.child_index as usize,
            source_left_block,
            source_left_key,
            source_right_block,
            source_right_key,
            split_at,
            total_records,
        )?;
        let next_right_key = first_object_node_key(object_copy_buffer, right_parent)?;
        write_filesystem_block(device, right_parent_block, object_copy_buffer)?;

        left_child_block = left_parent_block;
        left_key = next_left_key;
        right_child_block = right_parent_block;
        right_key = next_right_key;
        top_split_level = Some(parent.level);
    }

    let root = if let Some((parent_position, block, key)) = first_single_parent {
        let mut parent_read_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        copy_object_parent_range(
            device,
            current,
            path,
            0,
            parent_position,
            block,
            key,
            generation,
            &mut next_block,
            &mut parent_read_buffer,
            object_copy_buffer,
        )?
    } else {
        let level = top_split_level
            .and_then(|value| value.checked_add(1))
            .ok_or(PhoenixFsError::InvalidObjectTreeLevel)?;
        object_copy_buffer.fill(0);
        let start = TreeNodeHeader::entries_offset();
        ObjectInternalRecord::new(left_key, left_child_block)?
            .encode(&mut object_copy_buffer[start..start + OBJECT_INTERNAL_RECORD_SIZE])?;
        ObjectInternalRecord::new(right_key, right_child_block)?.encode(
            &mut object_copy_buffer
                [start + OBJECT_INTERNAL_RECORD_SIZE..start + 2 * OBJECT_INTERNAL_RECORD_SIZE],
        )?;
        let root = TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            generation,
            next_block,
            level,
            2,
            (2 * OBJECT_INTERNAL_RECORD_SIZE) as u32,
        )?;
        root.seal(object_copy_buffer)?;
        validate_object_internal(object_copy_buffer)?;
        write_filesystem_block(device, next_block, object_copy_buffer)?;
        let block = next_block;
        next_block = next_block
            .checked_add(1)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        (block, left_key)
    };

    let free_space_block = next_block;
    let allocation_end = allocation.allocated.end_block_exclusive()?;
    if free_space_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?
        != allocation_end
    {
        return Err(PhoenixFsError::InvalidTransactionRoots);
    }

    materialize_free_space_leaf_after_allocation(
        current_free_space,
        next_free_space,
        current.superblock.total_blocks,
        generation,
        free_space_block,
        allocation,
    )?;
    write_filesystem_block(device, free_space_block, next_free_space)?;

    let mut retired_blocks = [0_u64; MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS];
    retired_blocks[0] = path.leaf_block;
    for index in 0..path.parent_count {
        retired_blocks[index + 1] = path.parents[index].block_number;
    }
    let retired_count = path.parent_count + 2;
    retired_blocks[path.parent_count + 1] = current.superblock.roots.free_space_tree;
    sort_u64_prefix(&mut retired_blocks, retired_count);

    let roots = TransactionRoots::new(root.0, free_space_block);
    let plan = TransactionCommitPlan::new(current, roots, &retired_blocks[..retired_count])?;
    let committed = commit_transaction(device, current, plan, superblock_buffer)?;

    Ok(DeepRecordUpdateResult {
        active: committed.active,
        retired_blocks,
        retired_count,
        allocated: allocation.allocated,
    })
}

#[allow(clippy::too_many_arguments)]
fn commit_root_leaf_split_insertions<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    insertions: &[ObjectLeafInsert<'_>],
    current_leaf: &[u8; FILESYSTEM_BLOCK_SIZE],
    object_copy_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    current_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    next_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    superblock_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<DeepRecordUpdateResult, PhoenixFsError> {
    let root = validate_object_leaf(current_leaf)?;
    if root.metadata.block_number != current.superblock.roots.object_tree {
        return Err(PhoenixFsError::InvalidTransactionRoots);
    }

    let split_index = choose_object_leaf_split_index(current_leaf, insertions)?;
    let total_items = root.item_count as usize + insertions.len();
    let generation = current
        .superblock
        .generation
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let allocation = plan_cow_allocation(current_free_space, current.superblock.total_blocks, 4)?;

    let left_block = allocation.allocated.start_block;
    let right_block = left_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let root_block = right_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let free_space_block = root_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    let left = materialize_object_leaf_insert_split_side(
        current_leaf,
        object_copy_buffer,
        insertions,
        generation,
        left_block,
        0,
        split_index,
    )?;
    let left_key = first_object_node_key(object_copy_buffer, left)?;
    write_filesystem_block(device, left_block, object_copy_buffer)?;

    let right = materialize_object_leaf_insert_split_side(
        current_leaf,
        object_copy_buffer,
        insertions,
        generation,
        right_block,
        split_index,
        total_items,
    )?;
    let right_key = first_object_node_key(object_copy_buffer, right)?;
    write_filesystem_block(device, right_block, object_copy_buffer)?;

    object_copy_buffer.fill(0);
    let entries_start = TreeNodeHeader::entries_offset();
    ObjectInternalRecord::new(left_key, left_block)?.encode(
        &mut object_copy_buffer[entries_start..entries_start + OBJECT_INTERNAL_RECORD_SIZE],
    )?;
    ObjectInternalRecord::new(right_key, right_block)?.encode(
        &mut object_copy_buffer[entries_start + OBJECT_INTERNAL_RECORD_SIZE
            ..entries_start + 2 * OBJECT_INTERNAL_RECORD_SIZE],
    )?;
    let new_root = TreeNodeHeader::new(
        MetadataKind::ObjectTree,
        generation,
        root_block,
        1,
        2,
        (2 * OBJECT_INTERNAL_RECORD_SIZE) as u32,
    )?;
    new_root.seal(object_copy_buffer)?;
    validate_object_internal(object_copy_buffer)?;
    write_filesystem_block(device, root_block, object_copy_buffer)?;

    materialize_free_space_leaf_after_allocation(
        current_free_space,
        next_free_space,
        current.superblock.total_blocks,
        generation,
        free_space_block,
        allocation,
    )?;
    write_filesystem_block(device, free_space_block, next_free_space)?;

    let mut retired_blocks = [0_u64; MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS];
    retired_blocks[0] = current.superblock.roots.object_tree;
    retired_blocks[1] = current.superblock.roots.free_space_tree;
    sort_u64_prefix(&mut retired_blocks, 2);

    let roots = TransactionRoots::new(root_block, free_space_block);
    let plan = TransactionCommitPlan::new(current, roots, &retired_blocks[..2])?;
    let committed = commit_transaction(device, current, plan, superblock_buffer)?;

    Ok(DeepRecordUpdateResult {
        active: committed.active,
        retired_blocks,
        retired_count: 2,
        allocated: allocation.allocated,
    })
}

pub fn materialize_object_leaf_with_inserted_value(
    current: &[u8],
    destination: &mut [u8],
    key: ObjectTreeKey,
    value: &[u8],
    new_generation: u64,
    new_block_number: u64,
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let insertion = [ObjectLeafInsert::new(key, value)];
    materialize_object_leaf_with_inserted_values(
        current,
        destination,
        &insertion,
        new_generation,
        new_block_number,
    )
}

pub fn materialize_object_leaf_with_inserted_values(
    current: &[u8],
    destination: &mut [u8],
    insertions: &[ObjectLeafInsert<'_>],
    new_generation: u64,
    new_block_number: u64,
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let current_node = validate_object_leaf(current)?;
    validate_rewrite_target(current_node, new_generation, new_block_number)?;
    validate_object_insertions(insertions)?;

    let entries_start = TreeNodeHeader::entries_offset();
    let entries_end = entries_start
        .checked_add(current_node.entries_bytes as usize)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    destination.fill(0);
    let mut source_cursor = entries_start;
    let mut destination_cursor = entries_start;
    let mut insertion_index = 0_usize;

    for _ in 0..current_node.item_count {
        let (record, old_value, source_size) =
            ObjectLeafRecordHeader::decode_with_value(&current[source_cursor..entries_end])?;

        while insertion_index < insertions.len() && insertions[insertion_index].key < record.key {
            let insertion = insertions[insertion_index];
            let value_bytes = u32::try_from(insertion.value.len())
                .map_err(|_| PhoenixFsError::InvalidObjectRecordSize)?;
            let inserted_record = ObjectLeafRecordHeader::new(insertion.key, value_bytes)?;
            destination_cursor = encode_leaf_record_at(
                destination,
                destination_cursor,
                inserted_record,
                insertion.value,
            )?;
            insertion_index += 1;
        }

        if insertion_index < insertions.len() && insertions[insertion_index].key == record.key {
            return Err(PhoenixFsError::ObjectRecordAlreadyExists);
        }

        destination_cursor =
            encode_leaf_record_at(destination, destination_cursor, record, old_value)?;
        source_cursor = source_cursor
            .checked_add(source_size)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    }

    while insertion_index < insertions.len() {
        let insertion = insertions[insertion_index];
        let value_bytes = u32::try_from(insertion.value.len())
            .map_err(|_| PhoenixFsError::InvalidObjectRecordSize)?;
        let inserted_record = ObjectLeafRecordHeader::new(insertion.key, value_bytes)?;
        destination_cursor = encode_leaf_record_at(
            destination,
            destination_cursor,
            inserted_record,
            insertion.value,
        )?;
        insertion_index += 1;
    }

    let inserted_count =
        u32::try_from(insertions.len()).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    let item_count = current_node
        .item_count
        .checked_add(inserted_count)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    seal_object_leaf_rewrite(
        destination,
        current_node,
        new_generation,
        new_block_number,
        item_count,
        destination_cursor,
    )
}

pub fn materialize_object_leaf_without_keys(
    current: &[u8],
    destination: &mut [u8],
    deletions: &[ObjectLeafDelete],
    new_generation: u64,
    new_block_number: u64,
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let current_node = validate_object_leaf(current)?;
    validate_rewrite_target(current_node, new_generation, new_block_number)?;
    validate_object_deletions(deletions)?;

    let entries_start = TreeNodeHeader::entries_offset();
    let entries_end = entries_start
        .checked_add(current_node.entries_bytes as usize)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    destination.fill(0);
    let mut source_cursor = entries_start;
    let mut destination_cursor = entries_start;
    let mut deletion_index = 0_usize;

    for _ in 0..current_node.item_count {
        let (record, value, source_size) =
            ObjectLeafRecordHeader::decode_with_value(&current[source_cursor..entries_end])?;

        while deletion_index < deletions.len() && deletions[deletion_index].key < record.key {
            return Err(PhoenixFsError::ObjectRecordNotFound);
        }

        if deletion_index < deletions.len() && deletions[deletion_index].key == record.key {
            deletion_index += 1;
        } else {
            destination_cursor =
                encode_leaf_record_at(destination, destination_cursor, record, value)?;
        }

        source_cursor = source_cursor
            .checked_add(source_size)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    }

    if deletion_index != deletions.len() {
        return Err(PhoenixFsError::ObjectRecordNotFound);
    }

    let deleted_count =
        u32::try_from(deletions.len()).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    let item_count = current_node
        .item_count
        .checked_sub(deleted_count)
        .ok_or(PhoenixFsError::InvalidObjectMutationBatch)?;

    seal_object_leaf_rewrite(
        destination,
        current_node,
        new_generation,
        new_block_number,
        item_count,
        destination_cursor,
    )
}

fn validate_object_deletions(deletions: &[ObjectLeafDelete]) -> Result<(), PhoenixFsError> {
    if deletions.is_empty() {
        return Err(PhoenixFsError::InvalidObjectMutationBatch);
    }

    let mut previous_key = None;
    for deletion in deletions {
        deletion.key.validate()?;
        if previous_key.is_some_and(|previous| previous >= deletion.key) {
            return Err(PhoenixFsError::InvalidObjectMutationBatch);
        }
        previous_key = Some(deletion.key);
    }
    Ok(())
}

pub fn materialize_object_leaf_without_key(
    current: &[u8],
    destination: &mut [u8],
    target_key: ObjectTreeKey,
    new_generation: u64,
    new_block_number: u64,
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let current_node = validate_object_leaf(current)?;
    validate_rewrite_target(current_node, new_generation, new_block_number)?;
    target_key.validate()?;

    let entries_start = TreeNodeHeader::entries_offset();
    let entries_end = entries_start
        .checked_add(current_node.entries_bytes as usize)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    destination.fill(0);
    let mut source_cursor = entries_start;
    let mut destination_cursor = entries_start;
    let mut removed = false;

    for _ in 0..current_node.item_count {
        let (record, value, source_size) =
            ObjectLeafRecordHeader::decode_with_value(&current[source_cursor..entries_end])?;

        if record.key == target_key {
            removed = true;
        } else {
            destination_cursor =
                encode_leaf_record_at(destination, destination_cursor, record, value)?;
        }

        source_cursor = source_cursor
            .checked_add(source_size)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    }

    if !removed {
        return Err(PhoenixFsError::ObjectRecordNotFound);
    }

    seal_object_leaf_rewrite(
        destination,
        current_node,
        new_generation,
        new_block_number,
        current_node.item_count - 1,
        destination_cursor,
    )
}

fn encode_leaf_record_at(
    destination: &mut [u8],
    cursor: usize,
    record: ObjectLeafRecordHeader,
    value: &[u8],
) -> Result<usize, PhoenixFsError> {
    let encoded_size = record.encoded_size()?;
    let end = cursor
        .checked_add(encoded_size)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    if end > destination.len() {
        return Err(PhoenixFsError::BufferSize);
    }

    record.encode_with_value(&mut destination[cursor..end], value)?;
    Ok(end)
}

fn seal_object_leaf_rewrite(
    destination: &mut [u8],
    current_node: TreeNodeHeader,
    new_generation: u64,
    new_block_number: u64,
    item_count: u32,
    destination_cursor: usize,
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let entries_start = TreeNodeHeader::entries_offset();
    let entries_bytes = destination_cursor
        .checked_sub(entries_start)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let entries_bytes =
        u32::try_from(entries_bytes).map_err(|_| PhoenixFsError::InvalidObjectRecordSize)?;

    let next = TreeNodeHeader::new(
        MetadataKind::ObjectTree,
        new_generation,
        new_block_number,
        0,
        item_count,
        entries_bytes,
    )?;
    if current_node.level != 0 {
        return Err(PhoenixFsError::InvalidObjectLeafNode);
    }
    next.seal(destination)?;
    validate_object_leaf(destination)?;
    Ok(next)
}

pub fn materialize_object_leaf_with_replaced_value(
    current: &[u8],
    destination: &mut [u8],
    target_key: ObjectTreeKey,
    new_value: &[u8],
    new_generation: u64,
    new_block_number: u64,
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let current_node = validate_object_leaf(current)?;
    validate_rewrite_target(current_node, new_generation, new_block_number)?;
    target_key.validate()?;

    let entries_start = TreeNodeHeader::entries_offset();
    let entries_end = entries_start
        .checked_add(current_node.entries_bytes as usize)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let value_bytes =
        u32::try_from(new_value.len()).map_err(|_| PhoenixFsError::InvalidObjectRecordSize)?;

    destination.fill(0);
    let mut source_cursor = entries_start;
    let mut destination_cursor = entries_start;
    let mut replaced = false;

    for _ in 0..current_node.item_count {
        let (record, old_value, source_size) =
            ObjectLeafRecordHeader::decode_with_value(&current[source_cursor..entries_end])?;
        let value = if record.key == target_key {
            replaced = true;
            new_value
        } else {
            old_value
        };
        let bytes = if record.key == target_key {
            value_bytes
        } else {
            record.value_bytes
        };
        let next_record = ObjectLeafRecordHeader::new(record.key, bytes)?;
        let encoded_size = next_record.encoded_size()?;
        let destination_end = destination_cursor
            .checked_add(encoded_size)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        if destination_end > destination.len() {
            return Err(PhoenixFsError::BufferSize);
        }

        next_record
            .encode_with_value(&mut destination[destination_cursor..destination_end], value)?;
        source_cursor = source_cursor
            .checked_add(source_size)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        destination_cursor = destination_end;
    }

    if !replaced {
        return Err(PhoenixFsError::ObjectRecordNotFound);
    }

    let entries_bytes = destination_cursor
        .checked_sub(entries_start)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let entries_bytes =
        u32::try_from(entries_bytes).map_err(|_| PhoenixFsError::InvalidObjectRecordSize)?;
    let next = TreeNodeHeader::new(
        MetadataKind::ObjectTree,
        new_generation,
        new_block_number,
        0,
        current_node.item_count,
        entries_bytes,
    )?;
    next.seal(destination)?;
    validate_object_leaf(destination)?;
    Ok(next)
}

pub fn materialize_free_space_leaf_with_reclaimed_blocks(
    current: &[u8],
    destination: &mut [u8],
    total_blocks: u64,
    new_generation: u64,
    new_block_number: u64,
    reclaimed_blocks: &[u64],
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let current_node = validate_free_space_leaf(current, total_blocks)?;
    if destination.len() != FILESYSTEM_BLOCK_SIZE {
        return Err(PhoenixFsError::BufferSize);
    }
    if new_generation <= current_node.metadata.generation {
        return Err(PhoenixFsError::GenerationSequence);
    }
    if new_block_number < SUPERBLOCK_COPY_COUNT || new_block_number >= total_blocks {
        return Err(PhoenixFsError::InvalidMetadataBlock(new_block_number));
    }

    validate_reclaimed_input(reclaimed_blocks, total_blocks, new_block_number)?;

    destination.fill(0);
    let entries_start = TreeNodeHeader::entries_offset();
    let entries_bytes = current_node.entries_bytes as usize;
    let entries_end = entries_start
        .checked_add(entries_bytes)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    destination[entries_start..entries_end].copy_from_slice(&current[entries_start..entries_end]);

    let mut item_count = current_node.item_count as usize;
    for block in reclaimed_blocks.iter().copied() {
        insert_reclaimed_block(destination, &mut item_count, block)?;
    }

    let entries_bytes = item_count
        .checked_mul(FREE_SPACE_RECORD_SIZE)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let next = TreeNodeHeader::new(
        MetadataKind::FreeSpaceTree,
        new_generation,
        new_block_number,
        0,
        item_count as u32,
        entries_bytes as u32,
    )?;
    next.seal(destination)?;
    validate_free_space_leaf(destination, total_blocks)?;
    Ok(next)
}

fn validate_reclaimed_input(
    reclaimed_blocks: &[u64],
    total_blocks: u64,
    new_block_number: u64,
) -> Result<(), PhoenixFsError> {
    let mut previous = None;
    for block in reclaimed_blocks.iter().copied() {
        if block < SUPERBLOCK_COPY_COUNT || block >= total_blocks || block == new_block_number {
            return Err(PhoenixFsError::InvalidReclaimedBlock(block));
        }
        if previous.is_some_and(|value| value >= block) {
            return Err(PhoenixFsError::InvalidReclaimedBlock(block));
        }
        previous = Some(block);
    }
    Ok(())
}

fn insert_reclaimed_block(
    block: &mut [u8],
    item_count: &mut usize,
    reclaimed: u64,
) -> Result<(), PhoenixFsError> {
    let mut index = 0_usize;
    while index < *item_count {
        let extent = free_space_extent_at(block, index)?;
        let end = extent.end_block_exclusive()?;
        if reclaimed >= extent.start_block && reclaimed < end {
            return Err(PhoenixFsError::ReclaimedBlockAlreadyFree(reclaimed));
        }
        if reclaimed < extent.start_block {
            break;
        }
        index += 1;
    }

    let previous = if index == 0 {
        None
    } else {
        Some(free_space_extent_at(block, index - 1)?)
    };
    let next = if index == *item_count {
        None
    } else {
        Some(free_space_extent_at(block, index)?)
    };

    let joins_previous = match previous {
        Some(extent) => extent.end_block_exclusive()? == reclaimed,
        None => false,
    };
    let joins_next = next
        .map(|extent| reclaimed.checked_add(1) == Some(extent.start_block))
        .unwrap_or(false);

    if joins_previous && joins_next {
        let previous = previous.ok_or(PhoenixFsError::InvalidFreeSpaceRange)?;
        let next = next.ok_or(PhoenixFsError::InvalidFreeSpaceRange)?;
        let next_end = next.end_block_exclusive()?;
        let merged_count = next_end
            .checked_sub(previous.start_block)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        let merged = FreeSpaceExtent::new(previous.start_block, merged_count);
        write_free_space_extent(block, index - 1, merged)?;
        remove_free_space_extent(block, item_count, index)?;
        return Ok(());
    }

    if joins_previous {
        let previous = previous.ok_or(PhoenixFsError::InvalidFreeSpaceRange)?;
        let block_count = previous
            .block_count
            .checked_add(1)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        let merged = FreeSpaceExtent::new(previous.start_block, block_count);
        return write_free_space_extent(block, index - 1, merged);
    }

    if joins_next {
        let next = next.ok_or(PhoenixFsError::InvalidFreeSpaceRange)?;
        let block_count = next
            .block_count
            .checked_add(1)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        let merged = FreeSpaceExtent::new(reclaimed, block_count);
        return write_free_space_extent(block, index, merged);
    }

    insert_free_space_extent(block, item_count, index, FreeSpaceExtent::new(reclaimed, 1))
}

fn insert_free_space_extent(
    block: &mut [u8],
    item_count: &mut usize,
    index: usize,
    extent: FreeSpaceExtent,
) -> Result<(), PhoenixFsError> {
    let start = TreeNodeHeader::entries_offset();
    let used_bytes = item_count
        .checked_mul(FREE_SPACE_RECORD_SIZE)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let new_used_bytes = used_bytes
        .checked_add(FREE_SPACE_RECORD_SIZE)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    if start + new_used_bytes > block.len() {
        return Err(PhoenixFsError::BufferSize);
    }

    let insert_offset = start + index * FREE_SPACE_RECORD_SIZE;
    let used_end = start + used_bytes;
    block.copy_within(
        insert_offset..used_end,
        insert_offset + FREE_SPACE_RECORD_SIZE,
    );
    *item_count += 1;
    write_free_space_extent(block, index, extent)
}

fn remove_free_space_extent(
    block: &mut [u8],
    item_count: &mut usize,
    index: usize,
) -> Result<(), PhoenixFsError> {
    if index >= *item_count {
        return Err(PhoenixFsError::InvalidFreeSpaceRecordSize);
    }

    let start = TreeNodeHeader::entries_offset();
    let remove_offset = start + index * FREE_SPACE_RECORD_SIZE;
    let used_end = start + *item_count * FREE_SPACE_RECORD_SIZE;
    let next_offset = remove_offset + FREE_SPACE_RECORD_SIZE;
    block.copy_within(next_offset..used_end, remove_offset);
    block[used_end - FREE_SPACE_RECORD_SIZE..used_end].fill(0);
    *item_count -= 1;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RootLeafUpdateResult {
    pub active: ActiveSuperblock,
    pub retired_blocks: [u64; 2],
    pub allocated: FreeSpaceExtent,
}

pub fn commit_root_leaf_record_update<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    target_key: ObjectTreeKey,
    new_value: &[u8],
    current_object: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    current_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    next_object: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    next_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    superblock_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<RootLeafUpdateResult, PhoenixFsError> {
    let total_blocks = current.superblock.total_blocks;
    read_filesystem_block(device, current.superblock.roots.object_tree, current_object)?;
    read_filesystem_block(
        device,
        current.superblock.roots.free_space_tree,
        current_free_space,
    )?;

    let object_node = validate_object_leaf(current_object)?;
    let free_node = validate_free_space_leaf(current_free_space, total_blocks)?;
    if object_node.metadata.block_number != current.superblock.roots.object_tree {
        return Err(PhoenixFsError::InvalidTransactionRoots);
    }
    if free_node.metadata.block_number != current.superblock.roots.free_space_tree {
        return Err(PhoenixFsError::InvalidTransactionRoots);
    }

    let generation = current
        .superblock
        .generation
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let allocation = plan_cow_allocation(current_free_space, total_blocks, 2)?;
    let object_block = allocation.allocated.start_block;
    let free_space_block = object_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    materialize_object_leaf_with_replaced_value(
        current_object,
        next_object,
        target_key,
        new_value,
        generation,
        object_block,
    )?;
    materialize_free_space_leaf_after_allocation(
        current_free_space,
        next_free_space,
        total_blocks,
        generation,
        free_space_block,
        allocation,
    )?;

    write_filesystem_block(device, object_block, next_object)?;
    write_filesystem_block(device, free_space_block, next_free_space)?;

    let retired_blocks = [
        current.superblock.roots.object_tree,
        current.superblock.roots.free_space_tree,
    ];
    let roots = TransactionRoots::new(object_block, free_space_block);
    let plan = TransactionCommitPlan::new(current, roots, &retired_blocks)?;
    let committed = commit_transaction(device, current, plan, superblock_buffer)?;

    Ok(RootLeafUpdateResult {
        active: committed.active,
        retired_blocks,
        allocated: allocation.allocated,
    })
}

pub fn materialize_object_leaf_copy(
    current: &[u8],
    destination: &mut [u8],
    new_generation: u64,
    new_block_number: u64,
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let current_node = validate_object_leaf(current)?;
    validate_rewrite_target(current_node, new_generation, new_block_number)?;

    destination.fill(0);
    let start = TreeNodeHeader::entries_offset();
    let entries_bytes = current_node.entries_bytes as usize;
    let end = start
        .checked_add(entries_bytes)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    destination[start..end].copy_from_slice(&current[start..end]);

    let next = TreeNodeHeader::new(
        MetadataKind::ObjectTree,
        new_generation,
        new_block_number,
        0,
        current_node.item_count,
        current_node.entries_bytes,
    )?;
    next.seal(destination)?;
    validate_object_leaf(destination)?;
    Ok(next)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectChildRewrite {
    pub child_index: u32,
    pub child_block: u64,
    pub child_first_key: ObjectTreeKey,
}

impl ObjectChildRewrite {
    pub const fn new(child_index: u32, child_block: u64, child_first_key: ObjectTreeKey) -> Self {
        Self {
            child_index,
            child_block,
            child_first_key,
        }
    }
}

pub fn materialize_object_internal_after_child_copy(
    current: &[u8],
    destination: &mut [u8],
    new_generation: u64,
    new_block_number: u64,
    child_index: usize,
    new_child_block: u64,
    new_child_first_key: ObjectTreeKey,
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let child_index =
        u32::try_from(child_index).map_err(|_| PhoenixFsError::InvalidObjectRewriteTarget)?;
    let rewrite = [ObjectChildRewrite::new(
        child_index,
        new_child_block,
        new_child_first_key,
    )];
    materialize_object_internal_after_child_rewrites(
        current,
        destination,
        new_generation,
        new_block_number,
        &rewrite,
    )
}

pub fn materialize_object_internal_after_child_rewrites(
    current: &[u8],
    destination: &mut [u8],
    new_generation: u64,
    new_block_number: u64,
    rewrites: &[ObjectChildRewrite],
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let current_node = validate_object_internal(current)?;
    validate_rewrite_target(current_node, new_generation, new_block_number)?;
    if rewrites.is_empty() {
        return Err(PhoenixFsError::InvalidObjectMutationBatch);
    }

    let mut previous_index = None;
    for rewrite in rewrites {
        let child_index = rewrite.child_index as usize;
        if child_index >= current_node.item_count as usize
            || previous_index.is_some_and(|previous| previous >= rewrite.child_index)
        {
            return Err(PhoenixFsError::InvalidObjectRewriteTarget);
        }
        validate_object_child_block(rewrite.child_block)?;
        if rewrite.child_block == new_block_number {
            return Err(PhoenixFsError::InvalidObjectRewriteTarget);
        }
        rewrite.child_first_key.validate()?;
        previous_index = Some(rewrite.child_index);
    }

    destination.fill(0);
    let start = TreeNodeHeader::entries_offset();
    let entries_bytes = current_node.entries_bytes as usize;
    let end = start
        .checked_add(entries_bytes)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    destination[start..end].copy_from_slice(&current[start..end]);

    for rewrite in rewrites {
        let record_offset = start
            .checked_add(
                (rewrite.child_index as usize)
                    .checked_mul(OBJECT_INTERNAL_RECORD_SIZE)
                    .ok_or(PhoenixFsError::ArithmeticOverflow)?,
            )
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        ObjectInternalRecord::new(rewrite.child_first_key, rewrite.child_block)?
            .encode(&mut destination[record_offset..record_offset + OBJECT_INTERNAL_RECORD_SIZE])?;
    }

    let next = TreeNodeHeader::new(
        MetadataKind::ObjectTree,
        new_generation,
        new_block_number,
        current_node.level,
        current_node.item_count,
        current_node.entries_bytes,
    )?;
    next.seal(destination)?;
    validate_object_internal(destination)?;
    Ok(next)
}

fn object_internal_capacity() -> usize {
    (FILESYSTEM_BLOCK_SIZE - TreeNodeHeader::entries_offset()) / OBJECT_INTERNAL_RECORD_SIZE
}

fn object_internal_can_accept_split(block: &[u8]) -> Result<bool, PhoenixFsError> {
    let node = validate_object_internal(block)?;
    let next_count = (node.item_count as usize)
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    Ok(next_count <= object_internal_capacity())
}

fn materialize_object_internal_child_split_side(
    current: &[u8],
    destination: &mut [u8],
    new_generation: u64,
    new_block_number: u64,
    child_index: usize,
    left_block: u64,
    left_first_key: ObjectTreeKey,
    right_block: u64,
    right_first_key: ObjectTreeKey,
    range_start: usize,
    range_end: usize,
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let current_node = validate_object_internal(current)?;
    validate_rewrite_target(current_node, new_generation, new_block_number)?;

    if child_index >= current_node.item_count as usize
        || left_first_key >= right_first_key
        || range_start >= range_end
        || left_block == right_block
        || left_block == new_block_number
        || right_block == new_block_number
    {
        return Err(PhoenixFsError::InvalidObjectRewriteTarget);
    }
    validate_object_child_block(left_block)?;
    validate_object_child_block(right_block)?;
    left_first_key.validate()?;
    right_first_key.validate()?;

    let total_records = current_node.item_count as usize + 1;
    if range_end > total_records {
        return Err(PhoenixFsError::InvalidObjectRewriteTarget);
    }

    destination.fill(0);
    let start = TreeNodeHeader::entries_offset();
    let mut output_index = 0_usize;
    let mut merged_index = 0_usize;

    for input_index in 0..current_node.item_count as usize {
        if input_index == child_index {
            for record in [
                ObjectInternalRecord::new(left_first_key, left_block)?,
                ObjectInternalRecord::new(right_first_key, right_block)?,
            ] {
                if (range_start..range_end).contains(&merged_index) {
                    record.encode(
                        &mut destination[start + output_index * OBJECT_INTERNAL_RECORD_SIZE
                            ..start + (output_index + 1) * OBJECT_INTERNAL_RECORD_SIZE],
                    )?;
                    output_index += 1;
                }
                merged_index += 1;
            }
            continue;
        }

        let record = object_internal_record_at(current, input_index)?;
        if (range_start..range_end).contains(&merged_index) {
            record.encode(
                &mut destination[start + output_index * OBJECT_INTERNAL_RECORD_SIZE
                    ..start + (output_index + 1) * OBJECT_INTERNAL_RECORD_SIZE],
            )?;
            output_index += 1;
        }
        merged_index += 1;
    }

    let item_count =
        u32::try_from(range_end - range_start).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    let entries_bytes = output_index
        .checked_mul(OBJECT_INTERNAL_RECORD_SIZE)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let next = TreeNodeHeader::new(
        MetadataKind::ObjectTree,
        new_generation,
        new_block_number,
        current_node.level,
        item_count,
        entries_bytes as u32,
    )?;
    next.seal(destination)?;
    validate_object_internal(destination)?;
    Ok(next)
}

pub fn materialize_object_internal_after_child_split(
    current: &[u8],
    destination: &mut [u8],
    new_generation: u64,
    new_block_number: u64,
    child_index: usize,
    left_block: u64,
    left_first_key: ObjectTreeKey,
    right_block: u64,
    right_first_key: ObjectTreeKey,
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let current_node = validate_object_internal(current)?;
    validate_rewrite_target(current_node, new_generation, new_block_number)?;

    if child_index >= current_node.item_count as usize
        || left_first_key >= right_first_key
        || left_block == right_block
        || left_block == new_block_number
        || right_block == new_block_number
    {
        return Err(PhoenixFsError::InvalidObjectRewriteTarget);
    }
    validate_object_child_block(left_block)?;
    validate_object_child_block(right_block)?;
    left_first_key.validate()?;
    right_first_key.validate()?;

    let next_item_count = current_node
        .item_count
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let next_entries_bytes = (next_item_count as usize)
        .checked_mul(OBJECT_INTERNAL_RECORD_SIZE)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let start = TreeNodeHeader::entries_offset();
    let end = start
        .checked_add(next_entries_bytes)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    if end > destination.len() {
        return Err(PhoenixFsError::BufferSize);
    }

    destination.fill(0);
    let mut output_index = 0_usize;
    for input_index in 0..current_node.item_count as usize {
        if input_index == child_index {
            ObjectInternalRecord::new(left_first_key, left_block)?.encode(
                &mut destination[start + output_index * OBJECT_INTERNAL_RECORD_SIZE
                    ..start + (output_index + 1) * OBJECT_INTERNAL_RECORD_SIZE],
            )?;
            output_index += 1;
            ObjectInternalRecord::new(right_first_key, right_block)?.encode(
                &mut destination[start + output_index * OBJECT_INTERNAL_RECORD_SIZE
                    ..start + (output_index + 1) * OBJECT_INTERNAL_RECORD_SIZE],
            )?;
            output_index += 1;
            continue;
        }

        let record = object_internal_record_at(current, input_index)?;
        record.encode(
            &mut destination[start + output_index * OBJECT_INTERNAL_RECORD_SIZE
                ..start + (output_index + 1) * OBJECT_INTERNAL_RECORD_SIZE],
        )?;
        output_index += 1;
    }

    let next = TreeNodeHeader::new(
        MetadataKind::ObjectTree,
        new_generation,
        new_block_number,
        current_node.level,
        next_item_count,
        next_entries_bytes as u32,
    )?;
    next.seal(destination)?;
    validate_object_internal(destination)?;
    Ok(next)
}

pub fn materialize_object_internal_without_child(
    current: &[u8],
    destination: &mut [u8],
    new_generation: u64,
    new_block_number: u64,
    child_index: usize,
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let current_node = validate_object_internal(current)?;
    validate_rewrite_target(current_node, new_generation, new_block_number)?;

    if child_index >= current_node.item_count as usize || current_node.item_count <= 1 {
        return Err(PhoenixFsError::ObjectLeafWouldBecomeEmpty);
    }

    destination.fill(0);
    let start = TreeNodeHeader::entries_offset();
    let mut output_index = 0_usize;
    for input_index in 0..current_node.item_count as usize {
        if input_index == child_index {
            continue;
        }

        let record = object_internal_record_at(current, input_index)?;
        record.encode(
            &mut destination[start + output_index * OBJECT_INTERNAL_RECORD_SIZE
                ..start + (output_index + 1) * OBJECT_INTERNAL_RECORD_SIZE],
        )?;
        output_index += 1;
    }

    let item_count = current_node.item_count - 1;
    let entries_bytes = output_index
        .checked_mul(OBJECT_INTERNAL_RECORD_SIZE)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let next = TreeNodeHeader::new(
        MetadataKind::ObjectTree,
        new_generation,
        new_block_number,
        current_node.level,
        item_count,
        entries_bytes as u32,
    )?;
    next.seal(destination)?;
    validate_object_internal(destination)?;
    Ok(next)
}

fn validate_rewrite_target(
    current: TreeNodeHeader,
    new_generation: u64,
    new_block_number: u64,
) -> Result<(), PhoenixFsError> {
    if new_generation <= current.metadata.generation {
        return Err(PhoenixFsError::GenerationSequence);
    }
    if new_block_number < SUPERBLOCK_COPY_COUNT || new_block_number == current.metadata.block_number
    {
        return Err(PhoenixFsError::InvalidObjectRewriteTarget);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransactionCommitPlan<'a> {
    pub next: Superblock,
    pub retired_blocks: &'a [u64],
}

impl<'a> TransactionCommitPlan<'a> {
    pub fn new(
        current: ActiveSuperblock,
        roots: TransactionRoots,
        retired_blocks: &'a [u64],
    ) -> Result<Self, PhoenixFsError> {
        roots.validate(current.superblock.total_blocks)?;

        let next_generation = current
            .superblock
            .generation
            .checked_add(1)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        let next = Superblock {
            generation: next_generation,
            total_blocks: current.superblock.total_blocks,
            volume_id: current.superblock.volume_id,
            incompatible_features: current.superblock.incompatible_features,
            roots,
        };
        next.validate()?;
        validate_retired_blocks(next, retired_blocks)?;

        Ok(Self {
            next,
            retired_blocks,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommittedTransaction<'a> {
    pub active: ActiveSuperblock,
    retired_blocks: &'a [u64],
}

impl<'a> CommittedTransaction<'a> {
    pub const fn retired_blocks(&self) -> &'a [u64] {
        self.retired_blocks
    }
}

pub fn commit_transaction<'a, D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    plan: TransactionCommitPlan<'a>,
    buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<CommittedTransaction<'a>, PhoenixFsError> {
    validate_retired_blocks(plan.next, plan.retired_blocks)?;
    device.flush()?;
    let active = commit_next_superblock(device, current, plan.next, buffer)?;

    Ok(CommittedTransaction {
        active,
        retired_blocks: plan.retired_blocks,
    })
}

fn validate_retired_blocks(next: Superblock, retired_blocks: &[u64]) -> Result<(), PhoenixFsError> {
    for (index, block) in retired_blocks.iter().copied().enumerate() {
        if block < SUPERBLOCK_COPY_COUNT || block >= next.total_blocks {
            return Err(PhoenixFsError::InvalidRetiredBlock(block));
        }
        if block == next.roots.object_tree || block == next.roots.free_space_tree {
            return Err(PhoenixFsError::RetiredBlockStillReferenced(block));
        }
        if retired_blocks[..index].contains(&block) {
            return Err(PhoenixFsError::DuplicateRetiredBlock(block));
        }
    }
    Ok(())
}

fn align_up(value: usize, alignment: usize) -> Result<usize, PhoenixFsError> {
    let mask = alignment
        .checked_sub(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let adjusted = value
        .checked_add(mask)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    Ok(adjusted & !mask)
}

pub fn format_volume<D: BlockDevice>(
    device: &mut D,
    volume_id: [u8; 16],
    timestamp_ns: u64,
    buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<ActiveSuperblock, PhoenixFsError> {
    let total_blocks = filesystem_block_count(device)?;
    if total_blocks < MINIMUM_FILESYSTEM_BLOCKS {
        return Err(PhoenixFsError::InvalidVolumeGeometry);
    }

    let roots = TransactionRoots::new(INITIAL_OBJECT_TREE_BLOCK, INITIAL_FREE_SPACE_TREE_BLOCK);
    let superblock = Superblock::new(1, total_blocks, volume_id, roots)?;

    write_initial_object_tree(device, timestamp_ns, buffer)?;
    write_initial_free_space_tree(device, total_blocks, buffer)?;
    device.flush()?;

    superblock.encode(buffer)?;
    write_filesystem_block(device, SuperblockSlot::First.filesystem_block(), buffer)?;
    device.flush()?;
    write_filesystem_block(device, SuperblockSlot::Second.filesystem_block(), buffer)?;
    device.flush()?;

    Ok(ActiveSuperblock {
        superblock,
        slot: SuperblockSlot::First,
    })
}

fn write_initial_object_tree<D: BlockDevice>(
    device: &mut D,
    timestamp_ns: u64,
    buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<(), PhoenixFsError> {
    buffer.fill(0);

    let metadata = ObjectMetadataValue::new(
        ObjectType::Directory,
        0,
        timestamp_ns,
        timestamp_ns,
        timestamp_ns,
    );
    let mut value = [0_u8; OBJECT_METADATA_VALUE_SIZE];
    metadata.encode(&mut value)?;

    let key = ObjectTreeKey::new(ROOT_OBJECT_ID, ObjectRecordKind::Metadata, 0);
    let record = ObjectLeafRecordHeader::new(key, OBJECT_METADATA_VALUE_SIZE as u32)?;
    let start = TreeNodeHeader::entries_offset();
    let encoded_size = record.encode_with_value(&mut buffer[start..], &value)?;

    let node = TreeNodeHeader::new(
        MetadataKind::ObjectTree,
        1,
        INITIAL_OBJECT_TREE_BLOCK,
        0,
        1,
        encoded_size as u32,
    )?;
    node.seal(buffer)?;
    write_filesystem_block(device, INITIAL_OBJECT_TREE_BLOCK, buffer)
}

fn write_initial_free_space_tree<D: BlockDevice>(
    device: &mut D,
    total_blocks: u64,
    buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<(), PhoenixFsError> {
    buffer.fill(0);

    let first_free_block = INITIAL_FREE_SPACE_TREE_BLOCK
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let free_blocks = total_blocks
        .checked_sub(first_free_block)
        .ok_or(PhoenixFsError::InvalidVolumeGeometry)?;
    if free_blocks == 0 {
        return Err(PhoenixFsError::InvalidVolumeGeometry);
    }

    let extent = FreeSpaceExtent::new(first_free_block, free_blocks);
    write_free_space_extent(buffer, 0, extent)?;

    let node = TreeNodeHeader::new(
        MetadataKind::FreeSpaceTree,
        1,
        INITIAL_FREE_SPACE_TREE_BLOCK,
        0,
        1,
        FREE_SPACE_RECORD_SIZE as u32,
    )?;
    node.seal(buffer)?;
    write_filesystem_block(device, INITIAL_FREE_SPACE_TREE_BLOCK, buffer)
}

pub struct PhoenixVfs<D: BlockDevice> {
    device: RefCell<D>,
    active: ActiveSuperblock,
    node_buffer: RefCell<[u8; FILESYSTEM_BLOCK_SIZE]>,
    data_buffer: RefCell<[u8; FILESYSTEM_BLOCK_SIZE]>,
    metadata_buffer: RefCell<[u8; OBJECT_METADATA_VALUE_SIZE]>,
    object_copy_buffer: [u8; FILESYSTEM_BLOCK_SIZE],
    current_free_space_buffer: [u8; FILESYSTEM_BLOCK_SIZE],
    next_free_space_buffer: [u8; FILESYSTEM_BLOCK_SIZE],
    superblock_buffer: [u8; FILESYSTEM_BLOCK_SIZE],
}

impl<D: BlockDevice> PhoenixVfs<D> {
    pub fn mount(
        mut device: D,
        first_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
        second_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    ) -> Result<Self, PhoenixFsError> {
        let active = read_active_superblock(&mut device, first_buffer, second_buffer)?;
        Self::new(device, active)
    }

    pub fn format_new(
        mut device: D,
        volume_id: [u8; 16],
        timestamp_ns: u64,
        buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    ) -> Result<Self, PhoenixFsError> {
        let active = format_volume(&mut device, volume_id, timestamp_ns, buffer)?;
        Self::new(device, active)
    }

    pub fn new(device: D, active: ActiveSuperblock) -> Result<Self, PhoenixFsError> {
        let total_blocks = filesystem_block_count(&device)?;
        if active.superblock.total_blocks != total_blocks {
            return Err(PhoenixFsError::InvalidVolumeGeometry);
        }

        Ok(Self {
            device: RefCell::new(device),
            active,
            node_buffer: RefCell::new([0; FILESYSTEM_BLOCK_SIZE]),
            data_buffer: RefCell::new([0; FILESYSTEM_BLOCK_SIZE]),
            metadata_buffer: RefCell::new([0; OBJECT_METADATA_VALUE_SIZE]),
            object_copy_buffer: [0; FILESYSTEM_BLOCK_SIZE],
            current_free_space_buffer: [0; FILESYSTEM_BLOCK_SIZE],
            next_free_space_buffer: [0; FILESYSTEM_BLOCK_SIZE],
            superblock_buffer: [0; FILESYSTEM_BLOCK_SIZE],
        })
    }

    pub const fn active_superblock(&self) -> ActiveSuperblock {
        self.active
    }

    pub fn into_device(self) -> D {
        self.device.into_inner()
    }

    fn object_metadata(&self, object_id: u64) -> Result<ObjectMetadataValue, VfsError> {
        let mut device = self
            .device
            .try_borrow_mut()
            .map_err(|_| VfsError::Storage)?;
        let mut node_buffer = self
            .node_buffer
            .try_borrow_mut()
            .map_err(|_| VfsError::Storage)?;
        let mut metadata_buffer = self
            .metadata_buffer
            .try_borrow_mut()
            .map_err(|_| VfsError::Storage)?;

        read_object_metadata(
            &mut *device,
            self.active.superblock.roots.object_tree,
            object_id,
            self.active.superblock.generation,
            &mut *node_buffer,
            &mut *metadata_buffer,
        )
        .map_err(map_phoenix_fs_to_vfs)
    }
}

fn next_vfs_identifiers<D: BlockDevice>(
    device: &mut D,
    active: ActiveSuperblock,
    parent_object_id: u64,
    node_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<(u64, u64), PhoenixFsError> {
    let mut maximum_object_id = ROOT_OBJECT_ID;
    let mut maximum_entry_id = 0_u64;

    scan_object_tree_records(
        device,
        active.superblock.roots.object_tree,
        active.superblock.generation,
        node_buffer,
        |key, _| {
            maximum_object_id = core::cmp::max(maximum_object_id, key.object_id);
            if key.object_id == parent_object_id && key.kind == ObjectRecordKind::DirectoryEntry {
                maximum_entry_id = core::cmp::max(maximum_entry_id, key.offset);
            }
            Ok(false)
        },
    )?;

    let object_id = maximum_object_id
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let entry_id = maximum_entry_id
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    Ok((object_id, entry_id))
}

fn materialize_object_leaf_with_replacement_and_insertion(
    current: &[u8],
    destination: &mut [u8],
    replacement_key: ObjectTreeKey,
    replacement_value: &[u8],
    insertion_key: ObjectTreeKey,
    insertion_value: &[u8],
    new_generation: u64,
    new_block_number: u64,
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let current_node = validate_object_leaf(current)?;
    validate_rewrite_target(current_node, new_generation, new_block_number)?;
    replacement_key.validate()?;
    insertion_key.validate()?;
    if replacement_key == insertion_key {
        return Err(PhoenixFsError::InvalidObjectMutationBatch);
    }

    let replacement_bytes = u32::try_from(replacement_value.len())
        .map_err(|_| PhoenixFsError::InvalidObjectRecordSize)?;
    let insertion_bytes = u32::try_from(insertion_value.len())
        .map_err(|_| PhoenixFsError::InvalidObjectRecordSize)?;
    let insertion_record = ObjectLeafRecordHeader::new(insertion_key, insertion_bytes)?;

    let entries_start = TreeNodeHeader::entries_offset();
    let entries_end = entries_start
        .checked_add(current_node.entries_bytes as usize)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    destination.fill(0);

    let mut source_cursor = entries_start;
    let mut destination_cursor = entries_start;
    let mut replaced = false;
    let mut inserted = false;

    for _ in 0..current_node.item_count {
        let (record, old_value, source_size) =
            ObjectLeafRecordHeader::decode_with_value(&current[source_cursor..entries_end])?;

        if !inserted && insertion_key < record.key {
            destination_cursor = encode_leaf_record_at(
                destination,
                destination_cursor,
                insertion_record,
                insertion_value,
            )?;
            inserted = true;
        }
        if insertion_key == record.key {
            return Err(PhoenixFsError::ObjectRecordAlreadyExists);
        }

        let (next_record, value) = if record.key == replacement_key {
            replaced = true;
            (
                ObjectLeafRecordHeader::new(record.key, replacement_bytes)?,
                replacement_value,
            )
        } else {
            (record, old_value)
        };
        destination_cursor =
            encode_leaf_record_at(destination, destination_cursor, next_record, value)?;
        source_cursor = source_cursor
            .checked_add(source_size)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    }

    if !replaced {
        return Err(PhoenixFsError::ObjectRecordNotFound);
    }
    if !inserted {
        destination_cursor = encode_leaf_record_at(
            destination,
            destination_cursor,
            insertion_record,
            insertion_value,
        )?;
    }

    let item_count = current_node
        .item_count
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    seal_object_leaf_rewrite(
        destination,
        current_node,
        new_generation,
        new_block_number,
        item_count,
        destination_cursor,
    )
}

fn materialize_object_leaf_with_replacement_and_deletion(
    current: &[u8],
    destination: &mut [u8],
    replacement_key: ObjectTreeKey,
    replacement_value: &[u8],
    deletion_key: ObjectTreeKey,
    new_generation: u64,
    new_block_number: u64,
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let current_node = validate_object_leaf(current)?;
    validate_rewrite_target(current_node, new_generation, new_block_number)?;
    if replacement_key == deletion_key || current_node.item_count <= 1 {
        return Err(PhoenixFsError::InvalidObjectMutationBatch);
    }

    let entries_start = TreeNodeHeader::entries_offset();
    let entries_end = entries_start
        .checked_add(current_node.entries_bytes as usize)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    destination.fill(0);
    let mut source_cursor = entries_start;
    let mut destination_cursor = entries_start;
    let mut replaced = false;
    let mut deleted = false;

    for _ in 0..current_node.item_count {
        let (record, old_value, source_size) =
            ObjectLeafRecordHeader::decode_with_value(&current[source_cursor..entries_end])?;
        source_cursor = source_cursor
            .checked_add(source_size)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        if record.key == deletion_key {
            deleted = true;
            continue;
        }
        let value = if record.key == replacement_key {
            replaced = true;
            replacement_value
        } else {
            old_value
        };
        let value_bytes =
            u32::try_from(value.len()).map_err(|_| PhoenixFsError::InvalidObjectRecordSize)?;
        destination_cursor = encode_leaf_record_at(
            destination,
            destination_cursor,
            ObjectLeafRecordHeader::new(record.key, value_bytes)?,
            value,
        )?;
    }

    if !replaced || !deleted {
        return Err(PhoenixFsError::ObjectRecordNotFound);
    }
    seal_object_leaf_rewrite(
        destination,
        current_node,
        new_generation,
        new_block_number,
        current_node.item_count - 1,
        destination_cursor,
    )
}

fn materialize_object_leaf_with_two_replacements(
    current: &[u8],
    destination: &mut [u8],
    first_key: ObjectTreeKey,
    first_value: &[u8],
    second_key: ObjectTreeKey,
    second_value: &[u8],
    new_generation: u64,
    new_block_number: u64,
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let current_node = validate_object_leaf(current)?;
    validate_rewrite_target(current_node, new_generation, new_block_number)?;
    first_key.validate()?;
    second_key.validate()?;
    if first_key == second_key {
        return Err(PhoenixFsError::InvalidObjectMutationBatch);
    }

    let entries_start = TreeNodeHeader::entries_offset();
    let entries_end = entries_start
        .checked_add(current_node.entries_bytes as usize)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    destination.fill(0);

    let mut source_cursor = entries_start;
    let mut destination_cursor = entries_start;
    let mut first_replaced = false;
    let mut second_replaced = false;

    for _ in 0..current_node.item_count {
        let (record, old_value, source_size) =
            ObjectLeafRecordHeader::decode_with_value(&current[source_cursor..entries_end])?;
        let value = if record.key == first_key {
            first_replaced = true;
            first_value
        } else if record.key == second_key {
            second_replaced = true;
            second_value
        } else {
            old_value
        };
        let value_bytes =
            u32::try_from(value.len()).map_err(|_| PhoenixFsError::InvalidObjectRecordSize)?;
        let next_record = ObjectLeafRecordHeader::new(record.key, value_bytes)?;
        destination_cursor =
            encode_leaf_record_at(destination, destination_cursor, next_record, value)?;
        source_cursor = source_cursor
            .checked_add(source_size)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    }

    if !first_replaced || !second_replaced {
        return Err(PhoenixFsError::ObjectRecordNotFound);
    }

    seal_object_leaf_rewrite(
        destination,
        current_node,
        new_generation,
        new_block_number,
        current_node.item_count,
        destination_cursor,
    )
}

fn read_single_file_extent<D: BlockDevice>(
    device: &mut D,
    active: ActiveSuperblock,
    object_id: u64,
    node_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<(ObjectTreeKey, ExtentValue), PhoenixFsError> {
    let total_blocks = active.superblock.total_blocks;
    let mut found = None;

    scan_object_tree_records(
        device,
        active.superblock.roots.object_tree,
        active.superblock.generation,
        node_buffer,
        |key, value| {
            if key.object_id != object_id || key.kind != ObjectRecordKind::Extent {
                return Ok(false);
            }
            if found.is_some() {
                return Err(PhoenixFsError::InvalidObjectMutationBatch);
            }

            let extent = ExtentValue::decode(value)?;
            extent.validate_for_key(key, total_blocks)?;
            found = Some((key, extent));
            Ok(false)
        },
    )?;

    found.ok_or(PhoenixFsError::ObjectRecordNotFound)
}

fn append_retired_extent_blocks(
    retired_blocks: &mut [u64; MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS],
    retired_count: &mut usize,
    extent: ExtentValue,
) -> Result<(), PhoenixFsError> {
    for delta in 0..extent.block_count {
        let block = extent
            .physical_start_block
            .checked_add(delta)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        push_unique_retired(retired_blocks, retired_count, block)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn commit_rewrite_single_extent_file<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    object_id: u64,
    data: &[u8],
    object_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    object_copy_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    data_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    current_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    next_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    superblock_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<DeepRecordUpdateResult, PhoenixFsError> {
    if object_id == 0 || data.is_empty() {
        return Err(PhoenixFsError::InvalidObjectMutationBatch);
    }

    let mut metadata_bytes = [0_u8; OBJECT_METADATA_VALUE_SIZE];
    let metadata = read_object_metadata(
        device,
        current.superblock.roots.object_tree,
        object_id,
        current.superblock.generation,
        object_buffer,
        &mut metadata_bytes,
    )?;
    let data_len = u64::try_from(data.len()).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    if metadata.object_type != ObjectType::File
        || metadata.size_bytes == 0
        || metadata.size_bytes != data_len
    {
        return Err(PhoenixFsError::InvalidObjectMutationBatch);
    }

    let (extent_key, old_extent) =
        read_single_file_extent(device, current, object_id, object_buffer)?;
    if extent_key.offset != 0 || old_extent.data_bytes != metadata.size_bytes {
        return Err(PhoenixFsError::InvalidObjectMutationBatch);
    }

    let metadata_key = ObjectTreeKey::new(object_id, ObjectRecordKind::Metadata, 0);
    let metadata_path = find_object_tree_path(
        device,
        current.superblock.roots.object_tree,
        metadata_key,
        current.superblock.generation,
        object_buffer,
    )?;
    let extent_path = find_object_tree_path(
        device,
        current.superblock.roots.object_tree,
        extent_key,
        current.superblock.generation,
        object_buffer,
    )?;
    if metadata_path != extent_path {
        return Err(PhoenixFsError::ObjectMutationBatchSpansLeaves);
    }

    let metadata_retired = metadata_path
        .node_count()
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let data_retired =
        usize::try_from(old_extent.block_count).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    if metadata_retired
        .checked_add(data_retired)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?
        > MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS
    {
        return Err(PhoenixFsError::BufferSize);
    }

    let total_blocks = current.superblock.total_blocks;
    read_filesystem_block(
        device,
        current.superblock.roots.free_space_tree,
        current_free_space,
    )?;
    let free_node = validate_free_space_leaf(current_free_space, total_blocks)?;
    if free_node.metadata.block_number != current.superblock.roots.free_space_tree {
        return Err(PhoenixFsError::InvalidTransactionRoots);
    }
    if free_node.metadata.generation > current.superblock.generation {
        return Err(PhoenixFsError::ObjectNodeGenerationAhead);
    }

    let data_blocks = old_extent.block_count;
    let metadata_blocks = u64::try_from(metadata_path.node_count() + 1)
        .map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    let requested_blocks = data_blocks
        .checked_add(metadata_blocks)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let allocation = plan_cow_allocation(current_free_space, total_blocks, requested_blocks)?;
    let generation = current
        .superblock
        .generation
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    let data_start_block = allocation.allocated.start_block;
    let written_blocks = write_data_extent(device, data_start_block, data, data_buffer)?;
    if written_blocks != data_blocks {
        return Err(PhoenixFsError::InvalidExtent);
    }

    let next_metadata = ObjectMetadataValue::new(
        ObjectType::File,
        metadata.size_bytes,
        metadata.created_ns,
        metadata.modified_ns,
        metadata.changed_ns,
    );
    next_metadata.encode(&mut metadata_bytes)?;
    validate_object_record_value(metadata_key, &metadata_bytes, total_blocks)?;

    let next_extent = ExtentValue::new(data_start_block, data_blocks, data_len);
    let mut extent_bytes = [0_u8; EXTENT_VALUE_SIZE];
    next_extent.encode(&mut extent_bytes)?;
    validate_object_record_value(extent_key, &extent_bytes, total_blocks)?;

    read_filesystem_block(device, metadata_path.leaf_block, object_buffer)?;
    let object_block = data_start_block
        .checked_add(data_blocks)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let leaf = materialize_object_leaf_with_two_replacements(
        object_buffer,
        object_copy_buffer,
        metadata_key,
        &metadata_bytes,
        extent_key,
        &extent_bytes,
        generation,
        object_block,
    )?;
    write_filesystem_block(device, object_block, object_copy_buffer)?;

    let mut new_child_block = object_block;
    let mut new_child_first_key = first_object_node_key(object_copy_buffer, leaf)?;
    let mut next_block = object_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    for parent_position in (0..metadata_path.parent_count).rev() {
        let step = metadata_path.parents[parent_position];
        read_filesystem_block(device, step.block_number, object_buffer)?;
        let parent = validate_object_internal(object_buffer)?;
        if parent.metadata.generation > current.superblock.generation {
            return Err(PhoenixFsError::ObjectNodeGenerationAhead);
        }
        validate_path_pointer(object_buffer, step)?;

        materialize_object_internal_after_child_copy(
            object_buffer,
            object_copy_buffer,
            generation,
            next_block,
            step.child_index as usize,
            new_child_block,
            new_child_first_key,
        )?;
        write_filesystem_block(device, next_block, object_copy_buffer)?;
        let copied_parent = TreeNodeHeader::decode(object_copy_buffer)?;
        new_child_first_key = first_object_node_key(object_copy_buffer, copied_parent)?;
        new_child_block = next_block;
        next_block = next_block
            .checked_add(1)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    }

    let free_space_block = next_block;
    if free_space_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?
        != allocation.allocated.end_block_exclusive()?
    {
        return Err(PhoenixFsError::InvalidTransactionRoots);
    }
    materialize_free_space_leaf_after_allocation(
        current_free_space,
        next_free_space,
        total_blocks,
        generation,
        free_space_block,
        allocation,
    )?;
    write_filesystem_block(device, free_space_block, next_free_space)?;

    let mut retired_blocks = [0_u64; MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS];
    let mut retired_count = 0_usize;
    push_unique_retired(
        &mut retired_blocks,
        &mut retired_count,
        metadata_path.leaf_block,
    )?;
    for index in 0..metadata_path.parent_count {
        push_unique_retired(
            &mut retired_blocks,
            &mut retired_count,
            metadata_path.parents[index].block_number,
        )?;
    }
    push_unique_retired(
        &mut retired_blocks,
        &mut retired_count,
        current.superblock.roots.free_space_tree,
    )?;
    append_retired_extent_blocks(&mut retired_blocks, &mut retired_count, old_extent)?;
    sort_u64_prefix(&mut retired_blocks, retired_count);

    let roots = TransactionRoots::new(new_child_block, free_space_block);
    let plan = TransactionCommitPlan::new(current, roots, &retired_blocks[..retired_count])?;
    let committed = commit_transaction(device, current, plan, superblock_buffer)?;

    Ok(DeepRecordUpdateResult {
        active: committed.active,
        retired_blocks,
        retired_count,
        allocated: allocation.allocated,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn commit_truncate_single_extent_file_to_zero<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    object_id: u64,
    object_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    object_copy_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    current_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    next_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    superblock_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<DeepRecordUpdateResult, PhoenixFsError> {
    if object_id == 0 {
        return Err(PhoenixFsError::InvalidObjectId);
    }

    let mut metadata_bytes = [0_u8; OBJECT_METADATA_VALUE_SIZE];
    let metadata = read_object_metadata(
        device,
        current.superblock.roots.object_tree,
        object_id,
        current.superblock.generation,
        object_buffer,
        &mut metadata_bytes,
    )?;
    if metadata.object_type != ObjectType::File || metadata.size_bytes == 0 {
        return Err(PhoenixFsError::InvalidObjectMutationBatch);
    }

    let (extent_key, old_extent) =
        read_single_file_extent(device, current, object_id, object_buffer)?;
    if extent_key.offset != 0 || old_extent.data_bytes != metadata.size_bytes {
        return Err(PhoenixFsError::InvalidObjectMutationBatch);
    }

    let metadata_key = ObjectTreeKey::new(object_id, ObjectRecordKind::Metadata, 0);
    let metadata_path = find_object_tree_path(
        device,
        current.superblock.roots.object_tree,
        metadata_key,
        current.superblock.generation,
        object_buffer,
    )?;
    let extent_path = find_object_tree_path(
        device,
        current.superblock.roots.object_tree,
        extent_key,
        current.superblock.generation,
        object_buffer,
    )?;
    if metadata_path != extent_path {
        return Err(PhoenixFsError::ObjectMutationBatchSpansLeaves);
    }

    let metadata_retired = metadata_path.node_count() + 1;
    let data_retired =
        usize::try_from(old_extent.block_count).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    if metadata_retired
        .checked_add(data_retired)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?
        > MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS
    {
        return Err(PhoenixFsError::BufferSize);
    }

    let total_blocks = current.superblock.total_blocks;
    read_filesystem_block(
        device,
        current.superblock.roots.free_space_tree,
        current_free_space,
    )?;
    let free_node = validate_free_space_leaf(current_free_space, total_blocks)?;
    if free_node.metadata.block_number != current.superblock.roots.free_space_tree {
        return Err(PhoenixFsError::InvalidTransactionRoots);
    }

    let requested_blocks = u64::try_from(metadata_path.node_count() + 1)
        .map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    let allocation = plan_cow_allocation(current_free_space, total_blocks, requested_blocks)?;
    let generation = current
        .superblock
        .generation
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    ObjectMetadataValue::new(
        ObjectType::File,
        0,
        metadata.created_ns,
        metadata.modified_ns,
        metadata.changed_ns,
    )
    .encode(&mut metadata_bytes)?;
    validate_object_record_value(metadata_key, &metadata_bytes, total_blocks)?;

    read_filesystem_block(device, metadata_path.leaf_block, object_buffer)?;
    let object_block = allocation.allocated.start_block;
    let leaf = materialize_object_leaf_with_replacement_and_deletion(
        object_buffer,
        object_copy_buffer,
        metadata_key,
        &metadata_bytes,
        extent_key,
        generation,
        object_block,
    )?;
    write_filesystem_block(device, object_block, object_copy_buffer)?;

    let mut new_child_block = object_block;
    let mut new_child_first_key = first_object_node_key(object_copy_buffer, leaf)?;
    let mut next_block = object_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    for parent_position in (0..metadata_path.parent_count).rev() {
        let step = metadata_path.parents[parent_position];
        read_filesystem_block(device, step.block_number, object_buffer)?;
        validate_path_pointer(object_buffer, step)?;
        materialize_object_internal_after_child_copy(
            object_buffer,
            object_copy_buffer,
            generation,
            next_block,
            step.child_index as usize,
            new_child_block,
            new_child_first_key,
        )?;
        write_filesystem_block(device, next_block, object_copy_buffer)?;
        let copied_parent = TreeNodeHeader::decode(object_copy_buffer)?;
        new_child_first_key = first_object_node_key(object_copy_buffer, copied_parent)?;
        new_child_block = next_block;
        next_block = next_block
            .checked_add(1)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    }

    let free_space_block = next_block;
    if free_space_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?
        != allocation.allocated.end_block_exclusive()?
    {
        return Err(PhoenixFsError::InvalidTransactionRoots);
    }
    materialize_free_space_leaf_after_allocation(
        current_free_space,
        next_free_space,
        total_blocks,
        generation,
        free_space_block,
        allocation,
    )?;
    write_filesystem_block(device, free_space_block, next_free_space)?;

    let mut retired_blocks = [0_u64; MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS];
    let mut retired_count = 0_usize;
    push_unique_retired(
        &mut retired_blocks,
        &mut retired_count,
        metadata_path.leaf_block,
    )?;
    for index in 0..metadata_path.parent_count {
        push_unique_retired(
            &mut retired_blocks,
            &mut retired_count,
            metadata_path.parents[index].block_number,
        )?;
    }
    push_unique_retired(
        &mut retired_blocks,
        &mut retired_count,
        current.superblock.roots.free_space_tree,
    )?;
    append_retired_extent_blocks(&mut retired_blocks, &mut retired_count, old_extent)?;
    sort_u64_prefix(&mut retired_blocks, retired_count);

    let roots = TransactionRoots::new(new_child_block, free_space_block);
    let plan = TransactionCommitPlan::new(current, roots, &retired_blocks[..retired_count])?;
    let committed = commit_transaction(device, current, plan, superblock_buffer)?;
    Ok(DeepRecordUpdateResult {
        active: committed.active,
        retired_blocks,
        retired_count,
        allocated: allocation.allocated,
    })
}

fn write_data_extent<D: BlockDevice>(
    device: &mut D,
    start_block: u64,
    data: &[u8],
    block_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<u64, PhoenixFsError> {
    if data.is_empty() {
        return Err(PhoenixFsError::InvalidExtent);
    }

    let block_count = data
        .len()
        .checked_add(FILESYSTEM_BLOCK_SIZE - 1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?
        / FILESYSTEM_BLOCK_SIZE;
    let block_count = u64::try_from(block_count).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;

    for block_index in 0..block_count {
        let byte_start = usize::try_from(block_index)
            .map_err(|_| PhoenixFsError::ArithmeticOverflow)?
            .checked_mul(FILESYSTEM_BLOCK_SIZE)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        let byte_end = core::cmp::min(
            byte_start
                .checked_add(FILESYSTEM_BLOCK_SIZE)
                .ok_or(PhoenixFsError::ArithmeticOverflow)?,
            data.len(),
        );
        block_buffer.fill(0);
        block_buffer[..byte_end - byte_start].copy_from_slice(&data[byte_start..byte_end]);
        let block_number = start_block
            .checked_add(block_index)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
        write_filesystem_block(device, block_number, block_buffer)?;
    }

    Ok(block_count)
}

#[allow(clippy::too_many_arguments)]
pub fn commit_write_empty_file<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    object_id: u64,
    data: &[u8],
    object_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    object_copy_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    data_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    current_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    next_free_space: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    superblock_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<DeepRecordUpdateResult, PhoenixFsError> {
    if object_id == 0 || data.is_empty() {
        return Err(PhoenixFsError::InvalidObjectMutationBatch);
    }

    let mut metadata_bytes = [0_u8; OBJECT_METADATA_VALUE_SIZE];
    let metadata = read_object_metadata(
        device,
        current.superblock.roots.object_tree,
        object_id,
        current.superblock.generation,
        object_buffer,
        &mut metadata_bytes,
    )?;
    if metadata.object_type != ObjectType::File || metadata.size_bytes != 0 {
        return Err(PhoenixFsError::InvalidObjectMutationBatch);
    }

    let metadata_key = ObjectTreeKey::new(object_id, ObjectRecordKind::Metadata, 0);
    let extent_key = ObjectTreeKey::new(object_id, ObjectRecordKind::Extent, 0);
    let metadata_path = find_object_tree_path(
        device,
        current.superblock.roots.object_tree,
        metadata_key,
        current.superblock.generation,
        object_buffer,
    )?;
    let extent_path = find_object_tree_path_for_insertion(
        device,
        current.superblock.roots.object_tree,
        extent_key,
        current.superblock.generation,
        object_buffer,
    )?;
    if metadata_path != extent_path {
        return Err(PhoenixFsError::ObjectMutationBatchSpansLeaves);
    }

    let total_blocks = current.superblock.total_blocks;
    read_filesystem_block(
        device,
        current.superblock.roots.free_space_tree,
        current_free_space,
    )?;
    let free_node = validate_free_space_leaf(current_free_space, total_blocks)?;
    if free_node.metadata.block_number != current.superblock.roots.free_space_tree {
        return Err(PhoenixFsError::InvalidTransactionRoots);
    }
    if free_node.metadata.generation > current.superblock.generation {
        return Err(PhoenixFsError::ObjectNodeGenerationAhead);
    }

    let data_blocks = data
        .len()
        .checked_add(FILESYSTEM_BLOCK_SIZE - 1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?
        / FILESYSTEM_BLOCK_SIZE;
    let data_blocks = u64::try_from(data_blocks).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    let metadata_blocks = u64::try_from(metadata_path.node_count() + 1)
        .map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    let requested_blocks = data_blocks
        .checked_add(metadata_blocks)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let allocation = plan_cow_allocation(current_free_space, total_blocks, requested_blocks)?;
    let generation = current
        .superblock
        .generation
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    let data_start_block = allocation.allocated.start_block;
    let written_blocks = write_data_extent(device, data_start_block, data, data_buffer)?;
    if written_blocks != data_blocks {
        return Err(PhoenixFsError::InvalidExtent);
    }

    let next_metadata = ObjectMetadataValue::new(
        ObjectType::File,
        data.len() as u64,
        metadata.created_ns,
        metadata.modified_ns,
        metadata.changed_ns,
    );
    next_metadata.encode(&mut metadata_bytes)?;
    validate_object_record_value(metadata_key, &metadata_bytes, total_blocks)?;

    let extent = ExtentValue::new(data_start_block, data_blocks, data.len() as u64);
    let mut extent_bytes = [0_u8; EXTENT_VALUE_SIZE];
    extent.encode(&mut extent_bytes)?;
    validate_object_record_value(extent_key, &extent_bytes, total_blocks)?;

    read_filesystem_block(device, metadata_path.leaf_block, object_buffer)?;
    let object_block = data_start_block
        .checked_add(data_blocks)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    let leaf = materialize_object_leaf_with_replacement_and_insertion(
        object_buffer,
        object_copy_buffer,
        metadata_key,
        &metadata_bytes,
        extent_key,
        &extent_bytes,
        generation,
        object_block,
    )?;
    write_filesystem_block(device, object_block, object_copy_buffer)?;

    let mut new_child_block = object_block;
    let mut new_child_first_key = first_object_node_key(object_copy_buffer, leaf)?;
    let mut next_block = object_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    for parent_position in (0..metadata_path.parent_count).rev() {
        let step = metadata_path.parents[parent_position];
        read_filesystem_block(device, step.block_number, object_buffer)?;
        let parent = validate_object_internal(object_buffer)?;
        if parent.metadata.generation > current.superblock.generation {
            return Err(PhoenixFsError::ObjectNodeGenerationAhead);
        }
        validate_path_pointer(object_buffer, step)?;

        materialize_object_internal_after_child_copy(
            object_buffer,
            object_copy_buffer,
            generation,
            next_block,
            step.child_index as usize,
            new_child_block,
            new_child_first_key,
        )?;
        write_filesystem_block(device, next_block, object_copy_buffer)?;
        let copied_parent = TreeNodeHeader::decode(object_copy_buffer)?;
        new_child_first_key = first_object_node_key(object_copy_buffer, copied_parent)?;
        new_child_block = next_block;
        next_block = next_block
            .checked_add(1)
            .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    }

    let free_space_block = next_block;
    if free_space_block
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?
        != allocation.allocated.end_block_exclusive()?
    {
        return Err(PhoenixFsError::InvalidTransactionRoots);
    }
    materialize_free_space_leaf_after_allocation(
        current_free_space,
        next_free_space,
        total_blocks,
        generation,
        free_space_block,
        allocation,
    )?;
    write_filesystem_block(device, free_space_block, next_free_space)?;

    let mut retired_blocks = [0_u64; MAX_OBJECT_TRANSACTION_RETIRED_BLOCKS];
    retired_blocks[0] = metadata_path.leaf_block;
    for index in 0..metadata_path.parent_count {
        retired_blocks[index + 1] = metadata_path.parents[index].block_number;
    }
    let retired_count = metadata_path.parent_count + 2;
    retired_blocks[metadata_path.parent_count + 1] = current.superblock.roots.free_space_tree;
    sort_u64_prefix(&mut retired_blocks, retired_count);

    let roots = TransactionRoots::new(new_child_block, free_space_block);
    let plan = TransactionCommitPlan::new(current, roots, &retired_blocks[..retired_count])?;
    let committed = commit_transaction(device, current, plan, superblock_buffer)?;

    Ok(DeepRecordUpdateResult {
        active: committed.active,
        retired_blocks,
        retired_count,
        allocated: allocation.allocated,
    })
}

impl<D: BlockDevice> FileSystem for PhoenixVfs<D> {
    fn root_node(&self) -> Result<NodeId, VfsError> {
        let metadata = self.object_metadata(ROOT_OBJECT_ID)?;
        if metadata.object_type != ObjectType::Directory {
            return Err(VfsError::NotDirectory);
        }
        Ok(NodeId(ROOT_OBJECT_ID))
    }

    fn metadata(&self, node: NodeId) -> Result<NodeMetadata, VfsError> {
        let metadata = self.object_metadata(node.0)?;
        let kind = match metadata.object_type {
            ObjectType::File => NodeKind::File,
            ObjectType::Directory => NodeKind::Directory,
        };

        Ok(NodeMetadata {
            kind,
            length: metadata.size_bytes,
        })
    }

    fn lookup_child(&self, parent: NodeId, name: &[u8]) -> Result<NodeId, VfsError> {
        if self.metadata(parent)?.kind != NodeKind::Directory {
            return Err(VfsError::NotDirectory);
        }

        let name = core::str::from_utf8(name).map_err(|_| VfsError::InvalidPath)?;
        let mut device = self
            .device
            .try_borrow_mut()
            .map_err(|_| VfsError::Storage)?;
        let mut node_buffer = self
            .node_buffer
            .try_borrow_mut()
            .map_err(|_| VfsError::Storage)?;

        let entry = lookup_directory_entry(
            &mut *device,
            self.active.superblock.roots.object_tree,
            parent.0,
            name,
            self.active.superblock.generation,
            &mut *node_buffer,
        )
        .map_err(map_phoenix_fs_to_vfs)?;

        Ok(NodeId(entry.target_object_id))
    }

    fn create_node(
        &mut self,
        parent: NodeId,
        name: &[u8],
        kind: NodeKind,
    ) -> Result<NodeId, VfsError> {
        let name = core::str::from_utf8(name).map_err(|_| VfsError::InvalidPath)?;
        let object_type = match kind {
            NodeKind::File => ObjectType::File,
            NodeKind::Directory => ObjectType::Directory,
        };

        let device = self.device.get_mut();
        let node_buffer = self.node_buffer.get_mut();
        let (object_id, entry_id) =
            next_vfs_identifiers(device, self.active, parent.0, node_buffer)
                .map_err(map_phoenix_fs_to_vfs)?;

        let result = commit_create_object(
            device,
            self.active,
            parent.0,
            entry_id,
            object_id,
            name,
            object_type,
            0,
            node_buffer,
            &mut self.object_copy_buffer,
            &mut self.current_free_space_buffer,
            &mut self.next_free_space_buffer,
            &mut self.superblock_buffer,
        )
        .map_err(map_phoenix_fs_to_vfs)?;

        self.active = result.active;
        Ok(NodeId(object_id))
    }

    fn remove_node(&mut self, parent: NodeId, name: &[u8]) -> Result<(), VfsError> {
        let name = core::str::from_utf8(name).map_err(|_| VfsError::InvalidPath)?;
        let device = self.device.get_mut();
        let node_buffer = self.node_buffer.get_mut();

        let result = commit_remove_object(
            device,
            self.active,
            parent.0,
            name,
            node_buffer,
            &mut self.object_copy_buffer,
            &mut self.current_free_space_buffer,
            &mut self.next_free_space_buffer,
            &mut self.superblock_buffer,
        )
        .map_err(map_phoenix_fs_to_vfs)?;

        self.active = result.active;
        Ok(())
    }

    fn read_node(&self, node: NodeId, offset: u64, output: &mut [u8]) -> Result<usize, VfsError> {
        let metadata = self.metadata(node)?;
        if metadata.kind != NodeKind::File {
            return Err(VfsError::IsDirectory);
        }
        if offset >= metadata.length || output.is_empty() {
            return Ok(0);
        }

        let remaining = metadata.length - offset;
        let limit = core::cmp::min(output.len() as u64, remaining) as usize;

        let mut device = self
            .device
            .try_borrow_mut()
            .map_err(|_| VfsError::Storage)?;
        let mut node_buffer = self
            .node_buffer
            .try_borrow_mut()
            .map_err(|_| VfsError::Storage)?;
        let mut data_buffer = self
            .data_buffer
            .try_borrow_mut()
            .map_err(|_| VfsError::Storage)?;

        read_file_range(
            &mut *device,
            self.active.superblock.roots.object_tree,
            node.0,
            offset,
            &mut output[..limit],
            self.active.superblock.generation,
            &mut *node_buffer,
            &mut *data_buffer,
        )
        .map_err(map_phoenix_fs_to_vfs)
    }

    fn write_node(&mut self, node: NodeId, offset: u64, data: &[u8]) -> Result<usize, VfsError> {
        let metadata = self.metadata(node)?;
        if metadata.kind != NodeKind::File {
            return Err(VfsError::IsDirectory);
        }
        if data.is_empty() {
            return Ok(0);
        }
        if offset != 0 {
            return Err(VfsError::InvalidOffset);
        }

        let result = if metadata.length == 0 {
            commit_write_empty_file(
                self.device.get_mut(),
                self.active,
                node.0,
                data,
                self.node_buffer.get_mut(),
                &mut self.object_copy_buffer,
                self.data_buffer.get_mut(),
                &mut self.current_free_space_buffer,
                &mut self.next_free_space_buffer,
                &mut self.superblock_buffer,
            )
        } else if metadata.length == data.len() as u64 {
            commit_rewrite_single_extent_file(
                self.device.get_mut(),
                self.active,
                node.0,
                data,
                self.node_buffer.get_mut(),
                &mut self.object_copy_buffer,
                self.data_buffer.get_mut(),
                &mut self.current_free_space_buffer,
                &mut self.next_free_space_buffer,
                &mut self.superblock_buffer,
            )
        } else {
            return Err(VfsError::InvalidOffset);
        }
        .map_err(map_phoenix_fs_to_vfs)?;

        self.active = result.active;
        Ok(data.len())
    }

    fn truncate_node(&mut self, node: NodeId, length: u64) -> Result<(), VfsError> {
        let metadata = self.metadata(node)?;
        if metadata.kind != NodeKind::File {
            return Err(VfsError::IsDirectory);
        }
        if length == metadata.length {
            return Ok(());
        }
        if length != 0 || metadata.length == 0 {
            return Err(VfsError::InvalidOffset);
        }

        let result = commit_truncate_single_extent_file_to_zero(
            self.device.get_mut(),
            self.active,
            node.0,
            self.node_buffer.get_mut(),
            &mut self.object_copy_buffer,
            &mut self.current_free_space_buffer,
            &mut self.next_free_space_buffer,
            &mut self.superblock_buffer,
        )
        .map_err(map_phoenix_fs_to_vfs)?;
        self.active = result.active;
        Ok(())
    }
}

fn map_phoenix_fs_to_vfs(error: PhoenixFsError) -> VfsError {
    match error {
        PhoenixFsError::ObjectRecordNotFound | PhoenixFsError::DirectoryEntryNotFound => {
            VfsError::NotFound
        }
        PhoenixFsError::InvalidDirectoryName => VfsError::InvalidPath,
        PhoenixFsError::ObjectRecordAlreadyExists => VfsError::AlreadyExists,
        PhoenixFsError::ParentNotDirectory => VfsError::NotDirectory,
        PhoenixFsError::BufferSize | PhoenixFsError::NoFreeSpace => VfsError::NoSpace,
        _ => VfsError::Storage,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Superblock {
    pub generation: u64,
    pub total_blocks: u64,
    pub volume_id: [u8; 16],
    pub incompatible_features: u64,
    pub roots: TransactionRoots,
}

impl Superblock {
    pub fn new(
        generation: u64,
        total_blocks: u64,
        volume_id: [u8; 16],
        roots: TransactionRoots,
    ) -> Result<Self, PhoenixFsError> {
        let superblock = Self {
            generation,
            total_blocks,
            volume_id,
            incompatible_features: 0,
            roots,
        };
        superblock.validate()?;
        Ok(superblock)
    }

    pub fn encode(&self, destination: &mut [u8]) -> Result<(), PhoenixFsError> {
        self.validate()?;
        if destination.len() != FILESYSTEM_BLOCK_SIZE {
            return Err(PhoenixFsError::BufferSize);
        }

        destination.fill(0);
        destination[..8].copy_from_slice(&SUPERBLOCK_MAGIC);
        write_u32(destination, 8, FORMAT_VERSION);
        write_u32(destination, 12, SUPERBLOCK_HEADER_SIZE);
        write_u32(destination, 16, FILESYSTEM_BLOCK_SIZE as u32);
        write_u64(destination, 24, self.generation);
        write_u64(destination, 32, self.total_blocks);
        destination[40..56].copy_from_slice(&self.volume_id);
        write_u64(destination, 56, self.incompatible_features);
        write_u64(destination, 72, self.roots.object_tree);
        write_u64(destination, 80, self.roots.free_space_tree);

        let checksum = superblock_crc32c(destination);
        write_u32(destination, SUPERBLOCK_CHECKSUM_OFFSET, checksum);
        Ok(())
    }

    pub fn decode(source: &[u8]) -> Result<Self, PhoenixFsError> {
        if source.len() != FILESYSTEM_BLOCK_SIZE {
            return Err(PhoenixFsError::BufferSize);
        }
        if source[..8] != SUPERBLOCK_MAGIC {
            return Err(PhoenixFsError::InvalidMagic);
        }

        let version = read_u32(source, 8);
        if version != FORMAT_VERSION {
            return Err(PhoenixFsError::UnsupportedVersion(version));
        }
        if read_u32(source, 12) != SUPERBLOCK_HEADER_SIZE {
            return Err(PhoenixFsError::InvalidHeaderSize);
        }
        if read_u32(source, 16) != FILESYSTEM_BLOCK_SIZE as u32 {
            return Err(PhoenixFsError::InvalidFilesystemBlockSize);
        }
        if read_u32(source, 20) != 0 || read_u32(source, 68) != 0 {
            return Err(PhoenixFsError::InvalidReservedField);
        }

        let expected = read_u32(source, SUPERBLOCK_CHECKSUM_OFFSET);
        let actual = superblock_crc32c(source);
        if expected != actual {
            return Err(PhoenixFsError::ChecksumMismatch);
        }

        let mut volume_id = [0_u8; 16];
        volume_id.copy_from_slice(&source[40..56]);
        let superblock = Self {
            generation: read_u64(source, 24),
            total_blocks: read_u64(source, 32),
            volume_id,
            incompatible_features: read_u64(source, 56),
            roots: TransactionRoots {
                object_tree: read_u64(source, 72),
                free_space_tree: read_u64(source, 80),
            },
        };
        superblock.validate()?;
        Ok(superblock)
    }

    fn validate(&self) -> Result<(), PhoenixFsError> {
        if self.generation == 0 {
            return Err(PhoenixFsError::InvalidGeneration);
        }
        if self.total_blocks <= SUPERBLOCK_COPY_COUNT {
            return Err(PhoenixFsError::InvalidVolumeGeometry);
        }
        if self.volume_id.iter().all(|byte| *byte == 0) {
            return Err(PhoenixFsError::InvalidVolumeId);
        }

        self.roots.validate(self.total_blocks)?;

        let unsupported = self.incompatible_features & !SUPPORTED_INCOMPATIBLE_FEATURES;
        if unsupported != 0 {
            return Err(PhoenixFsError::UnsupportedFeatures(unsupported));
        }
        Ok(())
    }
}

pub fn filesystem_block_count<D: BlockDevice>(device: &D) -> Result<u64, PhoenixFsError> {
    let geometry = device.geometry();
    if geometry.block_size == 0 || geometry.block_count == 0 {
        return Err(PhoenixFsError::InvalidDeviceGeometry);
    }
    if geometry.block_size > FILESYSTEM_BLOCK_SIZE
        || !FILESYSTEM_BLOCK_SIZE.is_multiple_of(geometry.block_size)
    {
        return Err(PhoenixFsError::UnsupportedDeviceBlockSize);
    }

    let blocks_per_filesystem_block = FILESYSTEM_BLOCK_SIZE / geometry.block_size;
    let blocks_per_filesystem_block = u64::try_from(blocks_per_filesystem_block)
        .map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    if !geometry
        .block_count
        .is_multiple_of(blocks_per_filesystem_block)
    {
        return Err(PhoenixFsError::InvalidDeviceGeometry);
    }

    let total = geometry.block_count / blocks_per_filesystem_block;
    if total <= SUPERBLOCK_COPY_COUNT {
        return Err(PhoenixFsError::InvalidVolumeGeometry);
    }
    Ok(total)
}

pub fn read_active_superblock<D: BlockDevice>(
    device: &mut D,
    first_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
    second_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<ActiveSuperblock, PhoenixFsError> {
    let total_blocks = filesystem_block_count(device)?;
    let first = read_superblock_copy(device, 0, total_blocks, first_buffer);
    let second = read_superblock_copy(device, 1, total_blocks, second_buffer);

    match (first, second) {
        (Ok(left), Ok(right)) if left.generation > right.generation => Ok(ActiveSuperblock {
            superblock: left,
            slot: SuperblockSlot::First,
        }),
        (Ok(left), Ok(right)) if right.generation > left.generation => Ok(ActiveSuperblock {
            superblock: right,
            slot: SuperblockSlot::Second,
        }),
        (Ok(left), Ok(right)) if left == right => Ok(ActiveSuperblock {
            superblock: left,
            slot: SuperblockSlot::First,
        }),
        (Ok(left), Ok(_)) => Err(PhoenixFsError::ConflictingGeneration(left.generation)),
        (Ok(valid), Err(_)) => Ok(ActiveSuperblock {
            superblock: valid,
            slot: SuperblockSlot::First,
        }),
        (Err(_), Ok(valid)) => Ok(ActiveSuperblock {
            superblock: valid,
            slot: SuperblockSlot::Second,
        }),
        (Err(_), Err(_)) => Err(PhoenixFsError::NoValidSuperblock),
    }
}

pub fn commit_next_superblock<D: BlockDevice>(
    device: &mut D,
    current: ActiveSuperblock,
    next: Superblock,
    buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<ActiveSuperblock, PhoenixFsError> {
    let expected_generation = current
        .superblock
        .generation
        .checked_add(1)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    if next.generation != expected_generation {
        return Err(PhoenixFsError::GenerationSequence);
    }
    if next.volume_id != current.superblock.volume_id {
        return Err(PhoenixFsError::VolumeIdentityChanged);
    }
    if next.total_blocks != current.superblock.total_blocks {
        return Err(PhoenixFsError::VolumeGeometryChanged);
    }
    if next.incompatible_features != current.superblock.incompatible_features {
        return Err(PhoenixFsError::FeatureSetChanged);
    }

    let total_blocks = filesystem_block_count(device)?;
    if next.total_blocks != total_blocks {
        return Err(PhoenixFsError::InvalidVolumeGeometry);
    }

    next.encode(buffer)?;
    let slot = current.slot.other();
    write_filesystem_block(device, slot.filesystem_block(), buffer)?;
    device.flush()?;

    Ok(ActiveSuperblock {
        superblock: next,
        slot,
    })
}

fn read_superblock_copy<D: BlockDevice>(
    device: &mut D,
    filesystem_block: u64,
    total_blocks: u64,
    buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<Superblock, PhoenixFsError> {
    read_filesystem_block(device, filesystem_block, buffer)?;
    let superblock = Superblock::decode(buffer)?;
    if superblock.total_blocks != total_blocks {
        return Err(PhoenixFsError::InvalidVolumeGeometry);
    }
    Ok(superblock)
}

fn read_filesystem_block<D: BlockDevice>(
    device: &mut D,
    filesystem_block: u64,
    destination: &mut [u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<(), PhoenixFsError> {
    let geometry = device.geometry();
    if geometry.block_size == 0 || !FILESYSTEM_BLOCK_SIZE.is_multiple_of(geometry.block_size) {
        return Err(PhoenixFsError::UnsupportedDeviceBlockSize);
    }

    let device_blocks = FILESYSTEM_BLOCK_SIZE / geometry.block_size;
    let device_blocks =
        u64::try_from(device_blocks).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    let first_device_block = filesystem_block
        .checked_mul(device_blocks)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    device.read_blocks(first_device_block, destination)?;
    Ok(())
}

fn write_filesystem_block<D: BlockDevice>(
    device: &mut D,
    filesystem_block: u64,
    source: &[u8; FILESYSTEM_BLOCK_SIZE],
) -> Result<(), PhoenixFsError> {
    let geometry = device.geometry();
    if geometry.block_size == 0 || !FILESYSTEM_BLOCK_SIZE.is_multiple_of(geometry.block_size) {
        return Err(PhoenixFsError::UnsupportedDeviceBlockSize);
    }

    let device_blocks = FILESYSTEM_BLOCK_SIZE / geometry.block_size;
    let device_blocks =
        u64::try_from(device_blocks).map_err(|_| PhoenixFsError::ArithmeticOverflow)?;
    let first_device_block = filesystem_block
        .checked_mul(device_blocks)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;

    device.write_blocks(first_device_block, source)?;
    Ok(())
}

fn superblock_crc32c(bytes: &[u8]) -> u32 {
    crc32c_with_zeroed_range(bytes, SUPERBLOCK_CHECKSUM_OFFSET, SUPERBLOCK_CHECKSUM_END)
}

fn metadata_crc32c(bytes: &[u8]) -> u32 {
    crc32c_with_zeroed_range(bytes, METADATA_CHECKSUM_OFFSET, METADATA_CHECKSUM_END)
}

fn crc32c_with_zeroed_range(bytes: &[u8], zero_start: usize, zero_end: usize) -> u32 {
    let mut crc = !0_u32;
    for (index, byte) in bytes.iter().copied().enumerate() {
        let value = if (zero_start..zero_end).contains(&index) {
            0
        } else {
            byte
        };
        crc ^= u32::from(value);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0x82f6_3b78 & mask);
        }
    }
    !crc
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ])
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoenix_block::MemoryBlockDevice;

    const VOLUME_ID: [u8; 16] = *b"phoenix-volume-1";
    const ROOTS: TransactionRoots = TransactionRoots::new(2, 3);

    #[test]
    fn metadata_header_round_trip_preserves_payload() {
        let header = MetadataBlockHeader::new(MetadataKind::ObjectTree, 9, 7, 5);
        let mut block = [0_u8; FILESYSTEM_BLOCK_SIZE];
        block[METADATA_HEADER_SIZE as usize..METADATA_HEADER_SIZE as usize + 5]
            .copy_from_slice(b"hello");

        header.seal(&mut block).unwrap();

        assert_eq!(MetadataBlockHeader::decode(&block), Ok(header));
        assert_eq!(
            &block[METADATA_HEADER_SIZE as usize..METADATA_HEADER_SIZE as usize + 5],
            b"hello"
        );
    }

    #[test]
    fn metadata_checksum_covers_payload() {
        let header = MetadataBlockHeader::new(MetadataKind::FreeSpaceTree, 3, 6, 1);
        let mut block = [0_u8; FILESYSTEM_BLOCK_SIZE];
        block[METADATA_HEADER_SIZE as usize] = 0x5a;
        header.seal(&mut block).unwrap();

        block[METADATA_HEADER_SIZE as usize] ^= 1;

        assert_eq!(
            MetadataBlockHeader::decode(&block),
            Err(PhoenixFsError::MetadataChecksumMismatch)
        );
    }

    #[test]
    fn metadata_header_rejects_reserved_or_out_of_volume_block() {
        let reserved = MetadataBlockHeader::new(MetadataKind::ObjectTree, 1, 1, 0);
        let outside = MetadataBlockHeader::new(MetadataKind::ObjectTree, 1, 8, 0);
        let mut block = [0_u8; FILESYSTEM_BLOCK_SIZE];

        assert_eq!(
            reserved.seal(&mut block),
            Err(PhoenixFsError::InvalidMetadataBlock(1))
        );
        assert_eq!(
            outside.validate_for_volume(8),
            Err(PhoenixFsError::InvalidMetadataBlock(8))
        );
    }

    #[test]
    fn tree_node_round_trip_preserves_entries() {
        let node = TreeNodeHeader::new(MetadataKind::ObjectTree, 12, 7, 0, 2, 8).unwrap();
        let mut block = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let offset = TreeNodeHeader::entries_offset();
        block[offset..offset + 8].copy_from_slice(b"entries!");

        node.seal(&mut block).unwrap();

        assert_eq!(TreeNodeHeader::decode(&block), Ok(node));
        assert_eq!(&block[offset..offset + 8], b"entries!");
    }

    #[test]
    fn tree_node_accepts_empty_leaf() {
        let node = TreeNodeHeader::new(MetadataKind::FreeSpaceTree, 4, 5, 0, 0, 0).unwrap();
        let mut block = [0_u8; FILESYSTEM_BLOCK_SIZE];

        node.seal(&mut block).unwrap();

        assert_eq!(TreeNodeHeader::decode(&block), Ok(node));
    }

    #[test]
    fn tree_node_rejects_item_count_without_entry_bytes() {
        assert_eq!(
            TreeNodeHeader::new(MetadataKind::ObjectTree, 2, 4, 0, 1, 0),
            Err(PhoenixFsError::InvalidTreeNodeItemCount)
        );
    }

    #[test]
    fn format_volume_creates_root_directory_and_free_space() {
        let mut device = MemoryBlockDevice::<512, 40>::new();
        let mut buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];

        let active = format_volume(&mut device, VOLUME_ID, 123, &mut buffer).unwrap();

        assert_eq!(active.superblock.generation, 1);
        assert_eq!(
            active.superblock.roots,
            TransactionRoots::new(INITIAL_OBJECT_TREE_BLOCK, INITIAL_FREE_SPACE_TREE_BLOCK)
        );

        let mut first = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let detected = read_active_superblock(&mut device, &mut first, &mut second).unwrap();
        assert_eq!(detected.superblock, active.superblock);

        let mut node_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut metadata_buffer = [0_u8; OBJECT_METADATA_VALUE_SIZE];
        let metadata = read_object_metadata(
            &mut device,
            INITIAL_OBJECT_TREE_BLOCK,
            ROOT_OBJECT_ID,
            1,
            &mut node_buffer,
            &mut metadata_buffer,
        )
        .unwrap();
        assert_eq!(metadata.object_type, ObjectType::Directory);
        assert_eq!(metadata.created_ns, 123);

        read_filesystem_block(&mut device, INITIAL_FREE_SPACE_TREE_BLOCK, &mut node_buffer)
            .unwrap();
        validate_free_space_leaf(&node_buffer, active.superblock.total_blocks).unwrap();
        assert_eq!(
            free_space_extent_at(&node_buffer, 0).unwrap(),
            FreeSpaceExtent::new(
                INITIAL_FREE_SPACE_TREE_BLOCK + 1,
                active.superblock.total_blocks - (INITIAL_FREE_SPACE_TREE_BLOCK + 1),
            )
        );
    }

    #[test]
    fn formatted_volume_mounts_through_phoenix_vfs() {
        use phoenix_vfs::{FileSystem, NodeId};

        let mut device = MemoryBlockDevice::<512, 40>::new();
        let mut buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let active = format_volume(&mut device, VOLUME_ID, 1, &mut buffer).unwrap();

        let filesystem = PhoenixVfs::new(device, active).unwrap();

        assert_eq!(filesystem.root_node(), Ok(NodeId(ROOT_OBJECT_ID)));
        assert_eq!(
            filesystem.metadata(NodeId(ROOT_OBJECT_ID)),
            Ok(NodeMetadata {
                kind: NodeKind::Directory,
                length: 0,
            })
        );
    }

    #[test]
    fn phoenix_vfs_can_format_and_remount_volume() {
        use phoenix_vfs::{FileSystem, NodeId};

        let device = MemoryBlockDevice::<512, 40>::new();
        let mut format_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let filesystem = PhoenixVfs::format_new(device, VOLUME_ID, 77, &mut format_buffer).unwrap();

        assert_eq!(filesystem.root_node(), Ok(NodeId(ROOT_OBJECT_ID)));

        let device = filesystem.into_device();
        let mut first = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let remounted = PhoenixVfs::mount(device, &mut first, &mut second).unwrap();

        assert_eq!(remounted.root_node(), Ok(NodeId(ROOT_OBJECT_ID)));
        assert_eq!(remounted.active_superblock().superblock.generation, 1);
    }

    #[test]
    fn phoenix_vfs_resolves_path_and_reads_file() {
        use phoenix_vfs::{FileSystem, NodeId, resolve_path};

        let device = MemoryBlockDevice::<512, 40>::new();
        let root_metadata = ObjectMetadataValue::new(ObjectType::Directory, 0, 1, 1, 1);
        let file_metadata = ObjectMetadataValue::new(ObjectType::File, 5, 1, 1, 1);
        let directory_entry = DirectoryEntryValue::new(2, ObjectType::File).unwrap();
        let extent = ExtentValue::new(4, 1, 5);

        let mut root_metadata_bytes = [0_u8; OBJECT_METADATA_VALUE_SIZE];
        let mut file_metadata_bytes = [0_u8; OBJECT_METADATA_VALUE_SIZE];
        let mut directory_entry_bytes = [0_u8; 64];
        let mut extent_bytes = [0_u8; EXTENT_VALUE_SIZE];
        root_metadata.encode(&mut root_metadata_bytes).unwrap();
        file_metadata.encode(&mut file_metadata_bytes).unwrap();
        let directory_entry_size = directory_entry
            .encode("hello", &mut directory_entry_bytes)
            .unwrap();
        extent.encode(&mut extent_bytes).unwrap();

        let records = [
            (
                ObjectTreeKey::new(1, ObjectRecordKind::Metadata, 0),
                &root_metadata_bytes[..],
            ),
            (
                ObjectTreeKey::new(1, ObjectRecordKind::DirectoryEntry, 1),
                &directory_entry_bytes[..directory_entry_size],
            ),
            (
                ObjectTreeKey::new(2, ObjectRecordKind::Metadata, 0),
                &file_metadata_bytes[..],
            ),
            (
                ObjectTreeKey::new(2, ObjectRecordKind::Extent, 0),
                &extent_bytes[..],
            ),
        ];

        let mut object_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let start = TreeNodeHeader::entries_offset();
        let mut cursor = start;
        for (key, value) in records {
            let header = ObjectLeafRecordHeader::new(key, value.len() as u32).unwrap();
            cursor += header
                .encode_with_value(&mut object_root[cursor..], value)
                .unwrap();
        }
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            5,
            2,
            0,
            records.len() as u32,
            (cursor - start) as u32,
        )
        .unwrap()
        .seal(&mut object_root)
        .unwrap();

        let data = {
            let mut block = [0_u8; FILESYSTEM_BLOCK_SIZE];
            block[..5].copy_from_slice(b"hello");
            block
        };
        let mut device = device;
        write_filesystem_block(&mut device, 2, &object_root).unwrap();
        write_filesystem_block(&mut device, 4, &data).unwrap();

        let active = ActiveSuperblock {
            superblock: Superblock::new(5, 5, VOLUME_ID, TransactionRoots::new(2, 3)).unwrap(),
            slot: SuperblockSlot::First,
        };
        let filesystem = PhoenixVfs::new(device, active).unwrap();

        assert_eq!(filesystem.root_node(), Ok(NodeId(1)));
        assert_eq!(resolve_path(&filesystem, "/hello"), Ok(NodeId(2)));
        assert_eq!(
            filesystem.metadata(NodeId(2)),
            Ok(NodeMetadata {
                kind: NodeKind::File,
                length: 5,
            })
        );

        let mut output = [0_u8; 5];
        assert_eq!(filesystem.read_node(NodeId(2), 0, &mut output), Ok(5));
        assert_eq!(&output, b"hello");
    }

    #[test]
    fn phoenix_vfs_writes_new_file_and_reads_it_after_remount() {
        use phoenix_vfs::{FileSystem, NodeKind};

        let device = MemoryBlockDevice::<512, 160>::new();
        let mut format_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut filesystem =
            PhoenixVfs::format_new(device, VOLUME_ID, 77, &mut format_buffer).unwrap();
        let root = filesystem.root_node().unwrap();
        let file = filesystem
            .create_node(root, b"note.txt", NodeKind::File)
            .unwrap();
        let data = b"PhoenixFS atomarnaya zapis";

        assert_eq!(filesystem.write_node(file, 0, data), Ok(data.len()));
        assert_eq!(filesystem.metadata(file).unwrap().length, data.len() as u64);
        assert_eq!(
            filesystem.write_node(file, 0, b"second"),
            Err(VfsError::InvalidOffset)
        );

        let mut output = [0_u8; 64];
        assert_eq!(filesystem.read_node(file, 0, &mut output), Ok(data.len()));
        assert_eq!(&output[..data.len()], data);

        let mut device = filesystem.into_device();
        let mut first = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let active = read_active_superblock(&mut device, &mut first, &mut second).unwrap();
        let remounted = PhoenixVfs::new(device, active).unwrap();
        let mut remounted_output = [0_u8; 64];
        assert_eq!(
            remounted.read_node(file, 0, &mut remounted_output),
            Ok(data.len())
        );
        assert_eq!(&remounted_output[..data.len()], data);
    }

    #[test]
    fn phoenix_vfs_creates_and_removes_empty_nodes() {
        use phoenix_vfs::{FileSystem, NodeId};

        let device = MemoryBlockDevice::<512, 160>::new();
        let mut format_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut filesystem =
            PhoenixVfs::format_new(device, VOLUME_ID, 77, &mut format_buffer).unwrap();

        let file = filesystem
            .create_node(NodeId(ROOT_OBJECT_ID), b"hello", NodeKind::File)
            .unwrap();
        assert_ne!(file, NodeId(ROOT_OBJECT_ID));
        assert_eq!(
            filesystem.lookup_child(NodeId(ROOT_OBJECT_ID), b"hello"),
            Ok(file)
        );
        assert_eq!(
            filesystem.metadata(file),
            Ok(NodeMetadata {
                kind: NodeKind::File,
                length: 0,
            })
        );

        filesystem
            .remove_node(NodeId(ROOT_OBJECT_ID), b"hello")
            .unwrap();
        assert_eq!(
            filesystem.lookup_child(NodeId(ROOT_OBJECT_ID), b"hello"),
            Err(VfsError::NotFound)
        );
    }

    #[test]
    fn reads_metadata_value_by_exact_tree_key() {
        let mut device = MemoryBlockDevice::<512, 512>::new();
        let key = ObjectTreeKey::new(7, ObjectRecordKind::Metadata, 0);
        let metadata = ObjectMetadataValue::new(ObjectType::File, 123, 1, 2, 3);
        let mut value = [0_u8; OBJECT_METADATA_VALUE_SIZE];
        metadata.encode(&mut value).unwrap();

        let mut leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let start = TreeNodeHeader::entries_offset();
        let header = ObjectLeafRecordHeader::new(key, OBJECT_METADATA_VALUE_SIZE as u32).unwrap();
        let bytes = header
            .encode_with_value(&mut leaf[start..], &value)
            .unwrap();
        TreeNodeHeader::new(MetadataKind::ObjectTree, 4, 8, 0, 1, bytes as u32)
            .unwrap()
            .seal(&mut leaf)
            .unwrap();
        write_filesystem_block(&mut device, 8, &leaf).unwrap();

        let mut node_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut value_buffer = [0_u8; OBJECT_METADATA_VALUE_SIZE];
        let decoded =
            read_object_metadata(&mut device, 8, 7, 5, &mut node_buffer, &mut value_buffer)
                .unwrap();

        assert_eq!(decoded, metadata);
    }

    #[test]
    fn directory_lookup_finds_utf8_name_without_hash_key() {
        let mut device = MemoryBlockDevice::<512, 512>::new();
        let key = ObjectTreeKey::new(5, ObjectRecordKind::DirectoryEntry, 99);
        let entry = DirectoryEntryValue::new(42, ObjectType::Directory).unwrap();
        let mut encoded_entry = [0_u8; 128];
        let entry_size = entry.encode("Документы", &mut encoded_entry).unwrap();

        let mut leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let start = TreeNodeHeader::entries_offset();
        let header = ObjectLeafRecordHeader::new(key, entry_size as u32).unwrap();
        let bytes = header
            .encode_with_value(&mut leaf[start..], &encoded_entry[..entry_size])
            .unwrap();
        TreeNodeHeader::new(MetadataKind::ObjectTree, 4, 8, 0, 1, bytes as u32)
            .unwrap()
            .seal(&mut leaf)
            .unwrap();
        write_filesystem_block(&mut device, 8, &leaf).unwrap();

        let mut node_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let found =
            lookup_directory_entry(&mut device, 8, 5, "Документы", 5, &mut node_buffer).unwrap();

        assert_eq!(found.entry_id, 99);
        assert_eq!(found.target_object_id, 42);
        assert_eq!(found.target_type, ObjectType::Directory);
    }

    #[test]
    fn file_range_reads_across_extent_block_boundary() {
        let mut device = MemoryBlockDevice::<512, 512>::new();
        let extent_key = ObjectTreeKey::new(7, ObjectRecordKind::Extent, 0);
        let extent = ExtentValue::new(20, 2, 8192);
        let mut extent_bytes = [0_u8; EXTENT_VALUE_SIZE];
        extent.encode(&mut extent_bytes).unwrap();

        let mut leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let start = TreeNodeHeader::entries_offset();
        let header = ObjectLeafRecordHeader::new(extent_key, EXTENT_VALUE_SIZE as u32).unwrap();
        let bytes = header
            .encode_with_value(&mut leaf[start..], &extent_bytes)
            .unwrap();
        TreeNodeHeader::new(MetadataKind::ObjectTree, 4, 8, 0, 1, bytes as u32)
            .unwrap()
            .seal(&mut leaf)
            .unwrap();
        write_filesystem_block(&mut device, 8, &leaf).unwrap();

        let first = [0x11_u8; FILESYSTEM_BLOCK_SIZE];
        let second = [0x22_u8; FILESYSTEM_BLOCK_SIZE];
        write_filesystem_block(&mut device, 20, &first).unwrap();
        write_filesystem_block(&mut device, 21, &second).unwrap();

        let mut node_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut data_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut destination = [0_u8; 32];

        let read = read_file_range(
            &mut device,
            8,
            7,
            FILESYSTEM_BLOCK_SIZE as u64 - 16,
            &mut destination,
            5,
            &mut node_buffer,
            &mut data_buffer,
        )
        .unwrap();

        assert_eq!(read, 32);
        assert_eq!(&destination[..16], &[0x11; 16]);
        assert_eq!(&destination[16..], &[0x22; 16]);
    }

    #[test]
    fn object_metadata_value_round_trip_is_fixed_and_reserved_clean() {
        let value = ObjectMetadataValue::new(ObjectType::File, 12_345, 10, 20, 30);
        let mut encoded = [0_u8; OBJECT_METADATA_VALUE_SIZE];

        value.encode(&mut encoded).unwrap();

        assert_eq!(ObjectMetadataValue::decode(&encoded), Ok(value));
        assert_eq!(read_u32(&encoded, 4), 0);
        assert_eq!(read_u64(&encoded, 40), 0);
    }

    #[test]
    fn directory_entry_round_trip_preserves_utf8_name() {
        let entry = DirectoryEntryValue::new(44, ObjectType::Directory).unwrap();
        let mut encoded = [0_u8; 128];

        let size = entry.encode("Документы", &mut encoded).unwrap();
        let (decoded, name) = DirectoryEntryValue::decode(&encoded[..size]).unwrap();

        assert_eq!(decoded, entry);
        assert_eq!(name, "Документы");
    }

    #[test]
    fn directory_entry_rejects_path_components() {
        assert_eq!(
            DirectoryEntryValue::encoded_size("../secret"),
            Err(PhoenixFsError::InvalidDirectoryName)
        );
        assert_eq!(
            DirectoryEntryValue::encoded_size("dir/file"),
            Err(PhoenixFsError::InvalidDirectoryName)
        );
    }

    #[test]
    fn extent_value_checks_alignment_capacity_and_volume() {
        let key = ObjectTreeKey::new(7, ObjectRecordKind::Extent, 4096);
        let extent = ExtentValue::new(20, 2, 7000);
        let mut encoded = [0_u8; EXTENT_VALUE_SIZE];

        extent.encode(&mut encoded).unwrap();

        let decoded = ExtentValue::decode(&encoded).unwrap();
        assert_eq!(decoded, extent);
        assert_eq!(decoded.validate_for_key(key, 64), Ok(()));
        assert_eq!(
            decoded.validate_for_key(ObjectTreeKey::new(7, ObjectRecordKind::Extent, 1), 64,),
            Err(PhoenixFsError::InvalidExtent)
        );
    }

    #[test]
    fn object_record_value_validation_matches_key_kind() {
        let metadata = ObjectMetadataValue::new(ObjectType::Directory, 0, 1, 1, 1);
        let mut encoded = [0_u8; OBJECT_METADATA_VALUE_SIZE];
        metadata.encode(&mut encoded).unwrap();

        assert_eq!(
            validate_object_record_value(
                ObjectTreeKey::new(3, ObjectRecordKind::Metadata, 0),
                &encoded,
                64,
            ),
            Ok(())
        );
        assert_eq!(
            validate_object_record_value(
                ObjectTreeKey::new(3, ObjectRecordKind::Metadata, 8),
                &encoded,
                64,
            ),
            Err(PhoenixFsError::InvalidObjectValueSize)
        );
    }

    #[test]
    fn object_path_selects_greatest_separator_not_above_target() {
        let key_a = ObjectTreeKey::new(1, ObjectRecordKind::Metadata, 0);
        let key_b = ObjectTreeKey::new(10, ObjectRecordKind::Metadata, 0);
        let target = ObjectTreeKey::new(12, ObjectRecordKind::Metadata, 0);
        let mut root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let start = TreeNodeHeader::entries_offset();

        ObjectInternalRecord::new(key_a, 20)
            .unwrap()
            .encode(&mut root[start..start + OBJECT_INTERNAL_RECORD_SIZE])
            .unwrap();
        ObjectInternalRecord::new(key_b, 21)
            .unwrap()
            .encode(
                &mut root
                    [start + OBJECT_INTERNAL_RECORD_SIZE..start + 2 * OBJECT_INTERNAL_RECORD_SIZE],
            )
            .unwrap();
        let node = TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            5,
            8,
            1,
            2,
            (2 * OBJECT_INTERNAL_RECORD_SIZE) as u32,
        )
        .unwrap();
        node.seal(&mut root).unwrap();

        assert_eq!(
            select_object_child(&root, node, target, false).unwrap().0,
            1
        );
    }

    #[test]
    fn object_path_rejects_child_with_wrong_separator_key() {
        let mut device = MemoryBlockDevice::<512, 512>::new();
        let separator = ObjectTreeKey::new(7, ObjectRecordKind::Metadata, 0);
        let actual = ObjectTreeKey::new(8, ObjectRecordKind::Metadata, 0);

        let mut leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let leaf_start = TreeNodeHeader::entries_offset();
        let record = ObjectLeafRecordHeader::new(actual, 1).unwrap();
        let leaf_bytes = record
            .encode_with_value(&mut leaf[leaf_start..], b"x")
            .unwrap();
        TreeNodeHeader::new(MetadataKind::ObjectTree, 3, 10, 0, 1, leaf_bytes as u32)
            .unwrap()
            .seal(&mut leaf)
            .unwrap();

        let mut root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let root_start = TreeNodeHeader::entries_offset();
        ObjectInternalRecord::new(separator, 10)
            .unwrap()
            .encode(&mut root[root_start..root_start + OBJECT_INTERNAL_RECORD_SIZE])
            .unwrap();
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            4,
            8,
            1,
            1,
            OBJECT_INTERNAL_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut root)
        .unwrap();

        write_filesystem_block(&mut device, 8, &root).unwrap();
        write_filesystem_block(&mut device, 10, &leaf).unwrap();

        let mut buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        assert_eq!(
            find_object_tree_path(&mut device, 8, actual, 5, &mut buffer),
            Err(PhoenixFsError::ObjectTreePathMismatch)
        );
    }

    #[test]
    fn object_leaf_batch_insertion_merges_sorted_keys_atomically() {
        let existing_key = ObjectTreeKey::new(10, ObjectRecordKind::Metadata, 0);
        let first_key = ObjectTreeKey::new(5, ObjectRecordKind::Metadata, 0);
        let last_key = ObjectTreeKey::new(15, ObjectRecordKind::Metadata, 0);

        let mut current = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let start = TreeNodeHeader::entries_offset();
        let record = ObjectLeafRecordHeader::new(existing_key, 1).unwrap();
        let size = record
            .encode_with_value(&mut current[start..], b"x")
            .unwrap();
        TreeNodeHeader::new(MetadataKind::ObjectTree, 4, 8, 0, 1, size as u32)
            .unwrap()
            .seal(&mut current)
            .unwrap();

        let insertions = [
            ObjectLeafInsert::new(first_key, b"a"),
            ObjectLeafInsert::new(last_key, b"b"),
        ];
        let mut next = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let node =
            materialize_object_leaf_with_inserted_values(&current, &mut next, &insertions, 5, 20)
                .unwrap();

        assert_eq!(node.item_count, 3);
        let mut cursor = start;
        let mut keys = [existing_key; 3];
        for key in &mut keys {
            let (record, _, bytes) =
                ObjectLeafRecordHeader::decode_with_value(&next[cursor..]).unwrap();
            *key = record.key;
            cursor += bytes;
        }
        assert_eq!(keys, [first_key, existing_key, last_key]);
    }

    #[test]
    fn object_leaf_batch_rejects_unsorted_or_duplicate_input() {
        let key = ObjectTreeKey::new(5, ObjectRecordKind::Metadata, 0);
        let later = ObjectTreeKey::new(10, ObjectRecordKind::Metadata, 0);

        assert_eq!(
            validate_object_insertions(&[
                ObjectLeafInsert::new(later, b"a"),
                ObjectLeafInsert::new(key, b"b"),
            ]),
            Err(PhoenixFsError::InvalidObjectMutationBatch)
        );
        assert_eq!(
            validate_object_insertions(&[
                ObjectLeafInsert::new(key, b"a"),
                ObjectLeafInsert::new(key, b"b"),
            ]),
            Err(PhoenixFsError::InvalidObjectMutationBatch)
        );
    }

    #[test]
    fn atomic_create_spans_two_leaves_in_one_generation() {
        let mut device = MemoryBlockDevice::<512, 1024>::new();
        let current = ActiveSuperblock {
            superblock: Superblock::new(5, 128, VOLUME_ID, TransactionRoots::new(8, 9)).unwrap(),
            slot: SuperblockSlot::First,
        };

        let parent_metadata = ObjectMetadataValue::new(ObjectType::Directory, 0, 1, 1, 1);
        let dummy_metadata = ObjectMetadataValue::new(ObjectType::File, 0, 1, 1, 1);
        let mut parent_bytes = [0_u8; OBJECT_METADATA_VALUE_SIZE];
        let mut dummy_bytes = [0_u8; OBJECT_METADATA_VALUE_SIZE];
        parent_metadata.encode(&mut parent_bytes).unwrap();
        dummy_metadata.encode(&mut dummy_bytes).unwrap();

        let parent_key = ObjectTreeKey::new(1, ObjectRecordKind::Metadata, 0);
        let dummy_key = ObjectTreeKey::new(5, ObjectRecordKind::Metadata, 0);

        let mut left = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let start = TreeNodeHeader::entries_offset();
        let left_size = ObjectLeafRecordHeader::new(parent_key, OBJECT_METADATA_VALUE_SIZE as u32)
            .unwrap()
            .encode_with_value(&mut left[start..], &parent_bytes)
            .unwrap();
        TreeNodeHeader::new(MetadataKind::ObjectTree, 4, 10, 0, 1, left_size as u32)
            .unwrap()
            .seal(&mut left)
            .unwrap();

        let mut right = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let right_size = ObjectLeafRecordHeader::new(dummy_key, OBJECT_METADATA_VALUE_SIZE as u32)
            .unwrap()
            .encode_with_value(&mut right[start..], &dummy_bytes)
            .unwrap();
        TreeNodeHeader::new(MetadataKind::ObjectTree, 4, 11, 0, 1, right_size as u32)
            .unwrap()
            .seal(&mut right)
            .unwrap();

        let mut root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        ObjectInternalRecord::new(parent_key, 10)
            .unwrap()
            .encode(&mut root[start..start + OBJECT_INTERNAL_RECORD_SIZE])
            .unwrap();
        ObjectInternalRecord::new(dummy_key, 11)
            .unwrap()
            .encode(
                &mut root
                    [start + OBJECT_INTERNAL_RECORD_SIZE..start + 2 * OBJECT_INTERNAL_RECORD_SIZE],
            )
            .unwrap();
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            5,
            8,
            1,
            2,
            (2 * OBJECT_INTERNAL_RECORD_SIZE) as u32,
        )
        .unwrap()
        .seal(&mut root)
        .unwrap();

        let mut free_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        write_free_space_extent(&mut free_root, 0, FreeSpaceExtent::new(20, 40)).unwrap();
        TreeNodeHeader::new(
            MetadataKind::FreeSpaceTree,
            5,
            9,
            0,
            1,
            FREE_SPACE_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut free_root)
        .unwrap();

        write_filesystem_block(&mut device, 8, &root).unwrap();
        write_filesystem_block(&mut device, 9, &free_root).unwrap();
        write_filesystem_block(&mut device, 10, &left).unwrap();
        write_filesystem_block(&mut device, 11, &right).unwrap();

        let mut object_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut object_copy = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut current_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut next_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut superblock_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];

        let result = commit_create_object(
            &mut device,
            current,
            1,
            1,
            10,
            "cross-leaf",
            ObjectType::File,
            20,
            &mut object_buffer,
            &mut object_copy,
            &mut current_free,
            &mut next_free,
            &mut superblock_buffer,
        )
        .unwrap();

        assert_eq!(result.active.superblock.generation, 6);
        assert_eq!(result.active.superblock.roots.object_tree, 22);
        assert_eq!(result.active.superblock.roots.free_space_tree, 23);
        assert_eq!(result.retired_blocks(), &[8, 9, 10, 11]);

        let entry = lookup_directory_entry(
            &mut device,
            result.active.superblock.roots.object_tree,
            1,
            "cross-leaf",
            6,
            &mut object_buffer,
        )
        .unwrap();
        assert_eq!(entry.target_object_id, 10);

        let mut metadata_bytes = [0_u8; OBJECT_METADATA_VALUE_SIZE];
        let metadata = read_object_metadata(
            &mut device,
            result.active.superblock.roots.object_tree,
            10,
            6,
            &mut object_buffer,
            &mut metadata_bytes,
        )
        .unwrap();
        assert_eq!(metadata.object_type, ObjectType::File);
        assert_eq!(metadata.created_ns, 20);
    }

    #[test]
    fn atomic_remove_deletes_empty_object_and_directory_entry_together() {
        let mut device = MemoryBlockDevice::<512, 128>::new();
        let mut format_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let current = format_volume(&mut device, VOLUME_ID, 10, &mut format_buffer).unwrap();

        let mut object_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut object_copy = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut current_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut next_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut superblock_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];

        let created = commit_create_object(
            &mut device,
            current,
            ROOT_OBJECT_ID,
            1,
            2,
            "gone",
            ObjectType::File,
            20,
            &mut object_buffer,
            &mut object_copy,
            &mut current_free,
            &mut next_free,
            &mut superblock_buffer,
        )
        .unwrap();

        let removed = commit_remove_object(
            &mut device,
            created.active,
            ROOT_OBJECT_ID,
            "gone",
            &mut object_buffer,
            &mut object_copy,
            &mut current_free,
            &mut next_free,
            &mut superblock_buffer,
        )
        .unwrap();

        assert_eq!(removed.active.superblock.generation, 3);
        assert_eq!(
            lookup_directory_entry(
                &mut device,
                removed.active.superblock.roots.object_tree,
                ROOT_OBJECT_ID,
                "gone",
                removed.active.superblock.generation,
                &mut object_buffer,
            ),
            Err(PhoenixFsError::DirectoryEntryNotFound)
        );

        let mut metadata = [0_u8; OBJECT_METADATA_VALUE_SIZE];
        assert_eq!(
            read_object_metadata(
                &mut device,
                removed.active.superblock.roots.object_tree,
                2,
                removed.active.superblock.generation,
                &mut object_buffer,
                &mut metadata,
            ),
            Err(PhoenixFsError::ObjectRecordNotFound)
        );
    }

    #[test]
    fn atomic_remove_rejects_non_empty_directory() {
        let mut device = MemoryBlockDevice::<512, 192>::new();
        let mut format_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let current = format_volume(&mut device, VOLUME_ID, 10, &mut format_buffer).unwrap();

        let mut object_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut object_copy = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut current_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut next_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut superblock_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];

        let directory = commit_create_object(
            &mut device,
            current,
            ROOT_OBJECT_ID,
            1,
            2,
            "dir",
            ObjectType::Directory,
            20,
            &mut object_buffer,
            &mut object_copy,
            &mut current_free,
            &mut next_free,
            &mut superblock_buffer,
        )
        .unwrap();
        let child = commit_create_object(
            &mut device,
            directory.active,
            2,
            1,
            3,
            "child",
            ObjectType::File,
            30,
            &mut object_buffer,
            &mut object_copy,
            &mut current_free,
            &mut next_free,
            &mut superblock_buffer,
        )
        .unwrap();

        assert_eq!(
            commit_remove_object(
                &mut device,
                child.active,
                ROOT_OBJECT_ID,
                "dir",
                &mut object_buffer,
                &mut object_copy,
                &mut current_free,
                &mut next_free,
                &mut superblock_buffer,
            ),
            Err(PhoenixFsError::ObjectNotEmpty)
        );
    }

    #[test]
    fn deleting_from_sparse_leaf_rebalances_with_adjacent_sibling() {
        let mut device = MemoryBlockDevice::<512, 1024>::new();
        let current = ActiveSuperblock {
            superblock: Superblock::new(5, 128, VOLUME_ID, TransactionRoots::new(8, 9)).unwrap(),
            slot: SuperblockSlot::First,
        };
        let start = TreeNodeHeader::entries_offset();

        let first_key = ObjectTreeKey::new(1, ObjectRecordKind::Metadata, 0);
        let deleted_key = ObjectTreeKey::new(2, ObjectRecordKind::Metadata, 0);
        let mut first_leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let small_payload = [0x11_u8; 512];
        let mut cursor = start;
        cursor += ObjectLeafRecordHeader::new(first_key, small_payload.len() as u32)
            .unwrap()
            .encode_with_value(&mut first_leaf[cursor..], &small_payload)
            .unwrap();
        cursor += ObjectLeafRecordHeader::new(deleted_key, small_payload.len() as u32)
            .unwrap()
            .encode_with_value(&mut first_leaf[cursor..], &small_payload)
            .unwrap();
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            4,
            10,
            0,
            2,
            (cursor - start) as u32,
        )
        .unwrap()
        .seal(&mut first_leaf)
        .unwrap();

        let mut second_leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let large_payload = [0x22_u8; 850];
        let mut second_cursor = start;
        for object_id in 3..=6_u64 {
            let key = ObjectTreeKey::new(object_id, ObjectRecordKind::Metadata, 0);
            second_cursor += ObjectLeafRecordHeader::new(key, large_payload.len() as u32)
                .unwrap()
                .encode_with_value(&mut second_leaf[second_cursor..], &large_payload)
                .unwrap();
        }
        let second_first_key = ObjectTreeKey::new(3, ObjectRecordKind::Metadata, 0);
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            4,
            11,
            0,
            4,
            (second_cursor - start) as u32,
        )
        .unwrap()
        .seal(&mut second_leaf)
        .unwrap();

        let mut root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        ObjectInternalRecord::new(first_key, 10)
            .unwrap()
            .encode(&mut root[start..start + OBJECT_INTERNAL_RECORD_SIZE])
            .unwrap();
        ObjectInternalRecord::new(second_first_key, 11)
            .unwrap()
            .encode(
                &mut root
                    [start + OBJECT_INTERNAL_RECORD_SIZE..start + 2 * OBJECT_INTERNAL_RECORD_SIZE],
            )
            .unwrap();
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            5,
            8,
            1,
            2,
            (2 * OBJECT_INTERNAL_RECORD_SIZE) as u32,
        )
        .unwrap()
        .seal(&mut root)
        .unwrap();

        let mut free_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        write_free_space_extent(&mut free_root, 0, FreeSpaceExtent::new(20, 80)).unwrap();
        TreeNodeHeader::new(
            MetadataKind::FreeSpaceTree,
            5,
            9,
            0,
            1,
            FREE_SPACE_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut free_root)
        .unwrap();

        write_filesystem_block(&mut device, 8, &root).unwrap();
        write_filesystem_block(&mut device, 10, &first_leaf).unwrap();
        write_filesystem_block(&mut device, 11, &second_leaf).unwrap();
        write_filesystem_block(&mut device, 9, &free_root).unwrap();

        let mut object_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut object_copy = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut current_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut next_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut superblock_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];

        let result = commit_object_record_delete(
            &mut device,
            current,
            deleted_key,
            &mut object_buffer,
            &mut object_copy,
            &mut current_free,
            &mut next_free,
            &mut superblock_buffer,
        )
        .unwrap();

        let mut new_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(
            &mut device,
            result.active.superblock.roots.object_tree,
            &mut new_root,
        )
        .unwrap();
        let root_node = validate_object_internal(&new_root).unwrap();
        assert_eq!(root_node.item_count, 2);

        let left_pointer = object_internal_record_at(&new_root, 0).unwrap();
        let right_pointer = object_internal_record_at(&new_root, 1).unwrap();
        let mut left_leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut right_leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(&mut device, left_pointer.child_block, &mut left_leaf).unwrap();
        read_filesystem_block(&mut device, right_pointer.child_block, &mut right_leaf).unwrap();

        let left_node = validate_object_leaf(&left_leaf).unwrap();
        let right_node = validate_object_leaf(&right_leaf).unwrap();
        assert!(left_node.item_count > 1);
        assert!(right_node.item_count > 1);
        find_object_leaf_record(&left_leaf, first_key).unwrap();
        assert_eq!(
            find_object_leaf_record(&left_leaf, deleted_key),
            Err(PhoenixFsError::ObjectRecordNotFound)
        );
        find_object_leaf_record(
            &right_leaf,
            ObjectTreeKey::new(6, ObjectRecordKind::Metadata, 0),
        )
        .unwrap();
    }

    #[test]
    fn deleting_from_leaf_merges_adjacent_sibling_when_combined_fit() {
        let mut device = MemoryBlockDevice::<512, 1024>::new();
        let current = ActiveSuperblock {
            superblock: Superblock::new(5, 128, VOLUME_ID, TransactionRoots::new(8, 9)).unwrap(),
            slot: SuperblockSlot::First,
        };
        let start = TreeNodeHeader::entries_offset();
        let first_key = ObjectTreeKey::new(1, ObjectRecordKind::Metadata, 0);
        let second_key = ObjectTreeKey::new(2, ObjectRecordKind::Metadata, 0);
        let third_key = ObjectTreeKey::new(3, ObjectRecordKind::Metadata, 0);

        let mut first_leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut cursor = start;
        cursor += ObjectLeafRecordHeader::new(first_key, 1)
            .unwrap()
            .encode_with_value(&mut first_leaf[cursor..], b"a")
            .unwrap();
        cursor += ObjectLeafRecordHeader::new(second_key, 1)
            .unwrap()
            .encode_with_value(&mut first_leaf[cursor..], b"b")
            .unwrap();
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            4,
            10,
            0,
            2,
            (cursor - start) as u32,
        )
        .unwrap()
        .seal(&mut first_leaf)
        .unwrap();

        let mut second_leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let second_size = ObjectLeafRecordHeader::new(third_key, 1)
            .unwrap()
            .encode_with_value(&mut second_leaf[start..], b"c")
            .unwrap();
        TreeNodeHeader::new(MetadataKind::ObjectTree, 4, 11, 0, 1, second_size as u32)
            .unwrap()
            .seal(&mut second_leaf)
            .unwrap();

        let fourth_key = ObjectTreeKey::new(4, ObjectRecordKind::Metadata, 0);
        let mut third_leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let third_size = ObjectLeafRecordHeader::new(fourth_key, 1)
            .unwrap()
            .encode_with_value(&mut third_leaf[start..], b"d")
            .unwrap();
        TreeNodeHeader::new(MetadataKind::ObjectTree, 4, 12, 0, 1, third_size as u32)
            .unwrap()
            .seal(&mut third_leaf)
            .unwrap();

        let mut root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        for (index, record) in [
            ObjectInternalRecord::new(first_key, 10).unwrap(),
            ObjectInternalRecord::new(third_key, 11).unwrap(),
            ObjectInternalRecord::new(fourth_key, 12).unwrap(),
        ]
        .iter()
        .copied()
        .enumerate()
        {
            record
                .encode(
                    &mut root[start + index * OBJECT_INTERNAL_RECORD_SIZE
                        ..start + (index + 1) * OBJECT_INTERNAL_RECORD_SIZE],
                )
                .unwrap();
        }
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            5,
            8,
            1,
            3,
            (3 * OBJECT_INTERNAL_RECORD_SIZE) as u32,
        )
        .unwrap()
        .seal(&mut root)
        .unwrap();

        let mut free_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        write_free_space_extent(&mut free_root, 0, FreeSpaceExtent::new(20, 80)).unwrap();
        TreeNodeHeader::new(
            MetadataKind::FreeSpaceTree,
            5,
            9,
            0,
            1,
            FREE_SPACE_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut free_root)
        .unwrap();

        write_filesystem_block(&mut device, 8, &root).unwrap();
        write_filesystem_block(&mut device, 10, &first_leaf).unwrap();
        write_filesystem_block(&mut device, 11, &second_leaf).unwrap();
        write_filesystem_block(&mut device, 12, &third_leaf).unwrap();
        write_filesystem_block(&mut device, 9, &free_root).unwrap();

        let mut object_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut object_copy = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut current_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut next_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut superblock_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];

        let result = commit_object_record_delete(
            &mut device,
            current,
            second_key,
            &mut object_buffer,
            &mut object_copy,
            &mut current_free,
            &mut next_free,
            &mut superblock_buffer,
        )
        .unwrap();

        let mut new_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(
            &mut device,
            result.active.superblock.roots.object_tree,
            &mut new_root,
        )
        .unwrap();
        let root_node = validate_object_internal(&new_root).unwrap();
        assert_eq!(root_node.item_count, 2);

        let merged_pointer = object_internal_record_at(&new_root, 0).unwrap();
        let mut merged_leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(&mut device, merged_pointer.child_block, &mut merged_leaf).unwrap();
        find_object_leaf_record(&merged_leaf, first_key).unwrap();
        find_object_leaf_record(&merged_leaf, third_key).unwrap();
        assert_eq!(
            find_object_leaf_record(&merged_leaf, second_key),
            Err(PhoenixFsError::ObjectRecordNotFound)
        );
    }

    #[test]
    fn pruning_leaf_merges_adjacent_internal_nodes_when_they_fit() {
        let mut device = MemoryBlockDevice::<512, 1024>::new();
        let current = ActiveSuperblock {
            superblock: Superblock::new(5, 128, VOLUME_ID, TransactionRoots::new(8, 9)).unwrap(),
            slot: SuperblockSlot::First,
        };
        let start = TreeNodeHeader::entries_offset();
        let removed_key = ObjectTreeKey::new(1, ObjectRecordKind::Metadata, 0);
        let remaining_key = ObjectTreeKey::new(2, ObjectRecordKind::Metadata, 0);
        let sibling_key = ObjectTreeKey::new(3, ObjectRecordKind::Metadata, 0);
        let third_key = ObjectTreeKey::new(5, ObjectRecordKind::Metadata, 0);

        let mut removed_leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let removed_size = ObjectLeafRecordHeader::new(removed_key, 1)
            .unwrap()
            .encode_with_value(&mut removed_leaf[start..], b"a")
            .unwrap();
        TreeNodeHeader::new(MetadataKind::ObjectTree, 4, 12, 0, 1, removed_size as u32)
            .unwrap()
            .seal(&mut removed_leaf)
            .unwrap();

        let mut anchor = [0_u8; FILESYSTEM_BLOCK_SIZE];
        ObjectInternalRecord::new(removed_key, 12)
            .unwrap()
            .encode(&mut anchor[start..start + OBJECT_INTERNAL_RECORD_SIZE])
            .unwrap();
        ObjectInternalRecord::new(remaining_key, 13)
            .unwrap()
            .encode(
                &mut anchor
                    [start + OBJECT_INTERNAL_RECORD_SIZE..start + 2 * OBJECT_INTERNAL_RECORD_SIZE],
            )
            .unwrap();
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            4,
            10,
            1,
            2,
            (2 * OBJECT_INTERNAL_RECORD_SIZE) as u32,
        )
        .unwrap()
        .seal(&mut anchor)
        .unwrap();

        let mut sibling = [0_u8; FILESYSTEM_BLOCK_SIZE];
        ObjectInternalRecord::new(sibling_key, 15)
            .unwrap()
            .encode(&mut sibling[start..start + OBJECT_INTERNAL_RECORD_SIZE])
            .unwrap();
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            4,
            11,
            1,
            1,
            OBJECT_INTERNAL_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut sibling)
        .unwrap();

        let mut root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        for (index, record) in [
            ObjectInternalRecord::new(removed_key, 10).unwrap(),
            ObjectInternalRecord::new(sibling_key, 11).unwrap(),
            ObjectInternalRecord::new(third_key, 14).unwrap(),
        ]
        .into_iter()
        .enumerate()
        {
            record
                .encode(
                    &mut root[start + index * OBJECT_INTERNAL_RECORD_SIZE
                        ..start + (index + 1) * OBJECT_INTERNAL_RECORD_SIZE],
                )
                .unwrap();
        }
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            5,
            8,
            2,
            3,
            (3 * OBJECT_INTERNAL_RECORD_SIZE) as u32,
        )
        .unwrap()
        .seal(&mut root)
        .unwrap();

        let mut free_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        write_free_space_extent(&mut free_root, 0, FreeSpaceExtent::new(20, 80)).unwrap();
        TreeNodeHeader::new(
            MetadataKind::FreeSpaceTree,
            5,
            9,
            0,
            1,
            FREE_SPACE_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut free_root)
        .unwrap();

        write_filesystem_block(&mut device, 8, &root).unwrap();
        write_filesystem_block(&mut device, 10, &anchor).unwrap();
        write_filesystem_block(&mut device, 11, &sibling).unwrap();
        write_filesystem_block(&mut device, 12, &removed_leaf).unwrap();
        write_filesystem_block(&mut device, 9, &free_root).unwrap();

        let mut object_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut object_copy = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut current_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut next_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut superblock_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];

        let result = commit_object_record_delete(
            &mut device,
            current,
            removed_key,
            &mut object_buffer,
            &mut object_copy,
            &mut current_free,
            &mut next_free,
            &mut superblock_buffer,
        )
        .unwrap();

        let mut new_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(
            &mut device,
            result.active.superblock.roots.object_tree,
            &mut new_root,
        )
        .unwrap();
        let root_node = validate_object_internal(&new_root).unwrap();
        assert_eq!(root_node.item_count, 2);
        let merged_pointer = object_internal_record_at(&new_root, 0).unwrap();

        let mut merged = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(&mut device, merged_pointer.child_block, &mut merged).unwrap();
        let merged_node = validate_object_internal(&merged).unwrap();
        assert_eq!(merged_node.item_count, 2);
        assert_eq!(
            object_internal_record_at(&merged, 0).unwrap().key,
            remaining_key
        );
        assert_eq!(
            object_internal_record_at(&merged, 1).unwrap().key,
            sibling_key
        );
        assert!(result.retired_blocks().contains(&10));
        assert!(result.retired_blocks().contains(&11));
        assert!(result.retired_blocks().contains(&12));
    }

    #[test]
    fn pruning_leaf_merges_internal_children_and_collapses_two_child_root() {
        let mut device = MemoryBlockDevice::<512, 1024>::new();
        let current = ActiveSuperblock {
            superblock: Superblock::new(5, 128, VOLUME_ID, TransactionRoots::new(8, 9)).unwrap(),
            slot: SuperblockSlot::First,
        };
        let start = TreeNodeHeader::entries_offset();
        let removed_key = ObjectTreeKey::new(1, ObjectRecordKind::Metadata, 0);
        let remaining_key = ObjectTreeKey::new(2, ObjectRecordKind::Metadata, 0);
        let sibling_key = ObjectTreeKey::new(3, ObjectRecordKind::Metadata, 0);

        let mut removed_leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let removed_size = ObjectLeafRecordHeader::new(removed_key, 1)
            .unwrap()
            .encode_with_value(&mut removed_leaf[start..], b"a")
            .unwrap();
        TreeNodeHeader::new(MetadataKind::ObjectTree, 4, 12, 0, 1, removed_size as u32)
            .unwrap()
            .seal(&mut removed_leaf)
            .unwrap();

        let mut anchor = [0_u8; FILESYSTEM_BLOCK_SIZE];
        for (index, record) in [
            ObjectInternalRecord::new(removed_key, 12).unwrap(),
            ObjectInternalRecord::new(remaining_key, 13).unwrap(),
        ]
        .into_iter()
        .enumerate()
        {
            record
                .encode(
                    &mut anchor[start + index * OBJECT_INTERNAL_RECORD_SIZE
                        ..start + (index + 1) * OBJECT_INTERNAL_RECORD_SIZE],
                )
                .unwrap();
        }
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            4,
            10,
            1,
            2,
            (2 * OBJECT_INTERNAL_RECORD_SIZE) as u32,
        )
        .unwrap()
        .seal(&mut anchor)
        .unwrap();

        let mut sibling = [0_u8; FILESYSTEM_BLOCK_SIZE];
        ObjectInternalRecord::new(sibling_key, 15)
            .unwrap()
            .encode(&mut sibling[start..start + OBJECT_INTERNAL_RECORD_SIZE])
            .unwrap();
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            4,
            11,
            1,
            1,
            OBJECT_INTERNAL_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut sibling)
        .unwrap();

        let mut root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        for (index, record) in [
            ObjectInternalRecord::new(removed_key, 10).unwrap(),
            ObjectInternalRecord::new(sibling_key, 11).unwrap(),
        ]
        .into_iter()
        .enumerate()
        {
            record
                .encode(
                    &mut root[start + index * OBJECT_INTERNAL_RECORD_SIZE
                        ..start + (index + 1) * OBJECT_INTERNAL_RECORD_SIZE],
                )
                .unwrap();
        }
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            5,
            8,
            2,
            2,
            (2 * OBJECT_INTERNAL_RECORD_SIZE) as u32,
        )
        .unwrap()
        .seal(&mut root)
        .unwrap();

        let mut free_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        write_free_space_extent(&mut free_root, 0, FreeSpaceExtent::new(20, 80)).unwrap();
        TreeNodeHeader::new(
            MetadataKind::FreeSpaceTree,
            5,
            9,
            0,
            1,
            FREE_SPACE_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut free_root)
        .unwrap();

        write_filesystem_block(&mut device, 8, &root).unwrap();
        write_filesystem_block(&mut device, 10, &anchor).unwrap();
        write_filesystem_block(&mut device, 11, &sibling).unwrap();
        write_filesystem_block(&mut device, 12, &removed_leaf).unwrap();
        write_filesystem_block(&mut device, 9, &free_root).unwrap();

        let mut object_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut object_copy = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut current_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut next_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut superblock_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];

        let result = commit_object_record_delete(
            &mut device,
            current,
            removed_key,
            &mut object_buffer,
            &mut object_copy,
            &mut current_free,
            &mut next_free,
            &mut superblock_buffer,
        )
        .unwrap();

        assert_eq!(result.allocated.block_count, 2);
        assert!(result.retired_blocks().contains(&8));
        assert!(result.retired_blocks().contains(&10));
        assert!(result.retired_blocks().contains(&11));
        assert!(result.retired_blocks().contains(&12));

        let mut next_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(
            &mut device,
            result.active.superblock.roots.object_tree,
            &mut next_root,
        )
        .unwrap();
        let root_node = validate_object_internal(&next_root).unwrap();
        assert_eq!(root_node.level, 1);
        assert_eq!(root_node.item_count, 2);
        assert_eq!(
            object_internal_record_at(&next_root, 0).unwrap().key,
            remaining_key
        );
        assert_eq!(
            object_internal_record_at(&next_root, 1).unwrap().key,
            sibling_key
        );
    }

    #[test]
    fn pruning_leaf_rebalances_sparse_adjacent_internal_nodes() {
        let mut device = MemoryBlockDevice::<512, 1024>::new();
        let current = ActiveSuperblock {
            superblock: Superblock::new(5, 128, VOLUME_ID, TransactionRoots::new(8, 9)).unwrap(),
            slot: SuperblockSlot::First,
        };
        let start = TreeNodeHeader::entries_offset();
        let removed_key = ObjectTreeKey::new(1, ObjectRecordKind::Metadata, 0);
        let first_remaining_key = ObjectTreeKey::new(2, ObjectRecordKind::Metadata, 0);
        let second_remaining_key = ObjectTreeKey::new(3, ObjectRecordKind::Metadata, 0);
        let sibling_first_key = ObjectTreeKey::new(100, ObjectRecordKind::Metadata, 0);

        let mut removed_leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let removed_size = ObjectLeafRecordHeader::new(removed_key, 1)
            .unwrap()
            .encode_with_value(&mut removed_leaf[start..], b"a")
            .unwrap();
        TreeNodeHeader::new(MetadataKind::ObjectTree, 4, 12, 0, 1, removed_size as u32)
            .unwrap()
            .seal(&mut removed_leaf)
            .unwrap();

        let mut anchor = [0_u8; FILESYSTEM_BLOCK_SIZE];
        for (index, record) in [
            ObjectInternalRecord::new(removed_key, 12).unwrap(),
            ObjectInternalRecord::new(first_remaining_key, 13).unwrap(),
            ObjectInternalRecord::new(second_remaining_key, 14).unwrap(),
        ]
        .into_iter()
        .enumerate()
        {
            record
                .encode(
                    &mut anchor[start + index * OBJECT_INTERNAL_RECORD_SIZE
                        ..start + (index + 1) * OBJECT_INTERNAL_RECORD_SIZE],
                )
                .unwrap();
        }
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            4,
            10,
            1,
            3,
            (3 * OBJECT_INTERNAL_RECORD_SIZE) as u32,
        )
        .unwrap()
        .seal(&mut anchor)
        .unwrap();

        let sibling_items = object_internal_capacity();
        let mut sibling = [0_u8; FILESYSTEM_BLOCK_SIZE];
        for index in 0..sibling_items {
            let object_id = 100 + index as u64;
            ObjectInternalRecord::new(
                ObjectTreeKey::new(object_id, ObjectRecordKind::Metadata, 0),
                15,
            )
            .unwrap()
            .encode(
                &mut sibling[start + index * OBJECT_INTERNAL_RECORD_SIZE
                    ..start + (index + 1) * OBJECT_INTERNAL_RECORD_SIZE],
            )
            .unwrap();
        }
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            4,
            11,
            1,
            sibling_items as u32,
            (sibling_items * OBJECT_INTERNAL_RECORD_SIZE) as u32,
        )
        .unwrap()
        .seal(&mut sibling)
        .unwrap();

        let mut root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        for (index, record) in [
            ObjectInternalRecord::new(removed_key, 10).unwrap(),
            ObjectInternalRecord::new(sibling_first_key, 11).unwrap(),
        ]
        .into_iter()
        .enumerate()
        {
            record
                .encode(
                    &mut root[start + index * OBJECT_INTERNAL_RECORD_SIZE
                        ..start + (index + 1) * OBJECT_INTERNAL_RECORD_SIZE],
                )
                .unwrap();
        }
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            5,
            8,
            2,
            2,
            (2 * OBJECT_INTERNAL_RECORD_SIZE) as u32,
        )
        .unwrap()
        .seal(&mut root)
        .unwrap();

        let mut free_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        write_free_space_extent(&mut free_root, 0, FreeSpaceExtent::new(20, 80)).unwrap();
        TreeNodeHeader::new(
            MetadataKind::FreeSpaceTree,
            5,
            9,
            0,
            1,
            FREE_SPACE_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut free_root)
        .unwrap();

        write_filesystem_block(&mut device, 8, &root).unwrap();
        write_filesystem_block(&mut device, 10, &anchor).unwrap();
        write_filesystem_block(&mut device, 11, &sibling).unwrap();
        write_filesystem_block(&mut device, 12, &removed_leaf).unwrap();
        write_filesystem_block(&mut device, 9, &free_root).unwrap();

        let mut object_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut object_copy = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut current_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut next_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut superblock_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];

        let result = commit_object_record_delete(
            &mut device,
            current,
            removed_key,
            &mut object_buffer,
            &mut object_copy,
            &mut current_free,
            &mut next_free,
            &mut superblock_buffer,
        )
        .unwrap();

        assert_eq!(result.allocated.block_count, 4);
        assert!(result.retired_blocks().contains(&8));
        assert!(result.retired_blocks().contains(&10));
        assert!(result.retired_blocks().contains(&11));
        assert!(result.retired_blocks().contains(&12));

        let mut next_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(
            &mut device,
            result.active.superblock.roots.object_tree,
            &mut next_root,
        )
        .unwrap();
        let root_node = validate_object_internal(&next_root).unwrap();
        assert_eq!(root_node.item_count, 2);

        let left_pointer = object_internal_record_at(&next_root, 0).unwrap();
        let right_pointer = object_internal_record_at(&next_root, 1).unwrap();
        let mut left = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut right = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(&mut device, left_pointer.child_block, &mut left).unwrap();
        read_filesystem_block(&mut device, right_pointer.child_block, &mut right).unwrap();
        let left_node = validate_object_internal(&left).unwrap();
        let right_node = validate_object_internal(&right).unwrap();

        assert_eq!(left_node.item_count, 64);
        assert_eq!(right_node.item_count, 64);
        assert_eq!(
            object_internal_record_at(&left, 0).unwrap().key,
            first_remaining_key
        );
        assert_eq!(
            object_internal_record_at(&right, 0).unwrap().key,
            ObjectTreeKey::new(162, ObjectRecordKind::Metadata, 0)
        );
        assert_eq!(left_pointer.key, first_remaining_key);
        assert_eq!(
            right_pointer.key,
            ObjectTreeKey::new(162, ObjectRecordKind::Metadata, 0)
        );
    }

    #[test]
    fn deleting_last_leaf_record_collapses_single_child_parent_chain() {
        let mut device = MemoryBlockDevice::<512, 1024>::new();
        let current = ActiveSuperblock {
            superblock: Superblock::new(5, 128, VOLUME_ID, TransactionRoots::new(8, 9)).unwrap(),
            slot: SuperblockSlot::First,
        };

        let removed_key = ObjectTreeKey::new(1, ObjectRecordKind::Metadata, 0);
        let remaining_key = ObjectTreeKey::new(2, ObjectRecordKind::Metadata, 0);
        let start = TreeNodeHeader::entries_offset();

        let mut removed_leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let removed_size = ObjectLeafRecordHeader::new(removed_key, 1)
            .unwrap()
            .encode_with_value(&mut removed_leaf[start..], b"a")
            .unwrap();
        TreeNodeHeader::new(MetadataKind::ObjectTree, 4, 12, 0, 1, removed_size as u32)
            .unwrap()
            .seal(&mut removed_leaf)
            .unwrap();

        let mut remaining_leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let remaining_size = ObjectLeafRecordHeader::new(remaining_key, 1)
            .unwrap()
            .encode_with_value(&mut remaining_leaf[start..], b"b")
            .unwrap();
        TreeNodeHeader::new(MetadataKind::ObjectTree, 4, 11, 0, 1, remaining_size as u32)
            .unwrap()
            .seal(&mut remaining_leaf)
            .unwrap();

        let mut single_parent = [0_u8; FILESYSTEM_BLOCK_SIZE];
        ObjectInternalRecord::new(removed_key, 12)
            .unwrap()
            .encode(&mut single_parent[start..start + OBJECT_INTERNAL_RECORD_SIZE])
            .unwrap();
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            4,
            10,
            1,
            1,
            OBJECT_INTERNAL_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut single_parent)
        .unwrap();

        let mut root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        ObjectInternalRecord::new(removed_key, 10)
            .unwrap()
            .encode(&mut root[start..start + OBJECT_INTERNAL_RECORD_SIZE])
            .unwrap();
        ObjectInternalRecord::new(remaining_key, 11)
            .unwrap()
            .encode(
                &mut root
                    [start + OBJECT_INTERNAL_RECORD_SIZE..start + 2 * OBJECT_INTERNAL_RECORD_SIZE],
            )
            .unwrap();
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            5,
            8,
            2,
            2,
            (2 * OBJECT_INTERNAL_RECORD_SIZE) as u32,
        )
        .unwrap()
        .seal(&mut root)
        .unwrap();

        let mut free_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        write_free_space_extent(&mut free_root, 0, FreeSpaceExtent::new(20, 80)).unwrap();
        TreeNodeHeader::new(
            MetadataKind::FreeSpaceTree,
            5,
            9,
            0,
            1,
            FREE_SPACE_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut free_root)
        .unwrap();

        write_filesystem_block(&mut device, 8, &root).unwrap();
        write_filesystem_block(&mut device, 10, &single_parent).unwrap();
        write_filesystem_block(&mut device, 11, &remaining_leaf).unwrap();
        write_filesystem_block(&mut device, 12, &removed_leaf).unwrap();
        write_filesystem_block(&mut device, 9, &free_root).unwrap();

        let mut object_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut object_copy = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut current_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut next_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut superblock_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];

        let result = commit_object_record_delete(
            &mut device,
            current,
            removed_key,
            &mut object_buffer,
            &mut object_copy,
            &mut current_free,
            &mut next_free,
            &mut superblock_buffer,
        )
        .unwrap();

        assert_eq!(result.active.superblock.roots.object_tree, 11);
        assert!(result.retired_blocks().contains(&8));
        assert!(result.retired_blocks().contains(&10));
        assert!(result.retired_blocks().contains(&12));
    }

    #[test]
    fn deleting_last_leaf_record_collapses_two_child_root() {
        let mut device = MemoryBlockDevice::<512, 1024>::new();
        let current = ActiveSuperblock {
            superblock: Superblock::new(5, 128, VOLUME_ID, TransactionRoots::new(8, 9)).unwrap(),
            slot: SuperblockSlot::First,
        };

        let first_key = ObjectTreeKey::new(1, ObjectRecordKind::Metadata, 0);
        let second_key = ObjectTreeKey::new(2, ObjectRecordKind::Metadata, 0);

        let mut first_leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let start = TreeNodeHeader::entries_offset();
        let first_size = ObjectLeafRecordHeader::new(first_key, 1)
            .unwrap()
            .encode_with_value(&mut first_leaf[start..], b"a")
            .unwrap();
        TreeNodeHeader::new(MetadataKind::ObjectTree, 4, 10, 0, 1, first_size as u32)
            .unwrap()
            .seal(&mut first_leaf)
            .unwrap();

        let mut second_leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let second_size = ObjectLeafRecordHeader::new(second_key, 1)
            .unwrap()
            .encode_with_value(&mut second_leaf[start..], b"b")
            .unwrap();
        TreeNodeHeader::new(MetadataKind::ObjectTree, 4, 11, 0, 1, second_size as u32)
            .unwrap()
            .seal(&mut second_leaf)
            .unwrap();

        let mut root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        ObjectInternalRecord::new(first_key, 10)
            .unwrap()
            .encode(&mut root[start..start + OBJECT_INTERNAL_RECORD_SIZE])
            .unwrap();
        ObjectInternalRecord::new(second_key, 11)
            .unwrap()
            .encode(
                &mut root
                    [start + OBJECT_INTERNAL_RECORD_SIZE..start + 2 * OBJECT_INTERNAL_RECORD_SIZE],
            )
            .unwrap();
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            5,
            8,
            1,
            2,
            (2 * OBJECT_INTERNAL_RECORD_SIZE) as u32,
        )
        .unwrap()
        .seal(&mut root)
        .unwrap();

        let mut free_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        write_free_space_extent(&mut free_root, 0, FreeSpaceExtent::new(20, 80)).unwrap();
        TreeNodeHeader::new(
            MetadataKind::FreeSpaceTree,
            5,
            9,
            0,
            1,
            FREE_SPACE_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut free_root)
        .unwrap();

        write_filesystem_block(&mut device, 8, &root).unwrap();
        write_filesystem_block(&mut device, 10, &first_leaf).unwrap();
        write_filesystem_block(&mut device, 11, &second_leaf).unwrap();
        write_filesystem_block(&mut device, 9, &free_root).unwrap();

        let mut object_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut object_copy = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut current_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut next_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut superblock_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];

        let result = commit_object_record_delete(
            &mut device,
            current,
            first_key,
            &mut object_buffer,
            &mut object_copy,
            &mut current_free,
            &mut next_free,
            &mut superblock_buffer,
        )
        .unwrap();

        assert_eq!(result.active.superblock.roots.object_tree, 11);
        let mut remaining = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(&mut device, 11, &mut remaining).unwrap();
        find_object_leaf_record(&remaining, second_key).unwrap();
    }

    #[test]
    fn recursive_split_raises_new_root_when_parent_is_full() {
        let mut device = MemoryBlockDevice::<512, 512>::new();
        let current = ActiveSuperblock {
            superblock: Superblock::new(5, 64, VOLUME_ID, TransactionRoots::new(8, 9)).unwrap(),
            slot: SuperblockSlot::First,
        };

        let mut root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let root_start = TreeNodeHeader::entries_offset();
        for index in 0..126_usize {
            let key = ObjectTreeKey::new((index + 1) as u64, ObjectRecordKind::Metadata, 0);
            let child_block = if index == 125 { 11 } else { 10 };
            ObjectInternalRecord::new(key, child_block)
                .unwrap()
                .encode(
                    &mut root[root_start + index * OBJECT_INTERNAL_RECORD_SIZE
                        ..root_start + (index + 1) * OBJECT_INTERNAL_RECORD_SIZE],
                )
                .unwrap();
        }
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            5,
            8,
            1,
            126,
            (126 * OBJECT_INTERNAL_RECORD_SIZE) as u32,
        )
        .unwrap()
        .seal(&mut root)
        .unwrap();

        let target_leaf_block = 11_u64;
        let mut leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let leaf_start = TreeNodeHeader::entries_offset();
        let mut cursor = leaf_start;
        let payload = [0x44_u8; 192];
        for object_id in 126..=143_u64 {
            let key = ObjectTreeKey::new(object_id, ObjectRecordKind::Metadata, 0);
            let record = ObjectLeafRecordHeader::new(key, payload.len() as u32).unwrap();
            cursor += record
                .encode_with_value(&mut leaf[cursor..], &payload)
                .unwrap();
        }
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            4,
            target_leaf_block,
            0,
            18,
            (cursor - leaf_start) as u32,
        )
        .unwrap()
        .seal(&mut leaf)
        .unwrap();

        let mut free_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        write_free_space_extent(&mut free_root, 0, FreeSpaceExtent::new(20, 40)).unwrap();
        TreeNodeHeader::new(
            MetadataKind::FreeSpaceTree,
            5,
            9,
            0,
            1,
            FREE_SPACE_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut free_root)
        .unwrap();

        write_filesystem_block(&mut device, 8, &root).unwrap();
        write_filesystem_block(&mut device, target_leaf_block, &leaf).unwrap();
        write_filesystem_block(&mut device, 9, &free_root).unwrap();

        let inserted_key = ObjectTreeKey::new(144, ObjectRecordKind::Metadata, 0);
        let mut object_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut object_copy = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut current_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut next_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut superblock_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];

        let result = commit_object_record_insert(
            &mut device,
            current,
            inserted_key,
            &payload,
            &mut object_buffer,
            &mut object_copy,
            &mut current_free,
            &mut next_free,
            &mut superblock_buffer,
        )
        .unwrap();

        let mut new_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(
            &mut device,
            result.active.superblock.roots.object_tree,
            &mut new_root,
        )
        .unwrap();
        let root_node = validate_object_internal(&new_root).unwrap();
        assert_eq!(root_node.level, 2);
        assert_eq!(root_node.item_count, 2);
    }

    #[test]
    fn nonroot_leaf_split_inserts_second_child_into_parent() {
        let mut device = MemoryBlockDevice::<512, 512>::new();
        let current = ActiveSuperblock {
            superblock: Superblock::new(5, 64, VOLUME_ID, TransactionRoots::new(8, 9)).unwrap(),
            slot: SuperblockSlot::First,
        };

        let mut leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let start = TreeNodeHeader::entries_offset();
        let mut cursor = start;
        let payload = [0x5a_u8; 192];
        for object_id in 1..=18_u64 {
            let key = ObjectTreeKey::new(object_id, ObjectRecordKind::Metadata, 0);
            let record = ObjectLeafRecordHeader::new(key, payload.len() as u32).unwrap();
            cursor += record
                .encode_with_value(&mut leaf[cursor..], &payload)
                .unwrap();
        }
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            4,
            10,
            0,
            18,
            (cursor - start) as u32,
        )
        .unwrap()
        .seal(&mut leaf)
        .unwrap();

        let first_key = ObjectTreeKey::new(1, ObjectRecordKind::Metadata, 0);
        let mut root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let root_start = TreeNodeHeader::entries_offset();
        ObjectInternalRecord::new(first_key, 10)
            .unwrap()
            .encode(&mut root[root_start..root_start + OBJECT_INTERNAL_RECORD_SIZE])
            .unwrap();
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            5,
            8,
            1,
            1,
            OBJECT_INTERNAL_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut root)
        .unwrap();

        let mut free_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        write_free_space_extent(&mut free_root, 0, FreeSpaceExtent::new(20, 40)).unwrap();
        TreeNodeHeader::new(
            MetadataKind::FreeSpaceTree,
            5,
            9,
            0,
            1,
            FREE_SPACE_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut free_root)
        .unwrap();

        write_filesystem_block(&mut device, 8, &root).unwrap();
        write_filesystem_block(&mut device, 10, &leaf).unwrap();
        write_filesystem_block(&mut device, 9, &free_root).unwrap();

        let inserted_key = ObjectTreeKey::new(19, ObjectRecordKind::Metadata, 0);
        let mut object_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut object_copy = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut current_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut next_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut superblock_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];

        let result = commit_object_record_insert(
            &mut device,
            current,
            inserted_key,
            &payload,
            &mut object_buffer,
            &mut object_copy,
            &mut current_free,
            &mut next_free,
            &mut superblock_buffer,
        )
        .unwrap();

        let mut new_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(
            &mut device,
            result.active.superblock.roots.object_tree,
            &mut new_root,
        )
        .unwrap();
        let root_node = validate_object_internal(&new_root).unwrap();
        assert_eq!(root_node.level, 1);
        assert_eq!(root_node.item_count, 2);

        let right = object_internal_record_at(&new_root, 1).unwrap();
        let mut right_leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(&mut device, right.child_block, &mut right_leaf).unwrap();
        find_object_leaf_record(&right_leaf, inserted_key).unwrap();
    }

    #[test]
    fn root_leaf_split_creates_internal_root_when_insert_overflows_leaf() {
        let mut device = MemoryBlockDevice::<512, 1024>::new();
        let current = ActiveSuperblock {
            superblock: Superblock::new(5, 128, VOLUME_ID, TransactionRoots::new(8, 9)).unwrap(),
            slot: SuperblockSlot::First,
        };

        let mut root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let start = TreeNodeHeader::entries_offset();
        let mut cursor = start;
        let payload = [0x5a_u8; 192];

        for object_id in 1..=18_u64 {
            let key = ObjectTreeKey::new(object_id, ObjectRecordKind::Metadata, 0);
            let record = ObjectLeafRecordHeader::new(key, payload.len() as u32).unwrap();
            cursor += record
                .encode_with_value(&mut root[cursor..], &payload)
                .unwrap();
        }
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            5,
            8,
            0,
            18,
            (cursor - start) as u32,
        )
        .unwrap()
        .seal(&mut root)
        .unwrap();

        let mut free_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        write_free_space_extent(&mut free_root, 0, FreeSpaceExtent::new(20, 80)).unwrap();
        TreeNodeHeader::new(
            MetadataKind::FreeSpaceTree,
            5,
            9,
            0,
            1,
            FREE_SPACE_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut free_root)
        .unwrap();

        write_filesystem_block(&mut device, 8, &root).unwrap();
        write_filesystem_block(&mut device, 9, &free_root).unwrap();

        let inserted_key = ObjectTreeKey::new(19, ObjectRecordKind::Metadata, 0);
        let mut object_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut object_copy = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut current_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut next_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut superblock_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];

        let result = commit_object_record_insert(
            &mut device,
            current,
            inserted_key,
            &payload,
            &mut object_buffer,
            &mut object_copy,
            &mut current_free,
            &mut next_free,
            &mut superblock_buffer,
        )
        .unwrap();

        let mut new_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(
            &mut device,
            result.active.superblock.roots.object_tree,
            &mut new_root,
        )
        .unwrap();
        let root_node = validate_object_internal(&new_root).unwrap();
        assert_eq!(root_node.level, 1);
        assert_eq!(root_node.item_count, 2);

        let left = object_internal_record_at(&new_root, 0).unwrap();
        let right = object_internal_record_at(&new_root, 1).unwrap();
        assert_ne!(left.child_block, right.child_block);

        let mut right_leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(&mut device, right.child_block, &mut right_leaf).unwrap();
        find_object_leaf_record(&right_leaf, inserted_key).unwrap();
    }

    #[test]
    fn atomic_create_adds_metadata_and_directory_entry_in_one_generation() {
        let mut device = MemoryBlockDevice::<512, 80>::new();
        let mut format_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let current = format_volume(&mut device, VOLUME_ID, 10, &mut format_buffer).unwrap();

        let mut object_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut object_copy = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut current_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut next_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut superblock_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];

        let result = commit_create_object(
            &mut device,
            current,
            ROOT_OBJECT_ID,
            1,
            2,
            "hello",
            ObjectType::File,
            20,
            &mut object_buffer,
            &mut object_copy,
            &mut current_free,
            &mut next_free,
            &mut superblock_buffer,
        )
        .unwrap();

        assert_eq!(result.active.superblock.generation, 2);

        let found = lookup_directory_entry(
            &mut device,
            result.active.superblock.roots.object_tree,
            ROOT_OBJECT_ID,
            "hello",
            result.active.superblock.generation,
            &mut object_buffer,
        )
        .unwrap();
        assert_eq!(found.target_object_id, 2);
        assert_eq!(found.target_type, ObjectType::File);

        let mut metadata_value = [0_u8; OBJECT_METADATA_VALUE_SIZE];
        let metadata = read_object_metadata(
            &mut device,
            result.active.superblock.roots.object_tree,
            2,
            result.active.superblock.generation,
            &mut object_buffer,
            &mut metadata_value,
        )
        .unwrap();
        assert_eq!(metadata.object_type, ObjectType::File);
        assert_eq!(metadata.created_ns, 20);
    }

    #[test]
    fn atomic_create_rejects_duplicate_name_before_commit() {
        let mut device = MemoryBlockDevice::<512, 80>::new();
        let mut format_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let current = format_volume(&mut device, VOLUME_ID, 10, &mut format_buffer).unwrap();

        let mut object_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut object_copy = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut current_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut next_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut superblock_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];

        let first = commit_create_object(
            &mut device,
            current,
            ROOT_OBJECT_ID,
            1,
            2,
            "same",
            ObjectType::File,
            20,
            &mut object_buffer,
            &mut object_copy,
            &mut current_free,
            &mut next_free,
            &mut superblock_buffer,
        )
        .unwrap();

        assert_eq!(
            commit_create_object(
                &mut device,
                first.active,
                ROOT_OBJECT_ID,
                2,
                3,
                "same",
                ObjectType::File,
                30,
                &mut object_buffer,
                &mut object_copy,
                &mut current_free,
                &mut next_free,
                &mut superblock_buffer,
            ),
            Err(PhoenixFsError::ObjectRecordAlreadyExists)
        );
    }

    #[test]
    fn atomic_create_rejects_non_directory_parent() {
        let mut device = MemoryBlockDevice::<512, 80>::new();
        let mut format_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let current = format_volume(&mut device, VOLUME_ID, 10, &mut format_buffer).unwrap();

        let mut object_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut object_copy = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut current_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut next_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut superblock_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];

        let created = commit_create_object(
            &mut device,
            current,
            ROOT_OBJECT_ID,
            1,
            2,
            "file",
            ObjectType::File,
            20,
            &mut object_buffer,
            &mut object_copy,
            &mut current_free,
            &mut next_free,
            &mut superblock_buffer,
        )
        .unwrap();

        assert_eq!(
            commit_create_object(
                &mut device,
                created.active,
                2,
                1,
                3,
                "child",
                ObjectType::File,
                30,
                &mut object_buffer,
                &mut object_copy,
                &mut current_free,
                &mut next_free,
                &mut superblock_buffer,
            ),
            Err(PhoenixFsError::ParentNotDirectory)
        );
    }

    #[test]
    fn deep_record_insert_updates_first_separator_when_key_becomes_smaller() {
        let mut device = MemoryBlockDevice::<512, 512>::new();
        let old_key = ObjectTreeKey::new(10, ObjectRecordKind::Metadata, 0);
        let inserted_key = ObjectTreeKey::new(5, ObjectRecordKind::Metadata, 0);

        let mut leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let leaf_start = TreeNodeHeader::entries_offset();
        let old_record = ObjectLeafRecordHeader::new(old_key, 1).unwrap();
        let leaf_bytes = old_record
            .encode_with_value(&mut leaf[leaf_start..], b"x")
            .unwrap();
        TreeNodeHeader::new(MetadataKind::ObjectTree, 3, 10, 0, 1, leaf_bytes as u32)
            .unwrap()
            .seal(&mut leaf)
            .unwrap();

        let mut root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let root_start = TreeNodeHeader::entries_offset();
        ObjectInternalRecord::new(old_key, 10)
            .unwrap()
            .encode(&mut root[root_start..root_start + OBJECT_INTERNAL_RECORD_SIZE])
            .unwrap();
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            4,
            8,
            1,
            1,
            OBJECT_INTERNAL_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut root)
        .unwrap();

        let mut free_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        write_free_space_extent(&mut free_root, 0, FreeSpaceExtent::new(20, 10)).unwrap();
        TreeNodeHeader::new(
            MetadataKind::FreeSpaceTree,
            5,
            9,
            0,
            1,
            FREE_SPACE_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut free_root)
        .unwrap();

        write_filesystem_block(&mut device, 8, &root).unwrap();
        write_filesystem_block(&mut device, 10, &leaf).unwrap();
        write_filesystem_block(&mut device, 9, &free_root).unwrap();

        let current = ActiveSuperblock {
            superblock: Superblock::new(5, 64, VOLUME_ID, TransactionRoots::new(8, 9)).unwrap(),
            slot: SuperblockSlot::First,
        };
        let mut object_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut object_copy = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut current_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut next_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut superblock_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];

        let result = commit_object_record_insert(
            &mut device,
            current,
            inserted_key,
            b"y",
            &mut object_buffer,
            &mut object_copy,
            &mut current_free,
            &mut next_free,
            &mut superblock_buffer,
        )
        .unwrap();

        let mut new_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(
            &mut device,
            result.active.superblock.roots.object_tree,
            &mut new_root,
        )
        .unwrap();
        let pointer = object_internal_record_at(&new_root, 0).unwrap();
        assert_eq!(pointer.key, inserted_key);
    }

    #[test]
    fn deep_record_delete_updates_separator_to_remaining_first_key() {
        let mut device = MemoryBlockDevice::<512, 512>::new();
        let first_key = ObjectTreeKey::new(5, ObjectRecordKind::Metadata, 0);
        let second_key = ObjectTreeKey::new(10, ObjectRecordKind::Metadata, 0);

        let mut leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let leaf_start = TreeNodeHeader::entries_offset();
        let mut cursor = leaf_start;
        for key in [first_key, second_key] {
            let record = ObjectLeafRecordHeader::new(key, 1).unwrap();
            cursor += record.encode_with_value(&mut leaf[cursor..], b"x").unwrap();
        }
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            3,
            10,
            0,
            2,
            (cursor - leaf_start) as u32,
        )
        .unwrap()
        .seal(&mut leaf)
        .unwrap();

        let mut root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let root_start = TreeNodeHeader::entries_offset();
        ObjectInternalRecord::new(first_key, 10)
            .unwrap()
            .encode(&mut root[root_start..root_start + OBJECT_INTERNAL_RECORD_SIZE])
            .unwrap();
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            4,
            8,
            1,
            1,
            OBJECT_INTERNAL_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut root)
        .unwrap();

        let mut free_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        write_free_space_extent(&mut free_root, 0, FreeSpaceExtent::new(20, 10)).unwrap();
        TreeNodeHeader::new(
            MetadataKind::FreeSpaceTree,
            5,
            9,
            0,
            1,
            FREE_SPACE_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut free_root)
        .unwrap();

        write_filesystem_block(&mut device, 8, &root).unwrap();
        write_filesystem_block(&mut device, 10, &leaf).unwrap();
        write_filesystem_block(&mut device, 9, &free_root).unwrap();

        let current = ActiveSuperblock {
            superblock: Superblock::new(5, 64, VOLUME_ID, TransactionRoots::new(8, 9)).unwrap(),
            slot: SuperblockSlot::First,
        };
        let mut object_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut object_copy = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut current_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut next_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut superblock_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];

        let result = commit_object_record_delete(
            &mut device,
            current,
            first_key,
            &mut object_buffer,
            &mut object_copy,
            &mut current_free,
            &mut next_free,
            &mut superblock_buffer,
        )
        .unwrap();

        let mut new_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(
            &mut device,
            result.active.superblock.roots.object_tree,
            &mut new_root,
        )
        .unwrap();
        let pointer = object_internal_record_at(&new_root, 0).unwrap();
        assert_eq!(pointer.key, second_key);
    }

    #[test]
    fn deep_record_delete_rejects_emptying_non_root_leaf() {
        let mut device = MemoryBlockDevice::<512, 512>::new();
        let key = ObjectTreeKey::new(5, ObjectRecordKind::Metadata, 0);

        let mut leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let start = TreeNodeHeader::entries_offset();
        let record = ObjectLeafRecordHeader::new(key, 1).unwrap();
        let size = record.encode_with_value(&mut leaf[start..], b"x").unwrap();
        TreeNodeHeader::new(MetadataKind::ObjectTree, 3, 10, 0, 1, size as u32)
            .unwrap()
            .seal(&mut leaf)
            .unwrap();

        let mut root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let root_start = TreeNodeHeader::entries_offset();
        ObjectInternalRecord::new(key, 10)
            .unwrap()
            .encode(&mut root[root_start..root_start + OBJECT_INTERNAL_RECORD_SIZE])
            .unwrap();
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            4,
            8,
            1,
            1,
            OBJECT_INTERNAL_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut root)
        .unwrap();

        let mut free_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        write_free_space_extent(&mut free_root, 0, FreeSpaceExtent::new(20, 10)).unwrap();
        TreeNodeHeader::new(
            MetadataKind::FreeSpaceTree,
            5,
            9,
            0,
            1,
            FREE_SPACE_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut free_root)
        .unwrap();

        write_filesystem_block(&mut device, 8, &root).unwrap();
        write_filesystem_block(&mut device, 10, &leaf).unwrap();
        write_filesystem_block(&mut device, 9, &free_root).unwrap();

        let current = ActiveSuperblock {
            superblock: Superblock::new(5, 64, VOLUME_ID, TransactionRoots::new(8, 9)).unwrap(),
            slot: SuperblockSlot::First,
        };
        let mut object_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut object_copy = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut current_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut next_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut superblock_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];

        assert_eq!(
            commit_object_record_delete(
                &mut device,
                current,
                key,
                &mut object_buffer,
                &mut object_copy,
                &mut current_free,
                &mut next_free,
                &mut superblock_buffer,
            ),
            Err(PhoenixFsError::ObjectLeafWouldBecomeEmpty)
        );
    }

    #[test]
    fn deep_record_update_copies_leaf_and_parent_before_switching_root() {
        let mut device = MemoryBlockDevice::<512, 512>::new();
        let current = ActiveSuperblock {
            superblock: Superblock::new(5, 64, VOLUME_ID, TransactionRoots::new(8, 9)).unwrap(),
            slot: SuperblockSlot::First,
        };
        let key = ObjectTreeKey::new(7, ObjectRecordKind::Metadata, 0);

        let mut leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let leaf_start = TreeNodeHeader::entries_offset();
        let leaf_record = ObjectLeafRecordHeader::new(key, 3).unwrap();
        let leaf_bytes = leaf_record
            .encode_with_value(&mut leaf[leaf_start..], b"old")
            .unwrap();
        TreeNodeHeader::new(MetadataKind::ObjectTree, 3, 10, 0, 1, leaf_bytes as u32)
            .unwrap()
            .seal(&mut leaf)
            .unwrap();

        let mut root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let root_start = TreeNodeHeader::entries_offset();
        ObjectInternalRecord::new(key, 10)
            .unwrap()
            .encode(&mut root[root_start..root_start + OBJECT_INTERNAL_RECORD_SIZE])
            .unwrap();
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            4,
            8,
            1,
            1,
            OBJECT_INTERNAL_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut root)
        .unwrap();

        let mut free_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        write_free_space_extent(&mut free_root, 0, FreeSpaceExtent::new(20, 10)).unwrap();
        TreeNodeHeader::new(
            MetadataKind::FreeSpaceTree,
            5,
            9,
            0,
            1,
            FREE_SPACE_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut free_root)
        .unwrap();

        write_filesystem_block(&mut device, 8, &root).unwrap();
        write_filesystem_block(&mut device, 10, &leaf).unwrap();
        write_filesystem_block(&mut device, 9, &free_root).unwrap();

        let old_root = root;
        let old_leaf = leaf;
        let mut object_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut object_copy = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut current_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut next_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut superblock_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];

        let result = commit_object_record_update(
            &mut device,
            current,
            key,
            b"new-value",
            &mut object_buffer,
            &mut object_copy,
            &mut current_free,
            &mut next_free,
            &mut superblock_buffer,
        )
        .unwrap();

        assert_eq!(result.active.superblock.generation, 6);
        assert_eq!(
            result.active.superblock.roots,
            TransactionRoots::new(21, 22)
        );
        assert_eq!(result.retired_blocks(), &[8, 9, 10]);

        let mut new_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(&mut device, 21, &mut new_root).unwrap();
        let pointer = object_internal_record_at(&new_root, 0).unwrap();
        assert_eq!(pointer.child_block, 20);

        let mut new_leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(&mut device, 20, &mut new_leaf).unwrap();
        let (_, value, _) =
            ObjectLeafRecordHeader::decode_with_value(&new_leaf[leaf_start..]).unwrap();
        assert_eq!(value, b"new-value");

        let mut unchanged_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut unchanged_leaf = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(&mut device, 8, &mut unchanged_root).unwrap();
        read_filesystem_block(&mut device, 10, &mut unchanged_leaf).unwrap();
        assert_eq!(unchanged_root, old_root);
        assert_eq!(unchanged_leaf, old_leaf);
    }

    #[test]
    fn object_leaf_insertion_preserves_strict_key_order() {
        let first_key = ObjectTreeKey::new(1, ObjectRecordKind::Metadata, 0);
        let third_key = ObjectTreeKey::new(3, ObjectRecordKind::Metadata, 0);
        let inserted_key = ObjectTreeKey::new(2, ObjectRecordKind::Metadata, 0);
        let mut current = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let start = TreeNodeHeader::entries_offset();
        let mut cursor = start;

        for (key, value) in [(first_key, b"a".as_slice()), (third_key, b"c".as_slice())] {
            let record = ObjectLeafRecordHeader::new(key, value.len() as u32).unwrap();
            cursor += record
                .encode_with_value(&mut current[cursor..], value)
                .unwrap();
        }
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            4,
            8,
            0,
            2,
            (cursor - start) as u32,
        )
        .unwrap()
        .seal(&mut current)
        .unwrap();

        let mut next = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let node = materialize_object_leaf_with_inserted_value(
            &current,
            &mut next,
            inserted_key,
            b"b",
            5,
            20,
        )
        .unwrap();

        assert_eq!(node.item_count, 3);
        let mut keys = [first_key; 3];
        let mut offset = start;
        for key in &mut keys {
            let (record, _, size) =
                ObjectLeafRecordHeader::decode_with_value(&next[offset..]).unwrap();
            *key = record.key;
            offset += size;
        }
        assert_eq!(keys, [first_key, inserted_key, third_key]);
    }

    #[test]
    fn object_leaf_insertion_rejects_duplicate_key() {
        let key = ObjectTreeKey::new(1, ObjectRecordKind::Metadata, 0);
        let record = ObjectLeafRecordHeader::new(key, 1).unwrap();
        let mut current = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let start = TreeNodeHeader::entries_offset();
        let size = record
            .encode_with_value(&mut current[start..], b"a")
            .unwrap();
        TreeNodeHeader::new(MetadataKind::ObjectTree, 4, 8, 0, 1, size as u32)
            .unwrap()
            .seal(&mut current)
            .unwrap();

        let mut next = [0_u8; FILESYSTEM_BLOCK_SIZE];
        assert_eq!(
            materialize_object_leaf_with_inserted_value(&current, &mut next, key, b"x", 5, 20),
            Err(PhoenixFsError::ObjectRecordAlreadyExists)
        );
    }

    #[test]
    fn object_leaf_deletion_removes_only_requested_key() {
        let first_key = ObjectTreeKey::new(1, ObjectRecordKind::Metadata, 0);
        let second_key = ObjectTreeKey::new(2, ObjectRecordKind::Metadata, 0);
        let mut current = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let start = TreeNodeHeader::entries_offset();
        let mut cursor = start;

        for (key, value) in [(first_key, b"a".as_slice()), (second_key, b"b".as_slice())] {
            let record = ObjectLeafRecordHeader::new(key, value.len() as u32).unwrap();
            cursor += record
                .encode_with_value(&mut current[cursor..], value)
                .unwrap();
        }
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            4,
            8,
            0,
            2,
            (cursor - start) as u32,
        )
        .unwrap()
        .seal(&mut current)
        .unwrap();

        let mut next = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let node =
            materialize_object_leaf_without_key(&current, &mut next, first_key, 5, 20).unwrap();

        assert_eq!(node.item_count, 1);
        let (remaining, value, _) =
            ObjectLeafRecordHeader::decode_with_value(&next[start..]).unwrap();
        assert_eq!(remaining.key, second_key);
        assert_eq!(value, b"b");
    }

    #[test]
    fn object_leaf_replacement_changes_only_target_value() {
        let first_key = ObjectTreeKey::new(1, ObjectRecordKind::Metadata, 0);
        let second_key = ObjectTreeKey::new(2, ObjectRecordKind::Metadata, 0);
        let first = ObjectLeafRecordHeader::new(first_key, 3).unwrap();
        let second = ObjectLeafRecordHeader::new(second_key, 3).unwrap();
        let mut current = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let start = TreeNodeHeader::entries_offset();
        let mut cursor = start;
        cursor += first
            .encode_with_value(&mut current[cursor..], b"one")
            .unwrap();
        cursor += second
            .encode_with_value(&mut current[cursor..], b"two")
            .unwrap();
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            4,
            8,
            0,
            2,
            (cursor - start) as u32,
        )
        .unwrap()
        .seal(&mut current)
        .unwrap();

        let mut next = [0_u8; FILESYSTEM_BLOCK_SIZE];
        materialize_object_leaf_with_replaced_value(
            &current, &mut next, second_key, b"updated", 6, 20,
        )
        .unwrap();

        let (_, first_value, first_size) =
            ObjectLeafRecordHeader::decode_with_value(&next[start..]).unwrap();
        let (_, second_value, _) =
            ObjectLeafRecordHeader::decode_with_value(&next[start + first_size..]).unwrap();

        assert_eq!(first_value, b"one");
        assert_eq!(second_value, b"updated");
    }

    #[test]
    fn reclaimed_blocks_merge_adjacent_free_ranges() {
        let mut current = [0_u8; FILESYSTEM_BLOCK_SIZE];
        write_free_space_extent(&mut current, 0, FreeSpaceExtent::new(10, 2)).unwrap();
        write_free_space_extent(&mut current, 1, FreeSpaceExtent::new(14, 2)).unwrap();
        TreeNodeHeader::new(
            MetadataKind::FreeSpaceTree,
            6,
            7,
            0,
            2,
            (2 * FREE_SPACE_RECORD_SIZE) as u32,
        )
        .unwrap()
        .seal(&mut current)
        .unwrap();

        let mut next = [0_u8; FILESYSTEM_BLOCK_SIZE];
        materialize_free_space_leaf_with_reclaimed_blocks(
            &current,
            &mut next,
            64,
            8,
            30,
            &[12, 13],
        )
        .unwrap();

        let node = validate_free_space_leaf(&next, 64).unwrap();
        assert_eq!(node.item_count, 1);
        assert_eq!(
            free_space_extent_at(&next, 0).unwrap(),
            FreeSpaceExtent::new(10, 6)
        );
    }

    #[test]
    fn reclaimed_block_cannot_already_be_free() {
        let mut current = [0_u8; FILESYSTEM_BLOCK_SIZE];
        write_free_space_extent(&mut current, 0, FreeSpaceExtent::new(10, 4)).unwrap();
        TreeNodeHeader::new(
            MetadataKind::FreeSpaceTree,
            6,
            7,
            0,
            1,
            FREE_SPACE_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut current)
        .unwrap();

        let mut next = [0_u8; FILESYSTEM_BLOCK_SIZE];
        assert_eq!(
            materialize_free_space_leaf_with_reclaimed_blocks(
                &current,
                &mut next,
                64,
                8,
                30,
                &[12],
            ),
            Err(PhoenixFsError::ReclaimedBlockAlreadyFree(12))
        );
    }

    #[test]
    fn root_leaf_update_commits_new_roots_and_preserves_old_blocks() {
        let mut device = MemoryBlockDevice::<512, 512>::new();
        let current = ActiveSuperblock {
            superblock: Superblock::new(5, 64, VOLUME_ID, TransactionRoots::new(8, 9)).unwrap(),
            slot: SuperblockSlot::First,
        };
        let key = ObjectTreeKey::new(7, ObjectRecordKind::Metadata, 0);

        let mut object_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let start = TreeNodeHeader::entries_offset();
        let record = ObjectLeafRecordHeader::new(key, 3).unwrap();
        let object_bytes = record
            .encode_with_value(&mut object_root[start..], b"old")
            .unwrap();
        TreeNodeHeader::new(MetadataKind::ObjectTree, 3, 8, 0, 1, object_bytes as u32)
            .unwrap()
            .seal(&mut object_root)
            .unwrap();

        let mut free_root = [0_u8; FILESYSTEM_BLOCK_SIZE];
        write_free_space_extent(&mut free_root, 0, FreeSpaceExtent::new(20, 10)).unwrap();
        TreeNodeHeader::new(
            MetadataKind::FreeSpaceTree,
            5,
            9,
            0,
            1,
            FREE_SPACE_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut free_root)
        .unwrap();

        write_filesystem_block(&mut device, 8, &object_root).unwrap();
        write_filesystem_block(&mut device, 9, &free_root).unwrap();

        let old_object = object_root;
        let mut current_object = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut current_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut next_object = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut next_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut superblock_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];

        let result = commit_root_leaf_record_update(
            &mut device,
            current,
            key,
            b"new-value",
            &mut current_object,
            &mut current_free,
            &mut next_object,
            &mut next_free,
            &mut superblock_buffer,
        )
        .unwrap();

        assert_eq!(result.active.superblock.generation, 6);
        assert_eq!(
            result.active.superblock.roots,
            TransactionRoots::new(20, 21)
        );
        assert_eq!(result.retired_blocks, [8, 9]);

        let mut persisted_object = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(&mut device, 20, &mut persisted_object).unwrap();
        let (_, value, _) =
            ObjectLeafRecordHeader::decode_with_value(&persisted_object[start..]).unwrap();
        assert_eq!(value, b"new-value");

        let mut persisted_free = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(&mut device, 21, &mut persisted_free).unwrap();
        assert_eq!(
            free_space_extent_at(&persisted_free, 0).unwrap(),
            FreeSpaceExtent::new(22, 8)
        );

        let mut unchanged_old_object = [0_u8; FILESYSTEM_BLOCK_SIZE];
        read_filesystem_block(&mut device, 8, &mut unchanged_old_object).unwrap();
        assert_eq!(unchanged_old_object, old_object);
    }

    #[test]
    fn object_leaf_copy_moves_node_to_next_generation() {
        let key = ObjectTreeKey::new(4, ObjectRecordKind::Metadata, 0);
        let record = ObjectLeafRecordHeader::new(key, 3).unwrap();
        let mut current = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let start = TreeNodeHeader::entries_offset();
        let size = record
            .encode_with_value(&mut current[start..], b"abc")
            .unwrap();
        TreeNodeHeader::new(MetadataKind::ObjectTree, 5, 8, 0, 1, size as u32)
            .unwrap()
            .seal(&mut current)
            .unwrap();

        let mut next = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let node = materialize_object_leaf_copy(&current, &mut next, 6, 20).unwrap();

        assert_eq!(node.metadata.generation, 6);
        assert_eq!(node.metadata.block_number, 20);
        assert_eq!(&next[start..start + size], &current[start..start + size]);
    }

    #[test]
    fn object_internal_copy_repoints_only_selected_child() {
        let first_key = ObjectTreeKey::new(1, ObjectRecordKind::Metadata, 0);
        let second_key = ObjectTreeKey::new(2, ObjectRecordKind::Metadata, 0);
        let mut current = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let start = TreeNodeHeader::entries_offset();

        ObjectInternalRecord::new(first_key, 30)
            .unwrap()
            .encode(&mut current[start..start + OBJECT_INTERNAL_RECORD_SIZE])
            .unwrap();
        ObjectInternalRecord::new(second_key, 31)
            .unwrap()
            .encode(
                &mut current
                    [start + OBJECT_INTERNAL_RECORD_SIZE..start + 2 * OBJECT_INTERNAL_RECORD_SIZE],
            )
            .unwrap();
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            7,
            12,
            1,
            2,
            (2 * OBJECT_INTERNAL_RECORD_SIZE) as u32,
        )
        .unwrap()
        .seal(&mut current)
        .unwrap();

        let mut next = [0_u8; FILESYSTEM_BLOCK_SIZE];
        materialize_object_internal_after_child_copy(&current, &mut next, 8, 40, 1, 50, second_key)
            .unwrap();

        let first = ObjectInternalRecord::decode(&next[start..start + OBJECT_INTERNAL_RECORD_SIZE])
            .unwrap();
        let second = ObjectInternalRecord::decode(
            &next[start + OBJECT_INTERNAL_RECORD_SIZE..start + 2 * OBJECT_INTERNAL_RECORD_SIZE],
        )
        .unwrap();

        assert_eq!(first.child_block, 30);
        assert_eq!(second.child_block, 50);
    }

    #[test]
    fn transaction_plan_rejects_new_root_in_retired_set() {
        let current = ActiveSuperblock {
            superblock: Superblock::new(5, 64, VOLUME_ID, TransactionRoots::new(8, 9)).unwrap(),
            slot: SuperblockSlot::First,
        };
        let retired = [8, 20];

        assert_eq!(
            TransactionCommitPlan::new(current, TransactionRoots::new(20, 21), &retired,),
            Err(PhoenixFsError::RetiredBlockStillReferenced(20))
        );
    }

    #[test]
    fn committed_transaction_exposes_retired_blocks_only_after_commit() {
        let mut device = MemoryBlockDevice::<512, 512>::new();
        let current = ActiveSuperblock {
            superblock: Superblock::new(5, 64, VOLUME_ID, TransactionRoots::new(8, 9)).unwrap(),
            slot: SuperblockSlot::First,
        };
        let retired = [8, 9];
        let plan =
            TransactionCommitPlan::new(current, TransactionRoots::new(20, 21), &retired).unwrap();
        let mut buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];

        let committed = commit_transaction(&mut device, current, plan, &mut buffer).unwrap();

        assert_eq!(committed.active.superblock.generation, 6);
        assert_eq!(
            committed.active.superblock.roots,
            TransactionRoots::new(20, 21)
        );
        assert_eq!(committed.retired_blocks(), &retired);
    }

    #[test]
    fn free_space_leaf_accepts_sorted_non_adjacent_extents() {
        let mut block = [0_u8; FILESYSTEM_BLOCK_SIZE];
        write_free_space_extent(&mut block, 0, FreeSpaceExtent::new(10, 5)).unwrap();
        write_free_space_extent(&mut block, 1, FreeSpaceExtent::new(20, 8)).unwrap();
        let node = TreeNodeHeader::new(
            MetadataKind::FreeSpaceTree,
            4,
            7,
            0,
            2,
            (2 * FREE_SPACE_RECORD_SIZE) as u32,
        )
        .unwrap();
        node.seal(&mut block).unwrap();

        assert_eq!(validate_free_space_leaf(&block, 64), Ok(node));
    }

    #[test]
    fn free_space_leaf_rejects_adjacent_ranges() {
        let mut block = [0_u8; FILESYSTEM_BLOCK_SIZE];
        write_free_space_extent(&mut block, 0, FreeSpaceExtent::new(10, 5)).unwrap();
        write_free_space_extent(&mut block, 1, FreeSpaceExtent::new(15, 4)).unwrap();
        TreeNodeHeader::new(
            MetadataKind::FreeSpaceTree,
            4,
            7,
            0,
            2,
            (2 * FREE_SPACE_RECORD_SIZE) as u32,
        )
        .unwrap()
        .seal(&mut block)
        .unwrap();

        assert_eq!(
            validate_free_space_leaf(&block, 64),
            Err(PhoenixFsError::FreeSpaceRangesNotCanonical)
        );
    }

    #[test]
    fn cow_allocation_uses_first_sufficient_extent() {
        let mut block = [0_u8; FILESYSTEM_BLOCK_SIZE];
        write_free_space_extent(&mut block, 0, FreeSpaceExtent::new(10, 2)).unwrap();
        write_free_space_extent(&mut block, 1, FreeSpaceExtent::new(20, 8)).unwrap();
        TreeNodeHeader::new(
            MetadataKind::FreeSpaceTree,
            6,
            7,
            0,
            2,
            (2 * FREE_SPACE_RECORD_SIZE) as u32,
        )
        .unwrap()
        .seal(&mut block)
        .unwrap();

        let plan = plan_cow_allocation(&block, 64, 3).unwrap();
        assert_eq!(plan.record_index, 1);
        assert_eq!(plan.allocated, FreeSpaceExtent::new(20, 3));
        assert_eq!(plan.remaining, Some(FreeSpaceExtent::new(23, 5)));
    }

    #[test]
    fn cow_allocation_materializes_next_generation_without_touching_source() {
        let mut current = [0_u8; FILESYSTEM_BLOCK_SIZE];
        write_free_space_extent(&mut current, 0, FreeSpaceExtent::new(10, 10)).unwrap();
        TreeNodeHeader::new(
            MetadataKind::FreeSpaceTree,
            6,
            7,
            0,
            1,
            FREE_SPACE_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut current)
        .unwrap();
        let before = current;

        let plan = plan_cow_allocation(&current, 64, 3).unwrap();
        let mut next = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let node =
            materialize_free_space_leaf_after_allocation(&current, &mut next, 64, 7, 10, plan)
                .unwrap();

        assert_eq!(current, before);
        assert_eq!(node.metadata.generation, 7);
        assert_eq!(node.metadata.block_number, 10);
        assert_eq!(
            free_space_extent_at(&next, 0).unwrap(),
            FreeSpaceExtent::new(13, 7)
        );
    }

    #[test]
    fn free_space_copy_accepts_older_shared_generation() {
        let mut current = [0_u8; FILESYSTEM_BLOCK_SIZE];
        write_free_space_extent(&mut current, 0, FreeSpaceExtent::new(10, 8)).unwrap();
        TreeNodeHeader::new(
            MetadataKind::FreeSpaceTree,
            2,
            7,
            0,
            1,
            FREE_SPACE_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut current)
        .unwrap();

        let plan = plan_cow_allocation(&current, 64, 2).unwrap();
        let mut next = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let node =
            materialize_free_space_leaf_after_allocation(&current, &mut next, 64, 6, 10, plan)
                .unwrap();

        assert_eq!(node.metadata.generation, 6);
    }

    #[test]
    fn cow_allocation_rejects_target_outside_reserved_range() {
        let mut current = [0_u8; FILESYSTEM_BLOCK_SIZE];
        write_free_space_extent(&mut current, 0, FreeSpaceExtent::new(10, 10)).unwrap();
        TreeNodeHeader::new(
            MetadataKind::FreeSpaceTree,
            6,
            7,
            0,
            1,
            FREE_SPACE_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut current)
        .unwrap();

        let plan = plan_cow_allocation(&current, 64, 2).unwrap();
        let mut next = [0_u8; FILESYSTEM_BLOCK_SIZE];
        assert_eq!(
            materialize_free_space_leaf_after_allocation(&current, &mut next, 64, 7, 30, plan,),
            Err(PhoenixFsError::AllocationTargetOutsideRange)
        );
    }

    #[test]
    fn object_internal_record_round_trip_preserves_key_and_child() {
        let record =
            ObjectInternalRecord::new(ObjectTreeKey::new(7, ObjectRecordKind::Extent, 8192), 33)
                .unwrap();
        let mut encoded = [0_u8; OBJECT_INTERNAL_RECORD_SIZE];

        record.encode(&mut encoded).unwrap();
        assert_eq!(ObjectInternalRecord::decode(&encoded), Ok(record));
    }

    #[test]
    fn object_internal_node_requires_strict_keys() {
        let first =
            ObjectInternalRecord::new(ObjectTreeKey::new(1, ObjectRecordKind::Metadata, 0), 20)
                .unwrap();
        let second =
            ObjectInternalRecord::new(ObjectTreeKey::new(1, ObjectRecordKind::Extent, 4096), 21)
                .unwrap();

        let mut block = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let start = TreeNodeHeader::entries_offset();
        first
            .encode(&mut block[start..start + OBJECT_INTERNAL_RECORD_SIZE])
            .unwrap();
        second
            .encode(
                &mut block
                    [start + OBJECT_INTERNAL_RECORD_SIZE..start + 2 * OBJECT_INTERNAL_RECORD_SIZE],
            )
            .unwrap();

        let node = TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            8,
            10,
            1,
            2,
            (2 * OBJECT_INTERNAL_RECORD_SIZE) as u32,
        )
        .unwrap();
        node.seal(&mut block).unwrap();

        assert_eq!(validate_object_internal(&block), Ok(node));
    }

    #[test]
    fn object_tree_transition_checks_level_generation_block_and_first_key() {
        let key = ObjectTreeKey::new(5, ObjectRecordKind::Metadata, 0);
        let pointer = ObjectInternalRecord::new(key, 12).unwrap();

        let mut parent = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let parent_start = TreeNodeHeader::entries_offset();
        pointer
            .encode(&mut parent[parent_start..parent_start + OBJECT_INTERNAL_RECORD_SIZE])
            .unwrap();
        let parent_header = TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            9,
            11,
            1,
            1,
            OBJECT_INTERNAL_RECORD_SIZE as u32,
        )
        .unwrap();
        parent_header.seal(&mut parent).unwrap();

        let leaf_record = ObjectLeafRecordHeader::new(key, 3).unwrap();
        let mut child = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let child_start = TreeNodeHeader::entries_offset();
        let child_size = leaf_record
            .encode_with_value(&mut child[child_start..], b"abc")
            .unwrap();
        let child_header =
            TreeNodeHeader::new(MetadataKind::ObjectTree, 8, 12, 0, 1, child_size as u32).unwrap();
        child_header.seal(&mut child).unwrap();

        assert_eq!(
            validate_object_tree_transition(&parent, 0, &child),
            Ok(pointer)
        );
    }

    #[test]
    fn object_tree_transition_rejects_child_from_future_generation() {
        let key = ObjectTreeKey::new(5, ObjectRecordKind::Metadata, 0);
        let pointer = ObjectInternalRecord::new(key, 12).unwrap();

        let mut parent = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let start = TreeNodeHeader::entries_offset();
        pointer
            .encode(&mut parent[start..start + OBJECT_INTERNAL_RECORD_SIZE])
            .unwrap();
        TreeNodeHeader::new(
            MetadataKind::ObjectTree,
            9,
            11,
            1,
            1,
            OBJECT_INTERNAL_RECORD_SIZE as u32,
        )
        .unwrap()
        .seal(&mut parent)
        .unwrap();

        let leaf_record = ObjectLeafRecordHeader::new(key, 0).unwrap();
        let mut child = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let child_start = TreeNodeHeader::entries_offset();
        let child_size = leaf_record
            .encode_with_value(&mut child[child_start..], b"")
            .unwrap();
        TreeNodeHeader::new(MetadataKind::ObjectTree, 10, 12, 0, 1, child_size as u32)
            .unwrap()
            .seal(&mut child)
            .unwrap();

        assert_eq!(
            validate_object_tree_transition(&parent, 0, &child),
            Err(PhoenixFsError::ObjectChildGenerationAhead)
        );
    }

    #[test]
    fn object_leaf_record_round_trip_preserves_key_and_value() {
        let key = ObjectTreeKey::new(42, ObjectRecordKind::Metadata, 0);
        let record = ObjectLeafRecordHeader::new(key, 5).unwrap();
        let mut encoded = [0_u8; 64];

        let size = record.encode_with_value(&mut encoded, b"hello").unwrap();
        let (decoded, value, decoded_size) =
            ObjectLeafRecordHeader::decode_with_value(&encoded[..size]).unwrap();

        assert_eq!(decoded, record);
        assert_eq!(value, b"hello");
        assert_eq!(decoded_size, size);
        assert_eq!(size % OBJECT_LEAF_RECORD_ALIGNMENT, 0);
    }

    #[test]
    fn object_leaf_validates_strict_key_order() {
        let first =
            ObjectLeafRecordHeader::new(ObjectTreeKey::new(1, ObjectRecordKind::Metadata, 0), 3)
                .unwrap();
        let second =
            ObjectLeafRecordHeader::new(ObjectTreeKey::new(1, ObjectRecordKind::Extent, 4096), 4)
                .unwrap();

        let mut block = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut cursor = TreeNodeHeader::entries_offset();
        cursor += first
            .encode_with_value(&mut block[cursor..], b"one")
            .unwrap();
        cursor += second
            .encode_with_value(&mut block[cursor..], b"data")
            .unwrap();

        let entries_bytes = u32::try_from(cursor - TreeNodeHeader::entries_offset()).unwrap();
        let node =
            TreeNodeHeader::new(MetadataKind::ObjectTree, 5, 7, 0, 2, entries_bytes).unwrap();
        node.seal(&mut block).unwrap();

        assert_eq!(validate_object_leaf(&block), Ok(node));
    }

    #[test]
    fn object_leaf_rejects_duplicate_or_unsorted_key() {
        let key = ObjectTreeKey::new(9, ObjectRecordKind::DirectoryEntry, 0);
        let record = ObjectLeafRecordHeader::new(key, 1).unwrap();

        let mut block = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut cursor = TreeNodeHeader::entries_offset();
        cursor += record
            .encode_with_value(&mut block[cursor..], b"a")
            .unwrap();
        cursor += record
            .encode_with_value(&mut block[cursor..], b"b")
            .unwrap();

        let entries_bytes = u32::try_from(cursor - TreeNodeHeader::entries_offset()).unwrap();
        let node =
            TreeNodeHeader::new(MetadataKind::ObjectTree, 3, 6, 0, 2, entries_bytes).unwrap();
        node.seal(&mut block).unwrap();

        assert_eq!(
            validate_object_leaf(&block),
            Err(PhoenixFsError::ObjectRecordsOutOfOrder)
        );
    }

    #[test]
    fn object_leaf_rejects_zero_object_id() {
        assert_eq!(
            ObjectLeafRecordHeader::new(ObjectTreeKey::new(0, ObjectRecordKind::Metadata, 0), 0,),
            Err(PhoenixFsError::InvalidObjectId)
        );
    }

    #[test]
    fn superblock_round_trip_preserves_identity_and_generation() {
        let superblock = Superblock::new(7, 128, VOLUME_ID, ROOTS).unwrap();
        let mut encoded = [0_u8; FILESYSTEM_BLOCK_SIZE];

        superblock.encode(&mut encoded).unwrap();

        assert_eq!(Superblock::decode(&encoded), Ok(superblock));
    }

    #[test]
    fn superblock_detects_corrupted_contents() {
        let superblock = Superblock::new(1, 128, VOLUME_ID, ROOTS).unwrap();
        let mut encoded = [0_u8; FILESYSTEM_BLOCK_SIZE];
        superblock.encode(&mut encoded).unwrap();

        encoded[100] ^= 0x80;

        assert_eq!(
            Superblock::decode(&encoded),
            Err(PhoenixFsError::ChecksumMismatch)
        );
    }

    #[test]
    fn selects_newest_valid_superblock_copy() {
        let mut device = MemoryBlockDevice::<512, 64>::new();
        let older = Superblock::new(4, 8, VOLUME_ID, ROOTS).unwrap();
        let newer = Superblock::new(5, 8, VOLUME_ID, ROOTS).unwrap();
        let mut first = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second = [0_u8; FILESYSTEM_BLOCK_SIZE];
        older.encode(&mut first).unwrap();
        newer.encode(&mut second).unwrap();

        device.write_blocks(0, &first).unwrap();
        device.write_blocks(8, &second).unwrap();

        let mut first_read = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second_read = [0_u8; FILESYSTEM_BLOCK_SIZE];
        assert_eq!(
            read_active_superblock(&mut device, &mut first_read, &mut second_read),
            Ok(ActiveSuperblock {
                superblock: newer,
                slot: SuperblockSlot::Second,
            })
        );
    }

    #[test]
    fn falls_back_to_older_copy_when_newer_copy_is_corrupted() {
        let mut device = MemoryBlockDevice::<512, 64>::new();
        let older = Superblock::new(9, 8, VOLUME_ID, ROOTS).unwrap();
        let newer = Superblock::new(10, 8, VOLUME_ID, ROOTS).unwrap();
        let mut first = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second = [0_u8; FILESYSTEM_BLOCK_SIZE];
        older.encode(&mut first).unwrap();
        newer.encode(&mut second).unwrap();
        second[80] ^= 1;

        device.write_blocks(0, &first).unwrap();
        device.write_blocks(8, &second).unwrap();

        let mut first_read = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second_read = [0_u8; FILESYSTEM_BLOCK_SIZE];
        assert_eq!(
            read_active_superblock(&mut device, &mut first_read, &mut second_read),
            Ok(ActiveSuperblock {
                superblock: older,
                slot: SuperblockSlot::First,
            })
        );
    }

    #[test]
    fn rejects_equal_generation_with_different_metadata() {
        let mut device = MemoryBlockDevice::<512, 64>::new();
        let first_superblock = Superblock::new(3, 8, VOLUME_ID, ROOTS).unwrap();
        let mut other_id = VOLUME_ID;
        other_id[0] ^= 1;
        let second_superblock = Superblock::new(3, 8, other_id, ROOTS).unwrap();
        let mut first = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second = [0_u8; FILESYSTEM_BLOCK_SIZE];
        first_superblock.encode(&mut first).unwrap();
        second_superblock.encode(&mut second).unwrap();

        device.write_blocks(0, &first).unwrap();
        device.write_blocks(8, &second).unwrap();

        let mut first_read = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second_read = [0_u8; FILESYSTEM_BLOCK_SIZE];
        assert_eq!(
            read_active_superblock(&mut device, &mut first_read, &mut second_read),
            Err(PhoenixFsError::ConflictingGeneration(3))
        );
    }

    #[test]
    fn commits_next_generation_to_inactive_copy_and_flushes() {
        let mut device = MemoryBlockDevice::<512, 64>::new();
        let current_superblock = Superblock::new(11, 8, VOLUME_ID, ROOTS).unwrap();
        let next_superblock = Superblock::new(12, 8, VOLUME_ID, ROOTS).unwrap();
        let mut encoded = [0_u8; FILESYSTEM_BLOCK_SIZE];
        current_superblock.encode(&mut encoded).unwrap();
        device.write_blocks(0, &encoded).unwrap();
        device.flush().unwrap();

        let current = ActiveSuperblock {
            superblock: current_superblock,
            slot: SuperblockSlot::First,
        };
        let committed =
            commit_next_superblock(&mut device, current, next_superblock, &mut encoded).unwrap();

        assert_eq!(
            committed,
            ActiveSuperblock {
                superblock: next_superblock,
                slot: SuperblockSlot::Second,
            }
        );
        assert!(!device.is_dirty());

        let mut first_read = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second_read = [0_u8; FILESYSTEM_BLOCK_SIZE];
        assert_eq!(
            read_active_superblock(&mut device, &mut first_read, &mut second_read),
            Ok(committed)
        );
    }

    #[test]
    fn next_generation_can_switch_transaction_roots() {
        let mut device = MemoryBlockDevice::<512, 64>::new();
        let current_superblock = Superblock::new(20, 8, VOLUME_ID, ROOTS).unwrap();
        let next_roots = TransactionRoots::new(4, 5);
        let next_superblock = Superblock::new(21, 8, VOLUME_ID, next_roots).unwrap();
        let mut buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];

        current_superblock.encode(&mut buffer).unwrap();
        device.write_blocks(0, &buffer).unwrap();
        device.flush().unwrap();

        let current = ActiveSuperblock {
            superblock: current_superblock,
            slot: SuperblockSlot::First,
        };
        let committed =
            commit_next_superblock(&mut device, current, next_superblock, &mut buffer).unwrap();

        assert_eq!(committed.superblock.roots, next_roots);
        assert_eq!(committed.slot, SuperblockSlot::Second);
    }

    #[test]
    fn rejects_skipped_generation_before_writing() {
        let mut device = MemoryBlockDevice::<512, 64>::new();
        let current = ActiveSuperblock {
            superblock: Superblock::new(2, 8, VOLUME_ID, ROOTS).unwrap(),
            slot: SuperblockSlot::First,
        };
        let skipped = Superblock::new(4, 8, VOLUME_ID, ROOTS).unwrap();
        let mut buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];

        assert_eq!(
            commit_next_superblock(&mut device, current, skipped, &mut buffer),
            Err(PhoenixFsError::GenerationSequence)
        );
        assert!(!device.is_dirty());
    }

    #[test]
    fn superblock_preserves_transaction_roots() {
        let roots = TransactionRoots::new(7, 9);
        let superblock = Superblock::new(6, 128, VOLUME_ID, roots).unwrap();
        let mut encoded = [0_u8; FILESYSTEM_BLOCK_SIZE];

        superblock.encode(&mut encoded).unwrap();

        assert_eq!(Superblock::decode(&encoded).unwrap().roots, roots);
    }

    #[test]
    fn phoenix_vfs_rewrites_existing_file_atomically() {
        use phoenix_vfs::{FileSystem, NodeId, NodeKind};

        let device = MemoryBlockDevice::<512, 128>::new();
        let mut buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut filesystem = PhoenixVfs::format_new(device, VOLUME_ID, 1, &mut buffer).unwrap();
        let file = filesystem
            .create_node(NodeId(ROOT_OBJECT_ID), b"note.txt", NodeKind::File)
            .unwrap();

        assert_eq!(filesystem.write_node(file, 0, b"hello").unwrap(), 5);
        let first_generation = filesystem.active_superblock().superblock.generation;
        assert_eq!(filesystem.write_node(file, 0, b"world").unwrap(), 5);
        assert_eq!(
            filesystem.active_superblock().superblock.generation,
            first_generation + 1
        );

        let mut output = [0_u8; 5];
        assert_eq!(filesystem.read_node(file, 0, &mut output).unwrap(), 5);
        assert_eq!(&output, b"world");

        let device = filesystem.into_device();
        let mut first = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let remounted = PhoenixVfs::mount(device, &mut first, &mut second).unwrap();
        let mut output = [0_u8; 5];
        assert_eq!(remounted.read_node(file, 0, &mut output).unwrap(), 5);
        assert_eq!(&output, b"world");
    }

    #[test]
    fn phoenix_vfs_truncates_existing_file_to_zero_and_remounts() {
        use phoenix_vfs::{FileSystem, NodeId, NodeKind};

        let device = MemoryBlockDevice::<512, 256>::new();
        let mut buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut filesystem = PhoenixVfs::format_new(device, VOLUME_ID, 1, &mut buffer).unwrap();
        let file = filesystem
            .create_node(NodeId(ROOT_OBJECT_ID), b"truncate.txt", NodeKind::File)
            .unwrap();
        assert_eq!(filesystem.write_node(file, 0, b"hello").unwrap(), 5);

        let before = filesystem.active_superblock().superblock.generation;
        filesystem.truncate_node(file, 0).unwrap();
        assert_eq!(filesystem.metadata(file).unwrap().length, 0);
        assert_eq!(
            filesystem.active_superblock().superblock.generation,
            before + 1
        );
        assert_eq!(filesystem.truncate_node(file, 0), Ok(()));
        assert_eq!(
            filesystem.truncate_node(file, 1),
            Err(VfsError::InvalidOffset)
        );

        let device = filesystem.into_device();
        let mut first = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let mut second = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let remounted = PhoenixVfs::mount(device, &mut first, &mut second).unwrap();
        assert_eq!(remounted.metadata(file).unwrap().length, 0);
        let mut output = [0_u8; 5];
        assert_eq!(remounted.read_node(file, 0, &mut output).unwrap(), 0);
    }

    #[test]
    fn rejects_roots_outside_data_area() {
        assert_eq!(
            Superblock::new(
                1,
                8,
                VOLUME_ID,
                TransactionRoots::new(SuperblockSlot::First.filesystem_block(), 3),
            ),
            Err(PhoenixFsError::InvalidRootBlock(0))
        );
        assert_eq!(
            Superblock::new(1, 8, VOLUME_ID, TransactionRoots::new(2, 8)),
            Err(PhoenixFsError::InvalidRootBlock(8))
        );
    }

    #[test]
    fn rejects_duplicate_transaction_roots() {
        assert_eq!(
            Superblock::new(1, 8, VOLUME_ID, TransactionRoots::new(4, 4)),
            Err(PhoenixFsError::DuplicateRootBlock(4))
        );
    }

    #[test]
    fn rejects_device_block_size_that_cannot_form_four_kib_block() {
        let device = MemoryBlockDevice::<1000, 8>::new();

        assert_eq!(
            filesystem_block_count(&device),
            Err(PhoenixFsError::UnsupportedDeviceBlockSize)
        );
    }
}
