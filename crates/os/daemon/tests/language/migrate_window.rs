//! One migration, two hands on it: the person's agent calls the device's migrations
//! (`devices/<device>/migrate`) through `tilo_do`, and the person works the Migrations window
//! (`apps/tl/migrate`), whose controls call the same actions through the app. The device's engine
//! writes the migration's record as it goes, and the window shows what that record says: what
//! the agent chose and what the window changed of it, a start the device refused and why, a run
//! and what it brought, and the run taken back.
//!
//! Nothing here stands in for a part: the daemon serves its own migrations over a Thunderbird
//! profile in the test's home, the run lands in the Contacts app, and the window is the app's
//! page as a person's browser opens it.

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::{CatalogueQuery, InvocationState, application_action_target};
use tilo_server_protocol::page::{PageAction, PageAddress};
use tilo_workspace_service::app_serving::PageAct;

use crate::language::host;

/// The Migrations app.
const MIGRATE: &str = "so.tilo.migrate";
/// The package a device's migrations are published as.
const ENGINE: &str = "so.tilo.migrate.engine";

/// Where this device's migrations answer: the address of the package the device registered them
/// as, read from the registry the device hands its tilolang apps to compile their calls against,
/// as the Migrations window's own calls are.
pub(crate) fn engine(host: &host::Host) -> String {
    let packages = host.daemon().workspaces().providers().packages();
    packages
        .iter()
        .find(|package| package.app == ENGINE)
        .and_then(|package| package.resources.first())
        .map_or_else(
            || {
                let registered: Vec<&str> = packages
                    .iter()
                    .map(|package| package.app.as_str())
                    .collect();
                panic!("the device registers no migrations: {registered:?}")
            },
            |resource| resource.address.clone(),
        )
}

/// What the agent's `tilo_do` of `action` on `path` answered: its value, or what it said when
/// it did not run.
pub(crate) async fn agent_does(
    agent: &host::Agent,
    path: &str,
    action: &str,
    args: &Value,
) -> Result<Value, String> {
    let result = agent
        .tool_structured(
            "tilo_do",
            &json!({"path": format!("{path}#{action}"), "args": args}),
        )
        .await;
    let typed = &result["structuredContent"];
    if typed["status"] == "ok" {
        Ok(typed["result"].clone())
    } else {
        Err(result.to_string())
    }
}

/// The key the review lists the item of `kind` under, and its title.
fn item_of(review: &Value, kind: &str) -> (String, String) {
    let item = review["items"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|item| item["kind"] == kind)
        .unwrap_or_else(|| panic!("no {kind} in {review:#}"));
    (
        item["key"].as_str().expect("a key").to_owned(),
        item["title"].as_str().expect("a title").to_owned(),
    )
}

/// The migration of `source` the Migrations app keeps: the resource its `start` is published
/// on, as the catalogue lists it once the device wrote it.
async fn migration(host: &host::Host) -> String {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let page = host
            .dispatch()
            .query(
                &host.caller(),
                CatalogueQuery {
                    app_id: Some(MIGRATE.to_owned()),
                    action: Some("start".to_owned()),
                    ..CatalogueQuery::default()
                },
            )
            .await
            .expect("the catalogue answers")
            .expect("the catalogue lists the app");
        let found = page.rows.iter().find_map(|row| {
            let action = row.action.as_ref()?;
            (action.address.action == "start").then(|| action.address.resource.clone())
        });
        if let Some(resource) = found {
            return resource;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the device never wrote the migration: {:?}",
            page.rows
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The Migrations window on one migration, as the person's browser opens its page.
struct Window {
    host: std::sync::Arc<host::Host>,
    /// The migration it shows, or none for the list of them.
    migration: Option<String>,
}

impl Window {
    /// The page as it stands now, opened afresh.
    async fn body(&self) -> String {
        let (daemon, caller, workspace, migration) = (
            self.host.daemon(),
            self.host.caller(),
            self.host.workspace().to_owned(),
            self.migration.clone(),
        );
        tokio::task::spawn_blocking(move || {
            let pages = daemon
                .pages()
                .expect("the daemon serves its workspace's pages");
            let id = tilo_desktop_model::WorkspaceId::parse(&workspace).expect("a workspace id");
            let opened = settled(pages.open(
                id,
                &caller,
                PageAddress {
                    workspace,
                    app: "migrate".to_owned(),
                    show: migration,
                },
            ))
            .expect("the migration's page opens");
            pages.close(id, opened.page);
            opened.body
        })
        .await
        .unwrap()
    }

    /// Wait until the page shows `words`.
    async fn shows(&self, words: &str) -> String {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let body = self.body().await;
            if body.contains(words) {
                return body;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the window never shows {words:?}: {body}"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// Act on the control `find` picks on the page, as a click, Space or a screen reader does.
    async fn act(&self, what: &str, action: PageAction, find: fn(&str, &str) -> Option<u32>) {
        let (daemon, caller, workspace, migration, what) = (
            self.host.daemon(),
            self.host.caller(),
            self.host.workspace().to_owned(),
            self.migration.clone(),
            what.to_owned(),
        );
        tokio::task::spawn_blocking(move || {
            let pages = daemon
                .pages()
                .expect("the daemon serves its workspace's pages");
            let id = tilo_desktop_model::WorkspaceId::parse(&workspace).expect("a workspace id");
            let opened = settled(pages.open(
                id,
                &caller,
                PageAddress {
                    workspace,
                    app: "migrate".to_owned(),
                    show: migration,
                },
            ))
            .expect("the migration's page opens");
            let control = find(&opened.body, &what)
                .unwrap_or_else(|| panic!("the window has no {what:?}: {}", opened.body));
            settled(pages.act(
                id,
                opened.page,
                Some(PageAct {
                    id: control,
                    target: None,
                    action,
                    signed: None,
                }),
            ))
            .unwrap_or_else(|refused| panic!("{what:?} is taken: {refused:?}"));
            pages.close(id, opened.page);
        })
        .await
        .unwrap();
    }

    /// Press the button labelled `label`.
    async fn press(&self, label: &str) {
        self.act(label, PageAction::Activate, button_labelled).await;
    }

    /// Mark or clear the box of the item `title`.
    async fn check(&self, title: &str) {
        self.act(title, PageAction::Toggle, box_of).await;
    }
}

#[test]
fn an_agent_and_the_window_drive_one_migration_on_the_device() {
    host::run_with(&["migrate", "contacts"], |host| async move {
        // Installed in the workspace as a member installs them: an app keeps its own records
        // there, and the Migrations app writes its root as it opens.
        for app in [MIGRATE, "so.tilo.contacts"] {
            tilo_workspace_service::workspaces::fixtures::install_app(
                host.daemon().workspaces(),
                host.workspace(),
                app,
            )
            .await
            .unwrap_or_else(|error| panic!("the member installs {app}: {error}"));
        }
        let home = PathBuf::from(std::env::var_os("HOME").expect("the test's home"));
        tilo_migrate::thunderbird::fixture::write_sample_profile(&home);
        host.daemon().serve_migrations();
        let engine = engine(host.as_ref());
        let agent = host.agent();

        // The agent reviews Thunderbird: the device writes the migration, and the window shows
        // what the review found, nothing of it chosen yet.
        let review = agent_does(&agent, &engine, "review", &json!({"source": "thunderbird"}))
            .await
            .expect("the agent reviews Thunderbird");
        let (book, book_title) = item_of(&review, "contacts");
        let (calendar, calendar_title) = item_of(&review, "calendar");
        let resource = migration(host.as_ref()).await;
        let window = Window {
            migration: Some(resource.clone()),
            host: std::sync::Arc::clone(&host),
        };
        let body = window.shows(&book_title).await;
        assert_eq!(checked(&body, &book_title), Some(false), "{body}");
        assert!(
            button_labelled(&body, "Bring in").is_none(),
            "nothing chosen, nothing to bring in: {body}"
        );

        // A start with nothing chosen, asked of the migration as any caller asks it, is refused
        // by the device, and the window says why.
        let target = application_action_target(host.workspace(), MIGRATE, &resource, "start");
        let started = host
            .dispatch()
            .invoke(
                &host.caller(),
                host::call(&target.provider, target.address, &json!({})),
            )
            .await
            .expect("the dispatch answers")
            .expect("the start is admitted");
        let refused = ended(host.as_ref(), started.invocation).await;
        assert!(
            matches!(refused, InvocationUpdate::Failed { .. }),
            "nothing chosen is nothing to bring in: {refused:?}"
        );
        window.shows("Nothing of thunderbird is chosen").await;

        // The agent chooses the address book and the calendar; the window shows both chosen,
        // and the refusal is gone.
        agent_does(
            &agent,
            &engine,
            "choose",
            &json!({"source": "thunderbird", "items": [&book, &calendar], "only": true}),
        )
        .await
        .expect("the agent chooses");
        let body = window.shows("Bring in").await;
        assert_eq!(checked(&body, &book_title), Some(true), "{body}");
        assert_eq!(checked(&body, &calendar_title), Some(true), "{body}");
        assert!(!body.contains("Nothing of thunderbird is chosen"), "{body}");

        // The person leaves the calendar out in the window: the device keeps that, and the agent
        // reads the address book alone chosen.
        window.check(&calendar_title).await;
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let status = agent_does(&agent, &engine, "status", &json!({"source": "thunderbird"}))
                .await
                .expect("the agent reads the migration");
            if chosen(&status) == [book.clone()] {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the window's choice never reaches the device: {status:#}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let body = window.body().await;
        assert_eq!(checked(&body, &calendar_title), Some(false), "{body}");

        // The window brings the address book in: the run lands its contacts, and the window shows
        // the run complete and what it brought.
        window.press("Bring in").await;
        let body = window.shows("Complete").await;
        assert!(body.contains("2 contacts"), "{body}");
        let status = agent_does(&agent, &engine, "status", &json!({"source": "thunderbird"}))
            .await
            .expect("the agent reads the migration");
        assert_eq!(status["state"], "complete", "{status:#}");

        // The window takes the run back: what it brought leaves, and the migration stands
        // reviewed again, for the agent too.
        window.press("Undo").await;
        let body = window.shows("Reviewed").await;
        assert!(!body.contains("2 contacts"), "{body}");
        let status = agent_does(&agent, &engine, "status", &json!({"source": "thunderbird"}))
            .await
            .expect("the agent reads the migration");
        assert_eq!(status["state"], "reviewed", "{status:#}");
        drop(agent);
    });
}

/// The migration of the source whose title starts with `title` that the device offered: the
/// resource its `review` is published on, once the catalogue lists it.
async fn offered(host: &host::Host, title: &str) -> String {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let page = host
            .dispatch()
            .query(
                &host.caller(),
                CatalogueQuery {
                    app_id: Some(MIGRATE.to_owned()),
                    action: Some("review".to_owned()),
                    ..CatalogueQuery::default()
                },
            )
            .await
            .expect("the catalogue answers")
            .expect("the catalogue lists the app");
        let found = page.rows.iter().find_map(|row| {
            let action = row.action.as_ref()?;
            (action.address.action == "review" && row.resource.label.starts_with(title))
                .then(|| action.address.resource.clone())
        });
        if let Some(resource) = found {
            return resource;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the device never offered {title:?}: {:?}",
            page.rows
                .iter()
                .map(|row| row.resource.label.as_str())
                .collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// A person with nothing brought in yet opens Migrations, asks what this computer can bring, picks
// Thunderbird from the list, looks at what it holds, chooses the address book and brings it in:
// the run's result is on the page, and a notice tells them it finished. A mail account offered
// beside it asks for its server and password before anything is read, and connecting without the
// password says what it needs on the account's own page.
#[test]
fn a_person_picks_a_source_from_the_list_and_brings_it_in() {
    host::run_with(&["migrate", "contacts"], |host| async move {
        let home = PathBuf::from(std::env::var_os("HOME").expect("the test's home"));
        tilo_migrate::thunderbird::fixture::write_sample_profile(&home);
        host.daemon().serve_migrations();

        let list = Window {
            migration: None,
            host: std::sync::Arc::clone(&host),
        };
        list.shows("Bring something in").await;
        list.press("Bring something in").await;
        let body = list.shows("Thunderbird on this computer").await;
        assert!(body.contains("A mail account on its server"), "{body}");

        // The mail account asks what connecting needs, and says what is missing on its page.
        let mail = Window {
            migration: Some(offered(host.as_ref(), "A mail account").await),
            host: std::sync::Arc::clone(&host),
        };
        let body = mail.shows("Connect").await;
        assert!(
            body.contains("you@example.com"),
            "the sign-in name is asked for: {body}"
        );
        assert!(body.contains("Password or key"), "{body}");

        // Thunderbird needs no connecting: looking at it lists what it holds, nothing chosen.
        let window = Window {
            migration: Some(offered(host.as_ref(), "Thunderbird").await),
            host: std::sync::Arc::clone(&host),
        };
        window.press("Look at what it holds").await;
        let book = "Personal address book";
        let body = window.shows(book).await;
        assert_eq!(checked(&body, book), Some(false), "{body}");
        assert!(button_labelled(&body, "Bring in").is_none(), "{body}");

        window.check(book).await;
        window.shows("Bring in").await;
        window.press("Bring in").await;
        let body = window.shows("Complete").await;
        assert!(body.contains("2 contacts"), "{body}");

        // The person who started it is told it finished, wherever they are.
        let daemon = host.daemon();
        let workspaces = daemon.workspaces();
        let own = tilo_workspace_service::workspaces::person_workspace::found(workspaces)
            .await
            .expect("the person's own workspace");
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let record = tilo_workspace_service::workspaces::person_workspace::read_on::<
                tilo_workspace_service::workspaces::person_workspace::devices::Devices,
            >(
                workspaces,
                &own,
                tilo_workspace_service::workspaces::person_workspace::devices::DEVICES,
            )
            .await
            .expect("the record of their devices reads");
            let told = record
                .notices
                .values()
                .find(|notice| notice.app_id == MIGRATE && notice.title.starts_with("Thunderbird"));
            if let Some(notice) = told {
                assert!(notice.body.contains("2 contacts"), "{notice:?}");
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "nobody was told the run finished: {:?}",
                record.notices
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
}

/// The keys a status says are chosen.
fn chosen(status: &Value) -> Vec<String> {
    status["chosen"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|key| key.as_str().map(str::to_owned))
        .collect()
}

/// How invocation `id` ended, asked of the dispatch until it has.
async fn ended(host: &host::Host, id: tilo_actions::InvocationId) -> InvocationUpdate {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let state = host.dispatch().observe(&host.caller(), &id);
        if let Some(state) = state.filter(InvocationState::is_terminal) {
            return state.update;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "invocation {id:?} never ends"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// What `answer` settles to, waited for as a page's session waits.
fn settled<T>(answer: tilo_actions::remote::Pending<T>) -> T {
    answer
        .wait_timeout(Duration::from_secs(60))
        .ok()
        .flatten()
        .expect("the page answers")
}

/// Each element on `body` the page acts on: its number (`data-p`), its opening tag, and the
/// words up to the next tag.
fn elements(body: &str) -> impl Iterator<Item = (u32, &str, &str)> {
    body.split('<').filter_map(|part| {
        let (tag, words) = part.split_once('>')?;
        let (_, number) = tag.split_once("data-p=\"")?;
        let number = number.split('"').next()?.parse().ok()?;
        Some((number, tag, words.trim()))
    })
}

/// The value of `name` in the opening tag `tag`.
fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let (_, value) = tag.split_once(&format!(" {name}=\""))?;
    value.split('"').next()
}

/// The number of the button on `body` labelled `label`.
fn button_labelled(body: &str, label: &str) -> Option<u32> {
    let start = body
        .find(&format!(">{label}<"))
        .or_else(|| body.find(&format!("aria-label=\"{label}\"")))?;
    let before = &body[..start];
    let opening = before.rfind("<button")?;
    elements(&body[opening..])
        .next()
        .filter(|(_, tag, _)| tag.starts_with("button"))
        .map(|(number, _, _)| number)
}

/// The box of the item titled `title` on `body`: the checkbox named so, or the first one after
/// the title.
fn box_of(body: &str, title: &str) -> Option<u32> {
    checkbox(body, title).map(|(number, _)| number)
}

/// Whether the box of the item titled `title` is marked, when the page shows one.
fn checked(body: &str, title: &str) -> Option<bool> {
    checkbox(body, title).map(|(_, marked)| marked)
}

/// The box of the item titled `title` on `body`, and whether it is marked.
fn checkbox(body: &str, title: &str) -> Option<(u32, bool)> {
    if let Some((number, tag, _)) = elements(body)
        .find(|(_, tag, _)| is_box(tag) && attribute(tag, "aria-label") == Some(title))
    {
        return Some((number, marked(tag)));
    }
    // A box whose row the title names: the nearest before the title, else after it.
    let at = body.find(title)?;
    let before = elements(&body[..at])
        .filter(|(_, tag, _)| is_box(tag))
        .last();
    let after = || elements(&body[at..]).find(|(_, tag, _)| is_box(tag));
    let (number, tag, _) = before.or_else(after)?;
    Some((number, marked(tag)))
}

/// Whether the opening tag `tag` is a checkbox's.
fn is_box(tag: &str) -> bool {
    tag.contains("type=\"checkbox\"") || attribute(tag, "role") == Some("checkbox")
}

/// Whether the checkbox whose opening tag is `tag` is marked.
fn marked(tag: &str) -> bool {
    attribute(tag, "aria-checked") == Some("true") || tag.contains(" checked")
}
