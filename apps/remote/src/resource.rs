//! Remote Desktop as a resource: the saved connections are the [`Remote`]
//! model, their fields edited at their paths (renaming, pinning, the form's
//! Save); connect / disconnect / SSH host-key prompts are effects of the live
//! surface, and a connection's credentials stay in this device's secret store.

use std::rc::Rc;

use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Fixture, LiveRow, ResourceApp, ResourceSurface,
    SurfaceCx, ViewCx,
};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_resource::items::item_resource_id;
use ui::tilo_resource::{self, ActionCx, Id, Invoke, Refusal, resource};
use ui::widget::Element;

use crate::actions::RemoteEffect;
use crate::app::RemoteApp;
pub use crate::model::Remote;
use crate::model::{AuthMethod, ConnectionDraft, ConnectionId, ConnectionKind};

/// The app's id, which its settings are kept under.
pub const REMOTE_APP_ID: &str = "so.tilo.remote";

/// Remote's Fluent catalogs, source locale first.
pub const LOCALES: &[(&str, &str)] = &[("en-US", include_str!("../locales/en-US.ftl"))];

static CATALOG: EmbeddedCatalog =
    EmbeddedCatalog::new("en-US", LOCALES, "remote-add-connection").with_system_locale();

/// The published id of a saved connection.
pub fn connection_resource_id(id: ConnectionId) -> String {
    item_resource_id::<Remote>("connections", &Remote::item(id))
}

#[resource(kind = "remote.connection-list", label = "Remote connections")]
impl Remote {
    /// Save a new connection to another machine. It signs in the
    /// protocol's first way; its authentication and group are field edits.
    #[action]
    pub fn add_connection(
        &mut self,
        cx: &mut ActionCx,
        #[param("Name")] name: String,
        #[param("Host, or a Tilo endpoint id")] host: String,
        #[param("ssh, vnc or tilo")] kind: ConnectionKind,
        #[param("Port; the protocol's own when none")] port: Option<u16>,
        #[param("User")] user: Option<String>,
    ) -> Result<(), Refusal> {
        if !kind.is_supported() {
            return Err(Refusal::new("`kind` is ssh, vnc or tilo"));
        }
        let port = port.unwrap_or_else(|| kind.default_port());
        let draft = ConnectionDraft {
            name,
            host,
            port: port.to_string(),
            user: user.unwrap_or_default(),
            kind,
            auth: AuthMethod::available_for(kind)
                .first()
                .copied()
                .unwrap_or(AuthMethod::Password),
            group: String::new(),
        };
        if let Some(problem) = draft.problems().into_iter().next() {
            return Err(Refusal::new(problem));
        }
        let id = cx.uuid();
        self.connections.push(Self::item(id), draft.build(id));
        Ok(())
    }

    /// Remove a saved connection from every device.
    #[action(destructive, item = connections)]
    pub fn delete_connection(&mut self, connection: &Id) -> Result<(), Refusal> {
        self.connections
            .remove(connection)
            .map(drop)
            .ok_or_else(|| Refusal::new("no such connection"))
    }

    /// Record that a session to the connection was opened at `at_ms`
    /// (milliseconds since the Unix epoch). The later of two concurrent
    /// records wins on every replica; nobody takes it back.
    #[action(housekeeping, item = connections)]
    pub fn mark_used(
        &mut self,
        connection: &Id,
        #[param("When the session was opened, in milliseconds since the Unix epoch")] at_ms: i64,
    ) -> Result<(), Refusal> {
        let connection = self
            .connections
            .get_mut(connection)
            .ok_or("no such connection")?;
        if connection.last_used_ms.is_none_or(|last| last < at_ms) {
            connection.last_used_ms = Some(at_ms);
        }
        Ok(())
    }
}

/// Framework app: the remote widget over [`Remote`].
pub struct RemoteShell;

/// The screenshot scenarios; a plain `--shots` is the saved-connection list.
const SCENARIOS: &[&str] = &[
    "empty",
    "pinned",
    "narrow",
    "form",
    "form-tilo",
    "form-invalid",
    "edit",
    "session-ssh",
    "session-vnc",
    "session-rdp",
    "session-tilo",
    "session-connecting",
    "session-failed",
    "session-closed",
    "confirm-connect",
    "confirm-delete",
    "confirm-ssh-trust",
];

impl ResourceApp for RemoteShell {
    type Resource = Remote;
    const ID: &'static str = REMOTE_APP_ID;
    const TITLE: &'static str = "Remote Desktop";
    const ICON: Option<&'static str> = Some("remote");

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn view(cx: &mut ViewCx<Remote>) -> Element {
        let app = RemoteApp::new();
        // A picked search hit shows its connection in the list.
        let opened = app.open_requests();
        cx.on_open(move |anchor| opened.borrow_mut().push(anchor.to_owned()));
        cx.surface(app)
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        scenario
            .and_then(|name| (name == "narrow").then_some((800, 600)))
            .unwrap_or((1280, 800))
    }

    /// What a capture shows that the document does not hold: the time "last
    /// used" is read against, which machines answered, the Tilo devices
    /// nearby.
    fn fixture(fixture: &mut Fixture<Remote>) {
        fixture.provide(crate::app::Sample::new());
    }

    /// Every scenario but `empty` shows a lived-in list of saved machines,
    /// written through the same actions the form, the pin control and an
    /// opened session use.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Remote>> {
        if name == Some("empty") {
            return Vec::new();
        }
        let mut connections = crate::app::sample_connections();
        if name == Some("session-rdp") {
            connections.push(crate::app::sample_legacy_rdp());
        }
        let saved = Remote::paths().connections;
        connections
            .iter()
            .enumerate()
            .filter_map(|(at, connection)| {
                Invoke::edit(&saved.insert_as(&Remote::item(connection.id), at, connection)).ok()
            })
            .collect()
    }
}

impl ResourceSurface<Remote, RemoteEffect> for RemoteApp {
    fn present(&mut self, state: &Rc<Remote>) {
        self.adopt_resource_state(&state.list());
    }

    fn attach(&mut self, cx: SurfaceCx<Remote>) {
        self.attach_framework(&cx);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        RemoteApp::stage(self, scenario);
    }

    fn effect(&mut self, effect: RemoteEffect, cx: &mut EffectCx<Remote, Self>) -> EffectAnswer {
        self.answer_effect(effect, cx)
    }

    /// Text reaches only a connected SSH terminal.
    fn effects(&self) -> Vec<EffectStatus> {
        if self.can_send_text() {
            return Vec::new();
        }
        vec![EffectStatus::unavailable(
            RemoteEffect::SEND_TEXT,
            "terminal_not_connected",
            "Text input requires a connected SSH terminal",
        )]
    }

    /// Each saved connection's settings and connect, and the open
    /// session.
    fn live_rows(&self) -> Vec<LiveRow> {
        self.live_connection_rows()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Connection;
    use ui::prelude::TestApp;
    use ui::tilo_actions::{ConfirmationPolicy, OperationKind};
    use ui::tilo_resource::{Atom, Replica, Resource as _};
    use uuid::Uuid;

    fn sample(id: ConnectionId, name: &str) -> Connection {
        Connection {
            id,
            name: name.into(),
            host: Atom("10.0.0.9".into()),
            port: 22,
            user: Atom("admin".into()),
            kind: ConnectionKind::Ssh,
            auth: AuthMethod::PublicKey,
            last_used_ms: None,
            group: Atom("lab".into()),
            favorite: false,
        }
    }

    fn save(remote: &mut Replica<Remote>, connection: &Connection) {
        let at = remote.state().unwrap().connections.len();
        let saved = Remote::paths().connections;
        remote
            .apply(
                Invoke::edit(&saved.insert_as(&Remote::item(connection.id), at, connection))
                    .unwrap(),
            )
            .unwrap();
    }

    fn only(remote: &Replica<Remote>) -> Connection {
        let list = remote.state().unwrap().list();
        assert_eq!(list.len(), 1);
        list.into_iter().next().unwrap()
    }

    /// Every field round-trips; an edit writes only what it changes, so a
    /// pin and a recorded use survive the form's Save.
    #[test]
    fn connections_round_trip_and_edits_write_only_their_fields() {
        for kind in ConnectionKind::all() {
            let mut remote = Replica::<Remote>::new();
            let mut connection = sample(Uuid::new_v4(), "Build Box");
            connection.kind = kind;
            connection.auth = AuthMethod::available_for(kind)[0];
            if kind == ConnectionKind::Tilo {
                connection.host = Atom(iroh_base::SecretKey::generate().public().to_string());
                connection.port = 0;
            }
            save(&mut remote, &connection);
            assert_eq!(only(&remote), connection, "{kind:?} round-trips");
        }

        let mut remote = Replica::<Remote>::new();
        let id = Uuid::new_v4();
        let connection = sample(id, "Box");
        save(&mut remote, &connection);
        let paths = Remote::paths().connections.item(&Remote::item(id));
        remote
            .apply(Invoke::edit(&paths.favorite.set(true)).unwrap())
            .unwrap();
        remote
            .apply(Remote::MARK_USED.with(remote::MarkUsed {
                connection: Remote::item(id),
                at_ms: 5_000,
            }))
            .unwrap();
        let mut edited = connection.clone();
        edited.name = "Box (renamed)".into();
        edited.host = Atom("10.0.0.10".into());
        let edits = paths.path().edits(&connection, &edited);
        remote
            .apply(Invoke::group(
                edits.iter().map(|edit| Invoke::edit(edit).unwrap()),
            ))
            .unwrap();
        let back = only(&remote);
        assert_eq!(back.name, "Box (renamed)");
        assert_eq!(back.host.0, "10.0.0.10");
        assert!(back.favorite, "the edit kept the pin");
        assert_eq!(back.last_used_ms, Some(5_000), "and the recorded use");
        assert!(
            remote
                .apply(Invoke::edit(&paths.last_used_ms.set(Some(1))).unwrap())
                .is_err(),
            "only opening a session records a use"
        );
    }

    /// The latest use wins; deleting asks first; a verb on a connection
    /// that is gone is refused and changes nothing.
    #[test]
    fn uses_deletion_and_missing_connections() {
        let mut remote = Replica::<Remote>::new();
        let id = Uuid::new_v4();
        save(&mut remote, &sample(id, "Box"));
        let used = |at_ms| {
            Remote::MARK_USED.with(remote::MarkUsed {
                connection: Remote::item(id),
                at_ms,
            })
        };
        remote.apply(used(2_000)).unwrap();
        remote.apply(used(1_000)).unwrap();
        assert_eq!(
            only(&remote).last_used_ms,
            Some(2_000),
            "a late, older record does not move last used back"
        );

        let delete = || {
            Remote::DELETE_CONNECTION.with(remote::DeleteConnection {
                connection: Remote::item(id),
            })
        };
        assert!(remote.apply(delete()).is_err(), "deleting asks first");
        remote.apply_confirmed(delete()).unwrap();
        assert!(remote.state().unwrap().list().is_empty());
        assert!(remote.apply(used(3_000)).is_err());
        assert!(remote.apply_confirmed(delete()).is_err());
    }

    /// Whatever writes a connection is held to what a saved connection
    /// is: a name, a host, a usable port and a supported sign-in.
    #[test]
    fn the_invariant_refuses_unusable_profiles() {
        let mut remote = Replica::<Remote>::new();
        let id = Uuid::new_v4();
        save(&mut remote, &sample(id, "Box"));
        let paths = Remote::paths().connections.item(&Remote::item(id));
        for refused in [
            Invoke::edit(&paths.name.replace("  ")).unwrap(),
            Invoke::edit(&paths.host.set(Atom(String::new()))).unwrap(),
            Invoke::edit(&paths.port.set(0)).unwrap(),
            Invoke::edit(&paths.auth.set(AuthMethod::DeviceIdentity)).unwrap(),
        ] {
            assert!(remote.apply(refused).is_err());
        }
        assert_eq!(only(&remote), sample(id, "Box"));
    }

    /// Two devices editing the list at once converge: both new machines
    /// land, renames merge as text, the pin and the port both hold.
    #[test]
    fn concurrent_edits_converge() {
        let mut ours = Replica::<Remote>::new();
        let id = Uuid::new_v4();
        save(&mut ours, &sample(id, "Box"));
        let mut theirs = ours.fork();
        let paths = Remote::paths().connections.item(&Remote::item(id));
        save(&mut ours, &sample(Uuid::new_v4(), "A"));
        save(&mut theirs, &sample(Uuid::new_v4(), "B"));
        ours.apply(Invoke::edit(&paths.name.splice(3, 0, " one")).unwrap())
            .unwrap();
        theirs
            .apply(Invoke::edit(&paths.name.splice(0, 0, "Lab ")).unwrap())
            .unwrap();
        ours.apply(Invoke::edit(&paths.favorite.set(true)).unwrap())
            .unwrap();
        theirs
            .apply(Invoke::edit(&paths.port.set(2222)).unwrap())
            .unwrap();
        ours.sync_with(&mut theirs).unwrap();
        assert_eq!(ours.state().unwrap(), theirs.state().unwrap());
        let list = ours.state().unwrap().list();
        let mut names: Vec<_> = list.iter().map(|c| c.name.clone()).collect();
        names.sort();
        assert_eq!(names, ["A", "B", "Lab Box one"]);
        let merged = list.iter().find(|c| c.id == id).unwrap();
        assert!(merged.favorite && merged.port == 2222);
    }

    /// Adding validates the profile and mints its id.
    #[test]
    fn an_agent_adds_a_connection_in_one_call() {
        let mut app = TestApp::<RemoteShell>::new().unwrap();
        app.call(
            "add-connection",
            serde_json::json!({"name": "Lab", "host": "lab.local", "kind": "ssh"}),
        )
        .unwrap();
        assert!(
            app.call(
                "add-connection",
                serde_json::json!({"name": "Lab", "host": "friendly", "kind": "tilo"}),
            )
            .is_err(),
            "a Tilo connection needs an exact endpoint id"
        );
        let list = app.state().list();
        assert_eq!(list.len(), 1);
        let lab = &list[0];
        assert_eq!(
            (lab.port, lab.kind, lab.auth),
            (22, ConnectionKind::Ssh, AuthMethod::Password)
        );
    }

    /// Each saved connection is published with its settings, its connect,
    /// and the verbs bound to it; an agent works it one call at a time.
    #[test]
    fn an_agent_works_each_connection_in_one_call() {
        let mut app = TestApp::<RemoteShell>::new().unwrap();
        let lab = sample(Uuid::new_v4(), "Lab");
        let saved = Remote::paths()
            .connections
            .insert_as(&Remote::item(lab.id), 0, &lab);
        app.call("field-edit", serde_json::json!({ "edit": saved }))
            .unwrap();
        assert_eq!(app.state().list(), std::slice::from_ref(&lab));
        app.settle();

        let item = connection_resource_id(lab.id);
        let resources = app.harness().resources();
        let root = resources
            .iter()
            .find(|resource| resource.kind == Remote::KIND)
            .unwrap();
        let published = root
            .children
            .iter()
            .find(|child| child.id.as_str() == item)
            .expect("the connection is published");
        assert_eq!(published.label, "Lab");
        let ids: Vec<&str> = published.actions.iter().map(|a| a.id.as_str()).collect();
        for id in [
            "edit-connection",
            "connect",
            "delete-connection",
            "mark-used",
        ] {
            assert!(ids.contains(&id), "{id} on the connection: {ids:?}");
        }
        let connect = published
            .actions
            .iter()
            .find(|a| a.id == "connect")
            .unwrap();
        assert_eq!(connect.kind, OperationKind::Effect);
        let delete = published
            .actions
            .iter()
            .find(|a| a.id == "delete-connection")
            .unwrap();
        assert_ne!(delete.contract.confirmation, ConfirmationPolicy::None);

        let pin = Remote::paths()
            .connections
            .item(&Remote::item(lab.id))
            .favorite
            .set(true);
        app.call("field-edit", serde_json::json!({ "edit": pin }))
            .unwrap();
        assert!(app.state().list()[0].favorite);
        assert!(app.undo(), "a pin is one undo step");
        assert!(!app.state().list()[0].favorite);

        app.call_on(&item, "delete-connection", serde_json::json!({}))
            .expect_err("deleting asks first");
        app.call_confirmed_on(&item, "delete-connection", serde_json::json!({}))
            .unwrap();
        assert!(app.state().list().is_empty());
    }

    /// What an agent does to the saved connections is history the person
    /// reads as Remote's own sentences.
    #[test]
    fn history_entries_render_as_the_apps_sentences() {
        use ui::tilo_actions::{ActionInvoker, ActorKind};

        let mut app = TestApp::<RemoteShell>::new().unwrap();
        let agent = ActionInvoker::actor("agent:claude")
            .with_kind(ActorKind::Agent)
            .acting_for("ada");
        let lab = sample(Uuid::new_v4(), "Lab");
        let id = Remote::item(lab.id);
        app.call_by(
            agent.clone(),
            Remote::KIND,
            "field-edit",
            serde_json::json!({ "edit": Remote::paths().connections.insert_as(&id, 0, &lab) }),
        )
        .unwrap();
        let paths = Remote::paths().connections.item(&id);
        for edit in [
            paths.favorite.set(true),
            paths.port.set(2222),
            paths.user.set(Atom("root".into())),
            paths.auth.set(AuthMethod::Password),
            paths.name.replace("Lab box"),
        ] {
            app.call_by(
                agent.clone(),
                Remote::KIND,
                "field-edit",
                serde_json::json!({ "edit": edit }),
            )
            .unwrap();
        }
        let item = connection_resource_id(app.state().list()[0].id);
        app.call_by(
            agent,
            &item,
            "mark-used",
            serde_json::json!({ "at_ms": 1_700_000_000_000_i64 }),
        )
        .unwrap();

        let catalogue = tilo_history::Catalogue::new("en-US", &[LOCALES[0].1]);
        let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
        let entries: Vec<_> = app
            .versions()
            .into_iter()
            .filter_map(|version| version.context)
            .filter(|entry| {
                entry
                    .invoker
                    .as_ref()
                    .is_some_and(|invoker| invoker.actor == "agent:claude")
            })
            .collect();
        let sentences: Vec<String> = entries.iter().map(|entry| reader.sentence(entry)).collect();
        assert_eq!(
            sentences,
            [
                "Saved a connection",
                "Pinned Lab",
                "Set the port of Lab to 2222",
                "Set the user of Lab to root",
                "Set Lab to sign in with a password",
                "Renamed Lab",
                "Connected to Lab box",
            ]
        );
        let invoker = entries[0].invoker.as_ref().unwrap();
        assert_eq!(
            (invoker.kind, invoker.on_behalf_of.as_deref()),
            (ActorKind::Agent, Some("ada"))
        );
    }
}
