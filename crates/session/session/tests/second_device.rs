//! A person's second device in a shared workspace: Bob's phone joins one someone else's site
//! serves by an invitation of its own, or enters the one his laptop serves from his list, as his
//! further device. What is written there reaches the phone as it is
//! written and is still there after a restart, and what the phone writes reaches everyone else.
//! Each step reads what the apps show through the actions they publish, so it reads the phone's own
//! copy of the workspace, the one its apps read.

#![cfg(target_os = "linux")]

use std::time::{Duration, Instant};

use serde_json::json;
use tilo_server_protocol::workspaces::Role;

use crate::closed_tasks::{APP, titles, tracker};
use crate::common::people::{Device, Person, Site, share_the_fleet, shared_invitation};

/// How long a write may take to show on another device. A person waiting on a message notices
/// more than this; the suite's hang bound is for a write that never arrives.
const PROMPTLY: Duration = Duration::from_secs(5);

/// The provider and id of Tasks' `operation` as `device` lists it, read page by page: a device
/// with Chat open lists more actions than one page holds.
fn tasks_action(device: &Device, operation: &str) -> Option<(String, String)> {
    let mut cursor = String::new();
    loop {
        let page = device.ask(&json!({"op": "actions_enumerate", "cursor": cursor}));
        let found = page["answer"]["actions"].as_array().and_then(|rows| {
            rows.iter().find(|row| {
                row["application"] == APP
                    && row["operation"] == operation
                    && row["provider"]
                        .as_str()
                        .is_some_and(|provider| provider.parse::<u64>().is_ok())
            })
        });
        if let Some(row) = found {
            return Some((
                row["provider"].as_str()?.to_owned(),
                row["id"].as_str()?.to_owned(),
            ));
        }
        page["answer"]["next_cursor"]
            .as_str()?
            .clone_into(&mut cursor);
    }
}

/// Invoke Tasks' `operation` on `device` with `args` once its window offers it, the person's
/// confirmation given where it asks first: what the window answered.
fn tasks_act(device: &Device, operation: &str, args: &serde_json::Value) -> serde_json::Value {
    let deadline = tilo_test_env::hang_deadline();
    let (provider, id) = loop {
        if let Some(found) = tasks_action(device, operation) {
            break found;
        }
        assert!(
            Instant::now() < deadline,
            "Tasks on {} never offers {operation}; its windows: {}\n{}",
            device.label,
            device.ask(&json!({"op": "windows"})),
            device.diagnostic()
        );
        std::thread::sleep(Duration::from_millis(200));
    };
    let answer = device.act_wait(&provider, &id, args);
    if answer["err"]["code"].as_str() == Some("needs_confirmation") {
        return device.act_confirming(&provider, &id, args);
    }
    answer
}

/// Make an issue titled `title` in the workspace's tracker on `device`, as a person does: in Tasks'
/// window, through the `create` it publishes.
fn make_issue(device: &Device, title: &str) {
    device.open(APP);
    device.wait_for_window(APP);
    let made = tasks_act(device, "create", &json!({ "title": title }));
    assert!(
        made.get("err").is_none(),
        "{} made no issue: {made}",
        device.label
    );
}

/// Wait until `device` reads every issue of `wanted` in the tracker, and answer how long it took.
fn wait_for_issues(device: &Device, wanted: &[&str]) -> Duration {
    let began = Instant::now();
    let deadline = tilo_test_env::hang_deadline();
    let mut after = None;
    loop {
        let (list, revision) = tracker(device, after.as_deref());
        let held = titles(&list);
        if wanted
            .iter()
            .all(|title| held.iter().any(|held| held == title))
        {
            return began.elapsed();
        }
        assert!(
            Instant::now() < deadline,
            "{} reads the issues {held:?}, not {wanted:?}\n{}",
            device.label,
            device.diagnostic()
        );
        after = Some(revision);
    }
}

/// Wait until Tasks' window on `device` shows the issue titled `title`, as its own outline reads
/// what it draws: the issues it loaded from the device's own copy of the workspace. Answers how
/// long it took.
fn wait_for_tasks_showing(device: &Device, title: &str) -> Duration {
    device.open(APP);
    device.wait_for_window(APP);
    // Starting the app is not the write travelling: a window stands before its app answers, so
    // the app's first view is read before the clock starts. What is timed is the issue reaching a
    // window already reading, never the app's own start on a busy machine.
    let started = tasks_act(device, "outline", &json!({}));
    assert!(
        started.get("err").is_none(),
        "{} reads Tasks' view: {started}\n{}",
        device.label,
        device.diagnostic()
    );
    let began = Instant::now();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let outline = tasks_act(device, "outline", &json!({}));
        if outline.to_string().contains(title) {
            return began.elapsed();
        }
        assert!(
            Instant::now() < deadline,
            "Tasks on {} never shows {title:?}; it shows {outline}\nits Tasks actions: {:?}\n{}",
            device.label,
            device.actions()["answer"]["actions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|row| row["application"] == APP)
                .map(|row| row["operation"].to_string())
                .collect::<Vec<_>>(),
            device.diagnostic()
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Bring `workspace` to the front on `device`, as the person picking it on the shell does: a
/// workspace that arrived from the person's list stands behind the one they are in.
fn show(device: &Device, workspace: &str) {
    let shown = device.act(
        "so.tilo.session",
        "desktop:chrome\u{1f}switch-workspace",
        &json!({ "workspace": workspace }),
    );
    assert!(
        shown.get("err").is_none(),
        "{} shows {workspace}: {shown}",
        device.label
    );
    device.wait_showing(workspace);
}

/// Wait until `device`'s Chat shows `texts` from its conversation with `peer`, and answer how long
/// it took.
fn wait_for_texts(device: &Device, peer: &str, texts: &[&str]) -> Duration {
    let began = Instant::now();
    device.chat().wait_for_texts(peer, texts);
    began.elapsed()
}

fn promptly(what: &str, took: Duration) {
    eprintln!("{what}: {took:?}");
    assert!(took <= PROMPTLY, "{what} took {took:?}");
}

/// Ada and Bob's two devices in the Studio, Chat open on all three, each knowing the other person.
struct Studio {
    ada_laptop: Device,
    bob_laptop: Device,
    bob_phone: Device,
    ada_key: String,
    bob_key: String,
    workspace: String,
    /// The site hosting the workspace, when one does: kept until the devices stopped.
    _site: Option<Site>,
}

impl Studio {
    /// The Studio on a site, each of Ada's laptop, Bob's laptop and Bob's phone joined by an
    /// invitation of its own.
    fn at_the_site() -> Self {
        let ada = Person::new("Ada");
        let bob = Person::new("Bob");
        let site = Site::start(&ada, "Studio");
        site.publish(Role::Editor);
        let mut ada_laptop = ada.device("Ada's laptop");
        let mut bob_laptop = bob.device("Bob's laptop");
        let mut bob_phone = bob.device("Bob's phone");
        share_the_fleet(&bob, &[&bob_laptop, &bob_phone]);
        for device in [&ada_laptop, &bob_laptop, &bob_phone] {
            device.install_chat();
            device.install_tasks();
        }
        ada_laptop.join(&site.admit(ada.public));
        bob_laptop.join(&site.admit(bob.public));
        for device in [&ada_laptop, &bob_laptop] {
            device.wait_holding(&site.workspace);
            device.wait_showing(&site.workspace);
        }
        bob_phone.join(&site.admit(bob.public));
        let workspace = site.workspace.clone();
        Self::chatting(
            &ada,
            &bob,
            [ada_laptop, bob_laptop, bob_phone],
            workspace,
            Some(site),
        )
    }

    /// Bob's own workspace, the one his laptop made and serves: Ada's laptop joins it by the
    /// invitation his laptop hands out, and his phone, never invited, enters it from his list.
    fn at_bobs_laptop() -> Self {
        let ada = Person::new("Ada");
        let bob = Person::new("Bob");
        let mut ada_laptop = ada.device("Ada's laptop");
        let mut bob_laptop = bob.device("Bob's laptop");
        let mut bob_phone = bob.device("Bob's phone");
        share_the_fleet(&bob, &[&bob_laptop, &bob_phone]);
        for device in [&ada_laptop, &bob_laptop, &bob_phone] {
            device.install_chat();
            device.install_tasks();
        }
        bob_laptop.start();
        let workspace = tilo_test_env::wait_for("Bob's laptop showing its workspace", || {
            bob_laptop.showing().filter(|shown| !shown.is_empty())
        });
        ada_laptop.join(&shared_invitation(&bob_laptop, "editor", ada.public));
        ada_laptop.wait_holding(&workspace);
        ada_laptop.wait_showing(&workspace);
        bob_phone.start();
        Self::chatting(
            &ada,
            &bob,
            [ada_laptop, bob_laptop, bob_phone],
            workspace,
            None,
        )
    }

    /// The three devices once the phone holds `workspace` and shows it, Chat open on each and
    /// each person a contact of the other.
    fn chatting(
        ada: &Person,
        bob: &Person,
        [ada_laptop, bob_laptop, bob_phone]: [Device; 3],
        workspace: String,
        site: Option<Site>,
    ) -> Self {
        bob_phone.wait_holding(&workspace);
        show(&bob_phone, &workspace);
        for device in [&ada_laptop, &bob_laptop, &bob_phone] {
            device.open("so.tilo.chat");
            device.wait_for_window("so.tilo.chat");
        }
        let ada_key = hex::encode(ada.public);
        let bob_key = hex::encode(bob.public);
        for device in [&bob_laptop, &bob_phone] {
            device.chat().wait_for_contact(&ada_key);
        }
        ada_laptop.chat().wait_for_contact(&bob_key);
        Self {
            ada_laptop,
            bob_laptop,
            bob_phone,
            ada_key,
            bob_key,
            workspace,
            _site: site,
        }
    }

    /// Ada writes a message and an issue, which reach Bob's phone as they are written and are
    /// still there after it restarts; then the phone writes, which reaches Ada and Bob's laptop.
    fn the_phone_follows_the_workspace(mut self) {
        let Self {
            ada_key, bob_key, ..
        } = &self;
        let (ada_key, bob_key) = (ada_key.clone(), bob_key.clone());
        let workspace = self.workspace.clone();
        self.bob_phone.chat().open(&ada_key);
        self.ada_laptop.chat().send(&bob_key, "hello Bob");
        promptly(
            "Ada's message on Bob's phone",
            wait_for_texts(&self.bob_phone, &ada_key, &["hello Bob"]),
        );
        make_issue(&self.ada_laptop, "Book the ferry");
        promptly(
            "Ada's issue on Bob's phone",
            wait_for_issues(&self.bob_phone, &["Book the ferry"]),
        );
        for device in [&self.bob_laptop, &self.bob_phone] {
            promptly(
                &format!("Ada's issue in Tasks on {}", device.label),
                wait_for_tasks_showing(device, "Book the ferry"),
            );
        }

        self.bob_phone.stop();
        self.bob_phone.start();
        self.bob_phone.wait_holding(&workspace);
        show(&self.bob_phone, &workspace);
        self.bob_phone.open("so.tilo.chat");
        self.bob_phone.wait_for_window("so.tilo.chat");
        self.bob_phone
            .chat()
            .wait_for_texts(&ada_key, &["hello Bob"]);
        wait_for_issues(&self.bob_phone, &["Book the ferry"]);
        promptly(
            "Ada's issue in Tasks on the restarted phone",
            wait_for_tasks_showing(&self.bob_phone, "Book the ferry"),
        );

        self.bob_phone.chat().send(&ada_key, "from my phone");
        promptly(
            "the phone's answer on Ada's laptop",
            wait_for_texts(&self.ada_laptop, &bob_key, &["hello Bob", "from my phone"]),
        );
        promptly(
            "the phone's answer on Bob's laptop",
            wait_for_texts(&self.bob_laptop, &ada_key, &["hello Bob", "from my phone"]),
        );
        make_issue(&self.bob_phone, "Pack the tent");
        for device in [&self.ada_laptop, &self.bob_laptop] {
            promptly(
                &format!("the phone's issue on {}", device.label),
                wait_for_issues(device, &["Book the ferry", "Pack the tent"]),
            );
        }
        // The phone reads back what it wrote: an app's write reaches the workspace's copy its
        // own reads answer from.
        promptly(
            "the phone's issue on the phone that wrote it",
            wait_for_tasks_showing(&self.bob_phone, "Pack the tent"),
        );
        self.ada_laptop.chat().send(&bob_key, "seen it");
        promptly(
            "Ada's message on the restarted phone",
            wait_for_texts(
                &self.bob_phone,
                &ada_key,
                &["hello Bob", "from my phone", "seen it"],
            ),
        );
        for device in [
            &mut self.ada_laptop,
            &mut self.bob_laptop,
            &mut self.bob_phone,
        ] {
            device.stop();
        }
    }
}

/// Bob's phone joins the Studio by an invitation of its own, as his laptop did.
#[test]
fn a_persons_phone_joined_by_its_own_invitation_follows_the_workspace() {
    Studio::at_the_site().the_phone_follows_the_workspace();
}

/// Bob's own workspace, served by his laptop: his phone enters it from his list.
#[test]
fn a_persons_phone_enters_the_workspace_their_laptop_serves_and_follows_it() {
    Studio::at_bobs_laptop().the_phone_follows_the_workspace();
}
