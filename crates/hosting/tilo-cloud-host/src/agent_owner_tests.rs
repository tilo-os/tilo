//! Enrolled Store fixtures and the private agent ledger's Store boundary.
use super::*;

/// A fresh temporary directory on a filesystem the private owner profile
/// admits on Linux (ext4, btrfs, XFS or tmpfs).
///
/// The Cloud managed sender keeps its private-actions file under the Store
/// root (see [`enrolled_admitted`]). CI's test job runs in a container whose default temporary directory
/// is overlayfs, which that owner profile refuses by design (`unsupported`),
/// so the sender would never install. There the fixture falls back to the
/// build's target directory on the host-backed checkout (as main's qualified
/// imager fixtures do), then to `/dev/shm`.
fn admitted_temp_dir() -> tempfile::TempDir {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let admitted = |path: &std::path::Path| {
            let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
                return false;
            };
            let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
            // SAFETY: `path` is NUL-terminated and `stat` is a writable buffer.
            if unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
                return false;
            }
            // SAFETY: statfs returned 0, so it initialised the buffer.
            let stat = unsafe { stat.assume_init() };
            u64::try_from(stat.f_type).is_ok_and(|magic| {
                matches!(magic, 0xef53 | 0x9123_683e | 0x5846_5342 | 0x0102_1994)
            })
        };
        // Unit tests run from `<target>/<profile>/deps`; use `<target>/tmp`.
        let target = std::env::current_exe()
            .ok()
            .and_then(|exe| Some(exe.parent()?.parent()?.parent()?.join("tmp")));
        std::iter::once(std::env::temp_dir())
            .chain(target)
            .chain(std::iter::once(std::path::PathBuf::from("/dev/shm")))
            .find(|base| std::fs::create_dir_all(base).is_ok() && admitted(base))
            .map(|base| tempfile::tempdir_in(base).unwrap())
            .expect("Cloud Store fixtures need an ext4, btrfs, XFS or tmpfs temporary directory")
    }
    #[cfg(not(target_os = "linux"))]
    tempfile::tempdir().unwrap()
}

pub(super) fn enrolled() -> (tempfile::TempDir, Store, [u8; 32]) {
    enrolled_in(tempfile::tempdir().unwrap())
}
/// [`enrolled`] on a filesystem the managed sender's private file admits.
pub(super) fn enrolled_admitted() -> (tempfile::TempDir, Store, [u8; 32]) {
    enrolled_in(admitted_temp_dir())
}
fn enrolled_in(dir: tempfile::TempDir) -> (tempfile::TempDir, Store, [u8; 32]) {
    let root = SigningKey::from_bytes(&[7; 32]);
    let device = SigningKey::from_bytes(&[8; 32]);
    let mut store = Store::open_from_disk(dir.path(), "agent-admission").unwrap();
    let request = crate::tests::approved(store.inspect(10).unwrap(), &root, &device);
    store.apply(&request, 11).unwrap();
    (dir, store, device.verifying_key().to_bytes())
}
/// One retained private Spawn control of `device`, admitted through the
/// owner's admission path with `task` as its task text and then marked
/// outcome-unknown: it has no live custody and no pending launch.
pub(super) fn retained_unknown(
    store: &mut Store,
    device: [u8; 32],
    invocation: uuid::Uuid,
    task: &str,
) -> tilo_peer_state::owner_commit::AgentExecutionIds {
    use crate::agent_runtime_owner::{AgentAdmissionResult, AgentControlId};
    use tilo_agent_control_protocol::{AgentRequest, canonical_request, decode_request};
    let caller = store.agent_caller(device).unwrap();
    let mut intent = crate::agent_runtime_owner::launch_stage_test_intent(store, &caller);
    let AgentRequest::Spawn {
        version,
        name,
        grant_set_id,
        ..
    } = decode_request(intent.request.as_bytes()).unwrap()
    else {
        unreachable!()
    };
    intent.request = String::from_utf8(
        canonical_request(&AgentRequest::Spawn {
            version,
            name,
            task: task.into(),
            grant_set_id,
        })
        .unwrap(),
    )
    .unwrap();
    let AgentAdmissionResult::Fresh(admitted) =
        crate::agent_runtime_owner::launch_stage_test_admit(store, &caller, invocation, &intent)
            .unwrap()
    else {
        panic!("fresh admission required")
    };
    store
        .mark_agent_unknown(
            &AgentControlId::new(invocation).unwrap(),
            tilo_agent_control_protocol::AgentError {
                code: tilo_agent_control_protocol::AgentErrorCode::OutcomeUncertain,
                message: "Retained control has no live custody".into(),
            },
        )
        .unwrap();
    admitted.ids
}

#[test]
fn agent_owner_enrollment_creates_the_empty_private_ledger() {
    let (dir, store, _) = enrolled();
    let bytes = std::fs::read(dir.path().join("state.json")).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["version"], 4);
    assert_eq!(
        value["private_agent_ledger"],
        serde_json::json!({"version":1,"facts":{}})
    );
    drop(store);
    Store::open(dir.path(), "agent-admission").unwrap();
    assert_eq!(std::fs::read(dir.path().join("state.json")).unwrap(), bytes);
}

#[test]
fn agent_owner_unenrolled_store_has_no_private_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), "agent-admission").unwrap();
    assert!(store.state.private_agent_ledger.is_none());
    drop(store);
    let path = dir.path().join("state.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["private_agent_ledger"] = serde_json::json!({"version":1,"facts":{}});
    let bytes = serde_json::to_vec(&value).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    assert!(Store::open(dir.path(), "agent-admission").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn agent_owner_private_format_corruption_refuses_rewrite() {
    for mode in ["version", "unknown", "duplicate", "missing", "null"] {
        let (dir, store, _) = enrolled();
        drop(store);
        let path = dir.path().join("state.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        match mode {
            "version" => value["private_agent_ledger"]["version"] = 2.into(),
            "unknown" => value["private_agent_ledger"]["unrecognized"] = true.into(),
            "missing" => {
                value
                    .as_object_mut()
                    .unwrap()
                    .remove("private_agent_ledger");
            }
            "null" => value["private_agent_ledger"] = serde_json::Value::Null,
            _ => (),
        }
        let mut bytes = serde_json::to_vec(&value).unwrap();
        if mode == "duplicate" {
            bytes = String::from_utf8(bytes)
                .unwrap()
                .replacen(
                    "\"private_agent_ledger\":{",
                    "\"private_agent_ledger\":{\"version\":1,",
                    1,
                )
                .into_bytes();
        }
        std::fs::write(&path, &bytes).unwrap();
        assert!(
            Store::open(dir.path(), "agent-admission").is_err(),
            "{mode}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}
