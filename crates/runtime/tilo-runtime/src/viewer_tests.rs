use super::*;
use std::sync::{Arc, Mutex as StdMutex};

#[derive(Clone)]
struct Invite {
    id: String,
    role: String,
    status: &'static str,
    participant: Option<String>,
}

type Calls = Arc<StdMutex<Vec<(String, Option<Value>)>>>;

/// Answers like the device host's operator route.
#[derive(Clone, Default)]
struct FakeHost {
    invites: Arc<StdMutex<Vec<Invite>>>,
    calls: Calls,
    down: Arc<std::sync::atomic::AtomicBool>,
}

impl FakeHost {
    fn redeem(&self, id: &str, participant: &str) {
        let mut invites = self.invites.lock().unwrap();
        let invite = invites.iter_mut().find(|invite| invite.id == id).unwrap();
        invite.status = "redeemed";
        invite.participant = Some(participant.into());
        drop(invites);
    }
    fn actions(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|(action, _)| action.clone())
            .collect()
    }
}

impl HostOperator for FakeHost {
    fn call(&self, action: &str, params: Option<Value>) -> Result<Value, ViewerError> {
        if self.down.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(ViewerError::unavailable("down"));
        }
        self.calls
            .lock()
            .unwrap()
            .push((action.into(), params.clone()));
        let params = params.unwrap_or(Value::Null);
        let mut invites = self.invites.lock().unwrap();
        match action {
            "invitation.issue" => {
                let id = format!("{:064x}", invites.len().saturating_add(1));
                invites.push(Invite {
                    id: id.clone(),
                    role: params["role"].as_str().unwrap().into(),
                    status: "pending",
                    participant: None,
                });
                Ok(
                    json!({"invite": format!("text-{id}"), "invite_id": id, "expires_at_ms": 1_000_000}),
                )
            }
            "invitation.list" => Ok(json!({"invitations": invites.iter().map(|invite| {
                let mut entry = json!({"id": invite.id, "role": invite.role, "status": invite.status});
                if let Some(participant) = &invite.participant {
                    entry["participant_id"] = json!(participant);
                }
                entry
            }).collect::<Vec<_>>()})),
            "invitation.cancel" => {
                let invite = invites
                    .iter_mut()
                    .find(|invite| invite.id == params["invite_id"])
                    .ok_or_else(|| ViewerError::new(409, "unknown invitation"))?;
                invite.status = "revoked";
                Ok(json!({}))
            }
            "member.role.set" | "member.remove" => {
                if invites.iter().any(|invite| {
                    invite.participant.as_deref() == params["participant_id"].as_str()
                }) {
                    Ok(json!({"generation": "2"}))
                } else {
                    Err(ViewerError::new(409, "not a member"))
                }
            }
            other => Err(ViewerError::new(404, format!("unexpected {other}"))),
        }
    }
}

fn viewer(host: &FakeHost, dir: &tempfile::TempDir) -> Viewer {
    Viewer::new(
        "https://c1.viewer.example".into(),
        Box::new(host.clone()),
        dir.path().join("ledger.json"),
    )
    .unwrap()
}

#[test]
fn issue_answers_the_partner_shape_and_maps_modes_to_roles() {
    let dir = tempfile::tempdir().unwrap();
    let host = FakeHost::default();
    let viewer = viewer(&host, &dir);
    let watch = viewer
        .issue(br#"{"mode":"watch","ttl_seconds":300}"#)
        .unwrap();
    assert_eq!(watch["origin"], "https://c1.viewer.example");
    assert_eq!(
        watch["invitation"],
        format!("text-{}", watch["invite_id"].as_str().unwrap())
    );
    assert_eq!(watch["role"], "viewer");
    assert_eq!(watch["expires_at"], "1970-01-01T00:16:40Z");
    let control = viewer
        .issue(br#"{"mode":"control","ttl_seconds":60,"label":"Ada"}"#)
        .unwrap();
    assert_eq!(control["role"], "editor");
    let calls = host.calls.lock().unwrap().clone();
    assert_eq!(
        calls[0].1,
        Some(json!({"role":"viewer","expires_in_seconds":300,"invitee_name":"Viewer"}))
    );
    assert_eq!(calls[1].1.as_ref().unwrap()["invitee_name"], "Ada");
    for bad in [
        &br#"{"mode":"watch","ttl_seconds":59}"#[..],
        br#"{"mode":"watch","ttl_seconds":901}"#,
        br#"{"mode":"own","ttl_seconds":300}"#,
        br#"{"mode":"watch"}"#,
        br#"{"mode":"watch","ttl_seconds":300,"extra":true}"#,
        br#"{"mode":"watch","ttl_seconds":300,"label":""}"#,
        br#"{"mode":"watch","ttl_seconds":300,"label":"a\u0007b"}"#,
    ] {
        assert_eq!(viewer.issue(bad).unwrap_err().status, 400);
    }
    assert_eq!(host.calls.lock().unwrap().len(), 2);
}

#[test]
fn revoke_cancels_pending_and_removes_redeemed_but_only_its_own_invitations() {
    let dir = tempfile::tempdir().unwrap();
    let host = FakeHost::default();
    let viewer = viewer(&host, &dir);
    let pending = viewer
        .issue(br#"{"mode":"watch","ttl_seconds":300}"#)
        .unwrap();
    let redeemed = viewer
        .issue(br#"{"mode":"watch","ttl_seconds":300}"#)
        .unwrap();
    let redeemed_id = redeemed["invite_id"].as_str().unwrap();
    host.redeem(redeemed_id, "aa");
    let outcome = viewer
        .revoke(pending["invite_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(outcome["status"], "revoked");
    assert_eq!(viewer.revoke(redeemed_id).unwrap()["status"], "removed");
    assert!(host.actions().contains(&"member.remove".to_string()));
    assert_eq!(viewer.revoke(redeemed_id).unwrap_err().status, 404);
    // The session's own membership came from an invitation this ledger never saw.
    assert_eq!(viewer.revoke(&"f".repeat(64)).unwrap_err().status, 404);
}

#[test]
fn role_change_needs_redemption_and_names_the_participant() {
    let dir = tempfile::tempdir().unwrap();
    let host = FakeHost::default();
    let viewer = viewer(&host, &dir);
    let issued = viewer
        .issue(br#"{"mode":"watch","ttl_seconds":300}"#)
        .unwrap();
    let id = issued["invite_id"].as_str().unwrap();
    assert_eq!(
        viewer
            .set_role(id, br#"{"mode":"control"}"#)
            .unwrap_err()
            .status,
        409
    );
    host.redeem(id, "bb");
    assert_eq!(
        viewer.set_role(id, br#"{"mode":"control"}"#).unwrap()["role"],
        "editor"
    );
    let calls = host.calls.lock().unwrap().clone();
    assert_eq!(
        calls.last().unwrap().1,
        Some(json!({"participant_id":"bb","role":"editor"}))
    );
}

#[test]
fn sweep_removes_redeemed_viewers_after_expiry_plus_lifetime_unless_renewed() {
    let dir = tempfile::tempdir().unwrap();
    let host = FakeHost::default();
    let viewer = viewer(&host, &dir);
    // Expires at 1_000_000 ms; removal is due at 1_000_000 + 300_000.
    let kept = viewer
        .issue(br#"{"mode":"watch","ttl_seconds":300}"#)
        .unwrap();
    let swept = viewer
        .issue(br#"{"mode":"watch","ttl_seconds":300}"#)
        .unwrap();
    host.redeem(kept["invite_id"].as_str().unwrap(), "k1");
    host.redeem(swept["invite_id"].as_str().unwrap(), "s1");
    assert_eq!(viewer.sweep(1_299_999).unwrap(), 0);
    viewer
        .renew(
            kept["invite_id"].as_str().unwrap(),
            br#"{"ttl_seconds":600}"#,
            1_200_000,
        )
        .unwrap();
    host.down.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(viewer.sweep(1_300_000).unwrap(), 0, "retried later");
    host.down.store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(viewer.sweep(1_300_000).unwrap(), 1);
    let removed = host
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(action, _)| action == "member.remove")
        .map(|(_, params)| params.clone().unwrap()["participant_id"].clone())
        .collect::<Vec<_>>();
    assert_eq!(removed, vec![json!("s1")]);
    // The ledger survives a restart of the endpoint.
    let reopened = self::viewer(&host, &dir);
    assert_eq!(reopened.sweep(1_799_999).unwrap(), 0);
    assert_eq!(reopened.sweep(1_800_000).unwrap(), 1);
}

#[test]
fn rfc3339_formats_civil_utc() {
    assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
    assert_eq!(rfc3339(951_782_400_000), "2000-02-29T00:00:00Z");
    assert_eq!(rfc3339(1_790_553_600_999), "2026-09-28T00:00:00Z");
}

#[test]
fn origin_must_be_exact_https() {
    for bad in [
        "http://c1.example",
        "https://",
        "https://c1.example/",
        "https://u@c1.example",
    ] {
        assert!(
            Viewer::new(bad.into(), Box::new(FakeHost::default()), "/x".into()).is_err(),
            "{bad}"
        );
    }
}
