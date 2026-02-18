use fuser::*; 
use libc::{ENOENT, EISDIR, EIO};
use std::ffi::OsStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use crate::metadata::{MetadataStore};
use crate::hydration::Hydrator;
use crate::smart::Heuristics;
use std::io;
use iroh_base::EndpointAddr;
use iroh::SecretKey;

const TTL: Duration = Duration::from_secs(10); 

pub struct QuickFS {
    pub metadata: Arc<Mutex<MetadataStore>>,
    pub hydrator: Arc<Hydrator>,
    pub heuristics: Arc<Heuristics>,
}

impl QuickFS {
    pub fn new(metadata: Arc<Mutex<MetadataStore>>, hydrator: Arc<Hydrator>, heuristics: Arc<Heuristics>) -> Self {
        Self {
            metadata,
            hydrator,
            heuristics,
        }
    }
}

fn to_errno(e: i32) -> Errno {
    io::Error::from_raw_os_error(e).into()
}

impl Filesystem for QuickFS {
    fn init(&mut self, _req: &Request, config: &mut KernelConfig) -> Result<(), io::Error> {
        let _ = config.set_max_readahead(1024 * 1024); 
        Ok(())
    }

    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let name_str = name.to_str().expect("Invalid UTF-8 name");
        let store = self.metadata.lock().unwrap();
        
        if let Some(child) = store.get_child_by_name(parent.0, name_str) {
            reply.entry(&TTL, &child.to_file_attr(), Generation(0));
        } else {
             reply.error(to_errno(ENOENT));
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        let store = self.metadata.lock().unwrap();
        if let Some(node) = store.get_node(ino.0) {
            reply.attr(&TTL, &node.to_file_attr());
        } else {
             reply.error(to_errno(ENOENT));
        }
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64, 
        mut reply: ReplyDirectory,
    ) {
        let store = self.metadata.lock().unwrap();
        
        if let Some(node) = store.get_node(ino.0) {
            if let Some(children) = &node.children {
                let mut entries = Vec::new();
                if let Some(current_node) = store.get_node(ino.0) {
                    entries.push((ino.0, current_node.kind.to_file_type(), "."));
                }
                if let Some(parent_node) = store.get_node(node.parent) {
                     entries.push((node.parent, parent_node.kind.to_file_type(), ".."));
                }

                for &child_ino in children {
                    if let Some(child) = store.get_node(child_ino) {
                         entries.push((child.ino, child.kind.to_file_type(), &child.name));
                    }
                }

                for (i, entry) in entries.into_iter().enumerate().skip(offset as usize) {
                    if reply.add(INodeNo(entry.0), (i + 1) as u64, entry.1, entry.2) {
                        break;
                    }
                }
                reply.ok();

                drop(store);
                self.heuristics.on_readdir(ino.0);
            } else {
                 reply.error(to_errno(ENOENT));
            }
        } else {
              reply.error(to_errno(ENOENT));
        }
    }

    fn open(&self, _req: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        let store = self.metadata.lock().unwrap();
        if store.get_node(ino.0).is_some() {
             reply.opened(FileHandle(0), FopenFlags::empty());
             drop(store);
             self.heuristics.on_open(std::path::Path::new(""), 0);
        } else {
             reply.error(to_errno(ENOENT));
        }
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64, 
        size: u32,
        _flags: OpenFlags, 
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let hash = {
            let store = self.metadata.lock().unwrap();
            if let Some(node) = store.get_node(ino.0) {
                if let Some(h) = &node.hash {
                    *h
                } else {
                    reply.error(to_errno(EISDIR));
                    return;
                }
            } else {
                reply.error(to_errno(ENOENT));
                return;
            }
        };

        let hydrator = self.hydrator.clone();
        let rt = hydrator.runtime_handle.clone();
        
        // Placeholder NodeAddr
        let node_addr = EndpointAddr::new(SecretKey::from_bytes(&[0u8; 32]).public());

        let content_res = tokio::task::block_in_place(|| {
             rt.block_on(async move {
                 hydrator.fetch_range(hash, node_addr, offset, size as u64).await
             })
        });

        match content_res {
            Ok(content) => {
                reply.data(&content);
            }
            Err(_) => {
                reply.error(to_errno(EIO));
            }
        }
    }
}
