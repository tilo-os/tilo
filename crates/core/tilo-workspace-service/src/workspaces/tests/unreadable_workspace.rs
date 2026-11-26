//! A workspace whose own record cannot be read as the service opens: it is reported closed and
//! left exactly as it is on disk, while every other workspace opens and serves.

use std::collections::BTreeMap;
use std::path::Path;

use super::*;

#[tokio::test]
async fn a_workspace_whose_record_does_not_read_stays_closed_while_the_others_serve() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let (good, bad);
    {
        let server = test_host(dir.path(), &key, &cert).await;
        good = server.workspaces().create("Good").await.unwrap().workspace;
        bad = server.workspaces().create("Bad").await.unwrap().workspace;
        server.shutdown().await;
    }
    let records = dir.path().join("workspaces");
    let record = records.join(format!("{bad}.workspace"));
    // An interrupted write: no workspace's record reads from what is left, whatever this build
    // brings up.
    std::fs::write(&record, b"").unwrap();
    let kept = kept_beside(&records, &bad);

    let again = test_host(dir.path(), &key, &cert).await;
    let workspaces = again.workspaces();
    // Every other workspace opened and serves.
    let served = workspaces
        .summary(&good)
        .await
        .expect("the workspace that reads serves");
    assert_eq!(served.name, "Good");
    // The one that did not read is neither served nor silent: whoever asks for it hears why.
    let unsaid = workspaces
        .summary(&bad)
        .await
        .expect_err("a workspace whose record did not read is not served");
    assert!(
        unsaid.to_string().contains("stays closed"),
        "a closed workspace says why to whoever asks for it: {unsaid}"
    );
    let closed = workspaces.closed();
    let reported = closed
        .get(&bad)
        .unwrap_or_else(|| panic!("{bad} is reported closed: {closed:?}"));
    assert_eq!(reported.file, record);
    assert!(
        reported
            .reason
            .contains("a workspace record names its version first"),
        "the report says why it did not read: {}",
        reported.reason
    );
    assert!(
        reported.reason.contains(&record.display().to_string()),
        "the report names the file it could not read: {}",
        reported.reason
    );
    // Its files are exactly as they were.
    assert_eq!(
        kept_beside(&records, &bad),
        kept,
        "a workspace that did not read is left on disk as it is"
    );
    // Making a workspace under its id is refused, and writes nothing over its files.
    let refused = workspaces
        .create_for("Bad", Some(&bad), cert.user_public_key)
        .await;
    assert!(
        refused.is_err(),
        "a workspace whose record did not read is not made again over its files"
    );
    assert_eq!(
        kept_beside(&records, &bad),
        kept,
        "a refused create writes nothing over the closed workspace's files"
    );
    again.shutdown().await;
}

/// Every file of `workspace`'s beside the workspaces' records, by path, as it is now.
fn kept_beside(records: &Path, workspace: &str) -> BTreeMap<String, Vec<u8>> {
    let mut kept = BTreeMap::new();
    let mut dirs = vec![records.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            let relative = path
                .strip_prefix(records)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if !relative.starts_with(workspace) {
                continue;
            }
            if path.is_dir() {
                dirs.push(path);
            } else {
                kept.insert(relative, std::fs::read(&path).unwrap());
            }
        }
    }
    kept
}
