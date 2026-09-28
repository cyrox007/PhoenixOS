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
pub struct NodeMetadata {
    pub kind: NodeKind,
    pub length: u64,
}

/// Filesystem-independent node operations used by the VFS and file descriptors.
///
/// Paths are intentionally kept outside this contract. Mount traversal can resolve a
/// path component-by-component and then retain the returned `NodeId` in an open file
/// description without coupling callers to a concrete filesystem implementation.
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
    fn read_node(
        &self,
        node: NodeId,
        offset: u64,
        output: &mut [u8],
    ) -> Result<usize, VfsError>;
    fn write_node(
        &mut self,
        node: NodeId,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, VfsError>;
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
        if value.is_empty() || value.len() > NAME_CAPACITY || value.contains(&b'/') {
            return Err(if value.len() > NAME_CAPACITY {
                VfsError::NameTooLong
            } else {
                VfsError::InvalidPath
            });
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

    fn read_node(
        &self,
        node: NodeId,
        offset: u64,
        output: &mut [u8],
    ) -> Result<usize, VfsError> {
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

    fn write_node(
        &mut self,
        node: NodeId,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, VfsError> {
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
}
