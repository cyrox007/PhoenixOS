#![no_std]

pub const NAME_CAPACITY: usize = 63;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Directory,
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsError {
    InvalidPath,
    NameTooLong,
    AlreadyExists,
    NotFound,
    NotDirectory,
    IsDirectory,
    NoSpace,
    FileTooLarge,
    InvalidOffset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileSystemId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VfsNode {
    pub filesystem: FileSystemId,
    pub node: NodeId,
}

impl VfsNode {
    pub const fn new(filesystem: FileSystemId, node: NodeId) -> Self {
        Self { filesystem, node }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountPoint {
    pub location: VfsNode,
    pub root: VfsNode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountError {
    TableFull,
    AlreadyMounted,
    NotMounted,
}

pub struct MountTable<const CAPACITY: usize> {
    root: VfsNode,
    mounts: [Option<MountPoint>; CAPACITY],
    count: usize,
}

impl<const CAPACITY: usize> MountTable<CAPACITY> {
    pub const fn new(root: VfsNode) -> Self {
        Self {
            root,
            mounts: [None; CAPACITY],
            count: 0,
        }
    }

    pub const fn root(&self) -> VfsNode {
        self.root
    }

    pub const fn len(&self) -> usize {
        self.count
    }

    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn mount(&mut self, location: VfsNode, root: VfsNode) -> Result<(), MountError> {
        if self
            .mounts
            .iter()
            .flatten()
            .any(|point| point.location == location)
        {
            return Err(MountError::AlreadyMounted);
        }

        let Some(slot) = self.mounts.iter_mut().find(|slot| slot.is_none()) else {
            return Err(MountError::TableFull);
        };
        *slot = Some(MountPoint { location, root });
        self.count += 1;
        Ok(())
    }

    pub fn unmount(&mut self, location: VfsNode) -> Result<MountPoint, MountError> {
        let Some(slot) = self
            .mounts
            .iter_mut()
            .find(|slot| slot.is_some_and(|point| point.location == location))
        else {
            return Err(MountError::NotMounted);
        };
        let point = slot.take().ok_or(MountError::NotMounted)?;
        self.count -= 1;
        Ok(point)
    }

    pub fn cross_mount(&self, node: VfsNode) -> VfsNode {
        self.mounts
            .iter()
            .flatten()
            .find(|point| point.location == node)
            .map(|point| point.root)
            .unwrap_or(node)
    }
}

pub trait NamespaceLookup {
    fn lookup_child(&self, parent: VfsNode, name: &[u8]) -> Result<NodeId, VfsError>;
}

pub fn resolve_mounted_path<L: NamespaceLookup + ?Sized, const CAPACITY: usize>(
    lookup: &L,
    mounts: &MountTable<CAPACITY>,
    path: &str,
) -> Result<VfsNode, VfsError> {
    let mut current = mounts.cross_mount(mounts.root());
    if path == "/" {
        return Ok(current);
    }
    if !path.starts_with('/') || path.ends_with('/') {
        return Err(VfsError::InvalidPath);
    }

    for component in path[1..].split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(VfsError::InvalidPath);
        }
    }

    for component in path[1..].split('/') {
        current = mounts.cross_mount(current);
        let child = lookup.lookup_child(current, component.as_bytes())?;
        current = VfsNode::new(current.filesystem, child);
    }

    Ok(mounts.cross_mount(current))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeMetadata {
    pub kind: NodeKind,
    pub length: u64,
}

/// Независимые от конкретной файловой системы операции с узлами для ВФС и дескрипторов.
///
/// Пути намеренно остаются вне этого контракта. Слой монтирования может разрешать путь
/// по одному компоненту, а затем сохранять полученный `NodeId` в открытом описании
/// файла без привязки вызывающего кода к конкретной реализации файловой системы.
pub trait FileSystem {
    fn root_node(&self) -> Result<NodeId, VfsError>;
    fn metadata(&self, node: NodeId) -> Result<NodeMetadata, VfsError>;
    fn lookup_child(&self, parent: NodeId, name: &[u8]) -> Result<NodeId, VfsError>;
    fn create_node(
        &mut self,
        parent: NodeId,
        name: &[u8],
        kind: NodeKind,
    ) -> Result<NodeId, VfsError>;
    fn read_node(&self, node: NodeId, offset: u64, output: &mut [u8]) -> Result<usize, VfsError>;
    fn write_node(&mut self, node: NodeId, offset: u64, data: &[u8]) -> Result<usize, VfsError>;
    fn truncate_node(&mut self, node: NodeId, length: u64) -> Result<(), VfsError>;
}

pub fn resolve_path<F: FileSystem + ?Sized>(
    filesystem: &F,
    path: &str,
) -> Result<NodeId, VfsError> {
    if path == "/" {
        return filesystem.root_node();
    }
    if !path.starts_with('/') || path.ends_with('/') {
        return Err(VfsError::InvalidPath);
    }

    let mut current = filesystem.root_node()?;
    for component in path[1..].split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(VfsError::InvalidPath);
        }
        current = filesystem.lookup_child(current, component.as_bytes())?;
    }
    Ok(current)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessMode {
    ReadOnly,
    WriteOnly,
    ReadWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekOrigin {
    Start,
    Current,
    End,
}

impl AccessMode {
    const fn can_read(self) -> bool {
        matches!(self, Self::ReadOnly | Self::ReadWrite)
    }

    const fn can_write(self) -> bool {
        matches!(self, Self::WriteOnly | Self::ReadWrite)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct FileDescriptor {
    pub slot: u32,
    pub generation: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DescriptorError {
    BadDescriptor,
    PermissionDenied,
    TableFull,
    OffsetOverflow,
    Vfs(VfsError),
}

impl From<VfsError> for DescriptorError {
    fn from(error: VfsError) -> Self {
        Self::Vfs(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OpenFile {
    node: NodeId,
    position: u64,
    access: AccessMode,
    references: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DescriptorSlot {
    generation: u32,
    open_file: Option<u32>,
}

impl DescriptorSlot {
    const EMPTY: Self = Self {
        generation: 1,
        open_file: None,
    };
}

/// Таблица дескрипторов процесса с разделяемыми открытыми описаниями файлов.
///
/// Дублированные дескрипторы ссылаются на одно открытое описание и поэтому
/// совместно изменяют текущую позицию. Независимые вызовы `open` создают
/// разные описания даже для одного узла.
pub struct DescriptorTable<const CAPACITY: usize> {
    slots: [DescriptorSlot; CAPACITY],
    open_files: [Option<OpenFile>; CAPACITY],
    count: usize,
}

impl<const CAPACITY: usize> DescriptorTable<CAPACITY> {
    pub const fn new() -> Self {
        Self {
            slots: [DescriptorSlot::EMPTY; CAPACITY],
            open_files: [None; CAPACITY],
            count: 0,
        }
    }

    pub const fn len(&self) -> usize {
        self.count
    }

    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn open<F: FileSystem + ?Sized>(
        &mut self,
        filesystem: &F,
        node: NodeId,
        access: AccessMode,
    ) -> Result<FileDescriptor, DescriptorError> {
        if filesystem.metadata(node)?.kind != NodeKind::File {
            return Err(DescriptorError::Vfs(VfsError::IsDirectory));
        }

        let descriptor_index = self.free_descriptor_slot()?;
        let open_file_index = self.free_open_file_slot()?;
        self.open_files[open_file_index] = Some(OpenFile {
            node,
            position: 0,
            access,
            references: 1,
        });
        self.install_descriptor(descriptor_index, open_file_index);
        Ok(self.descriptor(descriptor_index))
    }

    pub fn duplicate(
        &mut self,
        descriptor: FileDescriptor,
    ) -> Result<FileDescriptor, DescriptorError> {
        let open_file_index = self.open_file_index(descriptor)?;
        let descriptor_index = self.free_descriptor_slot()?;
        let file = self.open_files[open_file_index]
            .as_mut()
            .ok_or(DescriptorError::BadDescriptor)?;
        file.references = file
            .references
            .checked_add(1)
            .ok_or(DescriptorError::TableFull)?;
        self.install_descriptor(descriptor_index, open_file_index);
        Ok(self.descriptor(descriptor_index))
    }

    pub fn close(&mut self, descriptor: FileDescriptor) -> Result<(), DescriptorError> {
        let open_file_index = self.open_file_index(descriptor)?;
        let slot = self
            .slots
            .get_mut(descriptor.slot as usize)
            .ok_or(DescriptorError::BadDescriptor)?;
        slot.open_file = None;
        slot.generation = next_generation(slot.generation);
        self.count -= 1;

        let file = self.open_files[open_file_index]
            .as_mut()
            .ok_or(DescriptorError::BadDescriptor)?;
        file.references -= 1;
        if file.references == 0 {
            self.open_files[open_file_index] = None;
        }
        Ok(())
    }

    pub fn position(&self, descriptor: FileDescriptor) -> Result<u64, DescriptorError> {
        Ok(self.file(descriptor)?.position)
    }

    pub fn set_position(
        &mut self,
        descriptor: FileDescriptor,
        position: u64,
    ) -> Result<(), DescriptorError> {
        self.file_mut(descriptor)?.position = position;
        Ok(())
    }

    pub fn seek<F: FileSystem + ?Sized>(
        &mut self,
        filesystem: &F,
        descriptor: FileDescriptor,
        offset: i64,
        origin: SeekOrigin,
    ) -> Result<u64, DescriptorError> {
        let file = self.file(descriptor)?;
        let base = match origin {
            SeekOrigin::Start => 0,
            SeekOrigin::Current => file.position,
            SeekOrigin::End => filesystem.metadata(file.node)?.length,
        };
        let position = base
            .checked_add_signed(offset)
            .ok_or(DescriptorError::OffsetOverflow)?;
        self.file_mut(descriptor)?.position = position;
        Ok(position)
    }

    pub fn read<F: FileSystem + ?Sized>(
        &mut self,
        filesystem: &F,
        descriptor: FileDescriptor,
        output: &mut [u8],
    ) -> Result<usize, DescriptorError> {
        let file = self.file_mut(descriptor)?;
        if !file.access.can_read() {
            return Err(DescriptorError::PermissionDenied);
        }
        let read = filesystem.read_node(file.node, file.position, output)?;
        file.position = file
            .position
            .checked_add(read as u64)
            .ok_or(DescriptorError::OffsetOverflow)?;
        Ok(read)
    }

    pub fn write<F: FileSystem + ?Sized>(
        &mut self,
        filesystem: &mut F,
        descriptor: FileDescriptor,
        data: &[u8],
    ) -> Result<usize, DescriptorError> {
        let file = self.file_mut(descriptor)?;
        if !file.access.can_write() {
            return Err(DescriptorError::PermissionDenied);
        }
        let written = filesystem.write_node(file.node, file.position, data)?;
        file.position = file
            .position
            .checked_add(written as u64)
            .ok_or(DescriptorError::OffsetOverflow)?;
        Ok(written)
    }

    fn free_descriptor_slot(&self) -> Result<usize, DescriptorError> {
        self.slots
            .iter()
            .position(|slot| slot.open_file.is_none())
            .ok_or(DescriptorError::TableFull)
    }

    fn free_open_file_slot(&self) -> Result<usize, DescriptorError> {
        self.open_files
            .iter()
            .position(Option::is_none)
            .ok_or(DescriptorError::TableFull)
    }

    fn install_descriptor(&mut self, descriptor_index: usize, open_file_index: usize) {
        self.slots[descriptor_index].open_file = Some(open_file_index as u32);
        self.count += 1;
    }

    fn descriptor(&self, index: usize) -> FileDescriptor {
        FileDescriptor {
            slot: index as u32,
            generation: self.slots[index].generation,
        }
    }

    fn open_file_index(&self, descriptor: FileDescriptor) -> Result<usize, DescriptorError> {
        let slot = self
            .slots
            .get(descriptor.slot as usize)
            .ok_or(DescriptorError::BadDescriptor)?;
        if slot.generation != descriptor.generation {
            return Err(DescriptorError::BadDescriptor);
        }
        slot.open_file
            .map(|index| index as usize)
            .ok_or(DescriptorError::BadDescriptor)
    }

    fn file(&self, descriptor: FileDescriptor) -> Result<&OpenFile, DescriptorError> {
        let index = self.open_file_index(descriptor)?;
        self.open_files[index]
            .as_ref()
            .ok_or(DescriptorError::BadDescriptor)
    }

    fn file_mut(&mut self, descriptor: FileDescriptor) -> Result<&mut OpenFile, DescriptorError> {
        let index = self.open_file_index(descriptor)?;
        self.open_files[index]
            .as_mut()
            .ok_or(DescriptorError::BadDescriptor)
    }
}

impl<const CAPACITY: usize> Default for DescriptorTable<CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

const fn next_generation(generation: u32) -> u32 {
    let next = generation.wrapping_add(1);
    if next == 0 { 1 } else { next }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Name {
    bytes: [u8; NAME_CAPACITY],
    len: u8,
}

impl Name {
    const fn root() -> Self {
        Self {
            bytes: [0; NAME_CAPACITY],
            len: 0,
        }
    }

    fn new(value: &[u8]) -> Result<Self, VfsError> {
        if value.len() > NAME_CAPACITY {
            return Err(VfsError::NameTooLong);
        }
        if value.is_empty() || value.contains(&b'/') {
            return Err(VfsError::InvalidPath);
        }

        let mut name = Self::root();
        name.bytes[..value.len()].copy_from_slice(value);
        name.len = value.len() as u8;
        Ok(name)
    }

    fn equals(self, value: &[u8]) -> bool {
        self.len as usize == value.len() && &self.bytes[..self.len as usize] == value
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Node<const FILE_CAPACITY: usize> {
    parent: Option<NodeId>,
    name: Name,
    kind: NodeKind,
    data: [u8; FILE_CAPACITY],
    len: usize,
}

impl<const FILE_CAPACITY: usize> Node<FILE_CAPACITY> {
    const fn root() -> Self {
        Self {
            parent: None,
            name: Name::root(),
            kind: NodeKind::Directory,
            data: [0; FILE_CAPACITY],
            len: 0,
        }
    }

    fn child(parent: NodeId, name: Name, kind: NodeKind) -> Self {
        Self {
            parent: Some(parent),
            name,
            kind,
            data: [0; FILE_CAPACITY],
            len: 0,
        }
    }
}

pub struct MemoryFileSystem<const NODES: usize, const FILE_CAPACITY: usize> {
    nodes: [Option<Node<FILE_CAPACITY>>; NODES],
    count: usize,
}

impl<const NODES: usize, const FILE_CAPACITY: usize> MemoryFileSystem<NODES, FILE_CAPACITY> {
    pub const fn new() -> Self {
        let mut nodes = [None; NODES];
        if NODES > 0 {
            nodes[0] = Some(Node::root());
        }
        Self {
            nodes,
            count: if NODES > 0 { 1 } else { 0 },
        }
    }

    pub const fn len(&self) -> usize {
        self.count
    }

    pub fn create_directory(&mut self, path: &str) -> Result<NodeId, VfsError> {
        self.create(path, NodeKind::Directory)
    }

    pub fn create_file(&mut self, path: &str) -> Result<NodeId, VfsError> {
        self.create(path, NodeKind::File)
    }

    pub fn kind(&self, path: &str) -> Result<NodeKind, VfsError> {
        let id = self.resolve(path)?;
        Ok(self.metadata(id)?.kind)
    }

    pub fn write(&mut self, path: &str, data: &[u8]) -> Result<(), VfsError> {
        if data.len() > FILE_CAPACITY {
            return Err(VfsError::FileTooLarge);
        }

        let id = self.resolve(path)?;
        self.truncate_node(id, 0)?;
        self.write_node(id, 0, data)?;
        Ok(())
    }

    pub fn read<'a>(&'a self, path: &str, output: &'a mut [u8]) -> Result<&'a [u8], VfsError> {
        let id = self.resolve(path)?;
        let metadata = self.metadata(id)?;
        let length = usize::try_from(metadata.length).map_err(|_| VfsError::FileTooLarge)?;
        if output.len() < length {
            return Err(VfsError::NoSpace);
        }
        let read = self.read_node(id, 0, output)?;
        Ok(&output[..read])
    }

    pub fn resolve(&self, path: &str) -> Result<NodeId, VfsError> {
        resolve_path(self, path)
    }

    fn create(&mut self, path: &str, kind: NodeKind) -> Result<NodeId, VfsError> {
        let (parent_path, name) = split_parent(path)?;
        let parent = self.resolve(parent_path)?;
        self.create_node(parent, name.as_bytes(), kind)
    }

    fn root(&self) -> Result<NodeId, VfsError> {
        if NODES == 0 || self.nodes[0].is_none() {
            return Err(VfsError::NoSpace);
        }
        Ok(NodeId(0))
    }

    fn find_child(&self, parent: NodeId, name: &[u8]) -> Result<NodeId, VfsError> {
        self.nodes
            .iter()
            .enumerate()
            .find_map(|(index, node)| {
                let node = node.as_ref()?;
                (node.parent == Some(parent) && node.name.equals(name))
                    .then_some(NodeId(index as u32))
            })
            .ok_or(VfsError::NotFound)
    }

    fn node(&self, id: NodeId) -> Result<&Node<FILE_CAPACITY>, VfsError> {
        self.nodes
            .get(id.0 as usize)
            .and_then(Option::as_ref)
            .ok_or(VfsError::NotFound)
    }

    fn node_mut(&mut self, id: NodeId) -> Result<&mut Node<FILE_CAPACITY>, VfsError> {
        self.nodes
            .get_mut(id.0 as usize)
            .and_then(Option::as_mut)
            .ok_or(VfsError::NotFound)
    }
}

impl<const NODES: usize, const FILE_CAPACITY: usize> FileSystem
    for MemoryFileSystem<NODES, FILE_CAPACITY>
{
    fn root_node(&self) -> Result<NodeId, VfsError> {
        self.root()
    }

    fn metadata(&self, node: NodeId) -> Result<NodeMetadata, VfsError> {
        let node = self.node(node)?;
        Ok(NodeMetadata {
            kind: node.kind,
            length: node.len as u64,
        })
    }

    fn lookup_child(&self, parent: NodeId, name: &[u8]) -> Result<NodeId, VfsError> {
        if self.node(parent)?.kind != NodeKind::Directory {
            return Err(VfsError::NotDirectory);
        }
        Name::new(name)?;
        self.find_child(parent, name)
    }

    fn create_node(
        &mut self,
        parent: NodeId,
        name: &[u8],
        kind: NodeKind,
    ) -> Result<NodeId, VfsError> {
        if self.node(parent)?.kind != NodeKind::Directory {
            return Err(VfsError::NotDirectory);
        }
        if self.find_child(parent, name).is_ok() {
            return Err(VfsError::AlreadyExists);
        }

        let name = Name::new(name)?;
        let Some(index) = self.nodes.iter().position(Option::is_none) else {
            return Err(VfsError::NoSpace);
        };
        self.nodes[index] = Some(Node::child(parent, name, kind));
        self.count += 1;
        Ok(NodeId(index as u32))
    }

    fn read_node(&self, node: NodeId, offset: u64, output: &mut [u8]) -> Result<usize, VfsError> {
        let node = self.node(node)?;
        if node.kind != NodeKind::File {
            return Err(VfsError::IsDirectory);
        }
        let offset = usize::try_from(offset).map_err(|_| VfsError::InvalidOffset)?;
        if offset >= node.len {
            return Ok(0);
        }
        let length = output.len().min(node.len - offset);
        output[..length].copy_from_slice(&node.data[offset..offset + length]);
        Ok(length)
    }

    fn write_node(&mut self, node: NodeId, offset: u64, data: &[u8]) -> Result<usize, VfsError> {
        let offset = usize::try_from(offset).map_err(|_| VfsError::InvalidOffset)?;
        let end = offset
            .checked_add(data.len())
            .ok_or(VfsError::InvalidOffset)?;
        if end > FILE_CAPACITY {
            return Err(VfsError::FileTooLarge);
        }

        let node = self.node_mut(node)?;
        if node.kind != NodeKind::File {
            return Err(VfsError::IsDirectory);
        }
        if offset > node.len {
            node.data[node.len..offset].fill(0);
        }
        node.data[offset..end].copy_from_slice(data);
        node.len = node.len.max(end);
        Ok(data.len())
    }

    fn truncate_node(&mut self, node: NodeId, length: u64) -> Result<(), VfsError> {
        let length = usize::try_from(length).map_err(|_| VfsError::InvalidOffset)?;
        if length > FILE_CAPACITY {
            return Err(VfsError::FileTooLarge);
        }
        let node = self.node_mut(node)?;
        if node.kind != NodeKind::File {
            return Err(VfsError::IsDirectory);
        }
        if length != node.len {
            let range = if length < node.len {
                length..node.len
            } else {
                node.len..length
            };
            node.data[range].fill(0);
        }
        node.len = length;
        Ok(())
    }
}

impl<const NODES: usize, const FILE_CAPACITY: usize> Default
    for MemoryFileSystem<NODES, FILE_CAPACITY>
{
    fn default() -> Self {
        Self::new()
    }
}

fn split_parent(path: &str) -> Result<(&str, &str), VfsError> {
    if !path.starts_with('/') || path == "/" || path.ends_with('/') {
        return Err(VfsError::InvalidPath);
    }

    let Some(index) = path.rfind('/') else {
        return Err(VfsError::InvalidPath);
    };
    let name = &path[index + 1..];
    if name.is_empty() || name == "." || name == ".." {
        return Err(VfsError::InvalidPath);
    }

    let parent = if index == 0 { "/" } else { &path[..index] };
    Ok((parent, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_table_keeps_filesystem_identity_and_crosses_mount() {
        let system_root = VfsNode::new(FileSystemId(1), NodeId(0));
        let media_directory = VfsNode::new(FileSystemId(1), NodeId(7));
        let media_root = VfsNode::new(FileSystemId(2), NodeId(0));
        let same_local_node = VfsNode::new(FileSystemId(3), NodeId(0));
        let mut mounts = MountTable::<2>::new(system_root);

        assert_eq!(mounts.root(), system_root);
        assert_eq!(mounts.cross_mount(media_directory), media_directory);

        mounts.mount(media_directory, media_root).unwrap();

        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts.cross_mount(media_directory), media_root);
        assert_eq!(mounts.cross_mount(same_local_node), same_local_node);
        assert_ne!(media_root, same_local_node);
    }

    #[test]
    fn mount_table_rejects_duplicate_and_capacity_overflow() {
        let root = VfsNode::new(FileSystemId(1), NodeId(0));
        let first_location = VfsNode::new(FileSystemId(1), NodeId(1));
        let second_location = VfsNode::new(FileSystemId(1), NodeId(2));
        let mounted_root = VfsNode::new(FileSystemId(2), NodeId(0));
        let mut mounts = MountTable::<1>::new(root);

        mounts.mount(first_location, mounted_root).unwrap();
        assert_eq!(
            mounts.mount(first_location, VfsNode::new(FileSystemId(3), NodeId(0))),
            Err(MountError::AlreadyMounted)
        );
        assert_eq!(
            mounts.mount(second_location, VfsNode::new(FileSystemId(4), NodeId(0))),
            Err(MountError::TableFull)
        );
    }

    #[test]
    fn unmount_restores_underlying_node() {
        let root = VfsNode::new(FileSystemId(1), NodeId(0));
        let location = VfsNode::new(FileSystemId(1), NodeId(4));
        let mounted_root = VfsNode::new(FileSystemId(2), NodeId(0));
        let mut mounts = MountTable::<1>::new(root);

        mounts.mount(location, mounted_root).unwrap();
        assert_eq!(mounts.cross_mount(location), mounted_root);

        let removed = mounts.unmount(location).unwrap();
        assert_eq!(
            removed,
            MountPoint {
                location,
                root: mounted_root,
            }
        );
        assert!(mounts.is_empty());
        assert_eq!(mounts.cross_mount(location), location);
        assert_eq!(mounts.unmount(location), Err(MountError::NotMounted));
    }

    struct TestNamespace {
        system: MemoryFileSystem<8, 32>,
        media: MemoryFileSystem<8, 32>,
    }

    impl NamespaceLookup for TestNamespace {
        fn lookup_child(&self, parent: VfsNode, name: &[u8]) -> Result<NodeId, VfsError> {
            match parent.filesystem {
                FileSystemId(1) => self.system.lookup_child(parent.node, name),
                FileSystemId(2) => self.media.lookup_child(parent.node, name),
                _ => Err(VfsError::NotFound),
            }
        }
    }

    #[test]
    fn mounted_path_resolution_switches_filesystem_at_mount_point() {
        let mut namespace = TestNamespace {
            system: MemoryFileSystem::new(),
            media: MemoryFileSystem::new(),
        };
        let system_root = namespace.system.root_node().unwrap();
        let media_root = namespace.media.root_node().unwrap();
        let media_directory = namespace
            .system
            .create_node(system_root, b"media", NodeKind::Directory)
            .unwrap();
        let etc_directory = namespace
            .system
            .create_node(system_root, b"etc", NodeKind::Directory)
            .unwrap();
        let photo = namespace
            .media
            .create_node(media_root, b"photo.jpg", NodeKind::File)
            .unwrap();

        let mut mounts = MountTable::<2>::new(VfsNode::new(FileSystemId(1), system_root));
        mounts
            .mount(
                VfsNode::new(FileSystemId(1), media_directory),
                VfsNode::new(FileSystemId(2), media_root),
            )
            .unwrap();

        assert_eq!(
            resolve_mounted_path(&namespace, &mounts, "/"),
            Ok(VfsNode::new(FileSystemId(1), system_root))
        );
        assert_eq!(
            resolve_mounted_path(&namespace, &mounts, "/etc"),
            Ok(VfsNode::new(FileSystemId(1), etc_directory))
        );
        assert_eq!(
            resolve_mounted_path(&namespace, &mounts, "/media"),
            Ok(VfsNode::new(FileSystemId(2), media_root))
        );
        assert_eq!(
            resolve_mounted_path(&namespace, &mounts, "/media/photo.jpg"),
            Ok(VfsNode::new(FileSystemId(2), photo))
        );
    }

    #[test]
    fn mounted_path_resolution_rejects_invalid_components() {
        let namespace = TestNamespace {
            system: MemoryFileSystem::new(),
            media: MemoryFileSystem::new(),
        };
        let root = namespace.system.root_node().unwrap();
        let mounts = MountTable::<1>::new(VfsNode::new(FileSystemId(1), root));

        assert_eq!(
            resolve_mounted_path(&namespace, &mounts, "relative"),
            Err(VfsError::InvalidPath)
        );
        assert_eq!(
            resolve_mounted_path(&namespace, &mounts, "/a/../b"),
            Err(VfsError::InvalidPath)
        );
    }

    #[test]
    fn creates_hierarchy_and_reads_file() {
        let mut fs = MemoryFileSystem::<8, 64>::new();
        fs.create_directory("/etc").unwrap();
        fs.create_file("/etc/hostname").unwrap();
        fs.write("/etc/hostname", b"phoenix").unwrap();

        let mut output = [0_u8; 16];
        assert_eq!(fs.read("/etc/hostname", &mut output).unwrap(), b"phoenix");
        assert_eq!(fs.kind("/etc").unwrap(), NodeKind::Directory);
        assert_eq!(fs.len(), 3);
    }

    #[test]
    fn rejects_missing_parent_and_duplicate_name() {
        let mut fs = MemoryFileSystem::<4, 16>::new();
        assert_eq!(fs.create_file("/missing/file"), Err(VfsError::NotFound));

        fs.create_directory("/tmp").unwrap();
        assert_eq!(fs.create_directory("/tmp"), Err(VfsError::AlreadyExists));
    }

    #[test]
    fn rejects_invalid_paths_and_capacity_overflow() {
        let mut fs = MemoryFileSystem::<2, 4>::new();
        assert_eq!(fs.create_file("relative"), Err(VfsError::InvalidPath));
        assert_eq!(fs.create_file("/a/"), Err(VfsError::InvalidPath));

        fs.create_file("/a").unwrap();
        assert_eq!(fs.write("/a", b"12345"), Err(VfsError::FileTooLarge));
        assert_eq!(fs.create_file("/b"), Err(VfsError::NoSpace));
    }

    #[test]
    fn filesystem_contract_supports_offset_io() {
        let mut fs = MemoryFileSystem::<4, 16>::new();
        let root = FileSystem::root_node(&fs).unwrap();
        let file = FileSystem::create_node(&mut fs, root, b"log", NodeKind::File).unwrap();

        assert_eq!(FileSystem::write_node(&mut fs, file, 3, b"OS").unwrap(), 2);
        assert_eq!(
            FileSystem::metadata(&fs, file).unwrap(),
            NodeMetadata {
                kind: NodeKind::File,
                length: 5,
            }
        );

        let mut output = [0xff; 4];
        assert_eq!(FileSystem::read_node(&fs, file, 1, &mut output).unwrap(), 4);
        assert_eq!(output, [0, 0, b'O', b'S']);
        assert_eq!(resolve_path(&fs, "/log"), Ok(file));
    }

    #[test]
    fn filesystem_contract_rejects_directory_io_and_large_offsets() {
        let mut fs = MemoryFileSystem::<2, 8>::new();
        let root = FileSystem::root_node(&fs).unwrap();
        let mut output = [0_u8; 1];

        assert_eq!(
            FileSystem::read_node(&fs, root, 0, &mut output),
            Err(VfsError::IsDirectory)
        );
        let file = FileSystem::create_node(&mut fs, root, b"f", NodeKind::File).unwrap();
        assert_eq!(
            FileSystem::write_node(&mut fs, file, u64::MAX, b"x"),
            Err(VfsError::InvalidOffset)
        );
    }

    #[test]
    fn descriptors_track_independent_positions_and_permissions() {
        let mut fs = MemoryFileSystem::<3, 16>::new();
        let file = fs.create_file("/data").unwrap();
        let mut descriptors = DescriptorTable::<3>::new();
        let writer = descriptors.open(&fs, file, AccessMode::WriteOnly).unwrap();
        let reader = descriptors.open(&fs, file, AccessMode::ReadOnly).unwrap();

        assert_eq!(descriptors.write(&mut fs, writer, b"phoenix").unwrap(), 7);
        assert_eq!(descriptors.position(writer), Ok(7));
        assert_eq!(
            descriptors.write(&mut fs, reader, b"x"),
            Err(DescriptorError::PermissionDenied)
        );

        let mut first = [0_u8; 3];
        assert_eq!(descriptors.read(&fs, reader, &mut first).unwrap(), 3);
        assert_eq!(&first, b"pho");
        assert_eq!(descriptors.position(reader), Ok(3));
        assert_eq!(descriptors.position(writer), Ok(7));
    }

    #[test]
    fn duplicated_descriptors_share_position_until_last_close() {
        let mut fs = MemoryFileSystem::<2, 16>::new();
        let file = fs.create_file("/shared").unwrap();
        fs.write("/shared", b"phoenix").unwrap();
        let mut descriptors = DescriptorTable::<3>::new();
        let first = descriptors.open(&fs, file, AccessMode::ReadOnly).unwrap();
        let duplicate = descriptors.duplicate(first).unwrap();

        let mut prefix = [0_u8; 3];
        assert_eq!(descriptors.read(&fs, first, &mut prefix).unwrap(), 3);
        assert_eq!(&prefix, b"pho");
        assert_eq!(descriptors.position(duplicate), Ok(3));

        descriptors.close(first).unwrap();
        let mut suffix = [0_u8; 4];
        assert_eq!(descriptors.read(&fs, duplicate, &mut suffix).unwrap(), 4);
        assert_eq!(&suffix, b"enix");
        descriptors.close(duplicate).unwrap();
        assert!(descriptors.is_empty());
    }

    #[test]
    fn closed_descriptor_cannot_alias_reused_slot() {
        let mut fs = MemoryFileSystem::<3, 8>::new();
        let first_node = fs.create_file("/a").unwrap();
        let second_node = fs.create_file("/b").unwrap();
        let mut descriptors = DescriptorTable::<1>::new();
        let stale = descriptors
            .open(&fs, first_node, AccessMode::ReadOnly)
            .unwrap();
        descriptors.close(stale).unwrap();
        let current = descriptors
            .open(&fs, second_node, AccessMode::ReadOnly)
            .unwrap();

        assert_eq!(stale.slot, current.slot);
        assert_ne!(stale.generation, current.generation);
        assert_eq!(
            descriptors.set_position(stale, 1),
            Err(DescriptorError::BadDescriptor)
        );
    }

    #[test]
    fn descriptor_table_rejects_directories_and_overflow() {
        let fs = MemoryFileSystem::<1, 8>::new();
        let root = fs.resolve("/").unwrap();
        let mut descriptors = DescriptorTable::<0>::new();

        assert_eq!(
            descriptors.open(&fs, root, AccessMode::ReadOnly),
            Err(DescriptorError::Vfs(VfsError::IsDirectory))
        );
    }

    #[test]
    fn descriptor_seek_uses_start_current_and_end_without_wrapping() {
        let mut fs = MemoryFileSystem::<2, 16>::new();
        let file = fs.create_file("/data").unwrap();
        fs.write("/data", b"phoenix").unwrap();
        let mut descriptors = DescriptorTable::<1>::new();
        let descriptor = descriptors.open(&fs, file, AccessMode::ReadOnly).unwrap();

        assert_eq!(
            descriptors.seek(&fs, descriptor, 2, SeekOrigin::Start),
            Ok(2)
        );
        assert_eq!(
            descriptors.seek(&fs, descriptor, 3, SeekOrigin::Current),
            Ok(5)
        );
        assert_eq!(
            descriptors.seek(&fs, descriptor, -2, SeekOrigin::End),
            Ok(5)
        );
        assert_eq!(
            descriptors.seek(&fs, descriptor, -8, SeekOrigin::End),
            Err(DescriptorError::OffsetOverflow)
        );
        assert_eq!(descriptors.position(descriptor), Ok(5));
    }
}
