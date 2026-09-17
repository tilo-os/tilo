use super::*;

// Catches absence of a durable initial namespace and successful reopen.
#[test]
fn empty_store_is_durable_and_exclusive() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap().join("connections");
    let store = ConnectionStore::open_at(&root).expect("initialize real private store");
    assert!(root.join("store-header").is_file());
    assert!(root.join("CURRENT").is_file());
    assert!(matches!(ConnectionStore::open_at(&root), Err(StoreError::Busy)));
    drop(store);
    assert!(ConnectionStore::open_at(&root).is_ok());
}

fn bindings()->(EndpointBinding,EndpointBinding) {
    (EndpointBinding {identity:EndpointIdentity {volume:[1;32],root:[2;32]},locator:EndpointLocator::new(Path::new("/source")).unwrap()},
    EndpointBinding {identity:EndpointIdentity {volume:[3;32],root:[4;32]},locator:EndpointLocator::new(Path::new("/destination")).unwrap()})
}
fn create(store:&mut ConnectionStore,id:u32)->ConnectionRecord {
    let mut bytes=[0;16]; bytes[12..].copy_from_slice(&id.to_be_bytes());
    let (source,destination)=bindings();
    store.create_connection(ConnectionId(bytes),source,destination,ConnectionPolicy {deletion_retention_seconds:2_592_000}).unwrap().value
}
fn intent(record:&ConnectionRecord)->OperationIntent {
    OperationIntent {connection:record.id,expected_revision:record.revision,expected_baseline:None,path:RelativePath::unix(b"file").unwrap(),source_before:Some(EntryState::File {sha256:[7;32],bytes:3,executable:false}),destination_before:None,decision:Decision::Propagate {from:Side::Source}}
}
fn fixture()->(tempfile::TempDir,std::path::PathBuf) {
    let directory=tempfile::tempdir().unwrap();
    let root=directory.path().canonicalize().unwrap().join("connections");
    (directory,root)
}
// Catches silently losing durable connection settings or inventing a baseline.
#[test]
fn connection_and_absent_baseline_survive_reopen() {
    let (_temp,path)=fixture(); let mut store=ConnectionStore::open_at(&path).unwrap();
    let record=create(&mut store,7); drop(store);
    let reopened=ConnectionStore::open_at(&path).unwrap();
    assert_eq!(reopened.load_connection(record.id).unwrap(),record);
    assert!(reopened.load_baseline(record.id).unwrap().entries().is_none());
}
// Catches lost fences across checkpoint/reopen and reuse of old live tokens.
#[test]
fn fence_blocks_restart_and_stale_ack() {
    let (_temp,path)=fixture(); let mut store=ConnectionStore::open_at(&path).unwrap();
    let record=create(&mut store,1);
    let fence=store.request_disconnect(record.id,record.revision).unwrap();
    store.checkpoint().unwrap(); drop(store);
    let mut reopened=ConnectionStore::open_at(&path).unwrap();
    assert!(matches!(reopened.begin_intent(intent(&record)),Err(StoreError::Disconnected)));
    assert!(matches!(reopened.finish_disconnect(&fence),Err(StoreError::StaleRevision)));
    let current=reopened.load_connection(record.id).unwrap();
    let fresh=reopened.request_disconnect(record.id,current.revision).unwrap();
    assert_eq!(reopened.finish_disconnect(&fresh).unwrap().value.lifecycle,Lifecycle::Disconnected);
    drop(reopened);
    assert_eq!(ConnectionStore::open_at(&path).unwrap().load_connection(record.id).unwrap().lifecycle,Lifecycle::Disconnected);
}
// Catches treating an unproved intent as safely aborted when disconnecting.
#[test]
fn pending_intent_prevents_disconnect_completion() {
    let (_temp,path)=fixture(); let mut store=ConnectionStore::open_at(&path).unwrap();
    let record=create(&mut store,1); store.begin_intent(intent(&record)).unwrap();
    let current=store.load_connection(record.id).unwrap();
    let fence=store.request_disconnect(record.id,current.revision).unwrap();
    assert!(matches!(store.finish_disconnect(&fence),Err(StoreError::RecoveryRequired)));
    drop(store);
    let reopened=ConnectionStore::open_at(&path).unwrap();
    assert_eq!(reopened.load_connection(record.id).unwrap().lifecycle,Lifecycle::Disconnecting);
}
// Catches whole-inventory collection and dropped keys during paged checkpoint merge.
#[test]
fn large_inventory_uses_bounded_pages() {
    let (_temp,path)=fixture(); let mut store=ConnectionStore::open_at(&path).unwrap();
    for id in 0..4097 {create(&mut store,id);}
    store.checkpoint().unwrap(); drop(store);
    let store=ConnectionStore::open_at(&path).unwrap();
    let mut cursor=None; let mut count=0;
    loop {
        let page=store.connection_page(cursor,128).unwrap();
        assert!(page.entries.len()<=128);
        for record in page.entries {assert_eq!(u32::from_be_bytes(record.id.0[12..].try_into().unwrap()),count); count+=1;}
        cursor=page.next; if cursor.is_none() {break;}
    }
    assert_eq!(count,4097);
}
