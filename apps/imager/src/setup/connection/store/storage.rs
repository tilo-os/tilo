//! Private, retained namespace. Every destructive store operation revalidates it.
use std::{ffi::OsStr, fs::File, io::{Read, Write}, path::{Path, PathBuf}, sync::Arc};
use cap_fs_ext::{MetadataExt, OpenOptionsFollowExt};
use cap_primitives::fs::{FollowSymlinks, Metadata, OpenOptions};
use serde::{Deserialize, Serialize};
#[cfg(all(test, target_os="macos"))]
use sha2::{Digest, Sha256};
use super::{StoreError, format};
use super::super::super::{destination_security, migration::{PinnedDirectory, OwnedTemporary}, migration_durability as durability, migration_store::ensure_private_root, nofollow::{metadata_nofollow, same_directory_identity}};
use super::super::model::EndpointIdentity;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Identity { pub volume: [u8;32], pub root: [u8;32] }
impl From<EndpointIdentity> for Identity {
    fn from(value: EndpointIdentity) -> Self { Self { volume: value.volume, root: value.root } }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header { schema: u32, identity: Identity }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Current {
    pub schema: u32, pub identity: Identity, pub generation: u64,
    pub root: Option<[u8;32]>, pub sequence: u64, pub digest: [u8;32], pub log: u64,
}
pub(super) struct Storage {
    pub root: Arc<PinnedDirectory>, pub pages: Arc<PinnedDirectory>,
    pub recovery: Arc<PinnedDirectory>, pub path: PathBuf, pub identity: Identity,
    lock: File, lock_identity: Metadata,
}

fn attest(path: &Path) -> Result<Identity, StoreError> {
    #[cfg(all(test, target_os="macos"))]
    {
        // Host source tests only: real volume UUID/inode, never runtime admission.
        let pin = PinnedDirectory::open_absolute(path)?;
        let bytes = serde_json::to_vec(&durability::identity(&pin.file)?).map_err(|_| StoreError::Corrupt)?;
        Ok(Identity { volume: Sha256::digest(b"host-test-only").into(), root: Sha256::digest(bytes).into() })
    }
    #[cfg(not(all(test, target_os="macos")))]
    { super::super::endpoint::probe_endpoint(path).map(|probe| probe.identity.into()).map_err(|_| StoreError::UnsupportedDurability) }
}
fn admitted() -> Result<(), StoreError> {
    durability::supported().map_err(|_| StoreError::UnsupportedDurability)?;
    #[cfg(not(any(target_os="linux", all(test, target_os="macos"))))]
    return Err(StoreError::UnsupportedDurability);
    #[cfg(any(target_os="linux", all(test, target_os="macos")))]
    Ok(())
}

pub(super) fn regular(file: &File) -> Result<Metadata, StoreError> {
    destination_security::check_private_file(file)?;
    let metadata = Metadata::from_file(file)?;
    super::super::super::migration::reject_windows_placeholder(&metadata)?;
    if !metadata.is_file() || metadata.nlink()!=1 { return Err(StoreError::IdentityChanged); }
    Ok(metadata)
}
pub(super) fn open_file(parent: &Arc<PinnedDirectory>, name: &str, create: bool) -> Result<File, StoreError> {
    parent.validate_destination()?;
    destination_security::check_private_file(&parent.file)?;
    let before = if create { None } else { Some(metadata_nofollow(&parent.file, Path::new(name))?) };
    if before.as_ref().is_some_and(|m| !m.is_file() || m.is_symlink() || m.nlink()!=1) { return Err(StoreError::IdentityChanged); }
    let mut options=OpenOptions::new();
    options.read(true).write(true).create_new(create).follow(FollowSymlinks::No);
    #[cfg(unix)] { use cap_primitives::fs::OpenOptionsExt; options.mode(0o600); }
    let file=cap_primitives::fs::open(&parent.file, Path::new(name), &options)?;
    let after=regular(&file)?;
    if before.as_ref().is_some_and(|m| !same_directory_identity(m,&after)) { return Err(StoreError::IdentityChanged); }
    validate_file(parent,name,&file,&after)?;
    Ok(file)
}
pub(super) fn validate_file(parent: &Arc<PinnedDirectory>, name:&str, file:&File, expected:&Metadata) -> Result<(),StoreError> {
    parent.validate_destination()?;
    let actual=regular(file)?;
    let named=metadata_nofollow(&parent.file,Path::new(name))?;
    if !named.is_file() || named.is_symlink() || named.nlink()!=1 || !same_directory_identity(&named,expected) || !same_directory_identity(&actual,expected) { return Err(StoreError::IdentityChanged); }
    Ok(())
}
fn child(parent:&Arc<PinnedDirectory>, name:&str, create:bool)->Result<Arc<PinnedDirectory>,StoreError> {
    let directory=if create { parent.create_child(OsStr::new(name))? } else {
        let m=metadata_nofollow(&parent.file,Path::new(name))?;
        parent.open_child(OsStr::new(name),&m)?
    };
    directory.validate_destination()?;
    destination_security::check_private_file(&directory.file)?;
    if create { durability::sync_directory(&directory.file)?; durability::sync_directory(&parent.file)?; }
    Ok(directory)
}
impl Storage {
    pub fn open(path:&Path)->Result<(Self,Current),StoreError> {
        admitted()?;
        let mut ancestor=path.to_path_buf();
        while !ancestor.try_exists()? {
            ancestor=ancestor.parent().ok_or(StoreError::Corrupt)?.to_path_buf();
        }
        let anchor=PinnedDirectory::open_absolute(&ancestor)?;
        anchor.validate_destination()?;
        let ancestor_id=attest(&ancestor)?;
        let fresh=ancestor!=path;
        if fresh {
            // Validate the retained ancestor before allowing the existing helper to create.
            anchor.validate_destination()?;
            if attest(&ancestor)?!=ancestor_id { return Err(StoreError::IdentityChanged); }
        }
        let root=ensure_private_root(path)?;
        anchor.validate_destination()?;
        if attest(&ancestor)?!=ancestor_id { return Err(StoreError::IdentityChanged); }
        let identity=attest(path)?;
        root.validate_destination()?;
        if fresh {
            // Sync each newly published child edge, including the first ancestor edge.
            let mut directory=path.to_path_buf();
            loop {
                let pin=PinnedDirectory::open_absolute(&directory)?;
                pin.validate_destination()?;
                durability::sync_directory(&pin.file)?;
                if directory==ancestor { break; }
                directory=directory.parent().ok_or(StoreError::Corrupt)?.to_path_buf();
            }
        }
        let lock=open_file(&root,"ownership.lock",fresh)?;
        let lock_identity=regular(&lock)?;
        lock.try_lock().map_err(|_| StoreError::Busy)?;
        validate_file(&root,"ownership.lock",&lock,&lock_identity)?;
        if fresh { durability::sync_file(&lock)?; durability::sync_directory(&root.file)?; }
        let pages=child(&root,"pages",fresh)?;
        let recovery=child(&root,"recovery",fresh)?;
        let storage=Self {root,pages,recovery,path:path.to_path_buf(),identity,lock,lock_identity};
        storage.validate()?;
        let current=if fresh {
            let header=Header {schema:1,identity:storage.identity.clone()};
            storage.create_bytes(&storage.root,"store-header",&format::encode(&header)?)?;
            let current=Current {schema:1,identity:storage.identity.clone(),generation:0,root:None,sequence:0,digest:[0;32],log:0};
            let mut log=open_file(&storage.root,"log-0",true)?;
            log.write_all(b"QOSCON1\n")?;
            let header=format::LogHeader {schema:1,identity:storage.identity.clone(),generation:0,first_sequence:1,predecessor:[0;32]};
            format::encode(&header)?;
            super::super::super::migration_journal_log::write_record(&mut log,&mut 0,&header)?;
            durability::sync_file(&log)?;
            durability::sync_directory(&storage.root.file)?;
            storage.create_bytes(&storage.root,"CURRENT",&format::encode(&current)?)?;
            current
        } else {
            let header:Header=serde_json::from_slice(&storage.read_bytes(&storage.root,"store-header",4096)?).map_err(|_|StoreError::Corrupt)?;
            if header.schema!=1 || header.identity!=storage.identity {return Err(StoreError::IdentityChanged);}
            let current:Current=serde_json::from_slice(&storage.read_bytes(&storage.root,"CURRENT",4096)?).map_err(|_|StoreError::Corrupt)?;
            if current.schema!=1 || current.identity!=storage.identity {return Err(StoreError::IdentityChanged);}
            current
        };
        Ok((storage,current))
    }
    pub fn validate(&self)->Result<(),StoreError> {
        self.root.validate_destination().map_err(|_|StoreError::IdentityChanged)?;
        if attest(&self.path)?!=self.identity {return Err(StoreError::IdentityChanged);}
        validate_file(&self.root,"ownership.lock",&self.lock,&self.lock_identity)?;
        Ok(())
    }
    pub fn read_bytes(&self,parent:&Arc<PinnedDirectory>,name:&str,max:u64)->Result<Vec<u8>,StoreError> {
        self.validate()?;
        let mut file=open_file(parent,name,false)?;
        let before=regular(&file)?;
        if before.len()>max {return Err(StoreError::LimitExceeded);}
        let mut bytes=Vec::new();
        Read::by_ref(&mut file).take(max.checked_add(1).ok_or(StoreError::LimitExceeded)?).read_to_end(&mut bytes)?;
        if bytes.len() as u64>max {return Err(StoreError::LimitExceeded);}
        validate_file(parent,name,&file,&before)?;
        if !super::super::super::migration::same_snapshot(&before,&regular(&file)?) {return Err(StoreError::IdentityChanged);}
        Ok(bytes)
    }
    pub fn create_bytes(&self,parent:&Arc<PinnedDirectory>,name:&str,bytes:&[u8])->Result<(),StoreError> {
        self.validate()?;
        let mut file=open_file(parent,name,true)?;
        let identity=regular(&file)?;
        file.write_all(bytes)?;
        durability::sync_file(&file)?;
        self.validate()?;
        validate_file(parent,name,&file,&identity)?;
        durability::sync_directory(&parent.file)?;
        Ok(())
    }
    pub fn publish_current(&self,current:&Current)->Result<(),StoreError> {
        self.validate()?;
        let mut temporary=OwnedTemporary::create(&self.root,OsStr::new("CURRENT"))?;
        temporary.file.write_all(&format::encode(current)?)?;
        durability::sync_file(&temporary.file)?;
        temporary.validate()?;
        self.validate()?;
        // CURRENT is the sole replaceable pointer; its target dependencies already exist.
        let existing=open_file(&self.root,"CURRENT",false)?;
        regular(&existing)?;
        cap_primitives::fs::rename(&self.root.file,Path::new(&temporary.name),&self.root.file,Path::new("CURRENT"))?;
        durability::sync_directory(&self.root.file)?;
        Ok(())
    }
}

impl Storage {
    pub fn page(&self,hash:[u8;32])->Result<format::PageNode,StoreError> {
        let bytes=self.read_bytes(&self.pages,&format::hex(&hash),format::MAX_BYTES as u64)?;
        if format::digest(&bytes)!=hash {return Err(StoreError::Corrupt);}
        let page:format::PageNode=serde_json::from_slice(&bytes).map_err(|_|StoreError::Corrupt)?;
        page.reference(hash)?;Ok(page)
    }
    pub fn save_page(&self,page:&format::PageNode)->Result<format::Child,StoreError> {
        let bytes=format::encode(page)?;let hash=format::digest(&bytes);let name=format::hex(&hash);
        let reference=page.reference(hash)?;
        match self.create_bytes(&self.pages,&name,&bytes) {
            Ok(())=>(),
            Err(StoreError::Persistence(error)) if error.kind()==std::io::ErrorKind::AlreadyExists=>{
                if self.read_bytes(&self.pages,&name,format::MAX_BYTES as u64)?!=bytes {return Err(StoreError::Corrupt);}
            },
            Err(error)=>return Err(error),
        }
        Ok(reference)
    }
    pub fn find(&self,root:Option<[u8;32]>,key:&[u8])->Result<Option<format::Row>,StoreError> {
        let Some(mut hash)=root else {return Ok(None)};let mut expected=None;
        for _ in 0..8 {
            let page=self.page(hash)?;
            if expected.as_ref().is_some_and(|e|page.reference(hash).as_ref().ok()!=Some(e)) {return Err(StoreError::Corrupt);}
            match page.node {
                format::Node::Leaf(rows)=>return Ok(rows.into_iter().find(|row|row.key()==key)),
                format::Node::Index(children)=>{
                    let Some(child)=children.into_iter().find(|child|child.first.as_slice()<=key && key<=child.last.as_slice()) else {return Ok(None)};
                    hash=child.hash;expected=Some(child);
                }
            }
        }
        Err(StoreError::LimitExceeded)
    }
}
pub(super) struct TreeIter<'a> {
    storage:&'a Storage,stack:Vec<format::Child>,rows:std::vec::IntoIter<format::Row>,failed:bool,visited:u64,
}
impl<'a> TreeIter<'a> {
    pub fn new(storage:&'a Storage,root:Option<[u8;32]>)->Result<Self,StoreError> {
        let mut stack=Vec::new();
        if let Some(hash)=root {stack.push(storage.page(hash)?.reference(hash)?);}
        Ok(Self {storage,stack,rows:Vec::new().into_iter(),failed:false,visited:0})
    }
}
impl Iterator for TreeIter<'_> {
    type Item=Result<format::Row,StoreError>;
    fn next(&mut self)->Option<Self::Item> {
        if self.failed {return None;}
        loop {
            if let Some(row)=self.rows.next() {return Some(Ok(row));}
            let child=self.stack.pop()?;
            self.visited=self.visited.saturating_add(1);
            let result=(|| {
                if self.visited>2_097_152 {return Err(StoreError::LimitExceeded);}
                let page=self.storage.page(child.hash)?;
                if page.reference(child.hash)?!=child {return Err(StoreError::Corrupt);}
                match page.node {
                    format::Node::Leaf(rows)=>self.rows=rows.into_iter(),
                    format::Node::Index(children)=>self.stack.extend(children.into_iter().rev()),
                }
                Ok(())
            })();
            if let Err(error)=result {self.failed=true;return Some(Err(error));}
        }
    }
}
pub(super) fn build_pages(storage:&Storage,rows:impl Iterator<Item=Result<format::Row,StoreError>>)->Result<Option<[u8;32]>,StoreError> {
    fn feed(storage:&Storage,levels:&mut [Vec<format::Child>],child:format::Child)->Result<(),StoreError> {
        let index=usize::from(child.level);
        let level=levels.get_mut(index).ok_or(StoreError::LimitExceeded)?;
        level.push(child);
        if level.len()==32 {
            let page=format::PageNode {schema:1,node:format::Node::Index(std::mem::take(level))};
            let parent=storage.save_page(&page)?;feed(storage,levels,parent)?;
        }
        Ok(())
    }
    let mut levels:Vec<Vec<format::Child>>=(0..8).map(|_|Vec::new()).collect();
    let mut leaf=Vec::new();let mut previous=None;
    for result in rows {
        let row=result?;let key=row.key();
        if previous.as_ref().is_some_and(|p|p>=&key) {return Err(StoreError::Corrupt);} previous=Some(key);
        leaf.push(row);
        let page=format::PageNode {schema:1,node:format::Node::Leaf(leaf.clone())};
        if leaf.len()>128 || format::encode(&page).is_err() {
            let last=leaf.pop().ok_or(StoreError::Corrupt)?;
            let page=format::PageNode {schema:1,node:format::Node::Leaf(std::mem::take(&mut leaf))};
            feed(storage,&mut levels,storage.save_page(&page)?)?;
            leaf.push(last);
        }
    }
    if !leaf.is_empty() {feed(storage,&mut levels,storage.save_page(&format::PageNode {schema:1,node:format::Node::Leaf(leaf)})?)?;}
    for level in 0..8 {
        let children=std::mem::take(levels.get_mut(level).ok_or(StoreError::LimitExceeded)?);
        if children.is_empty() {continue;}
        if children.len()==1 && levels.iter().all(Vec::is_empty) {return Ok(children.first().map(|child|child.hash));}
        feed(storage,&mut levels,storage.save_page(&format::PageNode {schema:1,node:format::Node::Index(children)})?)?;
    }
    if levels.iter().all(Vec::is_empty) {Ok(None)} else {Err(StoreError::LimitExceeded)}
}
