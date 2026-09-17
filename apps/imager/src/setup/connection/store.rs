//! Private durable Connected metadata. Persisted records never grant endpoint access.
mod format;
mod recovery;
mod storage;
#[cfg(test)]
mod tests;

use std::path::Path;

#[derive(Debug)]
pub(super) enum StoreError {
    Busy, UnsupportedDurability, IdentityChanged, Corrupt, LimitExceeded,
    StaleRevision, Disconnected, RecoveryRequired, RecoverySpace, Cancelled,
    Persistence(std::io::Error),
}
impl From<std::io::Error> for StoreError {
    fn from(error: std::io::Error) -> Self { Self::Persistence(error) }
}

use std::sync::Arc;
use super::{endpoint::EndpointLocator, model::*};
pub(super) type Revision=u64;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub(super) struct OperationId { connection:ConnectionId, serial:u64 }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub(super) struct RecoveryId { operation:OperationId, slot:u32 }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub(super) struct ConflictId { connection:ConnectionId, serial:u64 }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub(super) enum Lifecycle { Active, Disconnecting, Disconnected }
#[derive(Clone,Debug,PartialEq,Eq)]
pub(super) struct ConnectionPolicy { deletion_retention_seconds:u64 }
#[derive(Clone,Debug,PartialEq,Eq)]
pub(super) struct EndpointBinding { identity:EndpointIdentity, locator:EndpointLocator }
#[derive(Clone,Debug,PartialEq,Eq)]
pub(super) struct ConnectionRecord {
    id:ConnectionId, revision:Revision, source:EndpointBinding, destination:EndpointBinding,
    policy:ConnectionPolicy,lifecycle:Lifecycle,baseline_revision:Option<Revision>,fence_generation:u64,
}
#[derive(Clone,Debug,PartialEq,Eq)]
pub(super) struct OperationIntent {
    connection:ConnectionId,expected_revision:Revision,expected_baseline:Option<Revision>,
    path:RelativePath,source_before:Option<EntryState>,destination_before:Option<EntryState>,decision:Decision,
}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub(super) enum ReceiptStage { Intent, Preserved, Published, Verified, BaselineCommitted, Aborted }
#[derive(Clone,Debug,PartialEq,Eq)]
pub(super) struct OperationReceipt { id:OperationId,revision:Revision,stage:ReceiptStage,intent:OperationIntent,recovery:Vec<RecoveryId> }
#[derive(Clone,Debug,PartialEq,Eq)]
pub(super) enum RecoveryReason { NormalDeletion, DisplacedVersion, Conflict(ConflictId) }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub(super) enum RecoveryState { Reserved, BytesVerified, Attached, PurgeIntent, Purged }
#[derive(Clone,Debug,PartialEq,Eq)]
pub(super) struct RecoveryEntry {
    id:RecoveryId,original_side:Side,original_path:RelativePath,state:RecoveryState,reason:RecoveryReason,
    content:Option<EntryState>,retained_seconds:u64,
}
#[derive(Clone,Debug,PartialEq,Eq)]
pub(super) struct ConflictRecord {
    id:ConflictId,revision:Revision,kind:Conflict,path:RelativePath,source:Option<EntryState>,destination:Option<EntryState>,preserved:Vec<RecoveryId>,resolved:bool,
}
pub(super) struct PageCursor { checkpoint:u64,last_key:Vec<u8> }
pub(super) struct Page<T> { entries:Vec<T>,next:Option<PageCursor> }
pub(super) struct DurableAck<T> { value:T,sequence:u64,session:Arc<()> }
pub(super) struct DisconnectFence { connection:ConnectionId,generation:u64 }

use std::{collections::BTreeMap,fs::File,io::{Read,Seek,SeekFrom,Write}};
use super::super::{migration_durability as durability,migration_journal_log as framing};
use format::{Change,Connection,Event,Row};
pub(super) struct ConnectionStore {
    storage:storage::Storage,current:storage::Current,log:File,log_identity:cap_primitives::fs::Metadata,
    overlay:BTreeMap<Vec<u8>,Row>,overlay_bytes:usize,sequence:u64,digest:[u8;32],session:Arc<()>,
    poisoned:bool,connections:u64,active_intents:u64,
}
impl ConnectionStore {
    pub(super) fn open_default()->Result<Self,StoreError> {
        let path=dirs::data_local_dir().ok_or(StoreError::UnsupportedDurability)?.join("qos-imager").join("connections-v1");Self::open_at(&path)
    }
    pub(super) fn open_at(path:&Path)->Result<Self,StoreError> {
        let (storage,current)=storage::Storage::open(path)?;
        let log_name=format!("log-{}",current.log);
        let mut log=storage::open_file(&storage.root,&log_name,false)?;
        let log_identity=storage::regular(&log)?;
        if log_identity.len()>16*1024*1024 {return Err(StoreError::LimitExceeded);}
        let mut magic=[0;8];log.read_exact(&mut magic)?;
        if &magic!=b"QOSCON1\n" {return Err(StoreError::Corrupt);}
        let header:format::LogHeader=framing::read_record(&mut log,0).map_err(|_|StoreError::Corrupt)?.ok_or(StoreError::Corrupt)?;
        if header.schema!=1 || header.identity!=storage.identity || header.generation!=current.generation || header.first_sequence!=format::next(current.sequence)? || header.predecessor!=current.digest {return Err(StoreError::Corrupt);}
        let sequence=current.sequence;let digest=current.digest;
        let mut store=Self {storage,current,log,log_identity,overlay:BTreeMap::new(),overlay_bytes:0,sequence,digest,session:Arc::new(()),poisoned:false,connections:0,active_intents:0};
        for result in storage::TreeIter::new(&store.storage,store.current.root)? {
            if let Row::Connection(connection)=result? {store.connections=format::next(store.connections)?;if connection.pending.is_some() {store.active_intents=format::next(store.active_intents)?;}}
        }
        if store.connections>65536 || store.active_intents>1024 {return Err(StoreError::LimitExceeded);}
        loop {
            let offset=store.log.stream_position()?;
            let event:Option<Event>=framing::read_record(&mut store.log,format::next(store.sequence)?).map_err(|_|StoreError::Corrupt)?;
            let Some(event)=event else {
                if offset!=store.log.metadata()?.len() {
                    store.validate()?;
                    store.log.set_len(offset)?;
                    durability::sync_file(&store.log)?;
                    store.log.seek(SeekFrom::Start(offset))?;
                }
                break;
            };
            let rows=store.reduce(&event).map_err(|_|StoreError::Corrupt)?;
            let bytes=format::encode(&event)?;
            store.commit_rows(rows)?;
            store.sequence=format::next(store.sequence)?;store.digest=format::digest(&bytes);
        }
        Ok(store)
    }
    fn validate(&self)->Result<(),StoreError> {
        if self.poisoned {return Err(StoreError::RecoveryRequired);}
        self.storage.validate()?;
        storage::validate_file(&self.storage.root,&format!("log-{}",self.current.log),&self.log,&self.log_identity)
    }
    fn row(&self,key:&[u8])->Result<Option<Row>,StoreError> {
        if let Some(value)=self.overlay.get(key) {return Ok(Some(value.clone()));}
        self.storage.find(self.current.root,key)
    }
    fn connection(&self,id:ConnectionId)->Result<Connection,StoreError> {
        self.validate()?;
        match self.row(&format::key(0,id.0,0))? {Some(Row::Connection(c))=>Ok(c),_=>Err(StoreError::Corrupt)}
    }
    pub(super) fn load_connection(&self,id:ConnectionId)->Result<ConnectionRecord,StoreError> {self.connection(id)?.decode()}
    pub(super) fn load_baseline(&self,id:ConnectionId)->Result<VerifiedBaseline,StoreError> {
        let record=self.load_connection(id)?;
        if record.baseline_revision.is_some() {return Err(StoreError::RecoveryRequired);}
        Ok(VerifiedBaseline::absent(id,record.source.identity,record.destination.identity))
    }
    fn rows(&self)->Result<impl Iterator<Item=Result<Row,StoreError>>+'_,StoreError> {
        let mut old=storage::TreeIter::new(&self.storage,self.current.root)?.peekable();
        let mut overlay=self.overlay.values().peekable();
        Ok(std::iter::from_fn(move ||{
            match (old.peek(),overlay.peek()) {
                (Some(Err(_)),_)=>old.next(),
                (Some(Ok(a)),Some(b))=>match a.key().cmp(&b.key()) {
                    std::cmp::Ordering::Less=>old.next(),
                    std::cmp::Ordering::Equal=>{old.next();overlay.next().cloned().map(Ok)},
                    std::cmp::Ordering::Greater=>overlay.next().cloned().map(Ok),
                },
                (Some(_),None)=>old.next(),(None,Some(_))=>overlay.next().cloned().map(Ok),(None,None)=>None,
            }
        }))
    }
    pub(super) fn connection_page(&self,cursor:Option<PageCursor>,limit:usize)->Result<Page<ConnectionRecord>,StoreError> {
        self.validate()?;
        if !(1..=128).contains(&limit) {return Err(StoreError::LimitExceeded);}
        let after=match cursor {Some(c) if c.checkpoint==self.sequence && c.last_key.len()==25 && c.last_key.first()==Some(&0)=>Some(c.last_key),Some(_)=>return Err(StoreError::StaleRevision),None=>None};
        let mut entries=Vec::new();let mut last=None;let mut next=None;
        for result in self.rows()? {
            let row=result?;let key=row.key();
            if key.first()!=Some(&0) {break;}
            if after.as_ref().is_some_and(|after|&key<=after) {continue;}
            if entries.len()==limit {next=Some(PageCursor {checkpoint:self.sequence,last_key:last.ok_or(StoreError::Corrupt)?});break;}
            if let Row::Connection(connection)=row {entries.push(connection.decode()?);last=Some(key);}
        }
        Ok(Page {entries,next})
    }
    fn ack<T>(&self,value:T)->DurableAck<T> {DurableAck {value,sequence:self.sequence,session:Arc::clone(&self.session)}}
    fn check_ack<T>(&self,ack:&DurableAck<T>)->Result<(),StoreError> {
        if !Arc::ptr_eq(&self.session,&ack.session) || ack.sequence>self.sequence {return Err(StoreError::StaleRevision);}Ok(())
    }
    pub(super) fn create_connection(&mut self,id:ConnectionId,source:EndpointBinding,destination:EndpointBinding,policy:ConnectionPolicy)->Result<DurableAck<ConnectionRecord>,StoreError> {
        let source_path=source.locator.path();let destination_path=destination.locator.path();
        if source_path.starts_with(destination_path) || destination_path.starts_with(source_path) || self.storage.path.starts_with(source_path) || self.storage.path.starts_with(destination_path) {return Err(StoreError::IdentityChanged);}
        let record=Connection::new(id,source,destination,policy)?;
        let value=record.decode()?;
        self.append(Change::Create {record})?;Ok(self.ack(value))
    }
    pub(super) fn begin_intent(&mut self,intent:OperationIntent)->Result<DurableAck<OperationId>,StoreError> {
        let connection=self.connection(intent.connection)?;
        if connection.lifecycle!=0 {return Err(StoreError::Disconnected);}
        let serial=format::next(connection.serial)?;
        let id=OperationId {connection:intent.connection,serial};
        self.append(Change::Intent {operation:format::Operation::new(intent,serial)?})?;
        Ok(self.ack(id))
    }
    pub(super) fn request_disconnect(&mut self,id:ConnectionId,expected:Revision)->Result<DurableAck<DisconnectFence>,StoreError> {
        let connection=self.connection(id)?;
        if connection.revision!=expected {return Err(StoreError::StaleRevision);}
        if connection.lifecycle==2 {return Err(StoreError::Disconnected);}
        let generation=if connection.lifecycle==1 {connection.fence} else {
            let generation=format::next(connection.fence)?;
            self.append(Change::Fence {connection:id.0,expected,generation})?;generation
        };
        Ok(self.ack(DisconnectFence {connection:id,generation}))
    }
    pub(super) fn finish_disconnect(&mut self,fence:&DurableAck<DisconnectFence>)->Result<DurableAck<ConnectionRecord>,StoreError> {
        self.check_ack(fence)?;
        let connection=self.connection(fence.value.connection)?;
        if connection.lifecycle!=1 || connection.fence!=fence.value.generation {return Err(StoreError::StaleRevision);}
        if connection.pending.is_some() {return Err(StoreError::RecoveryRequired);}
        self.append(Change::Disconnect {connection:connection.id,expected:connection.revision,generation:connection.fence})?;
        Ok(self.ack(self.load_connection(fence.value.connection)?))
    }
    fn reduce(&self,event:&Event)->Result<Vec<Row>,StoreError> {
        if event.schema!=1 || event.predecessor!=self.digest {return Err(StoreError::Corrupt);}
        match &event.change {
            Change::Create {record}=>{
                record.decode()?;
                if record.revision!=1 || record.serial!=0 || record.pending.is_some() || record.baseline.is_some() || record.lifecycle!=0 || self.connections>=65536 {return Err(StoreError::LimitExceeded);}
                if self.row(&format::key(0,record.id,0))?.is_some() {return Err(StoreError::StaleRevision);}
                Ok(vec![Row::Connection(record.clone())])
            },
            Change::Intent {operation}=>{
                let receipt=operation.decode()?;
                let mut connection=self.connection(ConnectionId(operation.connection))?;
                if connection.lifecycle!=0 {return Err(StoreError::Disconnected);}
                if connection.revision!=operation.expected || connection.baseline!=operation.baseline || operation.serial!=format::next(connection.serial)? || operation.revision!=format::next(connection.revision)? {return Err(StoreError::StaleRevision);}
                if connection.pending.is_some() || self.active_intents>=1024 {return Err(StoreError::RecoveryRequired);}
                if operation.stage!=0 || !operation.recovery.is_empty() {return Err(StoreError::Corrupt);}
                match receipt.intent.decision {
                    Decision::VerifyEqual if operation.source!=operation.destination=>return Err(StoreError::Corrupt),
                    Decision::Propagate {from:Side::Source} if operation.source.is_none()=>return Err(StoreError::Corrupt),
                    Decision::Propagate {from:Side::Destination} if operation.destination.is_none()=>return Err(StoreError::Corrupt),
                    Decision::RecoverDeletion {at:Side::Source} if operation.source.is_none() || operation.destination.is_some()=>return Err(StoreError::Corrupt),
                    Decision::RecoverDeletion {at:Side::Destination} if operation.destination.is_none() || operation.source.is_some()=>return Err(StoreError::Corrupt),_=>(),
                }
                connection.pending=Some(operation.serial);connection.serial=operation.serial;connection.revision=operation.revision;
                Ok(vec![Row::Connection(connection),Row::Operation(operation.clone())])
            },
            Change::Fence {connection,expected,generation}|Change::Disconnect {connection,expected,generation}=>{
                let mut record=self.connection(ConnectionId(*connection))?;
                if record.revision!=*expected {return Err(StoreError::StaleRevision);}
                if matches!(event.change,Change::Fence {..}) {
                    if record.lifecycle!=0 || *generation!=format::next(record.fence)? {return Err(StoreError::Disconnected);}
                    record.lifecycle=1;record.fence=*generation;
                } else {
                    if record.lifecycle!=1 || record.fence!=*generation || record.pending.is_some() {return Err(StoreError::RecoveryRequired);}
                    record.lifecycle=2;
                }
                record.revision=format::next(record.revision)?;Ok(vec![Row::Connection(record)])
            }
        }
    }
    fn commit_rows(&mut self,rows:Vec<Row>)->Result<(),StoreError> {
        // All capacity checks precede insertion, so memory commit is atomic after fsync.
        let mut bytes=self.overlay_bytes;let mut keys=self.overlay.len();
        for row in &rows {
            if let Some(old)=self.overlay.get(&row.key()) {bytes=bytes.checked_sub(format::encode(old)?.len()).ok_or(StoreError::Corrupt)?;} else {keys=keys.checked_add(1).ok_or(StoreError::LimitExceeded)?;}
            bytes=bytes.checked_add(format::encode(row)?.len()).ok_or(StoreError::LimitExceeded)?;
        }
        if keys>1024 || bytes>8*1024*1024 {return Err(StoreError::LimitExceeded);}
        for row in rows {
            if let Row::Connection(connection)=&row {
                let old=self.row(&row.key())?;
                if old.is_none() {self.connections=format::next(self.connections)?;}
                let old_pending=matches!(old,Some(Row::Connection(c)) if c.pending.is_some());
                if connection.pending.is_some() && !old_pending {self.active_intents=format::next(self.active_intents)?;}
            }
            self.overlay.insert(row.key(),row);
        }
        self.overlay_bytes=bytes;Ok(())
    }
    fn append(&mut self,change:Change)->Result<(),StoreError> {
        let result=(|| {
            self.validate()?;
            let event=Event {schema:1,predecessor:self.digest,change};
            let rows=self.reduce(&event)?;let bytes=format::encode(&event)?;
            let control=matches!(event.change,Change::Fence {..}|Change::Disconnect {..});
            if matches!(event.change,Change::Fence {..}) && bytes.len()+44>512 {return Err(StoreError::LimitExceeded);}
            if self.overlay.len()+rows.len()>1024 || self.overlay_bytes+bytes.len()*2>8*1024*1024 || self.log.metadata()?.len()>8*1024*1024 {self.checkpoint()?;}
            let bound=if control {16*1024*1024} else {15*1024*1024};
            if self.log.metadata()?.len()+bytes.len() as u64+44>bound {return Err(StoreError::LimitExceeded);}
            self.validate()?;
            let mut sequence=format::next(self.sequence)?;
            framing::write_record(&mut self.log,&mut sequence,&event)?;
            durability::sync_file(&self.log)?;
            self.validate()?;
            self.commit_rows(rows)?;self.sequence=format::next(self.sequence)?;self.digest=format::digest(&bytes);Ok(())
        })();
        if matches!(result,Err(StoreError::Persistence(_)|StoreError::IdentityChanged)) {self.poisoned=true;}
        result
    }
    pub(super) fn checkpoint(&mut self)->Result<DurableAck<u64>,StoreError> {
        let result=(|| {
            self.validate()?;
            let root=storage::build_pages(&self.storage,self.rows()?)?;
            let generation=format::next(self.current.generation)?;
            let mut next=self.current.clone();next.generation=generation;next.root=root;next.sequence=self.sequence;next.digest=self.digest;next.log=generation;
            let mut log=storage::open_file(&self.storage.root,&format!("log-{generation}"),true)?;
            log.write_all(b"QOSCON1\n")?;
            let header=format::LogHeader {schema:1,identity:self.storage.identity.clone(),generation,first_sequence:format::next(self.sequence)?,predecessor:self.digest};
            format::encode(&header)?;framing::write_record(&mut log,&mut 0,&header)?;
            durability::sync_file(&log)?;durability::sync_directory(&self.storage.root.file)?;
            self.storage.publish_current(&next)?;
            self.log_identity=storage::regular(&log)?;self.log=log;self.current=next;self.overlay.clear();self.overlay_bytes=0;
            Ok(self.ack(generation))
        })();
        if result.is_err() {self.poisoned=true;}result
    }
}
