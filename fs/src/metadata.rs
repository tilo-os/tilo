use std::collections::HashMap;
use fuser::{FileAttr, FileType, INodeNo}; 
use serde::{Deserialize, Serialize};
use std::time::UNIX_EPOCH;
use iroh_blobs::Hash;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NodeKind {
    File,
    Directory,
}

impl NodeKind {
    pub fn to_file_type(&self) -> FileType {
        match self {
            NodeKind::File => FileType::RegularFile,
            NodeKind::Directory => FileType::Directory,
        }
    }
}

/// Represents a file or directory backed by Iroh Blobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileNode {
    pub ino: u64,
    pub name: String,
    pub kind: NodeKind,
    pub size: u64,
    pub hash: Option<Hash>, // Iroh Blob Hash
    pub children: Option<Vec<u64>>, // Only for directories
    pub parent: u64,
}

impl FileNode {
    pub fn new_dir(ino: u64, name: String, parent: u64) -> Self {
        Self {
            ino,
            name,
            kind: NodeKind::Directory,
            size: 0,
            hash: None,
            children: Some(Vec::new()),
            parent,
        }
    }

    pub fn new_file(ino: u64, name: String, parent: u64, size: u64, hash: Hash) -> Self {
        Self {
            ino,
            name,
            kind: NodeKind::File,
            size,
            hash: Some(hash),
            children: None,
            parent,
        }
    }

    pub fn to_file_attr(&self) -> FileAttr {
        FileAttr {
            ino: INodeNo(self.ino),
            size: self.size,
            blocks: (self.size + 511) / 512,
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind: self.kind.to_file_type(),
            perm: match self.kind {
                NodeKind::Directory => 0o755,
                NodeKind::File => 0o644,
            },
            nlink: match self.kind {
                NodeKind::Directory => 2,
                NodeKind::File => 1,
            },
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
            rdev: 0,
            blksize: 512,
            flags: 0,
        }
    }
}

pub struct MetadataStore {
    pub nodes: HashMap<u64, FileNode>,
    next_ino: u64,
}

impl MetadataStore {
    pub fn new() -> Self {
        let mut nodes = HashMap::new();
        nodes.insert(1, FileNode::new_dir(1, "/".to_string(), 1));
        
        Self {
            nodes,
            next_ino: 2,
        }
    }

    pub fn get_node(&self, ino: u64) -> Option<&FileNode> {
        self.nodes.get(&ino)
    }

    pub fn get_child_by_name(&self, parent_ino: u64, name: &str) -> Option<&FileNode> {
        if let Some(parent) = self.nodes.get(&parent_ino) {
            if let Some(children) = &parent.children {
                for child_ino in children {
                    if let Some(child) = self.nodes.get(child_ino) {
                        if child.name == name {
                            return Some(child);
                        }
                    }
                }
            }
        }
        None
    }

    pub fn create_file(&mut self, parent_ino: u64, name: &str, size: u64, hash: Hash) -> u64 {
        let ino = self.next_ino;
        self.next_ino += 1;
        let node = FileNode::new_file(ino, name.to_string(), parent_ino, size, hash);
        self.nodes.insert(ino, node);

        if let Some(parent) = self.nodes.get_mut(&parent_ino) {
            if let Some(children) = &mut parent.children {
                children.push(ino);
            }
        }
        ino
    }

    pub fn create_dir(&mut self, parent_ino: u64, name: &str) -> u64 {
        let ino = self.next_ino;
        self.next_ino += 1;
        let node = FileNode::new_dir(ino, name.to_string(), parent_ino);
        self.nodes.insert(ino, node);

        if let Some(parent) = self.nodes.get_mut(&parent_ino) {
            if let Some(children) = &mut parent.children {
                children.push(ino);
            }
        }
        ino
    }
}
