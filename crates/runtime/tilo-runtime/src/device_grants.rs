//! The person's camera and microphone decisions, as resources with a
//! `revoke` action, on the runtime's console socket: `tilo-control --socket
//! <profile>/console.sock actions list` shows every grant, and `actions
//! invoke runtime revoke@<grant>` forgets it — which stops a running capture
//! and asks the person again next time. The same door an assistive tool, a
//! script or an agent uses; the consent prompt is the only other surface.

use std::path::Path;
use std::sync::Arc;

use console_control::{ConsoleServer, Endpoint, Request, catalog, wire};
use serde_json::json;
use tilo_actions::{ActionSpec, EncodedAction, OperationKind, Resource, ResourcePresentation};
use tilo_capture::DeviceKind;
use tilo_capture::grants::{Decision, DeviceGrants, Grant};

use crate::i18n::{device_args, t, t_args};

/// `provider` in `tilo-control` terms.
pub const PROVIDER: &str = "runtime";
const ROOT: &str = "device-grants";
const REVOKE: &str = "revoke";
const OPERATION: &str = "tilo.runtime.device-grant";

/// A grant's resource id: `device-grant:<device>:<room>:<app>`. Neither a
/// room nor a package id holds `:`, and none holds `@`, which the console
/// address reserves.
fn resource_id(grant: &Grant) -> String {
    format!(
        "device-grant:{}:{}:{}",
        grant.device.name(),
        grant.room,
        grant.app
    )
}

fn parse_resource_id(id: &str) -> Option<(DeviceKind, &str, &str)> {
    let mut parts = id.strip_prefix("device-grant:")?.splitn(3, ':');
    let device = DeviceKind::from_name(parts.next()?)?;
    Some((device, parts.next()?, parts.next()?))
}

/// Every grant as a resource with its `revoke` action.
pub fn resources(grants: &DeviceGrants) -> Vec<Resource> {
    let children = grants.list().into_iter().map(|grant| {
        let args = device_args(&grant.app, grant.device);
        let label = t_args(
            match grant.decision {
                Decision::Allow => "device-grant-allowed",
                Decision::Deny => "device-grant-denied",
            },
            &args,
        );
        let encoded = EncodedAction::encode_operation(OPERATION, &json!({ "revoke": true }))
            .unwrap_or_else(|_| EncodedAction {
                type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                payload: Vec::new(),
            });
        Resource::new(resource_id(&grant), "tilo.device-grant", label)
            .with_presentation(ResourcePresentation {
                detail: Some(t_args(
                    "device-grant-room",
                    &[("room", grant.room.as_str().into())],
                )),
                badge: Some(grant.device.name().to_owned()),
                preview: None,
            })
            .with_action(
                ActionSpec::typed_operation(
                    REVOKE,
                    t("device-grant-revoke"),
                    "",
                    OperationKind::Effect,
                    OPERATION,
                    Vec::new(),
                    move |_| encoded.clone(),
                )
                .with_description(t("device-grant-revoke-description")),
            )
    });
    vec![Resource::new(ROOT, "tilo.device-grants", t("device-grants")).with_children(children)]
}

/// One console request against `grants`, answered as the wire expects.
pub fn answer(grants: &DeviceGrants, line: &str) -> String {
    match wire::parse(line) {
        Err(error) => wire::error("parse_error", &error.0),
        Ok(Request::Unsupported { op }) => wire::unsupported(&op),
        Ok(Request::StateDump) => wire::state(&json!({
            "app": PROVIDER,
            "device_grants": grants.list().iter().map(|grant| json!({
                "room": grant.room,
                "app": grant.app,
                "device": grant.device.name(),
                "decision": match grant.decision {
                    Decision::Allow => "allow",
                    Decision::Deny => "deny",
                },
            })).collect::<Vec<_>>(),
        })),
        Ok(Request::Enumerate { query }) => {
            wire::answer(&catalog::enumerate(PROVIDER, &resources(grants), &query))
        }
        Ok(Request::Invoke { provider, id, args }) => {
            let admitted =
                match console_control::admit(PROVIDER, &resources(grants), &provider, &id, &args) {
                    Ok(admitted) => admitted,
                    Err(refusal) => return wire::error(refusal.code, &refusal.message),
                };
            let Some((device, room, app)) = parse_resource_id(admitted.address.resource.as_str())
            else {
                return wire::error("unknown_action", &format!("no grant is addressed by {id}"));
            };
            match grants.revoke(room, app, device) {
                Ok(revoked) => wire::answer(&json!({
                    "provider": PROVIDER,
                    "id": id,
                    "revoked": revoked,
                })),
                Err(error) => wire::error("action_failed", &error.to_string()),
            }
        }
    }
}

/// Serve `grants` on `socket` until the process exits.
///
/// # Errors
///
/// The socket cannot be bound, or its directory is not private.
pub fn serve(grants: Arc<DeviceGrants>, socket: &Path) -> std::io::Result<()> {
    let server = ConsoleServer::new();
    let (wake, woken) = std::sync::mpsc::sync_channel::<()>(1);
    server.serve(
        &Endpoint::Socket(socket.to_path_buf()),
        Arc::new(move || {
            let _ = wake.try_send(());
        }),
    )?;
    std::thread::Builder::new()
        .name("tilo-runtime-console".into())
        .spawn(move || {
            while woken.recv().is_ok() {
                for pending in server.take_pending() {
                    let reply = answer(&grants, pending.line());
                    pending.respond(reply);
                }
            }
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn ask(grants: &DeviceGrants, line: &str) -> Value {
        serde_json::from_str(&answer(grants, line)).unwrap()
    }

    #[test]
    fn grants_are_listed_and_revoked_through_the_action_door() {
        let dir = tempfile::tempdir().unwrap();
        let grants = DeviceGrants::open(&dir.path().join("device-grants")).unwrap();
        let room = "6f7a3c2e-0000-4000-8000-000000000001";
        grants
            .record(room, "so.tilo.video", DeviceKind::Camera, Decision::Allow)
            .unwrap();
        grants
            .record(
                room,
                "so.tilo.video",
                DeviceKind::Microphone,
                Decision::Deny,
            )
            .unwrap();

        let listed = ask(&grants, r#"{"op":"actions_enumerate","query":""}"#);
        let actions = listed["answer"]["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 2, "{listed}");
        let camera = format!("revoke@device-grant:camera:{room}:so.tilo.video");
        assert!(
            actions.iter().any(|a| a["id"] == camera.as_str()),
            "{listed}"
        );
        assert!(
            actions
                .iter()
                .any(|a| a["subtitle"] == "so.tilo.video may use the camera"),
            "{listed}"
        );

        let revoked = ask(
            &grants,
            &json!({"op": "actions_invoke", "provider": PROVIDER, "id": camera}).to_string(),
        );
        assert_eq!(revoked["answer"]["revoked"], true, "{revoked}");
        assert_eq!(
            grants.decision(room, "so.tilo.video", DeviceKind::Camera),
            None
        );
        let again = ask(
            &grants,
            &json!({"op": "actions_invoke", "provider": PROVIDER, "id": camera}).to_string(),
        );
        assert_eq!(again["err"]["code"], "unknown_action", "{again}");

        let state = ask(&grants, r#"{"op":"state_dump"}"#);
        assert_eq!(state["state"]["device_grants"][0]["decision"], "deny");
        let typed = ask(&grants, r#"{"op":"type","text":"x"}"#);
        assert_eq!(typed["err"]["code"], "not_implemented");
    }

    #[test]
    fn the_console_socket_serves_the_same_door() {
        use std::io::{BufRead as _, Write as _};
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let grants = Arc::new(DeviceGrants::open(&root.join("device-grants")).unwrap());
        grants
            .record(
                "personal",
                "so.tilo.video",
                DeviceKind::Camera,
                Decision::Allow,
            )
            .unwrap();
        let socket = root.join("console/runtime.sock");
        serve(Arc::clone(&grants), &socket).unwrap();
        let mut stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        writeln!(
            stream,
            "{}",
            json!({"op": "actions_invoke", "provider": PROVIDER,
                   "id": "revoke@device-grant:camera:personal:so.tilo.video"})
        )
        .unwrap();
        let mut line = String::new();
        std::io::BufReader::new(stream)
            .read_line(&mut line)
            .unwrap();
        assert!(line.contains("\"revoked\":true"), "{line}");
        assert!(grants.list().is_empty());
    }
}
