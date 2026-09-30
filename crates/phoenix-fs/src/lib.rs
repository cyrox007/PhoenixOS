#![no_std]

use phoenix_block::{BlockDevice, BlockError};

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
    if new_generation != current_node.metadata.generation + 1 {
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

pub fn materialize_object_internal_after_child_copy(
    current: &[u8],
    destination: &mut [u8],
    new_generation: u64,
    new_block_number: u64,
    child_index: usize,
    new_child_block: u64,
    new_child_first_key: ObjectTreeKey,
) -> Result<TreeNodeHeader, PhoenixFsError> {
    let current_node = validate_object_internal(current)?;
    validate_rewrite_target(current_node, new_generation, new_block_number)?;

    if child_index >= current_node.item_count as usize {
        return Err(PhoenixFsError::InvalidObjectRewriteTarget);
    }
    validate_object_child_block(new_child_block)?;
    if new_child_block == new_block_number {
        return Err(PhoenixFsError::InvalidObjectRewriteTarget);
    }

    destination.fill(0);
    let start = TreeNodeHeader::entries_offset();
    let entries_bytes = current_node.entries_bytes as usize;
    let end = start
        .checked_add(entries_bytes)
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    destination[start..end].copy_from_slice(&current[start..end]);

    let record_offset = start
        .checked_add(
            child_index
                .checked_mul(OBJECT_INTERNAL_RECORD_SIZE)
                .ok_or(PhoenixFsError::ArithmeticOverflow)?,
        )
        .ok_or(PhoenixFsError::ArithmeticOverflow)?;
    ObjectInternalRecord::new(new_child_first_key, new_child_block)?
        .encode(&mut destination[record_offset..record_offset + OBJECT_INTERNAL_RECORD_SIZE])?;

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
    fn object_leaf_copy_accepts_older_shared_generation() {
        let key = ObjectTreeKey::new(4, ObjectRecordKind::Metadata, 0);
        let record = ObjectLeafRecordHeader::new(key, 1).unwrap();
        let mut current = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let start = TreeNodeHeader::entries_offset();
        let size = record
            .encode_with_value(&mut current[start..], b"x")
            .unwrap();
        TreeNodeHeader::new(MetadataKind::ObjectTree, 2, 8, 0, 1, size as u32)
            .unwrap()
            .seal(&mut current)
            .unwrap();

        let mut next = [0_u8; FILESYSTEM_BLOCK_SIZE];
        let node = materialize_object_leaf_copy(&current, &mut next, 6, 20).unwrap();

        assert_eq!(node.metadata.generation, 6);
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
        let mut device = MemoryBlockDevice::<{ FILESYSTEM_BLOCK_SIZE * 64 }>::new(512).unwrap();
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
