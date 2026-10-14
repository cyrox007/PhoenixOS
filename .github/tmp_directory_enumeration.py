from pathlib import Path


def replace_once(text: str, old: str, new: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"ожидалось одно совпадение, найдено {count}: {old[:120]!r}")
    return text.replace(old, new, 1)


vfs_path = Path("crates/vfs/src/lib.rs")
vfs = vfs_path.read_text()

vfs = replace_once(
    vfs,
    "    InvalidOffset,\n    ReadOnly,\n    Storage,\n",
    "    InvalidOffset,\n    BufferTooSmall,\n    Unsupported,\n    ReadOnly,\n    Storage,\n",
)

registry_anchor = '''    pub fn create_node(\n        &mut self,\n        parent: VfsNode,\n        name: &[u8],\n        kind: NodeKind,\n    ) -> Result<VfsNode, FileSystemRegistryError> {\n'''
registry_method = '''    pub fn read_directory_entry(\n        &self,\n        directory: VfsNode,\n        cursor: DirectoryCursor,\n        name_buffer: &mut [u8],\n    ) -> Result<DirectoryRead, FileSystemRegistryError> {\n        Ok(self\n            .filesystem(directory.filesystem)?\n            .read_directory_entry(directory.node, cursor, name_buffer)?)\n    }\n\n'''
vfs = replace_once(vfs, registry_anchor, registry_method + registry_anchor)

metadata_anchor = '''#[derive(Debug, Clone, Copy, PartialEq, Eq)]\npub struct NodeMetadata {\n    pub kind: NodeKind,\n    pub length: u64,\n}\n\n'''
directory_types = '''#[derive(Debug, Clone, Copy, PartialEq, Eq)]\npub struct DirectoryCursor(pub u64);\n\nimpl DirectoryCursor {\n    pub const START: Self = Self(0);\n}\n\n#[derive(Debug, Clone, Copy, PartialEq, Eq)]\npub struct DirectoryEntry {\n    pub node: NodeId,\n    pub kind: NodeKind,\n    pub name_length: usize,\n}\n\n#[derive(Debug, Clone, Copy, PartialEq, Eq)]\npub struct DirectoryRead {\n    pub entry: Option<DirectoryEntry>,\n    pub next_cursor: DirectoryCursor,\n}\n\nimpl DirectoryRead {\n    pub const fn end(cursor: DirectoryCursor) -> Self {\n        Self {\n            entry: None,\n            next_cursor: cursor,\n        }\n    }\n}\n\n'''
vfs = replace_once(vfs, metadata_anchor, metadata_anchor + directory_types)

trait_anchor = '''    fn lookup_child(&self, parent: NodeId, name: &[u8]) -> Result<NodeId, VfsError>;\n    fn create_node(\n'''
trait_method = '''    fn lookup_child(&self, parent: NodeId, name: &[u8]) -> Result<NodeId, VfsError>;\n    fn read_directory_entry(\n        &self,\n        directory: NodeId,\n        cursor: DirectoryCursor,\n        name_buffer: &mut [u8],\n    ) -> Result<DirectoryRead, VfsError> {\n        let _ = (directory, cursor, name_buffer);\n        Err(VfsError::Unsupported)\n    }\n    fn create_node(\n'''
vfs = replace_once(vfs, trait_anchor, trait_method)
vfs_path.write_text(vfs)

fs_path = Path("crates/phoenix-fs/src/lib.rs")
fs = fs_path.read_text()

fs = replace_once(
    fs,
    "use phoenix_vfs::{FileSystem, NodeId, NodeKind, NodeMetadata, VfsError};",
    "use phoenix_vfs::{DirectoryCursor, DirectoryEntry, DirectoryRead, FileSystem, NodeId, NodeKind, NodeMetadata, VfsError};",
)

lookup_anchor = '''pub fn lookup_directory_entry<D: BlockDevice>(\n    device: &mut D,\n    root_block: u64,\n    directory_object_id: u64,\n    name: &str,\n    maximum_generation: u64,\n    node_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],\n) -> Result<DirectoryLookupResult, PhoenixFsError> {\n'''
if lookup_anchor not in fs:
    raise SystemExit("не найден lookup_directory_entry")

insert_after_lookup = '''\n#[derive(Debug, Clone, Copy, PartialEq, Eq)]\npub struct DirectoryReadResult {\n    pub entry_id: u64,\n    pub target_object_id: u64,\n    pub target_type: ObjectType,\n    pub name_bytes: usize,\n}\n\npub fn read_next_directory_entry<D: BlockDevice>(\n    device: &mut D,\n    root_block: u64,\n    directory_object_id: u64,\n    after_entry_id: u64,\n    maximum_generation: u64,\n    node_buffer: &mut [u8; FILESYSTEM_BLOCK_SIZE],\n    name_buffer: &mut [u8],\n) -> Result<Option<DirectoryReadResult>, PhoenixFsError> {\n    if directory_object_id == 0 {\n        return Err(PhoenixFsError::InvalidObjectId);\n    }\n\n    let mut result = None;\n    scan_object_tree_records(\n        device,\n        root_block,\n        maximum_generation,\n        node_buffer,\n        |key, value| {\n            if key.object_id != directory_object_id\n                || key.kind != ObjectRecordKind::DirectoryEntry\n                || key.offset <= after_entry_id\n            {\n                return Ok(false);\n            }\n\n            let (entry, name) = DirectoryEntryValue::decode(value)?;\n            if name_buffer.len() < name.len() {\n                return Err(PhoenixFsError::BufferSize);\n            }\n            name_buffer[..name.len()].copy_from_slice(name.as_bytes());\n            result = Some(DirectoryReadResult {\n                entry_id: key.offset,\n                target_object_id: entry.target_object_id,\n                target_type: entry.target_type,\n                name_bytes: name.len(),\n            });\n            Ok(true)\n        },\n    )?;\n\n    Ok(result)\n}\n'''

ensure_anchor = "\nfn ensure_object_has_no_payload_records<D: BlockDevice>(\n"
fs = replace_once(fs, ensure_anchor, insert_after_lookup + ensure_anchor)

adapter_anchor = '''    fn create_node(\n        &mut self,\n        parent: NodeId,\n        name: &[u8],\n        kind: NodeKind,\n    ) -> Result<NodeId, VfsError> {\n'''
adapter_method = '''    fn read_directory_entry(\n        &self,\n        directory: NodeId,\n        cursor: DirectoryCursor,\n        name_buffer: &mut [u8],\n    ) -> Result<DirectoryRead, VfsError> {\n        let metadata = self.metadata(directory)?;\n        if metadata.kind != NodeKind::Directory {\n            return Err(VfsError::NotDirectory);\n        }\n\n        let mut device = self\n            .device\n            .try_borrow_mut()\n            .map_err(|_| VfsError::Storage)?;\n        let mut node_buffer = self\n            .node_buffer\n            .try_borrow_mut()\n            .map_err(|_| VfsError::Storage)?;\n\n        let next = read_next_directory_entry(\n            &mut *device,\n            self.active.superblock.roots.object_tree,\n            directory.0,\n            cursor.0,\n            self.active.superblock.generation,\n            &mut *node_buffer,\n            name_buffer,\n        );\n\n        let next = match next {\n            Ok(next) => next,\n            Err(PhoenixFsError::BufferSize) => return Err(VfsError::BufferTooSmall),\n            Err(error) => return Err(map_phoenix_fs_to_vfs(error)),\n        };\n        let Some(next) = next else {\n            return Ok(DirectoryRead::end(cursor));\n        };\n\n        let kind = match next.target_type {\n            ObjectType::File => NodeKind::File,\n            ObjectType::Directory => NodeKind::Directory,\n        };\n        Ok(DirectoryRead {\n            entry: Some(DirectoryEntry {\n                node: NodeId(next.target_object_id),\n                kind,\n                name_length: next.name_bytes,\n            }),\n            next_cursor: DirectoryCursor(next.entry_id),\n        })\n    }\n\n'''
fs = replace_once(fs, adapter_anchor, adapter_method + adapter_anchor)

test_anchor = '''    #[test]\n    fn phoenix_vfs_creates_and_removes_empty_nodes() {\n'''
test_code = '''    #[test]\n    fn phoenix_vfs_enumerates_directory_entries_and_remounts() {\n        use phoenix_vfs::{DirectoryCursor, FileSystem, NodeKind, VfsError};\n\n        let device = MemoryBlockDevice::<512, 256>::new();\n        let mut format_buffer = [0_u8; FILESYSTEM_BLOCK_SIZE];\n        let mut filesystem =\n            PhoenixVfs::format_new(device, VOLUME_ID, 77, &mut format_buffer).unwrap();\n        let root = filesystem.root_node().unwrap();\n        let alpha = filesystem\n            .create_node(root, b"alpha.txt", NodeKind::File)\n            .unwrap();\n        let docs = filesystem\n            .create_node(root, b"docs", NodeKind::Directory)\n            .unwrap();\n\n        let mut name = [0_u8; 32];\n        let first = filesystem\n            .read_directory_entry(root, DirectoryCursor::START, &mut name)\n            .unwrap();\n        let first_entry = first.entry.unwrap();\n        assert_eq!(first_entry.node, alpha);\n        assert_eq!(first_entry.kind, NodeKind::File);\n        assert_eq!(&name[..first_entry.name_length], b"alpha.txt");\n\n        name.fill(0);\n        let second = filesystem\n            .read_directory_entry(root, first.next_cursor, &mut name)\n            .unwrap();\n        let second_entry = second.entry.unwrap();\n        assert_eq!(second_entry.node, docs);\n        assert_eq!(second_entry.kind, NodeKind::Directory);\n        assert_eq!(&name[..second_entry.name_length], b"docs");\n\n        let end = filesystem\n            .read_directory_entry(root, second.next_cursor, &mut name)\n            .unwrap();\n        assert_eq!(end.entry, None);\n        assert_eq!(end.next_cursor, second.next_cursor);\n\n        let empty = filesystem\n            .read_directory_entry(docs, DirectoryCursor::START, &mut name)\n            .unwrap();\n        assert_eq!(empty.entry, None);\n        assert_eq!(\n            filesystem.read_directory_entry(alpha, DirectoryCursor::START, &mut name),\n            Err(VfsError::NotDirectory)\n        );\n\n        let mut short = [0_u8; 3];\n        assert_eq!(\n            filesystem.read_directory_entry(root, DirectoryCursor::START, &mut short),\n            Err(VfsError::BufferTooSmall)\n        );\n\n        let mut device = filesystem.into_device();\n        let mut first_superblock = [0_u8; FILESYSTEM_BLOCK_SIZE];\n        let mut second_superblock = [0_u8; FILESYSTEM_BLOCK_SIZE];\n        let active = read_active_superblock(\n            &mut device,\n            &mut first_superblock,\n            &mut second_superblock,\n        )\n        .unwrap();\n        let remounted = PhoenixVfs::new(device, active).unwrap();\n\n        name.fill(0);\n        let remounted_first = remounted\n            .read_directory_entry(root, DirectoryCursor::START, &mut name)\n            .unwrap();\n        let remounted_entry = remounted_first.entry.unwrap();\n        assert_eq!(remounted_entry.node, alpha);\n        assert_eq!(&name[..remounted_entry.name_length], b"alpha.txt");\n    }\n\n'''
fs = replace_once(fs, test_anchor, test_code + test_anchor)
fs_path.write_text(fs)

roadmap_path = Path("ROADMAP.md")
roadmap = roadmap_path.read_text()
roadmap = replace_once(
    roadmap,
    "  - перечисление каталога через общий интерфейс ВФС;",
    "  - [x] перечисление каталога через общий интерфейс ВФС с курсором без выделения памяти в куче;",
)
roadmap_path.write_text(roadmap)

status_path = Path("STATUS.md")
status = status_path.read_text()
section = '''\n## PhoenixFS — перечисление каталогов\n\nОбщий интерфейс ВФС теперь поддерживает последовательное чтение записей каталога через непрозрачный курсор. Имя передаётся в буфер вызывающей стороны, поэтому перечисление не требует динамического выделения памяти и не ограничивает PhoenixFS фиксированным размером имени общего объекта.\n\nАдаптер PhoenixFS проверяет тип узла, возвращает идентификатор и тип дочернего объекта, сохраняет порядок записей каталога по идентификатору записи и корректно продолжает перечисление после повторного подключения тома. Недостаточный буфер имени возвращает отдельную ошибку ВФС без изменения курсора.\n'''
if "## PhoenixFS — перечисление каталогов" not in status:
    status = status.rstrip() + "\n" + section
status_path.write_text(status)
