//! What the server keeps of an agent's ask while its person is asked: kept
//! once, bounded, claimed by one reader of the answer, found started after
//! a restart, and gone once forgotten.

use serde_json::json;
use tilo_actions::remote::person_asking::Words;

use crate::workspaces::person_asking::kept_asks::{
    CARRIED_OUT, MAX_OPEN_ASKS, NOTHING_WAITS, NewAsk, TOO_MANY_WAIT,
};
use crate::workspaces::tests::{governor, test_host};

const ADA: [u8; 32] = [7; 32];
const BO: [u8; 32] = [9; 32];

/// The job `number` an agent asks for, in `workspace`.
fn asking(workspace: &str, number: usize) -> NewAsk {
    NewAsk {
        workspace: workspace.to_owned(),
        agent: "scout".into(),
        name: "Scout".into(),
        carrier: "action".into(),
        kept: json!({"job": number}),
        always: None,
        words: Words::default(),
        asked_in: json!({"conversation": "c_general"}),
    }
}

/// An ask is kept until it is forgotten, the same asked again adds none, and
/// a person has a bounded number waiting: one more is refused, another
/// person's are their own.
#[tokio::test]
async fn an_ask_is_kept_once_and_a_person_has_a_bounded_number_waiting() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();

    let first = workspaces
        .keep_ask(&ADA, asking("w", 0))
        .unwrap()
        .expect("kept");
    assert_eq!(first.ask.len(), 32);
    assert_eq!(first.person, hex::encode(ADA));
    assert!(!first.started);
    assert_eq!(
        workspaces.keep_ask(&ADA, asking("w", 0)).unwrap(),
        None,
        "asked the same again while nobody answered, the one question stands"
    );
    assert_eq!(workspaces.open_asks("w"), vec![first.clone()]);
    assert_eq!(
        workspaces.open_asks("another"),
        Vec::new(),
        "an ask is its workspace's"
    );

    for number in 1..MAX_OPEN_ASKS {
        workspaces
            .keep_ask(&ADA, asking("w", number))
            .unwrap()
            .expect("kept");
    }
    assert_eq!(
        workspaces.keep_ask(&ADA, asking("w", MAX_OPEN_ASKS)),
        Err(TOO_MANY_WAIT.to_owned())
    );
    let his = workspaces
        .keep_ask(&BO, asking("w", 0))
        .unwrap()
        .expect("another person's asks are their own");
    assert_eq!(workspaces.open_asks("w").len(), MAX_OPEN_ASKS + 1);

    // Forgotten, it does nothing any more and leaves a place free.
    workspaces.forget_ask(&first);
    assert_eq!(
        workspaces.claim_ask(&first.ask),
        Err(NOTHING_WAITS.to_owned())
    );
    workspaces
        .keep_ask(&ADA, asking("w", MAX_OPEN_ASKS))
        .unwrap()
        .expect("there is a place again");
    workspaces.forget_ask(&his);
    assert_eq!(workspaces.open_asks("w").len(), MAX_OPEN_ASKS);
    server.shutdown().await;
}

/// One reader of an answer claims its ask and every other is refused, also
/// after the server restarted: an ask claimed before the restart is read back
/// as interrupted and is not claimed again, one that only waited still
/// waits, and one forgotten is gone.
#[tokio::test]
async fn an_answer_is_claimed_once_also_across_a_restart_of_the_server() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let waiting = workspaces
        .keep_ask(&ADA, asking("w", 0))
        .unwrap()
        .expect("kept");
    let answered = workspaces
        .keep_ask(&ADA, asking("w", 1))
        .unwrap()
        .expect("kept");
    let gone = workspaces
        .keep_ask(&ADA, asking("w", 2))
        .unwrap()
        .expect("kept");

    let claimed = workspaces
        .claim_ask(&answered.ask)
        .expect("the first reader");
    assert!(claimed.started);
    assert_eq!(claimed.kept, answered.kept, "what is done is what was kept");
    assert_eq!(
        workspaces.claim_ask(&answered.ask),
        Err(CARRIED_OUT.to_owned()),
        "a second reader of the same answer does nothing"
    );
    assert_eq!(
        workspaces.claim_ask("0123456789abcdef0123456789abcdef"),
        Err(NOTHING_WAITS.to_owned())
    );
    assert_eq!(
        workspaces.claim_ask("../../people"),
        Err(NOTHING_WAITS.to_owned())
    );
    workspaces.forget_ask(&gone);
    server.shutdown().await;

    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let kept = workspaces.open_asks("w");
    assert_eq!(kept.len(), 2, "{kept:?}");
    let still_waiting = kept.iter().find(|ask| ask.ask == waiting.ask).unwrap();
    assert!(!still_waiting.started && !still_waiting.interrupted);
    let was_running = kept.iter().find(|ask| ask.ask == answered.ask).unwrap();
    assert!(
        was_running.started && was_running.interrupted,
        "the server stopped while its answer was carried out: {was_running:?}"
    );
    assert_eq!(
        workspaces.claim_ask(&answered.ask),
        Err(CARRIED_OUT.to_owned()),
        "it is not carried out a second time"
    );
    assert!(workspaces.claim_ask(&waiting.ask).is_ok());
    // Stored under the workspace store, never under people/<person>/, and
    // readable only by the person asked and the asking agent.
    assert!(
        !dir.path().join("people").exists(),
        "pending asks must not be stored under people/"
    );
    assert_eq!(workspaces.open_asks_for("w", &ADA, Some("scout")).len(), 2);
    assert!(
        workspaces
            .open_asks_for("w", &[0x99; 32], Some("scout"))
            .is_empty(),
        "another person cannot read pending asks"
    );
    assert!(
        workspaces
            .open_asks_for("w", &ADA, Some("other"))
            .is_empty(),
        "another agent cannot read pending asks"
    );
    server.shutdown().await;
}

/// A claim given back before anything of the answer was carried out leaves the answer waiting:
/// the next reader claims it, also after the server restarted, and it is not read back as
/// interrupted, so the person's answer is carried out once all the same.
#[tokio::test]
async fn an_answer_whose_claim_is_given_back_is_claimed_again_once() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let answered = workspaces
        .keep_ask(&ADA, asking("w", 0))
        .unwrap()
        .expect("kept");
    let claimed = workspaces
        .claim_ask(&answered.ask)
        .expect("the first reader");
    workspaces.release_ask(&claimed).expect("given back");
    let again = workspaces
        .claim_ask(&answered.ask)
        .expect("the next reader claims it");
    workspaces.release_ask(&again).expect("given back again");
    server.shutdown().await;

    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let kept = workspaces.open_asks("w");
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert!(
        !kept[0].started && !kept[0].interrupted,
        "nothing of it was carried out: {:?}",
        kept[0]
    );
    assert!(workspaces.claim_ask(&answered.ask).is_ok());
    assert_eq!(
        workspaces.claim_ask(&answered.ask),
        Err(CARRIED_OUT.to_owned()),
        "claimed again, it is still one reader's"
    );
    server.shutdown().await;
}

/// When giving back a claim cannot write to disk (for example because the ask's
/// file path is occupied by a non-empty directory, which fails atomic rename
/// even as root), the claim is still cleared in memory so this process can
/// claim it again on the next look instead of refusing with `CARRIED_OUT`.
#[tokio::test]
async fn release_ask_clears_claim_in_memory_even_when_disk_write_fails() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let ask = workspaces
        .keep_ask(&ADA, asking("w", 0))
        .unwrap()
        .expect("kept");
    let claimed = workspaces.claim_ask(&ask.ask).expect("first claim");

    // Occupy `<id>.json` with a non-empty directory so atomic rename fails with
    // EISDIR regardless of process UID (works as root in CI too).
    let file_path = workspaces
        .workspace_root("w")
        .join("asks")
        .join(format!("{}.json", ask.ask));
    assert!(file_path.is_file());
    std::fs::remove_file(&file_path).unwrap();
    std::fs::create_dir(&file_path).unwrap();
    std::fs::write(file_path.join("blocker"), b"x").unwrap();

    assert!(
        workspaces.release_ask(&claimed).is_err(),
        "disk write fails when path is a non-empty directory"
    );
    let open = workspaces.open_asks("w");
    assert_eq!(open.len(), 1);
    assert!(
        !open[0].started,
        "claim is cleared in memory even when disk write fails"
    );

    // Once the transient disk obstacle clears, the same process can claim it again.
    std::fs::remove_dir_all(&file_path).unwrap();
    assert!(
        workspaces.claim_ask(&ask.ask).is_ok(),
        "released ask can be claimed again in the same process"
    );
    assert_eq!(
        workspaces.claim_ask(&ask.ask),
        Err(CARRIED_OUT.to_owned()),
        "claimed again, a second claim is still refused"
    );
    server.shutdown().await;
}

/// Write raw `bytes` to `workspaces/workspaces/<hex(workspace)>/<subdir>/<filename>` under `dir`
/// (the state root passed to [`test_host`], which opens `Workspaces` at `state_root/workspaces`).
fn write_raw_ask(
    dir: &std::path::Path,
    workspace: &str,
    subdir: &str,
    filename: &str,
    bytes: &[u8],
) {
    let folder = dir
        .join("workspaces")
        .join("workspaces")
        .join(hex::encode(workspace.as_bytes()))
        .join(subdir);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join(filename), bytes).unwrap();
}

/// Literal bytes written by the current code for `KeptAsk` (`asks/`) open cleanly across
/// restarts, preserve oldest-first order, mark `started: true` asks as `interrupted: true`, and
/// skip corrupt or malformed files without breaking valid asks.
#[tokio::test]
async fn literal_bytes_of_kept_asks_open_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = "ws-fixture";
    let waiting_ask_id = "0123456789abcdef0123456789abcdef";
    let started_ask_id = "fedcba9876543210fedcba9876543210";
    let person_hex = hex::encode(ADA);

    let kept_waiting_bytes = format!(
        concat!(
            r#"{{"ask":"{id}","workspace":"{ws}","person":"{person}","agent":"scout","#,
            r#""name":"Scout","carrier":"action","kept":{{"job":1}},"always":null,"#,
            r#""words":{{"action":"Remove book","on":"Dusk","place":"Team"}},"#,
            r#""askedIn":{{"conversation":"c_general","cause":"m1","session":"s1"}},"#,
            r#""started":false,"askedAtMs":1700000000001}}"#,
        ),
        id = waiting_ask_id,
        ws = workspace,
        person = person_hex,
    );
    let kept_started_bytes = format!(
        concat!(
            r#"{{"ask":"{id}","workspace":"{ws}","person":"{person}","agent":"scout","#,
            r#""name":"Scout","carrier":"action","kept":{{"job":2}},"always":null,"#,
            r#""words":{{"action":"Remove book","place":"Team"}},"#,
            r#""askedIn":{{"conversation":"c_general","cause":"m2","session":"s1"}},"#,
            r#""started":true,"askedAtMs":1700000000002}}"#,
        ),
        id = started_ask_id,
        ws = workspace,
        person = person_hex,
    );
    write_raw_ask(
        dir.path(),
        workspace,
        "asks",
        &format!("{waiting_ask_id}.json"),
        kept_waiting_bytes.as_bytes(),
    );
    write_raw_ask(
        dir.path(),
        workspace,
        "asks",
        &format!("{started_ask_id}.json"),
        kept_started_bytes.as_bytes(),
    );
    write_raw_ask(dir.path(), workspace, "asks", "corrupt.json", b"{not json");
    write_raw_ask(
        dir.path(),
        workspace,
        "asks",
        "bad-id.json",
        kept_waiting_bytes
            .replace(waiting_ask_id, "not-32-hex-chars")
            .as_bytes(),
    );

    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();

    let kept = workspaces.open_asks(workspace);
    assert_eq!(kept.len(), 2, "{kept:?}");
    assert_eq!(kept[0].ask, waiting_ask_id);
    assert!(!kept[0].started && !kept[0].interrupted);
    assert_eq!(kept[0].words.action, "Remove book");
    assert_eq!(kept[0].words.on, "Dusk");
    assert_eq!(kept[0].words.place, "Team");
    assert_eq!(kept[1].ask, started_ask_id);
    assert!(kept[1].started && kept[1].interrupted);
    assert_eq!(
        workspaces.claim_ask(started_ask_id),
        Err(CARRIED_OUT.to_owned())
    );
    let claimed_kept = workspaces.claim_ask(waiting_ask_id).unwrap();
    assert!(claimed_kept.started);
    assert_eq!(
        workspaces.claim_ask(waiting_ask_id),
        Err(CARRIED_OUT.to_owned())
    );
    server.shutdown().await;
}

/// Literal bytes written by the current code for `OpenAsk` (`vault-asks/`) open cleanly across
/// restarts, preserve oldest-first order, mark `started: true` asks as `interrupted: true`, and
/// skip corrupt or malformed files without breaking valid asks.
#[tokio::test]
async fn literal_bytes_of_vault_use_asks_open_across_restarts() {
    use tilo_actions::remote::ActionCallError;

    let dir = tempfile::tempdir().unwrap();
    let workspace = "ws-fixture";
    let waiting_use_id = "1123456789abcdef0123456789abcdef";
    let started_use_id = "eecba98765432100fedcba9876543210";
    let person_hex = hex::encode(ADA);

    let use_waiting_bytes = format!(
        concat!(
            r#"{{"ask":"{id}","workspace":"{ws}","person":"{person}","agent":"scout","#,
            r#""name":"Scout","reason":"You asked me to read your mail.","#,
            r#""askedIn":{{"part":"p0"}},"detail":{{"use":{{"ask":"{id}"}}}},"#,
            r#""started":false,"askedAtNs":1700000000001000000,"#,
            r#""provider":"tilo.accounts","resource":"accounts","action":"add-mail-with-item","#,
            r#""arguments":{{"address":"ada@example.test"}}}}"#,
        ),
        id = waiting_use_id,
        ws = workspace,
        person = person_hex,
    );
    let use_started_bytes = format!(
        concat!(
            r#"{{"ask":"{id}","workspace":"{ws}","person":"{person}","agent":"scout","#,
            r#""name":"Scout","reason":"Second use","#,
            r#""askedIn":{{"part":"p1"}},"detail":{{"use":{{"ask":"{id}"}}}},"#,
            r#""started":true,"askedAtNs":1700000000002000000,"#,
            r#""provider":"tilo.accounts","resource":"accounts","action":"add-mail-with-item","#,
            r#""arguments":{{"address":"work@example.test"}}}}"#,
        ),
        id = started_use_id,
        ws = workspace,
        person = person_hex,
    );
    write_raw_ask(
        dir.path(),
        workspace,
        "vault-asks",
        &format!("{waiting_use_id}.json"),
        use_waiting_bytes.as_bytes(),
    );
    write_raw_ask(
        dir.path(),
        workspace,
        "vault-asks",
        &format!("{started_use_id}.json"),
        use_started_bytes.as_bytes(),
    );
    write_raw_ask(
        dir.path(),
        workspace,
        "vault-asks",
        "corrupt.json",
        b"{not json",
    );
    write_raw_ask(
        dir.path(),
        workspace,
        "vault-asks",
        "bad-id.json",
        use_waiting_bytes
            .replace(waiting_use_id, "UPPERCASE_IS_INVALID_ASK_ID_0000")
            .as_bytes(),
    );

    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();

    let uses = workspaces.open_use_asks(workspace);
    assert_eq!(uses.len(), 2, "{uses:?}");
    assert_eq!(uses[0].ask, waiting_use_id);
    assert!(!uses[0].started && !uses[0].interrupted);
    assert_eq!(uses[0].reason, "You asked me to read your mail.");
    assert_eq!(uses[1].ask, started_use_id);
    assert!(uses[1].started && uses[1].interrupted);
    assert_eq!(
        workspaces.claim_use(started_use_id),
        Err(ActionCallError::Refused {
            reason: CARRIED_OUT.to_owned(),
        })
    );
    let claimed_use = workspaces.claim_use(waiting_use_id).unwrap();
    assert!(claimed_use.started);
    assert_eq!(
        workspaces.claim_use(waiting_use_id),
        Err(ActionCallError::Refused {
            reason: CARRIED_OUT.to_owned(),
        })
    );
    server.shutdown().await;
}

/// Pinning test for `OpenAsk` (`vault-asks/`): file layout, deduplication, wrong-person refusal
/// on `answer_use`, and bad ask-id rejection.
#[tokio::test]
async fn vault_use_asks_pin_dedup_wrong_person_and_bad_ids() {
    use tilo_actions::remote::ActionCallError;

    use super::super::tests::{admit, caller, governed};
    use super::turn_use_tests::{a_mail_login, add_mail, asked, on_turn, proving};
    use crate::workspaces::hosted_use::{HostedTurn, TurnUse, UseAnswer};

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let as_ada = caller(&ada);
    let login = a_mail_login(&service, workspace_id, (&ada, &as_ada)).await;
    let _proved = proving(workspaces);

    let ada_caller = on_turn(&ada, &workspace, "scout");
    let bo_caller = on_turn(&bo, &workspace, "scout");
    let ada_turn = HostedTurn {
        workspace: workspace_id,
        caller: &ada_caller,
        bound: None,
    };
    let bo_turn = HostedTurn {
        workspace: workspace_id,
        caller: &bo_caller,
        bound: None,
    };

    let req0 = add_mail("u0@example.test", "imap.example.test", &login);
    let first = asked(
        workspaces
            .use_for_turn(&ada_turn, &req0, json!({"n": 0}))
            .await,
    );
    let file_path = workspaces
        .workspace_root(&workspace)
        .join("vault-asks")
        .join(format!("{}.json", first.ask));
    assert_eq!(
        file_path,
        dir.path()
            .join("workspaces")
            .join("workspaces")
            .join(hex::encode(workspace.as_bytes()))
            .join("vault-asks")
            .join(format!("{}.json", first.ask))
    );
    assert!(file_path.is_file(), "kept at {}", file_path.display());
    assert_eq!(
        workspaces.use_for_turn(&ada_turn, &req0, json!({})).await,
        Ok(TurnUse::Waits)
    );

    // Another person's turn cannot answer Ada's ask, and `answer_use` forgets it on refusal.
    let wrong_person = workspaces
        .answer_use(&bo_turn, &first.ask, &first.detail, UseAnswer::Once)
        .await;
    assert_eq!(wrong_person, Err(ActionCallError::UnknownAction));
    assert!(!file_path.exists());
    assert_eq!(
        workspaces.claim_use(&first.ask),
        Err(ActionCallError::Refused {
            reason: NOTHING_WAITS.to_owned(),
        })
    );

    for bad in ["0123456789abcdef0123456789abcdef", "../../people"] {
        assert_eq!(
            workspaces.claim_use(bad),
            Err(ActionCallError::Refused {
                reason: NOTHING_WAITS.to_owned(),
            })
        );
    }
    server.shutdown().await;
}

/// Pinning test for `OpenAsk` (`vault-asks/`): per-person cap (32), independent quota, workspace
/// isolation, single claim, `use_interrupted` vs `use_not_asked`, and restart recovery.
#[tokio::test]
async fn vault_use_asks_pin_cap_forget_and_restart_recovery() {
    use tilo_actions::remote::ActionCallError;

    use super::super::tests::{admit, caller, governed};
    use super::turn_use_tests::{a_mail_login, add_mail, asked, on_turn, proving};
    use crate::workspaces::hosted_use::HostedTurn;

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let as_ada = caller(&ada);
    let login = a_mail_login(&service, workspace_id, (&ada, &as_ada)).await;
    let _proved = proving(workspaces);

    let ada_caller = on_turn(&ada, &workspace, "scout");
    let bo_caller = on_turn(&bo, &workspace, "scout");
    let ada_turn = HostedTurn {
        workspace: workspace_id,
        caller: &ada_caller,
        bound: None,
    };
    let bo_turn = HostedTurn {
        workspace: workspace_id,
        caller: &bo_caller,
        bound: None,
    };

    let mut kept_asks = Vec::new();
    for i in 0..MAX_OPEN_ASKS {
        let req = add_mail(&format!("u{i}@example.test"), "imap.example.test", &login);
        kept_asks.push(asked(
            workspaces
                .use_for_turn(&ada_turn, &req, json!({"n": i}))
                .await,
        ));
    }
    let overflow = add_mail("overflow@example.test", "imap.example.test", &login);
    let refused = |reason: &str| ActionCallError::Refused {
        reason: reason.to_owned(),
    };
    assert_eq!(
        workspaces
            .use_for_turn(&ada_turn, &overflow, json!({}))
            .await,
        Err(refused(TOO_MANY_WAIT))
    );
    let bo_ask = asked(
        workspaces
            .use_for_turn(&bo_turn, &overflow, json!({"bo": true}))
            .await,
    );
    assert_eq!(
        workspaces.open_use_asks(&workspace).len(),
        MAX_OPEN_ASKS + 1
    );
    assert!(workspaces.open_use_asks("other-workspace").is_empty());

    // `use_interrupted` does NOT remove a non-interrupted ask.
    workspaces.use_interrupted(&bo_ask);
    assert_eq!(
        workspaces.open_use_asks(&workspace).len(),
        MAX_OPEN_ASKS + 1
    );

    let (waiting, started, forgotten) = (
        kept_asks[0].clone(),
        kept_asks[1].clone(),
        kept_asks[2].clone(),
    );
    let claimed = workspaces.claim_use(&started.ask).unwrap();
    assert!(claimed.started && !claimed.interrupted);
    assert_eq!(
        workspaces.claim_use(&started.ask),
        Err(refused(CARRIED_OUT))
    );
    workspaces.use_not_asked(&forgotten);
    assert!(
        !workspaces
            .workspace_root(&workspace)
            .join("vault-asks")
            .join(format!("{}.json", forgotten.ask))
            .exists()
    );
    server.shutdown().await;

    let server = test_host(dir.path(), &key, &owner).await;
    assert_reopened_vault_uses(server.workspaces(), &workspace, &waiting.ask, &started.ask);
    server.shutdown().await;
}

fn assert_reopened_vault_uses(
    workspaces: &crate::workspaces::Workspaces,
    workspace: &str,
    waiting_ask: &str,
    started_ask: &str,
) {
    use tilo_actions::remote::ActionCallError;

    let refused = |reason: &str| {
        Err(ActionCallError::Refused {
            reason: reason.to_owned(),
        })
    };
    let reopened = workspaces.open_use_asks(workspace);
    assert_eq!(reopened.len(), MAX_OPEN_ASKS);
    let r_wait = reopened.iter().find(|a| a.ask == waiting_ask).unwrap();
    assert!(!r_wait.started && !r_wait.interrupted);
    let r_start = reopened.iter().find(|a| a.ask == started_ask).unwrap();
    assert!(r_start.started && r_start.interrupted);
    assert_eq!(workspaces.claim_use(started_ask), refused(CARRIED_OUT));
    workspaces.use_interrupted(r_start);
    assert_eq!(workspaces.claim_use(started_ask), refused(NOTHING_WAITS));
    assert!(workspaces.claim_use(waiting_ask).is_ok());
}

#[cfg(feature = "agents")]
mod agents_kinds {
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use serde_json::{Value, json};
    use tilo_actions::remote::ActionCallError;
    use tilo_actions::remote::{
        InvokeArgument, InvokeRequest, RemoteActionTarget, person_accounts,
    };
    use tilo_actions::{ActionAddress, InvocationId};

    use super::super::super::tests::{admit, governed};
    use super::super::turn_use_tests::on_turn;
    use super::{ADA, write_raw_ask};
    use crate::workspaces::Workspaces;
    use crate::workspaces::hosted_use::{
        CALENDAR_CONNECT, CALENDARS, HostedTurn, MAIL_CONNECT, MailCarried, MailFound, MailServer,
        MailServers, TurnConnect, TurnMailConnect,
    };
    use crate::workspaces::person_asking::kept_asks::{CARRIED_OUT, NOTHING_WAITS};
    use crate::workspaces::tests::{governor, test_host};

    fn invoke_request(resource: &str, action: &str, argument: Value) -> InvokeRequest {
        let Value::Object(argument) = argument else {
            panic!("arguments are an object");
        };
        InvokeRequest {
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: person_accounts::PROVIDER.to_owned(),
                address: ActionAddress::new(resource, action),
            },
            argument: InvokeArgument::Json(argument),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }
    }

    fn cal_request(idx: usize) -> InvokeRequest {
        invoke_request(
            CALENDARS,
            CALENDAR_CONNECT,
            json!({
                "serverUrl": format!("https://cal{idx}.example.test/dav/"),
                "username": format!("ada{idx}@example.test"),
                "name": format!("Cal {idx}"),
            }),
        )
    }

    fn mail_request(address: &str) -> InvokeRequest {
        invoke_request(
            person_accounts::RESOURCE,
            MAIL_CONNECT,
            json!({"address": address}),
        )
    }

    fn install_mail_probes(workspaces: &Workspaces) -> Arc<Mutex<usize>> {
        workspaces.call_private_model_addresses(true);
        workspaces.sign_in_accounts_through(|_address, _password| {
            Err("IMAP server rejected the saved credential".to_owned())
        });
        let probes = Arc::new(Mutex::new(0usize));
        let seen_probes = Arc::clone(&probes);
        workspaces.find_mail_servers_through(move |_address, _| {
            let mut count = seen_probes.lock().unwrap();
            *count = count.saturating_add(1);
            drop(count);
            Ok(MailServers {
                incoming: MailServer::new("imap.example.test", 993),
                outgoing: MailServer::new("smtp.example.test", 465),
                found: MailFound::Published,
                username: None,
                own_sign_in: None,
            })
        });
        probes
    }

    /// Literal bytes written by the current code for `ConnectAsk` (`calendar-asks/`) open cleanly
    /// across restarts, preserve oldest-first order, mark `started: true` asks as interrupted, and
    /// ignore corrupt files.
    #[tokio::test]
    async fn literal_bytes_of_calendar_asks_open_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = "ws-fixture";
        let person_hex = hex::encode(ADA);
        let cal_waiting_id = "2123456789abcdef0123456789abcdef";
        let cal_started_id = "ddcbad9876543210fedcba9876543210";

        let cal_waiting_bytes = format!(
            concat!(
                r#"{{"ask":"{id}","workspace":"{ws}","person":"{person}","agent":"scout","#,
                r#""name":"Scout","serverUrl":"https://cal.example.test/dav/","#,
                r#""place":"https://cal.example.test","username":"ada@example.test","#,
                r#""account":"Work","accountId":"caldav:ada@example.test@cal.example.test","#,
                r#""askedIn":{{"conversation":"c_general","cause":"m1","session":"s1"}},"#,
                r#""started":false,"askedAtMs":1700000000001,"known":["item-1"]}}"#,
            ),
            id = cal_waiting_id,
            ws = workspace,
            person = person_hex,
        );
        let cal_started_bytes = format!(
            concat!(
                r#"{{"ask":"{id}","workspace":"{ws}","person":"{person}","agent":"scout","#,
                r#""name":"Scout","serverUrl":"https://cal2.example.test/dav/","#,
                r#""place":"https://cal2.example.test","username":"ada@example.test","#,
                r#""account":"Home","accountId":"caldav:ada@example.test@cal2.example.test","#,
                r#""askedIn":{{"conversation":"c_general","cause":"m2","session":"s1"}},"#,
                r#""started":true,"askedAtMs":1700000000002,"known":[]}}"#,
            ),
            id = cal_started_id,
            ws = workspace,
            person = person_hex,
        );
        write_raw_ask(
            dir.path(),
            workspace,
            "calendar-asks",
            &format!("{cal_waiting_id}.json"),
            cal_waiting_bytes.as_bytes(),
        );
        write_raw_ask(
            dir.path(),
            workspace,
            "calendar-asks",
            &format!("{cal_started_id}.json"),
            cal_started_bytes.as_bytes(),
        );
        write_raw_ask(
            dir.path(),
            workspace,
            "calendar-asks",
            "corrupt.json",
            b"{not json",
        );

        let (key, owner) = governor();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();

        let connects = workspaces.open_connects(workspace);
        assert_eq!(connects.len(), 2, "{connects:?}");
        assert_eq!(connects[0].ask, cal_waiting_id);
        assert!(!connects[0].started && !connects[0].interrupted);
        assert_eq!(connects[0].account, "Work");
        assert_eq!(connects[0].known, vec!["item-1"]);
        assert_eq!(connects[1].ask, cal_started_id);
        assert!(connects[1].started && connects[1].interrupted);
        assert_eq!(
            workspaces.claim_connect(cal_started_id),
            Err(ActionCallError::Refused {
                reason: CARRIED_OUT.to_owned(),
            })
        );
        let claimed = workspaces.claim_connect(cal_waiting_id).unwrap();
        assert!(claimed.started);
        assert_eq!(
            workspaces.claim_connect(cal_waiting_id),
            Err(ActionCallError::Refused {
                reason: CARRIED_OUT.to_owned(),
            })
        );
        server.shutdown().await;
    }

    /// Literal bytes written by the current code for `MailAsk` (`mail-asks/`) open cleanly across
    /// restarts, preserve oldest-first order, mark `started: true` asks as interrupted, and ignore
    /// corrupt files.
    #[tokio::test]
    async fn literal_bytes_of_mail_asks_open_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = "ws-fixture";
        let person_hex = hex::encode(ADA);
        let mail_waiting_id = "3123456789abcdef0123456789abcdef";
        let mail_started_id = "cccbba9876543210fedcba9876543210";

        let mail_waiting_bytes = format!(
            concat!(
                r#"{{"ask":"{id}","workspace":"{ws}","person":"{person}","agent":"scout","#,
                r#""name":"Scout","address":"ada@example.test","username":"ada@example.test","#,
                r#""account":"Ada","reads":"imap.example.test","readsPort":993,"#,
                r#""sends":"smtp.example.test","sendsPort":465,"#,
                r#""belongsTo":["imap://imap.example.test:993","smtp://smtp.example.test:465"],"#,
                r#""found":"as its provider publishes them for mail programs","refusedBy":"","#,
                r#""askedIn":{{"conversation":"c_general","cause":"m1","session":"s1"}},"#,
                r#""started":false,"askedAtMs":1700000000001,"known":["item-1"]}}"#,
            ),
            id = mail_waiting_id,
            ws = workspace,
            person = person_hex,
        );
        let mail_started_bytes = format!(
            concat!(
                r#"{{"ask":"{id}","workspace":"{ws}","person":"{person}","agent":"scout","#,
                r#""name":"Scout","address":"work@example.test","username":"work@example.test","#,
                r#""account":"","reads":"imap.work.test","readsPort":993,"#,
                r#""sends":"smtp.work.test","sendsPort":465,"#,
                r#""belongsTo":["imap://imap.work.test:993","smtp://smtp.work.test:465"],"#,
                r#""found":"as you named them","refusedBy":"imap.work.test","#,
                r#""askedIn":{{"conversation":"c_general","cause":"m2","session":"s1"}},"#,
                r#""started":true,"askedAtMs":1700000000002,"known":[]}}"#,
            ),
            id = mail_started_id,
            ws = workspace,
            person = person_hex,
        );
        write_raw_ask(
            dir.path(),
            workspace,
            "mail-asks",
            &format!("{mail_waiting_id}.json"),
            mail_waiting_bytes.as_bytes(),
        );
        write_raw_ask(
            dir.path(),
            workspace,
            "mail-asks",
            &format!("{mail_started_id}.json"),
            mail_started_bytes.as_bytes(),
        );
        write_raw_ask(
            dir.path(),
            workspace,
            "mail-asks",
            "corrupt.json",
            b"{not json",
        );

        let (key, owner) = governor();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();

        let mails = workspaces.open_mail_connects(workspace);
        assert_eq!(mails.len(), 2, "{mails:?}");
        assert_eq!(mails[0].ask, mail_waiting_id);
        assert!(!mails[0].started && !mails[0].interrupted);
        assert_eq!(mails[0].reads_port, 993);
        assert_eq!(mails[0].sends_port, 465);
        assert_eq!(mails[1].ask, mail_started_id);
        assert!(mails[1].started && mails[1].interrupted);
        assert_eq!(mails[1].refused_by, "imap.work.test");
        assert_eq!(
            workspaces.claim_mail_connect(mail_started_id),
            Err(ActionCallError::Refused {
                reason: CARRIED_OUT.to_owned(),
            })
        );
        let claimed = workspaces.claim_mail_connect(mail_waiting_id).unwrap();
        assert!(claimed.started);
        assert_eq!(
            workspaces.claim_mail_connect(mail_waiting_id),
            Err(ActionCallError::Refused {
                reason: CARRIED_OUT.to_owned(),
            })
        );
        server.shutdown().await;
    }

    /// Pinning test for `ConnectAsk` (`calendar-asks/`): file layout, deduplication, single claim,
    /// wrong-person refusal on `carry_out_connect`, and bad-id rejection.
    #[tokio::test]
    async fn calendar_connect_asks_pin_dedup_wrong_person_and_bad_ids() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        workspaces.call_private_model_addresses(true);
        let (workspace, workspace_id, _service) = governed(workspaces).await;
        let ada = admit(workspaces, &workspace, &owner, 1).await;
        let bo = admit(workspaces, &workspace, &owner, 2).await;

        let ada_caller = on_turn(&ada, &workspace, "scout");
        let bo_caller = on_turn(&bo, &workspace, "scout");
        let ada_turn = HostedTurn {
            workspace: workspace_id,
            caller: &ada_caller,
            bound: None,
        };
        let bo_turn = HostedTurn {
            workspace: workspace_id,
            caller: &bo_caller,
            bound: None,
        };

        let TurnConnect::Asks(first) = workspaces
            .connect_calendar_for_turn(&ada_turn, &cal_request(0), json!({"n": 0}))
            .await
            .unwrap()
        else {
            panic!("expected Asks");
        };
        let file_path = workspaces
            .workspace_root(&workspace)
            .join("calendar-asks")
            .join(format!("{}.json", first.ask));
        assert!(file_path.is_file(), "kept at {}", file_path.display());

        assert_eq!(
            workspaces
                .connect_calendar_for_turn(&ada_turn, &cal_request(0), json!({}))
                .await,
            Ok(TurnConnect::Waits)
        );

        let claimed = workspaces.claim_connect(&first.ask).unwrap();
        assert_eq!(
            workspaces.claim_connect(&first.ask),
            Err(ActionCallError::Refused {
                reason: CARRIED_OUT.to_owned(),
            })
        );
        let wrong = workspaces
            .carry_out_connect(
                &bo_turn,
                &claimed,
                Some("vault.item:00000000000000000000000000000001"),
            )
            .await
            .unwrap_err();
        assert!(wrong.contains("no longer acts for this person"), "{wrong}");
        assert!(!file_path.exists());
        assert_eq!(
            workspaces.claim_connect(&first.ask),
            Err(ActionCallError::Refused {
                reason: NOTHING_WAITS.to_owned(),
            })
        );

        for bad in ["0123456789abcdef0123456789abcdef", "../../people"] {
            assert_eq!(
                workspaces.claim_connect(bad),
                Err(ActionCallError::Refused {
                    reason: NOTHING_WAITS.to_owned(),
                })
            );
        }
        server.shutdown().await;
    }

    /// Pinning test for `ConnectAsk` (`calendar-asks/`): per-person cap (8), independent quota,
    /// workspace isolation, `connect_not_asked`, and restart recovery.
    #[tokio::test]
    async fn calendar_connect_asks_pin_cap_forget_and_restart_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        workspaces.call_private_model_addresses(true);
        let (workspace, workspace_id, _service) = governed(workspaces).await;
        let ada = admit(workspaces, &workspace, &owner, 1).await;
        let bo = admit(workspaces, &workspace, &owner, 2).await;

        let ada_caller = on_turn(&ada, &workspace, "scout");
        let bo_caller = on_turn(&bo, &workspace, "scout");
        let ada_turn = HostedTurn {
            workspace: workspace_id,
            caller: &ada_caller,
            bound: None,
        };
        let bo_turn = HostedTurn {
            workspace: workspace_id,
            caller: &bo_caller,
            bound: None,
        };

        let mut kept = Vec::new();
        for i in 0..8 {
            let TurnConnect::Asks(ask) = workspaces
                .connect_calendar_for_turn(&ada_turn, &cal_request(i), json!({"n": i}))
                .await
                .unwrap()
            else {
                panic!("expected Asks for {i}");
            };
            kept.push(*ask);
        }
        let overflow = workspaces
            .connect_calendar_for_turn(&ada_turn, &cal_request(8), json!({}))
            .await;
        assert!(
            matches!(&overflow, Err(ActionCallError::Refused { reason }) if reason.contains("Too many calendars")),
            "{overflow:?}"
        );
        assert!(matches!(
            workspaces
                .connect_calendar_for_turn(&bo_turn, &cal_request(8), json!({}))
                .await,
            Ok(TurnConnect::Asks(_))
        ));
        assert_eq!(workspaces.open_connects(&workspace).len(), 9);
        assert!(workspaces.open_connects("other-workspace").is_empty());

        let (waiting, started, forgotten) = (kept[0].clone(), kept[1].clone(), kept[2].clone());
        let _ = workspaces.claim_connect(&started.ask).unwrap();
        workspaces.connect_not_asked(&forgotten);
        server.shutdown().await;

        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let reopened = workspaces.open_connects(&workspace);
        assert_eq!(reopened.len(), 8);
        let r_wait = reopened.iter().find(|a| a.ask == waiting.ask).unwrap();
        assert!(!r_wait.started && !r_wait.interrupted);
        let r_start = reopened.iter().find(|a| a.ask == started.ask).unwrap();
        assert!(r_start.started && r_start.interrupted);
        assert_eq!(
            workspaces.claim_connect(&started.ask),
            Err(ActionCallError::Refused {
                reason: CARRIED_OUT.to_owned(),
            })
        );
        assert!(workspaces.claim_connect(&waiting.ask).is_ok());
        server.shutdown().await;
    }

    /// Pinning test for `MailAsk` (`mail-asks/`): file layout, deduplication before probe,
    /// single claim, wrong-person refusal on `carry_out_mail_connect`, and bad-id rejection.
    #[tokio::test]
    async fn mail_connect_asks_pin_dedup_wrong_person_and_bad_ids() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let probes = install_mail_probes(workspaces);

        let (workspace, workspace_id, service) = governed(workspaces).await;
        let ada = admit(workspaces, &workspace, &owner, 1).await;
        let bo = admit(workspaces, &workspace, &owner, 2).await;
        let as_ada = super::super::super::tests::caller(&ada);
        let login =
            super::super::turn_use_tests::a_mail_login(&service, workspace_id, (&ada, &as_ada))
                .await;
        let login_ref = login.as_str().unwrap().to_owned();

        let ada_caller = on_turn(&ada, &workspace, "scout");
        let bo_caller = on_turn(&bo, &workspace, "scout");
        let ada_turn = HostedTurn {
            workspace: workspace_id,
            caller: &ada_caller,
            bound: None,
        };
        let bo_turn = HostedTurn {
            workspace: workspace_id,
            caller: &bo_caller,
            bound: None,
        };

        let TurnMailConnect::Asks(first) = workspaces
            .connect_mail_for_turn(
                &ada_turn,
                &mail_request("ada@example.test"),
                json!({"n": 0}),
            )
            .await
            .unwrap()
        else {
            panic!("expected Asks");
        };
        assert_eq!(*probes.lock().unwrap(), 1);
        let file_path = workspaces
            .workspace_root(&workspace)
            .join("mail-asks")
            .join(format!("{}.json", first.ask));
        assert!(file_path.is_file(), "kept at {}", file_path.display());

        // Deduplicated BEFORE probing mail servers again.
        assert_eq!(
            workspaces
                .connect_mail_for_turn(&ada_turn, &mail_request("ada@example.test"), json!({}))
                .await,
            Ok(TurnMailConnect::Waits)
        );
        assert_eq!(*probes.lock().unwrap(), 1, "no second server probe");

        // Another person's turn cannot carry out Ada's ask.
        let claimed = workspaces.claim_mail_connect(&first.ask).unwrap();
        assert_eq!(
            workspaces.claim_mail_connect(&first.ask),
            Err(ActionCallError::Refused {
                reason: CARRIED_OUT.to_owned(),
            })
        );
        let wrong = workspaces
            .carry_out_mail_connect(&bo_turn, &claimed, Some(&login_ref))
            .await
            .unwrap_err();
        assert!(wrong.contains("no longer acts for this person"), "{wrong}");
        assert!(!file_path.exists());

        for bad in ["0123456789abcdef0123456789abcdef", "../../people"] {
            assert_eq!(
                workspaces.claim_mail_connect(bad),
                Err(ActionCallError::Refused {
                    reason: NOTHING_WAITS.to_owned(),
                })
            );
        }
        server.shutdown().await;
    }

    /// Pinning test for `MailAsk` (`mail-asks/`): `MailCarried::AsksAgain` replacement on disk,
    /// decline, `mail_connect_not_asked`, workspace isolation, and restart recovery.
    #[tokio::test]
    async fn mail_connect_asks_pin_retry_decline_and_restart_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let _probes = install_mail_probes(workspaces);

        let (workspace, workspace_id, service) = governed(workspaces).await;
        let ada = admit(workspaces, &workspace, &owner, 1).await;
        let as_ada = super::super::super::tests::caller(&ada);
        let login =
            super::super::turn_use_tests::a_mail_login(&service, workspace_id, (&ada, &as_ada))
                .await;
        let login_ref = login.as_str().unwrap().to_owned();
        let ada_caller = on_turn(&ada, &workspace, "scout");
        let ada_turn = HostedTurn {
            workspace: workspace_id,
            caller: &ada_caller,
            bound: None,
        };

        let TurnMailConnect::Asks(retrying) = workspaces
            .connect_mail_for_turn(&ada_turn, &mail_request("retry@example.test"), json!({}))
            .await
            .unwrap()
        else {
            panic!("expected Asks");
        };
        let claimed_retry = workspaces.claim_mail_connect(&retrying.ask).unwrap();
        let MailCarried::AsksAgain(again) = workspaces
            .carry_out_mail_connect(&ada_turn, &claimed_retry, Some(&login_ref))
            .await
            .unwrap()
        else {
            panic!("expected AsksAgain");
        };
        assert_ne!(again.ask, retrying.ask);
        assert_eq!(again.refused_by, "imap.example.test");
        assert!(!again.started && !again.interrupted);
        assert_eq!(
            workspaces.claim_mail_connect(&retrying.ask),
            Err(ActionCallError::Refused {
                reason: NOTHING_WAITS.to_owned(),
            })
        );

        let TurnMailConnect::Asks(declining) = workspaces
            .connect_mail_for_turn(&ada_turn, &mail_request("decline@example.test"), json!({}))
            .await
            .unwrap()
        else {
            panic!("expected Asks");
        };
        let claimed_decline = workspaces.claim_mail_connect(&declining.ask).unwrap();
        assert_eq!(
            workspaces
                .carry_out_mail_connect(&ada_turn, &claimed_decline, None)
                .await,
            Ok(MailCarried::Declined)
        );

        let TurnMailConnect::Asks(started) = workspaces
            .connect_mail_for_turn(&ada_turn, &mail_request("started@example.test"), json!({}))
            .await
            .unwrap()
        else {
            panic!("expected Asks");
        };
        let TurnMailConnect::Asks(forgotten) = workspaces
            .connect_mail_for_turn(
                &ada_turn,
                &mail_request("forgotten@example.test"),
                json!({}),
            )
            .await
            .unwrap()
        else {
            panic!("expected Asks");
        };
        let _ = workspaces.claim_mail_connect(&started.ask).unwrap();
        workspaces.mail_connect_not_asked(&forgotten);
        assert!(workspaces.open_mail_connects("other-workspace").is_empty());
        server.shutdown().await;

        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let reopened = workspaces.open_mail_connects(&workspace);
        assert_eq!(reopened.len(), 2, "{reopened:?}");
        let r_again = reopened.iter().find(|a| a.ask == again.ask).unwrap();
        assert!(!r_again.started && !r_again.interrupted);
        assert_eq!(r_again.refused_by, "imap.example.test");
        let r_start = reopened.iter().find(|a| a.ask == started.ask).unwrap();
        assert!(r_start.started && r_start.interrupted);
        assert_eq!(
            workspaces.claim_mail_connect(&started.ask),
            Err(ActionCallError::Refused {
                reason: CARRIED_OUT.to_owned(),
            })
        );
        assert!(workspaces.claim_mail_connect(&again.ask).is_ok());
        server.shutdown().await;
    }

    struct NoAgentKeys;

    impl crate::agents::agent_keys::AgentKeys for NoAgentKeys {
        fn grant(
            &self,
            _workspace: crate::workspaces::WorkspaceId,
            _agent: &str,
            _name: &str,
            _person: [u8; 32],
        ) -> Result<[u8; 32], String> {
            Err("no keys in unit test".to_owned())
        }

        fn sign(
            &self,
            _workspace: crate::workspaces::WorkspaceId,
            _key: &[u8; 32],
            _request: &mut InvokeRequest,
        ) -> Result<(), String> {
            Err("no keys in unit test".to_owned())
        }
    }

    fn seed_settle_fixtures(
        root: &Path,
        workspace: &str,
        person_hex: &str,
        (asked_millis, asked_nanos): (u64, u64),
    ) {
        for (id, started, at_ms) in [
            ("aa000000000000000000000000000001", false, 1u64),
            ("aa000000000000000000000000000002", false, asked_millis),
            ("aa000000000000000000000000000003", true, asked_millis),
        ] {
            let body = format!(
                r#"{{"ask":"{id}","workspace":"{workspace}","person":"{person_hex}","agent":"scout","name":"Scout","carrier":"action","kept":{{"job":1}},"always":null,"words":{{"action":"a","place":"p"}},"askedIn":{{}},"started":{started},"askedAtMs":{at_ms}}}"#
            );
            write_raw_ask(
                root,
                workspace,
                "asks",
                &format!("{id}.json"),
                body.as_bytes(),
            );
        }
        for (id, started, at_ns) in [
            ("bb000000000000000000000000000001", true, asked_nanos),
            ("bb000000000000000000000000000002", false, asked_nanos),
        ] {
            let body = format!(
                r#"{{"ask":"{id}","workspace":"{workspace}","person":"{person_hex}","agent":"scout","name":"Scout","reason":"r","askedIn":{{}},"detail":{{"use":{{"ask":"{id}"}}}},"started":{started},"askedAtNs":{at_ns},"provider":"tilo.accounts","resource":"accounts","action":"add-mail-with-item","arguments":{{}}}}"#
            );
            write_raw_ask(
                root,
                workspace,
                "vault-asks",
                &format!("{id}.json"),
                body.as_bytes(),
            );
        }
        for (id, started) in [
            ("cc000000000000000000000000000001", true),
            ("cc000000000000000000000000000002", false),
        ] {
            let body = format!(
                r#"{{"ask":"{id}","workspace":"{workspace}","person":"{person_hex}","agent":"scout","name":"Scout","serverUrl":"https://cal.example.test/dav/","place":"https://cal.example.test","username":"ada@example.test","account":"Work","accountId":"caldav:ada@example.test@cal.example.test","askedIn":{{}},"started":{started},"askedAtMs":{asked_millis},"known":[]}}"#
            );
            write_raw_ask(
                root,
                workspace,
                "calendar-asks",
                &format!("{id}.json"),
                body.as_bytes(),
            );
        }
        for (id, started) in [
            ("dd000000000000000000000000000001", true),
            ("dd000000000000000000000000000002", false),
        ] {
            let body = format!(
                r#"{{"ask":"{id}","workspace":"{workspace}","person":"{person_hex}","agent":"scout","name":"Scout","address":"ada@example.test","username":"ada@example.test","account":"","reads":"imap.example.test","readsPort":993,"sends":"smtp.example.test","sendsPort":465,"belongsTo":["imap://imap.example.test:993","smtp://smtp.example.test:465"],"found":"as you named them","refusedBy":"","askedIn":{{}},"started":{started},"askedAtMs":{asked_millis},"known":[]}}"#
            );
            write_raw_ask(
                root,
                workspace,
                "mail-asks",
                &format!("{id}.json"),
                body.as_bytes(),
            );
        }
    }

    /// Pinning test for the settle loops across all four kept-ask kinds via `ServerAgents::look`:
    /// - interrupted asks of all four kinds are forgotten;
    /// - an unasked `KeptAsk` older than `UNASKED_FOR` (300 s) is forgotten while a fresh one stays;
    /// - waiting `ConnectAsk` and `MailAsk` whose agent is absent from the workspace are forgotten.
    #[tokio::test]
    async fn settle_loops_clear_interrupted_and_expired_asks_across_all_four_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_host(dir.path(), &key, &owner).await;
        let (workspace, workspace_id, _service) = governed(server.workspaces()).await;
        server.shutdown().await;

        seed_settle_fixtures(
            dir.path(),
            &workspace,
            &hex::encode(ADA),
            (tilo_clock::unix_now_ms(), tilo_clock::unix_now_ns()),
        );

        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces().clone();
        let reopened_service = super::super::super::ServiceWorkspaceActions::new();
        reopened_service.attach(workspaces.clone());
        let actions = Arc::new(reopened_service.in_workspace_as(workspace_id, &owner));
        let agents = crate::agents::ServerAgents::on_device(
            workspaces.clone(),
            actions,
            Arc::new(NoAgentKeys),
        );
        agents.look(&workspace).await.unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while (!workspaces.open_connects(&workspace).is_empty()
            || !workspaces.open_mail_connects(&workspace).is_empty())
            && std::time::Instant::now() < deadline
        {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let remaining_asks: Vec<String> = workspaces
            .open_asks(&workspace)
            .into_iter()
            .map(|a| a.ask)
            .collect();
        assert_eq!(
            remaining_asks,
            vec!["aa000000000000000000000000000002".to_owned()]
        );
        assert_eq!(
            workspaces.claim_ask("aa000000000000000000000000000001"),
            Err(NOTHING_WAITS.to_owned())
        );
        assert_eq!(
            workspaces.claim_ask("aa000000000000000000000000000003"),
            Err(NOTHING_WAITS.to_owned())
        );

        let remaining_uses: Vec<String> = workspaces
            .open_use_asks(&workspace)
            .into_iter()
            .map(|a| a.ask)
            .collect();
        assert_eq!(
            remaining_uses,
            vec!["bb000000000000000000000000000002".to_owned()]
        );
        assert_eq!(
            workspaces.claim_use("bb000000000000000000000000000001"),
            Err(ActionCallError::Refused {
                reason: NOTHING_WAITS.to_owned(),
            })
        );

        assert!(workspaces.open_connects(&workspace).is_empty());
        assert!(workspaces.open_mail_connects(&workspace).is_empty());
        for id in [
            "cc000000000000000000000000000001",
            "cc000000000000000000000000000002",
        ] {
            assert_eq!(
                workspaces.claim_connect(id),
                Err(ActionCallError::Refused {
                    reason: NOTHING_WAITS.to_owned(),
                })
            );
        }
        for id in [
            "dd000000000000000000000000000001",
            "dd000000000000000000000000000002",
        ] {
            assert_eq!(
                workspaces.claim_mail_connect(id),
                Err(ActionCallError::Refused {
                    reason: NOTHING_WAITS.to_owned(),
                })
            );
        }
        server.shutdown().await;
    }

    fn seed_misplaced_ask_fixtures(
        root: &Path,
        workspace: &str,
        person_hex: &str,
        (asked_millis, asked_nanos): (u64, u64),
    ) {
        let wrong_ask_body = |id: &str, ws: &str| {
            format!(
                r#"{{"ask":"{id}","workspace":"{ws}","person":"{person_hex}","agent":"scout","name":"Scout","carrier":"action","kept":{{}},"always":null,"words":{{"action":"a","place":"p"}},"askedIn":{{}},"started":false,"askedAtMs":{asked_millis}}}"#
            )
        };
        let wrong_use_body = |id: &str, ws: &str, at_ns: u64| {
            format!(
                r#"{{"ask":"{id}","workspace":"{ws}","person":"{person_hex}","agent":"scout","name":"Scout","reason":"r","askedIn":{{}},"detail":{{"use":{{"ask":"{id}"}}}},"started":false,"askedAtNs":{at_ns},"provider":"tilo.accounts","resource":"accounts","action":"add-mail-with-item","arguments":{{}}}}"#
            )
        };
        let wrong_cal_body = |id: &str, ws: &str| {
            format!(
                r#"{{"ask":"{id}","workspace":"{ws}","person":"{person_hex}","agent":"scout","name":"Scout","serverUrl":"https://cal.example.test/dav/","place":"https://cal.example.test","username":"ada@example.test","account":"Work","accountId":"caldav:ada@example.test@cal.example.test","askedIn":{{}},"started":false,"askedAtMs":{asked_millis},"known":[]}}"#
            )
        };
        let wrong_mail_body = |id: &str, ws: &str| {
            format!(
                r#"{{"ask":"{id}","workspace":"{ws}","person":"{person_hex}","agent":"scout","name":"Scout","address":"ada@example.test","username":"ada@example.test","account":"","reads":"imap.example.test","readsPort":993,"sends":"smtp.example.test","sendsPort":465,"belongsTo":["imap://imap.example.test:993","smtp://smtp.example.test:465"],"found":"as you named them","refusedBy":"","askedIn":{{}},"started":false,"askedAtMs":{asked_millis},"known":[]}}"#
            )
        };

        write_raw_ask(
            root,
            workspace,
            "asks",
            "wrong-name.json",
            wrong_ask_body("e1000000000000000000000000000001", workspace).as_bytes(),
        );
        write_raw_ask(
            root,
            workspace,
            "asks",
            "e1000000000000000000000000000002.json",
            wrong_ask_body("e1000000000000000000000000000002", "other-ws").as_bytes(),
        );
        write_raw_ask(
            root,
            workspace,
            "vault-asks",
            "wrong-name.json",
            wrong_use_body("e2000000000000000000000000000001", workspace, asked_nanos).as_bytes(),
        );
        write_raw_ask(
            root,
            workspace,
            "vault-asks",
            "e2000000000000000000000000000002.json",
            wrong_use_body("e2000000000000000000000000000002", "other-ws", asked_nanos).as_bytes(),
        );
        write_raw_ask(
            root,
            workspace,
            "calendar-asks",
            "wrong-name.json",
            wrong_cal_body("e3000000000000000000000000000001", workspace).as_bytes(),
        );
        write_raw_ask(
            root,
            workspace,
            "calendar-asks",
            "e3000000000000000000000000000002.json",
            wrong_cal_body("e3000000000000000000000000000002", "other-ws").as_bytes(),
        );
        write_raw_ask(
            root,
            workspace,
            "mail-asks",
            "wrong-name.json",
            wrong_mail_body("e4000000000000000000000000000001", workspace).as_bytes(),
        );
        write_raw_ask(
            root,
            workspace,
            "mail-asks",
            "e4000000000000000000000000000002.json",
            wrong_mail_body("e4000000000000000000000000000002", "other-ws").as_bytes(),
        );
    }

    fn assert_bad_ids_refused(workspaces: &Workspaces) {
        for bad in [
            "",
            "not-32-hex",
            "E1000000000000000000000000000001",
            "../etc/passwd",
        ] {
            assert_eq!(workspaces.claim_ask(bad), Err(NOTHING_WAITS.to_owned()));
            assert_eq!(
                workspaces.claim_use(bad),
                Err(ActionCallError::Refused {
                    reason: NOTHING_WAITS.to_owned(),
                })
            );
            assert_eq!(
                workspaces.claim_connect(bad),
                Err(ActionCallError::Refused {
                    reason: NOTHING_WAITS.to_owned(),
                })
            );
            assert_eq!(
                workspaces.claim_mail_connect(bad),
                Err(ActionCallError::Refused {
                    reason: NOTHING_WAITS.to_owned(),
                })
            );
        }
    }

    /// Deliberate stricter load checks and secret-use non-expiry:
    /// - misplaced kept-ask files (wrong filename or wrong workspace directory) are ignored;
    /// - malformed ask ids are refused immediately by every `claim_*`;
    /// - an unasked secret-use `OpenAsk` older than `UNASKED_FOR` (300 s) is NOT expired by
    ///   `ServerAgents::look` and still answers `TurnUse::Waits`.
    #[tokio::test]
    async fn misplaced_files_are_ignored_and_unasked_vault_uses_do_not_expire() {
        use super::super::super::tests::caller;
        use super::super::turn_use_tests::{a_mail_login, add_mail, asked, proving};
        use crate::workspaces::fixtures::{TestGroupKeys, test_host_holding};
        use crate::workspaces::hosted_use::TurnUse;

        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let group_keys = Arc::new(TestGroupKeys::default());
        let server = test_host_holding(dir.path(), &key, &owner, Arc::clone(&group_keys)).await;
        let workspaces = server.workspaces();
        let (workspace, workspace_id, service) = governed(workspaces).await;
        let ada = admit(workspaces, &workspace, &owner, 1).await;
        let as_ada = caller(&ada);
        let login = a_mail_login(&service, workspace_id, (&ada, &as_ada)).await;
        let _proved = proving(workspaces);

        let ada_caller = on_turn(&ada, &workspace, "scout");
        let ada_turn = HostedTurn {
            workspace: workspace_id,
            caller: &ada_caller,
            bound: None,
        };
        let req = add_mail("old-unasked@example.test", "imap.example.test", &login);
        let kept = asked(workspaces.use_for_turn(&ada_turn, &req, json!({})).await);
        let kept_path = workspaces
            .workspace_root(&workspace)
            .join("vault-asks")
            .join(format!("{}.json", kept.ask));
        server.shutdown().await;

        // Backdate the kept secret-use file so it is older than UNASKED_FOR (300 s).
        let mut stored: Value =
            serde_json::from_slice(&std::fs::read(&kept_path).unwrap()).unwrap();
        stored["askedAtNs"] = json!(1_000_000u64);
        std::fs::write(&kept_path, serde_json::to_vec(&stored).unwrap()).unwrap();

        seed_misplaced_ask_fixtures(
            dir.path(),
            &workspace,
            &hex::encode(ada.user_public_key),
            (tilo_clock::unix_now_ms(), tilo_clock::unix_now_ns()),
        );

        let server = test_host_holding(dir.path(), &key, &owner, Arc::clone(&group_keys)).await;
        let workspaces = server.workspaces().clone();
        let _proved = proving(&workspaces);

        assert!(workspaces.open_asks(&workspace).is_empty());
        assert!(workspaces.open_asks("other-ws").is_empty());
        assert!(workspaces.open_connects(&workspace).is_empty());
        assert!(workspaces.open_connects("other-ws").is_empty());
        assert!(workspaces.open_mail_connects(&workspace).is_empty());
        assert!(workspaces.open_mail_connects("other-ws").is_empty());
        assert!(workspaces.open_use_asks("other-ws").is_empty());
        assert_bad_ids_refused(&workspaces);

        let reopened_service = super::super::super::ServiceWorkspaceActions::new();
        reopened_service.attach(workspaces.clone());
        let actions = Arc::new(reopened_service.in_workspace_as(workspace_id, &owner));
        let agents = crate::agents::ServerAgents::on_device(
            workspaces.clone(),
            actions,
            Arc::new(NoAgentKeys),
        );
        agents.look(&workspace).await.unwrap();

        let remaining_uses: Vec<String> = workspaces
            .open_use_asks(&workspace)
            .into_iter()
            .map(|a| a.ask)
            .collect();
        assert_eq!(
            remaining_uses,
            vec![kept.ask],
            "unasked secret-use ask older than UNASKED_FOR stays kept"
        );
        assert_eq!(
            workspaces.use_for_turn(&ada_turn, &req, json!({})).await,
            Ok(TurnUse::Waits),
            "secret-use ask older than UNASKED_FOR still answers Waits"
        );
        server.shutdown().await;
    }
}
