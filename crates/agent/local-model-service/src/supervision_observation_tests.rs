use super::*;

fn owned() -> ContainerOwnership {
    ContainerOwnership {
        container_id: "a".repeat(64),
        run_id: "b".repeat(64),
        image: format!("registry.example.invalid/runtime@sha256:{}", "c".repeat(64)),
        model_id_sha256: "d".repeat(64),
        arguments_sha256: arguments_digest(&["120".into()]),
    }
}

#[test]
fn manager_metadata_refuses_foreign_retired_missing_and_duplicate_lifetimes() {
    let owned = owned();
    let handle = ProcessHandle {
        id: owned.unit(),
        container: Some(owned),
    };
    let body = format!(
        "Id={}.service\nLoadState=loaded\nActiveState=active\nMainPID=123\nControlGroup=/user.slice/owned.service\nInvocationID={}\n",
        handle.id,
        "e".repeat(32)
    );
    assert!(parse_unit(&body, &handle).is_ok());
    for invalid in [
        body.replace(&handle.id, "foreign"),
        body.replace("active\n", "inactive\n"),
        body.replace("MainPID=123", "MainPID=0"),
        body.replace("MainPID=123", "MainPID=true"),
        body.replace(&"e".repeat(32), ""),
        format!("{body}MainPID=456\n"),
        body.replace("/user.slice/owned.service", "/"),
    ] {
        assert!(parse_unit(&invalid, &handle).is_err());
    }
}

#[test]
fn payload_pid_requires_the_exact_owned_live_container() {
    let owned = owned();
    let body = serde_json::json!({"Id":owned.container_id,"ImageName":owned.image,"ImageDigest":format!("sha256:{}", "c".repeat(64)),
        "Config":{"Cmd":["120"],"Labels":{"io.tilo.local-model.run":owned.run_id,"io.tilo.local-model.model":owned.model_id_sha256,"io.tilo.local-model.image":owned.image}},
        "State":{"Running":true,"Pid":456}});
    assert_eq!(payload_pid(&owned, &body).unwrap(), 456);
    for (pointer, value) in [
        ("/Id", serde_json::json!("f".repeat(64))),
        ("/ImageDigest", serde_json::json!("foreign")),
        ("/State/Running", serde_json::json!(false)),
        ("/State/Pid", serde_json::json!(0)),
        ("/State/Pid", serde_json::json!("456")),
        ("/Config/Cmd", serde_json::json!(["600"])),
    ] {
        let mut changed = body.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(payload_pid(&owned, &changed).is_err());
    }
}
