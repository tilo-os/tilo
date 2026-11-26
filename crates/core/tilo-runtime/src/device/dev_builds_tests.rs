use std::sync::Arc;

use tilo_action_policy::{ActionKind, Answer, Authority, Context, TreePath};
use tilo_actions::remote::{
    ActionCallError, Caller, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget,
    WORKSPACE_GOVERNANCE_PROVIDER,
};
use tilo_app_package::{Manifest, SigningIdentity};
use tilo_desktop_model::WorkspaceId;

use super::{DevBuilds, refusal};
use crate::device::workspaces::ServiceWorkspaces;

/// The app a development build is built of.
const OF: &str = "org.example.notes";

/// A development build of [`OF`] as a device signing with `device` packs one: its `.tapp`.
fn dev_build(device: &SigningIdentity) -> Vec<u8> {
    let directory = tempfile::tempdir().unwrap();
    let payload = directory.path().join("payload");
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::write(payload.join("bin/app"), b"program").unwrap();
    let key = hex::encode(device.public_key_bytes());
    let manifest = Manifest::from_toml_str(&format!(
        r#"[package]
id = "{OF}"
version = "0.1.0"
kind = "app"

[publisher]
identity = "ed25519:{key}"
signature = ""

[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "bin/app"
type = "gui"
"#
    ))
    .unwrap();
    let packed = tilo_app_package::pack_dev(&payload, &manifest, OF, device).unwrap();
    tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap()
}

/// Whether `app`, acting by itself, may change its own records in `workspace` as the
/// workspace's grants stand now: the decision a write it makes there is admitted by.
fn keeps_records(workspaces: &ServiceWorkspaces, workspace: WorkspaceId, app: &str) -> bool {
    let Some(policy) = workspaces.policy_of(workspace)() else {
        return false;
    };
    Authority::of_acting(&tilo_actions::Acting::by_app(app), &policy, &[])
        .decide(
            &TreePath::workspace(workspace.to_string()).app(app),
            ActionKind::Undoable,
            &Context::at_time(tilo_clock::wall()),
        )
        .answer
        == Answer::Allow
}

/// The workspace's uninstall of `app`, asked as the device's person.
fn uninstall(workspaces: &ServiceWorkspaces, workspace: &str, app: &str) {
    let tree = workspaces
        .copies()
        .providers()
        .get(WORKSPACE_GOVERNANCE_PROVIDER)
        .unwrap();
    let cert = workspaces.device_cert();
    let person =
        Caller::authenticated(cert.user_public_key, cert.device_public_key).in_workspace(workspace);
    let request = InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: WORKSPACE_GOVERNANCE_PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(
                tilo_actions::remote::workspace_app(workspace, app),
                "uninstall",
            ),
        },
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let receipt = tree
        .answers
        .invoke(&person, request)
        .wait_timeout(std::time::Duration::from_secs(60))
        .ok()
        .flatten()
        .expect("the workspace answers")
        .expect("the person may uninstall it");
    assert!(
        matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "the person uninstalls it: {:?}",
        receipt.status
    );
}

/// A device's development build installed in the workspace its person's services serve keeps its
/// own records there, apart from the app it is a build of; members find it among the workspace's
/// apps, marked as a development build and published by the device that signed it; uninstalling
/// it takes its records back; and another workspace the device keeps knows nothing of it.
#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the workspaces and their service stay up until the test ends"
)]
fn a_development_build_keeps_its_records_in_the_workspace_its_person_installs_it_in() {
    let placed = WorkspaceId::new();
    let other = WorkspaceId::new();
    let (workspaces, _directory) = tilo_rt::app()
        .block_on(ServiceWorkspaces::test_hosting(placed))
        .unwrap();
    tilo_rt::app()
        .block_on(workspaces.found(other, "Elsewhere"))
        .unwrap()
        .unwrap();
    let device = SigningIdentity::from_seed(&[0x42; 32]);
    let dev = tilo_app_package::dev_app_id(OF, &device.public_key_bytes());
    let workspace = placed.to_string();
    assert!(
        !keeps_records(&workspaces, placed, &dev),
        "a build installed on the device alone keeps no records in the workspace"
    );

    let builds = DevBuilds::new(Arc::clone(&workspaces), workspace.clone());
    let said = builds.install(&dev_build(&device)).unwrap();
    assert!(said.contains(&dev), "{said}");

    let installed = tilo_rt::app()
        .block_on(workspaces.copies().app_versions(&workspace))
        .unwrap()
        .unwrap();
    let release = &installed
        .iter()
        .find(|installed| installed.release.app == dev)
        .expect("members find it among the workspace's apps")
        .release;
    assert!(release.dev, "marked as a development build");
    assert_eq!(
        release.publisher,
        hex::encode(device.public_key_bytes()),
        "published by the device that signed it"
    );
    assert!(
        keeps_records(&workspaces, placed, &dev),
        "the build keeps its own records"
    );
    assert!(
        !keeps_records(&workspaces, placed, OF),
        "and the app it is a build of keeps none through it"
    );
    assert!(
        tilo_rt::app()
            .block_on(workspaces.copies().app_versions(&other.to_string()))
            .unwrap()
            .unwrap()
            .is_empty(),
        "another workspace knows nothing of it"
    );
    assert!(!keeps_records(&workspaces, other, &dev));

    uninstall(&workspaces, &workspace, &dev);
    assert!(
        !keeps_records(&workspaces, placed, &dev),
        "uninstalling it takes its records back"
    );
}

/// A person the workspace does not let install apps is told so, in words they act on, and the
/// build stays this device's alone.
#[test]
fn one_the_workspace_does_not_let_install_reads_why() {
    for refused in [
        ActionCallError::UnknownAction,
        ActionCallError::NotPermitted,
    ] {
        let said = refusal("org.example.notes.dev.42", &refused);
        assert!(
            said.contains("does not let you install apps") && said.contains("An owner"),
            "{said}"
        );
    }
}
