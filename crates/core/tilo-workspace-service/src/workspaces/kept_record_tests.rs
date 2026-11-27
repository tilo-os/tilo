//! The record an action leaves is kept for that record's next write: writing one record many
//! times reads it whole once, however the writes are spread over the device's workers.

use tilo_user_identity::workspace_list::{self, WorkspaceList};

use super::fixtures::{group_keys, hosting_device, test_host_holding};
use super::person_workspace::{self, apply_actions_on};

/// Two records of one kind, written by turns, are each read whole once: once for each record, not
/// once for every write.
#[tokio::test]
async fn a_record_is_read_whole_once_however_often_it_is_written() {
    let root = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let desk = test_host_holding(root.path(), &key, &cert, group_keys()).await;
    let own = person_workspace::found(desk.workspaces())
        .await
        .expect("the person's own workspace is founded");
    let app = workspace_list::APP;
    // Names only this test writes, so what it counts is its own.
    let salt = rand::random::<u64>();
    let records = [format!("first-{salt:x}"), format!("second-{salt:x}")];
    let address = |record: &str| format!("{own}/{app}/{record}");
    let before: Vec<usize> = records
        .iter()
        .map(|record| tilo_resource::action::whole_reads(&address(record)))
        .collect();
    for round in 0..8_u32 {
        for record in &records {
            let listed = format!("{round:08x}-0000-4000-8000-000000000000");
            let actions = vec![
                WorkspaceList::LIST_WORKSPACE.with((listed.clone(), format!("Writer {round}"))),
                WorkspaceList::HOST_WORKSPACE.with((listed, hex::encode(cert.device_public_key))),
            ];
            apply_actions_on(desk.workspaces(), &own, (app, record), actions, None)
                .await
                .expect("the person's list takes the workspace");
        }
    }
    for (record, before) in records.iter().zip(before) {
        assert_eq!(
            tilo_resource::action::whole_reads(&address(record)) - before,
            1,
            "{record} is read whole once, not once for each of its writes"
        );
    }
}
