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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeId(pub u32);

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
        Ok(self.node(id)?.kind)
    }

    pub fn write(&mut self, path: &str, data: &[u8]) -> Result<(), VfsError> {
        if data.len() > FILE_CAPACITY {
            return Err(VfsError::FileTooLarge);
        }

        let id = self.resolve(path)?;
        let node = self.node_mut(id)?;
        if node.kind != NodeKind::File {
            return Err(VfsError::IsDirectory);
        }

        node.data[..data.len()].copy_from_slice(data);
        if data.len() < node.len {
            node.data[data.len()..node.len].fill(0);
        }
        node.len = data.len();
        Ok(())
    }

    pub fn read<'a>(&'a self, path: &str, output: &'a mut [u8]) -> Result<&'a [u8], VfsError> {
        let id = self.resolve(path)?;
        let node = self.node(id)?;
        if node.kind != NodeKind::File {
            return Err(VfsError::IsDirectory);
        }
        if output.len() < node.len {
            return Err(VfsError::NoSpace);
        }

        output[..node.len].copy_from_slice(&node.data[..node.len]);
        Ok(&output[..node.len])
    }

    pub fn resolve(&self, path: &str) -> Result<NodeId, VfsError> {
        if path == "/" {
            return self.root();
        }
        if !path.starts_with('/') || path.ends_with('/') {
            return Err(VfsError::InvalidPath);
        }

        let mut current = self.root()?;
        for component in path[1..].split('/') {
            if component.is_empty() || component == "." || component == ".." {
                return Err(VfsError::InvalidPath);
            }
            current = self.find_child(current, component.as_bytes())?;
        }
        Ok(current)
    }

    fn create(&mut self, path: &str, kind: NodeKind) -> Result<NodeId, VfsError> {
        let (parent_path, name) = split_parent(path)?;
        let parent = self.resolve(parent_path)?;
        if self.node(parent)?.kind != NodeKind::Directory {
            return Err(VfsError::NotDirectory);
        }
        if self.find_child(parent, name.as_bytes()).is_ok() {
            return Err(VfsError::AlreadyExists);
        }

        let name = Name::new(name.as_bytes())?;
        let Some(index) = self.nodes.iter().position(Option::is_none) else {
            return Err(VfsError::NoSpace);
        };
        self.nodes[index] = Some(Node::child(parent, name, kind));
        self.count += 1;
        Ok(NodeId(index as u32))
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
}
