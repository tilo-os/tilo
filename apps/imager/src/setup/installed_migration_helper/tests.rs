use super::*;
#[test]
fn bounded_protocol_refuses_extra_fields_and_oversized_lines() {
    let (tx, rx) = mpsc::sync_channel(8);
    lines(
        io::Cursor::new(b"{\"op\":\"start\",\"source\":\"invented\"}\n".to_vec()),
        tx,
    );
    assert!(rx.recv().unwrap().is_err());
    let (tx, rx) = mpsc::sync_channel(8);
    lines(io::Cursor::new(vec![b'x'; LIMIT + 1]), tx);
    assert!(rx.recv().unwrap().is_err());
}

#[test]
fn shared_access_refuses_parent_dir_arguments_before_opening_grants() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let plans = root.join("plans");
    let home = root.join("home");
    let workspace = home.join(".local/state/tilo/migration/op-1");
    for dir in [
        &root,
        &plans,
        &home,
        &home.join(".local"),
        &home.join(".local/state"),
        &home.join(".local/state/tilo"),
        &home.join(".local/state/tilo/migration"),
        &workspace,
    ] {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    for dir in [&plans, &home, &workspace] {
        let sub = dir.join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    for (label, bad_plans, bad_home, bad_workspace) in [
        (
            "plans",
            plans.join("sub/.."),
            home.clone(),
            workspace.clone(),
        ),
        (
            "home",
            plans.clone(),
            home.join("sub/.."),
            workspace.clone(),
        ),
        ("workspace", plans, home, workspace.join("sub/..")),
    ] {
        let args = [
            ("--plans", bad_plans.into_os_string()),
            ("--operation", OsString::from("op-1")),
            ("--home", bad_home.into_os_string()),
            ("--workspace", bad_workspace.into_os_string()),
        ]
        .into_iter()
        .flat_map(|(key, value)| [OsString::from(key), value]);
        let mut output = Vec::new();
        let error = run_shared_access(args, io::Cursor::new(Vec::<u8>::new()), &mut output)
            .expect_err(label);
        assert_eq!(error.kind(), io::ErrorKind::Other, "{label}: {error}");
        assert_eq!(
            error.to_string(),
            "Installed migration is unavailable or its grants changed",
            "{label}"
        );
        assert!(output.is_empty(), "{label}");
    }
}
