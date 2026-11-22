//! A hosted workspace is its person's: its host serves it as one of its administrators, and
//! the changes only its owner makes (unmaking an administrator, handing the workspace on) are
//! signed on a device of the owner's own, which the host relays as it relays every member's
//! change. The owner makes them from a computer `tilo login` made a device of (its commands
//! over the one session its holder keeps) and from their page; an administrator who is not
//! the owner is refused them, and so is the host, asked through a key of the owner's that it
//! acts for.

use super::api::ISSUER;
use super::api_on_server::{OnServer, api_on_server, approved_key, headers, json_of};
use super::api_v1::{Holder, own_workspace};
use super::*;
use serde_json::{Value, json};
use std::sync::Mutex;
use tilo_server_client::MemberApprover;
use tilo_server_client::questions::{Answered, MemberQuestions, QuestionSheet, answer_questions};
use tilo_server_protocol::certify::{ConfirmationAnswer, PendingConfirmation};
use tokio::sync::mpsc;

/// A person at their page, answering what their computer's agent asks them: allowing it, or
/// refusing the next one when told to, and noting what each question asked for.
#[derive(Clone, Default)]
struct AtTheirPage {
    asked: Arc<Mutex<Vec<String>>>,
    refuse_next: Arc<Mutex<bool>>,
}

impl AtTheirPage {
    /// `root`'s page, answering the server's questions until the test ends.
    fn of(on_server: &OnServer, root: &SigningKey) -> Self {
        let person = Self::default();
        let approver = MemberApprover::new(on_server.server_host());
        let (root, mut sheet) = (root.clone(), person.clone());
        tokio::spawn(async move {
            let mut page = MemberQuestions {
                server: &approver,
                root: &root,
                device: None,
            };
            let (answered, mut answers) = mpsc::unbounded_channel();
            let mut sheet = Sheet {
                person: &mut sheet,
                answered,
            };
            let ended = answer_questions(&mut page, &mut sheet, &mut answers, || {
                tokio::time::sleep(Duration::from_millis(50))
            })
            .await;
            panic!("the page's session with the server ended: {ended}");
        });
        person
    }

    /// What they were asked to allow, in order.
    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }

    /// Refuse the next question they are asked.
    fn refuse_next(&self) {
        *self.refuse_next.lock().unwrap() = true;
    }
}

/// The page's sheet of questions, answered as [`AtTheirPage`] says.
struct Sheet<'a> {
    person: &'a mut AtTheirPage,
    answered: mpsc::UnboundedSender<Answered>,
}

impl QuestionSheet for Sheet<'_> {
    fn ask(&mut self, question: &PendingConfirmation) {
        self.person
            .asked
            .lock()
            .unwrap()
            .push(question.action_label.clone());
        let refuse = std::mem::take(&mut *self.person.refuse_next.lock().unwrap());
        let answer = if refuse {
            ConfirmationAnswer::Deny
        } else {
            ConfirmationAnswer::AllowOnce
        };
        let _ = self.answered.send((question.id, answer));
    }

    fn withdraw(&mut self, _id: [u8; 32]) {}
}

/// The member row of `person` (hex) in an answer of `workspace/members`, if they are in it: the
/// people a change answers, or the list itself.
fn row(answered: &Value, person: &str) -> Option<Value> {
    let result = &answered["result"];
    result["members"]
        .as_array()
        .or_else(|| result.as_array())?
        .iter()
        .find(|row| row["member"]["person"] == person && row["removed"] != true)
        .cloned()
}

/// The route of `workspace/members#action` in `workspace`.
fn people(workspace: &str, action: &str) -> String {
    format!("/v1/workspaces/{workspace}/resources/workspace/members/actions/{action}")
}

/// `POST path` with `body` as the key `token`: the status and the answer.
async fn as_key(on_server: &OnServer, token: &str, path: &str, body: &Value) -> (u16, Value) {
    let bearer = format!("Bearer {token}");
    json_of(
        on_server
            .api
            .dispatch(
                &hyper::Method::POST,
                path,
                "",
                &headers(&[
                    ("authorization", bearer.as_str()),
                    ("content-type", "application/json"),
                ]),
                body.to_string().as_bytes(),
            )
            .await,
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_owner_unmakes_administrators_and_hands_the_workspace_on_from_their_computer() {
    let on_server = api_on_server(ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let workspace = own_workspace(&on_server, &ada, &page).await;
    let add = people(&workspace, "add");
    let role = people(&workspace, "role");
    let remove = people(&workspace, "remove");
    let list = people(&workspace, "list");
    let ada_hex = hex::encode(ada.verifying_key().to_bytes());
    let (grace, _) = tilo_user_identity::UserIdentity::generate("Grace", 1);
    let grace_hex = hex::encode(grace.verifying_key().to_bytes());
    let (hal, _) = tilo_user_identity::UserIdentity::generate("Hal", 1);
    let hal_hex = hex::encode(hal.verifying_key().to_bytes());

    // Ada's computer makes Grace and Hal administrators, by their keys. It is her agent's: it asks
    // her first, on her page, and her own device signs each change once she allowed it.
    let ada_at_page = AtTheirPage::of(&on_server, &ada);
    let ada_mac = Holder::start(&on_server, &ada, &workspace, "Claude Code").await;
    for (person, name) in [(&grace_hex, "Grace Hopper"), (&hal_hex, "Hal")] {
        let added = ada_mac
            .command()
            .post(
                &add,
                &json!({"person": person, "role": "administrator", "name": name}),
            )
            .await
            .unwrap_or_else(|error| panic!("Ada makes {name} an administrator: {error:#}"));
        let made = row(&added, person).unwrap_or_else(|| panic!("{name} is listed: {added}"));
        assert_eq!(made["administrator"], true, "{added}");
        assert_eq!(made["name"], name, "{added}");
    }

    // Grace's own computer: an administrator, not the owner. Unmaking another administrator and
    // handing the workspace on are not hers to sign, and the workspace refuses them.
    let grace_at_page = AtTheirPage::of(&on_server, &grace);
    let grace_mac = Holder::start(&on_server, &grace, &workspace, "Claude Code").await;
    let removing = grace_mac
        .command()
        .post(&remove, &json!({ "person": hal_hex }))
        .await
        .expect_err("an administrator does not remove another");
    assert!(
        format!("{removing:#}").contains("refused"),
        "the workspace says it refused: {removing:#}"
    );
    let demoting = grace_mac
        .command()
        .post(&role, &json!({"person": hal_hex, "role": "viewer"}))
        .await
        .expect_err("an administrator does not demote another");
    assert!(format!("{demoting:#}").contains("refused"), "{demoting:#}");
    let taking = grace_mac
        .command()
        .post(&add, &json!({"person": grace_hex, "role": "owner"}))
        .await
        .expect_err("an administrator does not take the workspace");
    assert!(format!("{taking:#}").contains("refused"), "{taking:#}");

    // A key Ada approved acts for her through the host, which signs what it carries out: it may
    // not unmake an administrator on its own authority, whoever asks it.
    let token = approved_key(&on_server, &ada, &page, "Ada's backend").await;
    let (status, by_host) =
        as_key(&on_server, &token, &remove, &json!({ "person": hal_hex })).await;
    assert_ne!(
        status, 200,
        "the host does not remove an administrator: {by_host}"
    );
    assert_eq!(by_host["error"]["code"], "not_permitted", "{by_host}");

    let listed = ada_mac.command().post(&list, &json!({})).await.unwrap();
    assert!(
        row(&listed, &hal_hex).is_some_and(|hal| hal["administrator"] == true),
        "none of the refused changes took: {listed}"
    );

    assert!(
        grace_at_page.asked().is_empty(),
        "what is not Grace's to make is refused before she is asked: {:?}",
        grace_at_page.asked()
    );

    // Ada refuses her agent the removal it asks for: nothing changes.
    ada_at_page.refuse_next();
    let refused = ada_mac
        .command()
        .post(&remove, &json!({ "person": hal_hex }))
        .await
        .expect_err("what Ada refuses her agent is not done");
    assert!(
        format!("{refused:#}").contains("did not allow"),
        "{refused:#}"
    );
    let listed = ada_mac.command().post(&list, &json!({})).await.unwrap();
    assert!(row(&listed, &hal_hex).is_some(), "Hal stays: {listed}");

    // Ada's computer removes Hal and moves Grace to a role: her own device signs both.
    let removed = ada_mac
        .command()
        .post(&remove, &json!({ "person": hal_hex }))
        .await
        .expect("the owner removes an administrator");
    assert!(row(&removed, &hal_hex).is_none(), "Hal is gone: {removed}");
    let demoted = ada_mac
        .command()
        .post(&role, &json!({"person": grace_hex, "role": "editor"}))
        .await
        .expect("the owner moves an administrator to a role");
    let grace_row = row(&demoted, &grace_hex).expect("Grace stays");
    assert_eq!(grace_row["administrator"], false, "{demoted}");
    assert_eq!(grace_row["role"], "editor", "{demoted}");

    // And hands the workspace to Grace: she owns it, and Ada stays as an administrator.
    let handed = ada_mac
        .command()
        .post(&add, &json!({"person": grace_hex, "role": "owner"}))
        .await
        .expect("the owner hands the workspace on");
    assert_eq!(
        row(&handed, &grace_hex).map(|row| row["owner"].clone()),
        Some(json!(true)),
        "Grace owns it: {handed}"
    );
    let ada_row = row(&handed, &ada_hex).expect("Ada stays");
    assert_eq!(ada_row["owner"], false, "{handed}");
    assert_eq!(ada_row["administrator"], true, "{handed}");

    // Now Grace is its owner, and her computer unmakes Ada's administration.
    let unmade = grace_mac
        .command()
        .post(&role, &json!({"person": ada_hex, "role": "editor"}))
        .await
        .expect("the new owner unmakes the old one's administration");
    assert_eq!(
        row(&unmade, &ada_hex).map(|row| row["administrator"].clone()),
        Some(json!(false)),
        "{unmade}"
    );

    // Each change her computer's agent made, Ada was asked about first; Grace, for the one hers made.
    let asked = ada_at_page.asked();
    assert_eq!(
        asked
            .iter()
            .filter(|asked| *asked == "remove member")
            .count(),
        2,
        "the refused removal and the one she allowed: {asked:?}"
    );
    assert_eq!(
        asked
            .iter()
            .filter(|asked| *asked == "add a person")
            .count(),
        4,
        "two administrators, a role and the handover: {asked:?}"
    );
    assert_eq!(grace_at_page.asked(), ["add a person"]);
}

/// A page in the workspace, as a page enters it: its session, and the device over it that enters
/// the workspace its person is in.
async fn page_in(
    on_server: &OnServer,
    page: &Device,
    workspace: &str,
) -> tilo_session_client::Link {
    let session = Browser::session(&on_server.hosted.server, page, &on_server.host);
    let device = tilo_session_client::device::Device::of(
        session.clone(),
        page.cert.clone(),
        on_server.host.cert.clone(),
        &page.cert.device_label,
    );
    device
        .enter_workspace(workspace, &page.cert.device_label)
        .await
        .expect("the page enters its person's workspace");
    session
}

/// The page signs the same changes as its person's own device, as the desktop does: the owner's
/// page makes administrators and removes one, and the host relays it, so the workspace's people
/// as the host lists them change with it. An administrator's page signing the same removal is
/// refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_owners_page_removes_an_administrator_and_an_administrators_page_may_not() {
    use tilo_session_client::membership::{MemberPlace, Membership};
    let on_server = api_on_server(ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let ada_page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let workspace = own_workspace(&on_server, &ada, &ada_page).await;
    let id = WorkspaceId::parse(&workspace).unwrap();
    let list = people(&workspace, "list");
    let (grace, _) = tilo_user_identity::UserIdentity::generate("Grace", 1);
    let grace_key = grace.verifying_key().to_bytes();
    let (hal, _) = tilo_user_identity::UserIdentity::generate("Hal", 1);
    let hal_key = hal.verifying_key().to_bytes();
    let hal_hex = hex::encode(hal_key);

    let ada_tab = page_in(&on_server, &ada_page, &workspace).await;
    let ada_writes = Membership::of(ada_tab.engine(), id);
    for person in [grace_key, hal_key] {
        ada_writes
            .place(person, MemberPlace::Administrator, None)
            .await
            .expect("Ada's page makes an administrator");
        ada_writes
            .settled(&ada_tab)
            .await
            .expect("the host holds it");
    }
    // The host lists what the page signed, as soon as the page heard it held.
    let ada_mac = Holder::start(&on_server, &ada, &workspace, "Claude Code").await;
    let listed = ada_mac.command().post(&list, &json!({})).await.unwrap();
    assert!(
        row(&listed, &hal_hex).is_some_and(|hal| hal["administrator"] == true),
        "{listed}"
    );

    // Grace's page signs the removal of another administrator: not hers to make.
    let grace_page = enrol(&grace, "Grace's browser", DeviceCapabilities::member());
    let grace_tab = page_in(&on_server, &grace_page, &workspace).await;
    let grace_writes = Membership::of(grace_tab.engine(), id);
    let refused = match grace_writes.remove(hal_key).await {
        Err(why) => why,
        Ok(()) => grace_writes
            .settled(&grace_tab)
            .await
            .expect_err("the host does not take an administrator's removal of another"),
    };
    assert!(
        refused.contains("only the workspace's owner may"),
        "the page is told why: {refused}"
    );

    ada_writes
        .remove(hal_key)
        .await
        .expect("the owner's page removes an administrator");
    ada_writes
        .settled(&ada_tab)
        .await
        .expect("the host holds the removal");
    let listed = ada_mac.command().post(&list, &json!({})).await.unwrap();
    assert!(row(&listed, &hal_hex).is_none(), "Hal is gone: {listed}");
    assert!(
        row(&listed, &hex::encode(grace_key)).is_some_and(|grace| grace["administrator"] == true),
        "Grace stays: {listed}"
    );
}
