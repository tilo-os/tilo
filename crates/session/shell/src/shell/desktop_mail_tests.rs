//! The agent's review of the day's applications, laid out on the desktop
//! through the broker as the agent: eight applications fetched from Mail's
//! test IMAP server each get a reply drafted; for each, the agent opens the
//! message and the draft in Mail windows of their own and the CV, as
//! workspace content, as a tab of the message's window in the app
//! registered for PDFs, all with `so.tilo.desktop open`, on a strip named
//! after the candidate that it lays out side by side. list-windows reads
//! the 24 windows back on their 8 strips, and one undo takes it all back.
//!
//! The apps' windows are real Mail windows on the fetched profile: the
//! desktop opens each at its resource through the window's own `open`,
//! which reaches the window through the broker. What a launched process
//! would add (its connection to the broker, naming the window it was
//! launched for) is stood in for here.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use mail_review::actions::MailEffect;
use mail_review::app::MailApp;
use mail_review::resource::MailShell;
use mail_review::review_fixture::{CANDIDATES, cv_line, cv_pdf};
use serde_json::{Value, json};
use tilo_action_broker::collab_broker_capnp::{
    InvocationEnvelope, PublishedAction, PublishedSurface,
};
use tilo_actions::InvocationUpdate;
use tilo_app::framework::testing::TestApp;
use tilo_desktop_model::commands::layout;

use super::{AGENT, Fixture, OWNER, fixture, id_of};
use crate::shell::state::{StripId, TileContent};

const MAIL: &str = "so.tilo.mail";
const PDF_VIEWER: &str = "so.tilo.pdf-viewer";

/// A package `package_id` installed under `dir`, opening `mime`.
fn install(dir: &Path, package_id: &str, mime: &[&str]) {
    use std::fmt::Write as _;
    let payload = dir.join("payloads").join(package_id);
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    let manifest = payload.join("manifest.toml");
    let mime = mime
        .iter()
        .map(|mime| format!("\"{mime}\""))
        .collect::<Vec<_>>()
        .join(", ");
    std::fs::write(
        &manifest,
        crate::shell::test_support::canonical_manifest(
            &format!("id = \"{package_id}\"\nname = \"{package_id}\"\nversion = \"1.0.0\"\nkind = \"app\""),
            &format!(
                "[entrypoint]\nbin = \"bin/app\"\ntype = \"gui\"\n\
                 [providers]\nmime = [{mime}]\next = []\n"
            ),
        ),
    )
    .unwrap();
    let registry = dir.join("registry.toml");
    let mut entries = std::fs::read_to_string(&registry).unwrap_or_default();
    write!(
        entries,
        "[[entry]]\nid = '{package_id}'\npayload_root = {:?}\nmanifest_path = {:?}\n",
        payload.display().to_string(),
        manifest.display().to_string()
    )
    .unwrap();
    std::fs::write(&registry, entries).unwrap();
}

/// A stand-in `tilo-launcher` that writes down what it was asked to start,
/// after its registration gate as the real one waits on it; the file it
/// writes to.
fn recording_launcher(dir: &Path, env: &mut tilo_test_env::ScopedEnv) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let log = dir.join("launched.log");
    let bin = dir.join("tilo-launcher");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\nhead -c 1 >/dev/null\nprintf '%s\\n' \"$*\" >> '{}'\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    env.set("TILO_LAUNCHER_BIN", &bin);
    crate::shell::launcher::set_test_execution_program(Some(bin));
    crate::shell::launcher::set_wayland_socket_path(Some(dir.join("wayland-0")));
    log
}

/// The files under `from`, copied to `to`; a socket a window listens on
/// is its own, and stays.
fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            copy_dir(&entry.path(), &target);
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// A window the desktop launched, as it came up.
struct Window {
    app: String,
    /// The broker connection its own actions are published under.
    peer: u64,
    /// The Mail window, for one of Mail's.
    mail: Option<TestApp<MailShell>>,
    /// What the desktop asks the window, delivered by the broker.
    asked: Option<tokio::sync::mpsc::UnboundedReceiver<InvocationEnvelope>>,
}

/// The desktop, its apps' windows as they come up, and the Mail profile
/// they open on.
struct Desk {
    fixture: Fixture,
    profile: PathBuf,
    windows: HashMap<u64, Window>,
    next_instance: u64,
}

impl Desk {
    /// Each window the desktop holds a place for that is not up yet comes
    /// up: the launcher names its connection the window it launched, and
    /// one of Mail's publishes the actions a Mail window on the profile
    /// does.
    fn bring_up_windows(&mut self) {
        let opening: Vec<(u64, String)> = {
            let state = self.fixture.chrome.borrow();
            let tree = state.tree();
            tree.desktop
                .workspaces
                .iter()
                .flat_map(|workspace| &workspace.strips)
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .filter_map(|tab| match &tab.tile {
                    TileContent::LocalApp { app_id, .. } => Some((tab.id, app_id.clone())),
                    _ => None,
                })
                .filter(|(tab, _)| !self.windows.contains_key(tab))
                .collect()
        };
        for (tab, app) in opening {
            self.next_instance += 1;
            let instance = self.next_instance;
            let peer = 10_000 + instance;
            self.fixture
                .chrome
                .borrow()
                .tree_mut()
                .remember_local_app_record(tab, instance);
            self.fixture
                .broker
                .bind_window_instance_for_test(peer, instance);
            let (mail, asked) = if app == MAIL {
                // Each window reads the record as the profile holds it;
                // windows of one profile in one process would contend for
                // its files, which separate processes reach through the
                // broker.
                let own = self.profile.with_extension(format!("window-{instance}"));
                copy_dir(&self.profile, &own);
                let mut mail = mail_review::review_fixture::offline_window(&own);
                let asked = self.fixture.broker.capture_invocations_for_test(peer);
                self.fixture
                    .broker
                    .publish_surface_for_test(PublishedSurface {
                        peer_id: peer,
                        app_id: MAIL.into(),
                        route: tilo_action_broker::route::CrdtRoute {
                            app_id: MAIL.into(),
                            cluster_id: self.fixture.room.clone(),
                            record_id: String::new(),
                        },
                        actions: mail
                            .actions()
                            .unwrap()
                            .iter()
                            .map(PublishedAction::from)
                            .collect(),
                        live: true,
                    });
                (Some(mail), Some(asked))
            } else {
                (None, None)
            };
            self.windows.insert(
                tab,
                Window {
                    app,
                    peer,
                    mail,
                    asked,
                },
            );
        }
    }

    /// Carry what the desktop asked each window to it: the window's `open`
    /// at a resource, which the Mail window takes as a search hit or an
    /// agent's open.
    fn answer_windows(&mut self) {
        for window in self.windows.values_mut() {
            let (Some(asked), Some(mail)) = (window.asked.as_mut(), window.mail.as_mut()) else {
                continue;
            };
            while let Ok(envelope) = asked.try_recv() {
                assert_eq!(
                    envelope.action, "open",
                    "the desktop asks a window only to open"
                );
                let arguments: Value = serde_json::from_str(
                    envelope
                        .argument_for_test()
                        .and_then(|argument| {
                            argument.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
                        })
                        .expect("open takes typed arguments"),
                )
                .unwrap();
                mail.call("open", arguments.clone())
                    .unwrap_or_else(|error| panic!("the window opens {arguments}: {error:?}"));
                mail.settle();
            }
        }
    }

    /// Invoke `action` on the desktop with `arguments` as the agent, and
    /// run the desktop and the windows it opens until it ends; its answer,
    /// which must be a success.
    fn agent(&mut self, action: &str, arguments: Value) -> Value {
        let Value::Object(arguments) = arguments else {
            panic!("arguments are an object");
        };
        let caller = self.fixture.agent();
        let request = tilo_actions::remote::InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget {
                provider: layout::PROVIDER.into(),
                address: tilo_actions::ActionAddress::new(
                    tilo_desktop_model::commands::RESOURCE,
                    action,
                ),
            },
            argument: tilo_actions::remote::InvokeArgument::Json(arguments),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
        };
        self.fixture
            .broker
            .state()
            .action_invoke(&caller, None, None, &request)
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()
            .expect("admission answers at once")
            .unwrap_or_else(|error| panic!("{action} is admitted: {error}"));
        let deadline = tilo_test_env::hang_deadline();
        loop {
            self.fixture.host.serve_desktop_jobs();
            self.bring_up_windows();
            self.answer_windows();
            if let Some(observed) = self
                .fixture
                .broker
                .state()
                .action_observe(&caller, &request.invocation)
                && observed.update.is_terminal()
            {
                return match observed.update {
                    InvocationUpdate::Succeeded { output, .. } => {
                        output.value.unwrap_or(Value::Null)
                    }
                    other => panic!("{action} did not succeed: {other:?}"),
                };
            }
            assert!(
                std::time::Instant::now() < deadline,
                "{action} ends once its windows are up"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// The window whose tab is `window`, as the desktop answered it.
    fn window(&mut self, window: &Value) -> &mut Window {
        self.windows
            .get_mut(&id_of(window))
            .unwrap_or_else(|| panic!("window {window} came up"))
    }
}

/// What a Mail window shows: the message, and the draft its composer edits.
fn mail_shows(window: &mut Window) -> (Option<String>, Option<String>) {
    window
        .mail
        .as_mut()
        .expect("a Mail window")
        .with_surface(|mail: &mut MailApp| (mail.shown_message(), mail.composing_draft()))
        .unwrap()
}

#[test]
#[serial_test::serial(process_env)]
#[allow(
    clippy::too_many_lines,
    reason = "one review, each step building on the last"
)]
fn an_agent_lays_out_eight_applications_with_their_drafts_and_cvs_and_one_undo_takes_it_back() {
    let fixture = fixture();
    let room = fixture.room.clone();
    fixture.chrome.borrow_mut().act_as(OWNER.into());
    // As the session does: a launch's grants are judged once the policy
    // they were captured at is published.
    crate::shell::action_authority::follow_tree(&fixture.chrome.borrow().tree_handle());
    let tmp = tempfile::tempdir().unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    // Mail and a PDF viewer are installed; nothing of the machine's own.
    let apps = tmp.path().join("apps");
    install(&apps, MAIL, &[]);
    install(&apps, PDF_VIEWER, &["application/pdf"]);
    let host_data = tmp.path().join("host-data");
    std::fs::create_dir_all(&host_data).unwrap();
    env.set("TILO_APPS_DATA_DIR", &apps)
        .set("XDG_DATA_DIRS", &host_data)
        .set("XDG_CONFIG_DIRS", &host_data);
    let launched = recording_launcher(tmp.path(), &mut env);
    crate::shell::host_apps::invalidate_cache();
    crate::shell::launcher::invalidate_app_cache();
    crate::shell::launcher::clear_pending_placements();
    // The workspace's content, where what an agent hands over is kept.
    let blobs = tmp.path().join("blobs.sock");
    let _media =
        tilo_runtime::profile::MediaHost::start(&tmp.path().join("content"), &blobs).unwrap();
    env.set(::launcher::services::BLOB_SOCKET_ENV, &blobs);
    let content = runtime_fs::blobs::BlobClient::connect_session(&blobs)
        .and_then(|session| session.scope(&room, tilo_action_broker::services::CONTENT_PUT_SCOPE))
        .unwrap();

    // The day's applications, fetched once and kept; each gets a reply.
    let profile = tmp.path().join("mail");
    std::fs::create_dir_all(&profile).unwrap();
    let (_imap, smtp) = mail_review::review_fixture::fetched_into(&profile);
    let mut inbox = mail_review::review_fixture::offline_window(&profile);
    let found = inbox
        .effect(&MailEffect::SearchMessages {
            query: None,
            folder: None,
            from: None,
            subject: Some("application".into()),
            since: None,
            until: None,
            has_attachment: Some(true),
        })
        .unwrap()
        .value
        .unwrap();
    let applications = found["messages"].as_array().unwrap().clone();
    assert_eq!(applications.len(), CANDIDATES.len(), "{found}");
    let agent = tilo_actions::ActionInvoker::actor(format!("agent:{AGENT}"));
    // (candidate, message, draft, CV)
    let mut reviews = Vec::new();
    for listed in &applications {
        let message = listed["id"].as_str().unwrap().to_owned();
        let read = inbox
            .call_by(
                agent.clone(),
                &format!("mail.message:{message}"),
                "read-message",
                json!({}),
            )
            .unwrap()
            .value
            .unwrap();
        let (name, _) = CANDIDATES
            .iter()
            .find(|(_, address)| read["from"].as_str().unwrap().contains(address))
            .copied()
            .unwrap_or_else(|| panic!("a candidate: {read}"));
        let reply = inbox
            .effect(&MailEffect::DraftReply {
                message: message.clone(),
                body: format!("Thank you, {name}. We'd like to talk."),
                all: None,
                notes: Some(format!("Review of {name}: strong CV.")),
            })
            .unwrap()
            .value
            .unwrap();
        let draft = reply["draft"]["item"].as_str().unwrap().to_owned();
        let cv = read["attachments"][0]["item"].as_str().unwrap().to_owned();
        let handed = inbox
            .call_by(agent.clone(), &cv, "content", json!({}))
            .unwrap()
            .value
            .unwrap();
        assert_eq!(handed["media_type"], "application/pdf", "{handed}");
        // Kept in the workspace's content, as Mail's session keeps it.
        let bytes = cv_pdf(&cv_line(name));
        content.put(&bytes, "application/pdf").unwrap();
        assert_eq!(
            handed["content"],
            tilo_actions::content_resource(&tilo_content::hash_bytes(&bytes)).unwrap()
        );
        reviews.push((
            name,
            message,
            draft,
            handed["content"].as_str().unwrap().to_owned(),
        ));
    }
    inbox.settle();
    // Mail publishes what the desktop opens: its messages and drafts.
    fixture.broker.publish_surface_for_test(PublishedSurface {
        peer_id: 9_999,
        app_id: MAIL.into(),
        route: tilo_action_broker::route::CrdtRoute {
            app_id: MAIL.into(),
            cluster_id: room.clone(),
            record_id: String::new(),
        },
        actions: inbox
            .actions()
            .unwrap()
            .iter()
            .map(PublishedAction::from)
            .collect(),
        live: false,
    });
    drop(inbox);

    let mut fixture = fixture;
    fixture.host.attach_open_door(
        crate::shell::search::providers::app_content::OpenDoor::fixture(fixture.broker.clone()),
    );
    let mut desk = Desk {
        fixture,
        profile,
        windows: HashMap::new(),
        next_instance: 0,
    };
    let before = desk.fixture.strips();
    let mut strips = Vec::new();
    for (name, message, draft, cv) in &reviews {
        let strip = desk.agent(layout::CREATE_STRIP, json!({ "name": name }))["strip"].clone();
        let shown = desk.agent(
            layout::OPEN,
            json!({ "target": format!("mail.message:{message}"), "strip": strip }),
        );
        assert_eq!(shown["app"], MAIL, "{shown}");
        assert_eq!(shown["strip"], strip, "{shown}");
        let group = shown["group"].clone();
        let drafted = desk.agent(layout::OPEN, json!({ "target": draft, "strip": strip }));
        assert_eq!(drafted["app"], MAIL, "{drafted}");
        assert_ne!(drafted["group"], group, "the draft has a window of its own");
        let opened = desk.agent(
            layout::OPEN,
            json!({ "target": cv, "place": "tab-of", "group": group }),
        );
        assert_eq!(opened["app"], PDF_VIEWER, "{opened}");
        assert_eq!(
            opened["group"], group,
            "the CV is a tab of the message's window"
        );
        let arranged = desk.agent(
            layout::ARRANGE,
            json!({ "strip": strip, "widths": [600, 400] }),
        );
        assert_eq!(
            arranged["widths"].as_array().map(Vec::len),
            Some(2),
            "{arranged}"
        );

        // Each Mail window shows what it was opened at: the message; the
        // draft, its composer editing that draft and no other.
        let message_window = desk.window(&shown["window"]);
        assert_eq!(message_window.app, MAIL);
        assert!(message_window.peer > 0);
        assert_eq!(
            mail_shows(message_window).0.as_deref(),
            Some(message.as_str())
        );
        let draft_id = draft.strip_prefix("mail.draft:").unwrap();
        assert_eq!(
            mail_shows(desk.window(&drafted["window"])).1.as_deref(),
            Some(draft_id),
            "{name}'s draft window edits {name}'s draft"
        );
        assert_eq!(desk.window(&opened["window"]).app, PDF_VIEWER);
        strips.push((StripId(id_of(&strip)), (*name).to_owned()));
    }

    let listed = desk.agent(layout::LIST_WINDOWS, json!({}));
    let windows: Vec<&Value> = listed["windows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|window| {
            strips
                .iter()
                .any(|(strip, _)| id_of(&window["strip"]) == strip.raw())
        })
        .collect();
    assert_eq!(
        windows.len(),
        24,
        "three windows for each of eight candidates: {listed}"
    );
    for ((strip, name), (_, message, draft, cv)) in strips.iter().zip(&reviews) {
        let on: Vec<&&Value> = windows
            .iter()
            .filter(|window| id_of(&window["strip"]) == strip.raw())
            .collect();
        assert_eq!(on.len(), 3, "{name}'s strip");
        assert!(
            on.iter()
                .all(|window| window["strip_name"] == name.as_str())
        );
        let resources: Vec<&str> = on
            .iter()
            .map(|window| window["resource"].as_str().unwrap_or_default())
            .collect();
        for wanted in [format!("mail.message:{message}"), draft.clone(), cv.clone()] {
            assert!(
                resources.contains(&wanted.as_str()),
                "{name}: {resources:?}"
            );
        }
    }
    let laid_out = desk.fixture.strips();
    for (strip, _) in &strips {
        let (_, _, groups, widths) = laid_out.iter().find(|(id, ..)| id == strip).unwrap();
        assert_eq!(groups.len(), 2, "the message with its CV, and the draft");
        assert_eq!(
            *widths,
            vec![
                crate::shell::state::WindowWidth::Fraction(600),
                crate::shell::state::WindowWidth::Fraction(400)
            ]
        );
    }
    let commands = std::fs::read_to_string(&launched).unwrap_or_default();
    let started = |package: &str| {
        commands
            .lines()
            .filter(|line| line.contains(&format!("--app-id {package}")))
            .count()
    };
    let deadline = tilo_test_env::hang_deadline();
    while started(MAIL) + started(PDF_VIEWER) < 24 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let undone = desk.agent(layout::UNDO, json!({}));
    assert_eq!(undone["kept"], json!([]), "{undone}");
    assert_eq!(
        undone["undone"], 40,
        "8 strips, 24 windows and 8 arrangements, as one entry"
    );
    assert_eq!(desk.fixture.strips(), before, "the desktop is as it was");
    assert!(smtp.sent().is_empty(), "nothing was sent");
    let commands = std::fs::read_to_string(&launched).unwrap_or_default();
    let count = |package: &str| {
        commands
            .lines()
            .filter(|line| line.contains(&format!("--app-id {package}")))
            .count()
    };
    assert_eq!(
        count(MAIL),
        16,
        "a Mail window for each message and draft:\n{commands}"
    );
    assert_eq!(
        count(PDF_VIEWER),
        8,
        "the PDF viewer for each CV:\n{commands}\n{:?}",
        crate::shell::launcher::drain_events()
    );
    crate::shell::launcher::set_test_execution_program(None);
    crate::shell::launcher::set_wayland_socket_path(None);
    crate::shell::launcher::clear_pending_placements();
}
