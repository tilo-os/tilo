//! A record an app keeps for its whole workspace publishes its verbs beside
//! the app's own resource, so a member changes it through the published
//! action — whether the workspace held it before the window opened or
//! delivered it after, as a hosted workspace does: the record is on its way
//! when the window builds, and what it publishes then is what a member and
//! their agent reach it by.

use tilo_app::prelude::*;
use tilo_app_test_host::TestApp;

/// The workspace the window is in.
const WORKSPACE: &str = "2f0b8f0e-6a7c-4a83-9d4f-1f1f2b3c4d5e";

/// The record's document, named as Terminal's machine secrets are.
const SECRETS: &str = "uuid:587bfde5-3070-4a56-9b1b-bb5abd3a599b";

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Session {
    pub note: String,
}

#[resource(
    kind = "test.terminal-session",
    app = "so.tilo.test-terminal",
    catalogue = "tests/locales"
)]
impl Session {}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Secrets {
    pub secrets: Vec<Secret>,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Secret {
    pub variable: String,
    pub item: String,
}

#[resource(
    kind = "terminal.machine-secrets",
    app = "so.tilo.test-terminal",
    catalogue = "tests/locales"
)]
impl Secrets {
    /// Add secret.
    #[action]
    pub fn add_secret(&mut self, variable: String, item: String) {
        self.secrets.push(Secret { variable, item });
    }

    /// Remove secret.
    #[action(destructive)]
    pub fn remove_secret(&mut self, variable: String) {
        self.secrets.retain(|secret| secret.variable != variable);
    }
}

struct Terminal;

impl ResourceApp for Terminal {
    type Resource = Session;
    const ID: &'static str = "so.tilo.test-terminal";
    const TITLE: &'static str = "Terminal";

    fn view(cx: &mut ViewCx<Session>) -> Element {
        Box::new(cx.text(|session| session.note.clone()))
    }

    fn resources(resources: &mut Resources<'_>) {
        resources.workspace_record::<Secrets>(SECRETS);
    }
}

fn record<'a>(
    resources: &'a [tilo_actions::ResourceSpec],
    kind: &str,
) -> Option<&'a tilo_actions::ResourceSpec> {
    resources
        .iter()
        .flat_map(|resource| std::iter::once(resource).chain(resource.children.iter()))
        .find(|resource| resource.kind == kind)
}

#[test]
fn the_workspace_records_verbs_are_published_beside_the_apps_own() {
    let mut app = TestApp::<Terminal>::in_workspace(WORKSPACE).unwrap();
    let resources = app.harness().resources();
    let secrets = record(&resources, "terminal.machine-secrets")
        .expect("the machine secrets record is published");
    let ids: Vec<&str> = secrets
        .actions
        .iter()
        .map(|spec| spec.id.as_str())
        .collect();
    assert!(ids.contains(&"add-secret"), "{ids:?}");
    assert!(ids.contains(&"remove-secret"), "{ids:?}");
}

#[test]
fn a_members_change_through_the_published_action_reaches_the_record() {
    let mut app = TestApp::<Terminal>::in_workspace(WORKSPACE).unwrap();
    app.call_on(
        "terminal.machine-secrets",
        "add-secret",
        serde_json::json!({
            "variable": "GITHUB_TOKEN",
            "item": "vault.item:0123456789abcdef0123456789abcdef",
        }),
    )
    .unwrap();
    assert_holds_the_secret(&app);
}

#[test]
fn the_record_publishes_its_verbs_once_the_workspace_delivers_it() {
    let mut app = TestApp::<Terminal>::awaiting_the_workspace(WORKSPACE, Terminal::ID).unwrap();
    assert!(
        record(&app.harness().resources(), "terminal.machine-secrets").is_none(),
        "the workspace may hold it: the window opens without it"
    );
    app.workspace_delivers_record::<Secrets>(Terminal::ID, SECRETS, |_| Ok(()))
        .unwrap();
    for _ in 0..4 {
        app.frame();
    }
    let resources = app.harness().resources();
    let secrets = record(&resources, "terminal.machine-secrets")
        .expect("the record is published once the workspace delivers it");
    let ids: Vec<&str> = secrets
        .actions
        .iter()
        .map(|spec| spec.id.as_str())
        .collect();
    assert!(ids.contains(&"add-secret"), "{ids:?}");
    assert!(ids.contains(&"remove-secret"), "{ids:?}");
    app.call_on(
        "terminal.machine-secrets",
        "add-secret",
        serde_json::json!({
            "variable": "GITHUB_TOKEN",
            "item": "vault.item:0123456789abcdef0123456789abcdef",
        }),
    )
    .unwrap();
    assert_holds_the_secret(&app);
}

/// The record holds what the member added through the published action.
fn assert_holds_the_secret(app: &TestApp<Terminal>) {
    let id = tilo_app::resource::DocumentId::parse(SECRETS).unwrap();
    let held = app
        .read_document_of(&id, |doc| {
            <Secrets as tilo_app::tilo_resource::Resource>::read(doc).map(|state| state.secrets)
        })
        .expect("the document is held here")
        .unwrap();
    assert_eq!(
        held,
        vec![Secret {
            variable: "GITHUB_TOKEN".to_owned(),
            item: "vault.item:0123456789abcdef0123456789abcdef".to_owned(),
        }]
    );
}
