//! Tilo links opened in place.
//!
//! Every URI the shell's own widgets open (`tilo_dialogs::open_uri`, a
//! detected link, `ui::widget::try_open_external_url`) passes through
//! [`LinkOpener`] first. A link in Tilo's grammar
//! (`docs/architecture/web-routing.md`) that names one of this person's
//! workspaces opens there, through the same launcher the `open` action uses,
//! instead of loading the web page in a browser. Anything else, including a
//! link to a room this person is not in, goes to the browser unchanged, where
//! the page's own admission applies.
//!
//! A link that names a resource, a span of its text or a view then shows
//! exactly that: see [`reveal`].
//!
//! The URI stays in process: a secret in its fragment never reaches a command
//! line, and logs only ever see [`Link::public`].

mod reveal;

use std::sync::{Arc, RwLock};

use tilo_actions::remote::{ActionHost, Caller};
use tilo_dialogs::UriOpener;
use tilo_reference::link::{Link, Mode, RecordSegment, Scope, WebEntries, WebEntry};

pub use reveal::{LinkDoor, PUBLISH_TIMEOUT, Reveal, UNAVAILABLE, reveal, view_invocation};

use crate::shell::state::WorkspaceTreeHandle;
use tilo_action_broker::collab_broker::{AppActionBroker, WeakAppActionBroker};
use tilo_lock::RwLockRecover as _;

/// Where Tilo's public web entry serves links. Hosts announce their own
/// entries in their catalogue locators; those join this list once the
/// session tracks them.
const TILO_ENTRY: &str = "https://tilo.so/";

/// What a link in Tilo's grammar asks the session to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkOpen {
    /// The workspace room, or `None` for `/me`: the person's own room.
    pub room: Option<tilo_desktop_model::RoomId>,
    /// The app to open, as its package id.
    pub app: Option<String>,
    /// The record to open, `None` for the app's primary record.
    pub record: Option<String>,
    /// Full-tab links ask for the window maximized.
    pub maximized: bool,
}

impl LinkOpen {
    fn of(link: &Link) -> Self {
        let room = match &link.target.scope {
            Scope::Own => None,
            Scope::Room { room, .. } => Some((*room).into()),
        };
        let record = match &link.target.record {
            Some(RecordSegment::Id(id)) => Some(id.clone()),
            Some(RecordSegment::Primary) | None => None,
        };
        Self {
            room,
            app: link.target.app.as_ref().map(|app| app.as_str().to_owned()),
            record,
            maximized: link.mode == Mode::App,
        }
    }
}

/// The session's workspaces, as the opener sees them.
pub trait LinkWorkspaces: Send + Sync {
    /// Open `open` in the workspace whose room it names, answering that
    /// room. `None` when this person has no such workspace, so the link is
    /// not theirs to open here.
    fn open(&self, open: &LinkOpen) -> Option<Result<tilo_desktop_model::RoomId, String>>;
}

/// Runs a reveal off the calling thread.
type Spawn = fn(Box<dyn FnOnce() + Send>);

fn spawn_reveal(job: Box<dyn FnOnce() + Send>) {
    if let Err(error) = std::thread::Builder::new()
        .name("tilo-link-reveal".into())
        .spawn(job)
    {
        tracing::warn!(%error, "could not start showing a linked resource");
    }
}

/// The host opener with Tilo links routed to the person's own workspaces.
pub struct LinkOpener {
    entries: WebEntries,
    workspaces: Arc<dyn LinkWorkspaces>,
    door: Arc<dyn LinkDoor>,
    browser: Arc<dyn UriOpener>,
    spawn: Spawn,
    /// How long a just-launched app has to publish what the link names.
    timeout: std::time::Duration,
}

impl LinkOpener {
    /// `browser` takes everything that is not a link to one of `workspaces`;
    /// `door` shows the resource a link names once its app is in front.
    pub fn new(
        entries: WebEntries,
        workspaces: Arc<dyn LinkWorkspaces>,
        door: Arc<dyn LinkDoor>,
        browser: Arc<dyn UriOpener>,
    ) -> Self {
        Self {
            entries,
            workspaces,
            door,
            browser,
            spawn: spawn_reveal,
            timeout: PUBLISH_TIMEOUT,
        }
    }

    /// Show the linked resource once `room` has the app in front. `open` runs
    /// on the shell's thread and a just-launched app takes seconds to
    /// publish, so the wait happens elsewhere and its outcome is logged.
    fn reveal_after_open(&self, link: &Link, room: tilo_desktop_model::RoomId, public: &str) {
        let (Some(app), Some(resource)) = (&link.target.app, &link.target.resource) else {
            return;
        };
        let wanted = Reveal {
            room: room.to_string(),
            app_id: app.as_str().to_owned(),
            record: LinkOpen::of(link).record,
            resource: resource.clone(),
            view: link.target.view.clone(),
        };
        let door = Arc::clone(&self.door);
        let public = public.to_owned();
        let timeout = self.timeout;
        (self.spawn)(Box::new(move || {
            match reveal(door.as_ref(), &wanted, timeout) {
                Ok(()) => tracing::info!(link = %public, "showed a linked resource"),
                Err(error) => {
                    tracing::warn!(link = %public, %error, "could not show a linked resource");
                }
            }
        }));
    }

    /// The entries this device knows without asking anyone.
    pub fn known_entries() -> WebEntries {
        WebEntries::new(WebEntry::new(TILO_ENTRY).into_iter().collect())
    }

    fn parse(&self, uri: &str) -> Option<Link> {
        if uri.starts_with("tilo://open") {
            return match Link::parse_native(uri) {
                Ok(link) => Some(link),
                Err(error) => {
                    // The error names the rule, never the route or its secrets.
                    tracing::warn!(%error, "not opening a malformed tilo://open link");
                    None
                }
            };
        }
        self.entries.find(uri)?;
        Link::parse(uri, &self.entries).ok()
    }
}

impl UriOpener for LinkOpener {
    fn open(&self, uri: &str) -> std::io::Result<()> {
        let Some(link) = self.parse(uri) else {
            if uri.starts_with("tilo://open") {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "this Tilo link is malformed",
                ));
            }
            return self.browser.open(uri);
        };
        let public = link.public().to_native().unwrap_or_default();
        match self.workspaces.open(&LinkOpen::of(&link)) {
            Some(Ok(room)) => {
                tracing::info!(link = %public, "opened a Tilo link in place");
                self.reveal_after_open(&link, room, &public);
                Ok(())
            }
            Some(Err(error)) => {
                tracing::warn!(link = %public, %error, "could not open a Tilo link");
                Err(std::io::Error::other(error))
            }
            // Not a member. There is no native join flow yet, so an invite or
            // ticket is admitted by the page, which reads it from the fragment.
            None if uri.starts_with("tilo://") => {
                let entry = WebEntry::new(TILO_ENTRY).map_err(std::io::Error::other)?;
                let web = link.to_url(&entry).map_err(std::io::Error::other)?;
                tracing::info!(link = %public, "not a member of this room; opening it on the web");
                self.browser.open(&web)
            }
            None => {
                tracing::info!(link = %public, "not a member of this room; opening it on the web");
                self.browser.open(uri)
            }
        }
    }
}

/// [`LinkWorkspaces`] over the session's workspace tree, launching through
/// the action broker's launcher.
pub struct SessionWorkspaces(pub WorkspaceTreeHandle);

impl LinkWorkspaces for SessionWorkspaces {
    fn open(&self, open: &LinkOpen) -> Option<Result<tilo_desktop_model::RoomId, String>> {
        let workspace = {
            let mut tree = self.0.write();
            let workspace = match open.room {
                Some(room) => tree
                    .desktop
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.room == room)?,
                // On a native desktop the person's own room is the one they
                // are in.
                None => tree.desktop.active_workspace()?,
            }
            .clone();
            tree.set_active(workspace.id);
            workspace
        };
        let Some(app) = &open.app else {
            return Some(Ok(workspace.room));
        };
        if open.maximized {
            tracing::debug!(app, "full-tab links open as a regular window natively");
        }
        let launcher = crate::shell::broker_services::NativeLauncher(Some(self.0.clone()), None);
        Some(
            tilo_action_broker::services::LauncherServices::launch(
                &launcher,
                app,
                &workspace,
                open.record.as_deref(),
                &tilo_action_broker::services::LaunchPlace::Focus,
            )
            .map(|()| workspace.room),
        )
    }
}

/// The session's action broker, once it runs. The opener is installed before
/// the broker binds, and the broker may restart with the session's identity.
static SESSION_BROKER: RwLock<Option<WeakAppActionBroker>> = RwLock::new(None);

/// Let links reveal through `broker`, the session's current action broker.
pub fn attach_broker(broker: &AppActionBroker) {
    *SESSION_BROKER.write_recover() = Some(broker.downgrade());
}

/// [`LinkDoor`] over the session's action broker, admitting this device's
/// selected person in the room as the palette does.
pub struct SessionDoor;

impl LinkDoor for SessionDoor {
    fn session(&self, room: &str) -> Result<(Arc<dyn ActionHost + Send + Sync>, Caller), String> {
        let broker = SESSION_BROKER
            .read_recover()
            .clone()
            .ok_or("the session's action broker is not running yet")?
            .upgrade()?;
        let session = crate::shell::certified_action_session::CertifiedActionSession::open(
            broker, room, None,
        )?;
        Ok(session.door())
    }
}

/// The opener the shell's in-process widgets get: `browser` for everything,
/// with Tilo links to the session's workspaces opened in place.
pub fn session_opener(
    tree: WorkspaceTreeHandle,
    browser: Arc<dyn UriOpener>,
) -> Arc<dyn UriOpener> {
    Arc::new(LinkOpener::new(
        LinkOpener::known_entries(),
        Arc::new(SessionWorkspaces(tree)),
        Arc::new(SessionDoor),
        browser,
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tilo_actions::remote::{
        ActionHostError, CataloguePage, CatalogueProvider, CatalogueQuery, CatalogueRow,
        InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
    };
    use tilo_actions::{ActionAddress, ActionDescriptor, ResourceViewDescriptor};

    use super::*;

    const ROOM: &str = "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90";
    const STRANGER: &str = "0e9d8c7b-6a5f-4e3d-8c2b-1a0f9e8d7c6b";

    #[derive(Default)]
    struct Rooms {
        member_of: Vec<tilo_desktop_model::RoomId>,
        opened: Mutex<Vec<LinkOpen>>,
    }

    impl LinkWorkspaces for Rooms {
        fn open(&self, open: &LinkOpen) -> Option<Result<tilo_desktop_model::RoomId, String>> {
            let member = open.room.is_none_or(|room| self.member_of.contains(&room));
            member.then(|| {
                self.opened.lock().unwrap().push(open.clone());
                Ok(open.room.unwrap_or(self.member_of[0]))
            })
        }
    }

    /// The live connection the linked app publishes on.
    const PROVIDER: &str = "7";
    const RECORD: &str = "uuid:3e0d5a1c-7b2f-4e8a-9c6d-1a2b3c4d5e6f";
    const BLOCK: &str = "word_processor.block:b12";

    /// An action door publishing `rows` from one live word processor and
    /// recording what it is asked to run.
    #[derive(Default)]
    struct Door {
        rows: Vec<ActionDescriptor>,
        refuse: bool,
        invoked: Mutex<Vec<InvokeRequest>>,
    }

    impl ActionHost for Door {
        fn query(
            &self,
            _caller: &Caller,
            query: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionHostError>> {
            let rows = self
                .rows
                .iter()
                .cloned()
                .map(|action| CatalogueRow::for_action(PROVIDER, "so.tilo.word-processor", action))
                .collect();
            let mut page = CataloguePage::from_rows("1".into(), rows, &query);
            if query.provider.is_none() {
                page.providers = vec![CatalogueProvider {
                    provider: PROVIDER.into(),
                    app_id: "so.tilo.word-processor".into(),
                    room: ROOM.into(),
                    record_id: RECORD.into(),
                    browsable: false,
                }];
            }
            Pending::ready(Ok(page))
        }

        fn invoke(
            &self,
            _caller: &Caller,
            request: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
            let invocation = request.invocation;
            self.invoked.lock().unwrap().push(request);
            Pending::ready(if self.refuse {
                Err(ActionHostError::NotPermitted)
            } else {
                Ok(InvocationReceipt {
                    invocation,
                    status: ReceiptStatus::Accepted,
                })
            })
        }

        fn observe(&self, _: &Caller, _: &tilo_actions::InvocationId) -> Option<InvocationState> {
            None
        }

        fn cancel(
            &self,
            _: &Caller,
            _: &tilo_actions::InvocationId,
        ) -> Result<(), ActionHostError> {
            Err(ActionHostError::UnknownInvocation)
        }
    }

    impl LinkDoor for Arc<Door> {
        fn session(
            &self,
            room: &str,
        ) -> Result<(Arc<dyn ActionHost + Send + Sync>, Caller), String> {
            Ok((self.clone(), Caller::local_person("me").in_room(room)))
        }
    }

    fn descriptor(action: &str, view: Option<ResourceViewDescriptor>) -> ActionDescriptor {
        ActionDescriptor {
            address: ActionAddress::new(BLOCK, action),
            semantic_identity: None,
            resource_kind: "word_processor.block".into(),
            resource_label: "Block".into(),
            action_label: action.into(),
            description: None,
            locus: tilo_actions::ExecutionLocus::default(),
            kind: tilo_actions::OperationKind::View,
            contract: tilo_actions::ActionContract::view(),
            input: tilo_actions::InputSpec::None,
            params: Vec::new(),
            view,
        }
    }

    fn view(id: &str, open: &str, primary: bool) -> ActionDescriptor {
        descriptor(
            open,
            Some(ResourceViewDescriptor {
                id: id.into(),
                label: id.into(),
                description: None,
                open_action: open.into(),
                primary,
                mount: None,
                range: None,
            }),
        )
    }

    /// The word processor's block: a primary text view that can show a
    /// range, and a named outline view.
    fn word_processor() -> Arc<Door> {
        let mut text = view("text", "reveal", true);
        if let Some(descriptor) = &mut text.view {
            descriptor.range = Some(tilo_actions::ResourceViewRangeSpec::new("reveal-range"));
        }
        Arc::new(Door {
            rows: vec![
                descriptor("rename", None),
                view("outline", "show-outline", false),
                text,
            ],
            ..Door::default()
        })
    }

    fn opener_with(door: Arc<Door>) -> (LinkOpener, Arc<Rooms>, ui::host::HostRecorder) {
        let rooms = Arc::new(Rooms {
            member_of: vec![tilo_desktop_model::RoomId::parse(ROOM).unwrap()],
            ..Rooms::default()
        });
        let browser = ui::host::HostRecorder::default();
        let mut opener = LinkOpener::new(
            LinkOpener::known_entries(),
            rooms.clone(),
            Arc::new(door),
            Arc::new(browser.clone()),
        );
        opener.spawn = |job| job();
        opener.timeout = std::time::Duration::from_millis(1);
        (opener, rooms, browser)
    }

    fn opener() -> (LinkOpener, Arc<Rooms>, ui::host::HostRecorder) {
        opener_with(Arc::new(Door::default()))
    }

    fn block_link(query: &str) -> String {
        format!(
            "tilo://open#/@ada.tilo.example/w/{ROOM}/so.tilo.word-processor/{RECORD}/{BLOCK}{query}"
        )
    }

    fn reveal_of(uri: &str) -> Reveal {
        let link = Link::parse_native(uri).unwrap();
        Reveal {
            room: ROOM.into(),
            app_id: "so.tilo.word-processor".into(),
            record: Some(RECORD.into()),
            resource: link.target.resource.unwrap(),
            view: link.target.view,
        }
    }

    #[test]
    fn a_range_link_runs_the_views_range_action_with_its_span() {
        let door = word_processor();
        let (opener, _, _) = opener_with(door.clone());
        opener.open(&block_link("?chars=40-96")).unwrap();
        let invoked = door.invoked.lock().unwrap().clone();
        assert_eq!(invoked.len(), 1);
        assert_eq!(invoked[0].target.provider, PROVIDER);
        assert_eq!(
            invoked[0].target.address,
            ActionAddress::new(BLOCK, "reveal-range")
        );
        let InvokeArgument::Json(args) = &invoked[0].argument else {
            panic!("a range is typed arguments: {:?}", invoked[0].argument);
        };
        assert_eq!(
            serde_json::Value::Object(args.clone()),
            serde_json::json!({"start": 40, "end": 96, "unit": "Chars"})
        );
    }

    #[test]
    fn a_view_link_opens_the_named_view_and_a_plain_one_the_primary() {
        let door = word_processor();
        let (opener, _, _) = opener_with(door.clone());
        opener.open(&block_link("?view=outline")).unwrap();
        opener.open(&block_link("")).unwrap();
        let invoked = door.invoked.lock().unwrap().clone();
        let actions: Vec<_> = invoked
            .iter()
            .map(|request| (request.target.address.action.as_str(), &request.argument))
            .collect();
        assert_eq!(
            actions,
            [
                ("show-outline", &InvokeArgument::None),
                ("reveal", &InvokeArgument::None)
            ]
        );
    }

    #[test]
    fn missing_and_refused_targets_fail_alike() {
        let short = std::time::Duration::from_millis(1);
        let missing = reveal(
            &word_processor(),
            &reveal_of(&block_link("?view=history")),
            short,
        );
        let unpublished = reveal(
            &Arc::new(Door::default()),
            &reveal_of(&block_link("")),
            short,
        );
        let refusing = Arc::new(Door {
            refuse: true,
            ..Arc::into_inner(word_processor()).unwrap()
        });
        let refused = reveal(&refusing, &reveal_of(&block_link("")), short);
        assert_eq!(refusing.invoked.lock().unwrap().len(), 1);
        for outcome in [missing, unpublished, refused] {
            assert_eq!(outcome, Err(UNAVAILABLE.to_owned()));
        }
    }

    #[test]
    fn a_link_to_a_members_room_opens_in_place() {
        let (opener, rooms, browser) = opener();
        let uri = format!(
            "https://tilo.so/app/#/@ada.tilo.example/w/reading~{ROOM}/so.tilo.chat/uuid:3e0d5a1c-7b2f-4e8a-9c6d-1a2b3c4d5e6f/chat.channel:general"
        );
        opener.open(&uri).unwrap();
        assert!(browser.opened_uris().is_empty());
        assert_eq!(
            *rooms.opened.lock().unwrap(),
            [LinkOpen {
                room: tilo_desktop_model::RoomId::parse(ROOM),
                app: Some("so.tilo.chat".into()),
                record: Some("uuid:3e0d5a1c-7b2f-4e8a-9c6d-1a2b3c4d5e6f".into()),
                maximized: true,
            }]
        );
    }

    #[test]
    fn a_native_link_parses_and_opens_in_place() {
        let (opener, rooms, browser) = opener();
        opener
            .open(&format!(
                "tilo://open#/@ada.tilo.example/w/{ROOM}/so.tilo.chat/-/chat.channel:general"
            ))
            .unwrap();
        assert!(browser.opened_uris().is_empty());
        let launched = rooms.opened.lock().unwrap().clone();
        assert_eq!(launched.len(), 1);
        assert_eq!(launched[0].app.as_deref(), Some("so.tilo.chat"));
        assert_eq!(launched[0].record, None, "`-` is the app's primary record");
        assert!(!launched[0].maximized);
    }

    #[test]
    fn links_outside_the_persons_rooms_go_to_the_browser() {
        let (opener, rooms, browser) = opener();
        let stranger = format!("https://tilo.so/#/@ada.tilo.example/w/{STRANGER}/so.tilo.chat");
        for uri in [
            "https://example.com/#/me/so.tilo.chat",
            "https://tilo.so/pricing",
            stranger.as_str(),
        ] {
            opener.open(uri).unwrap();
        }
        assert!(rooms.opened.lock().unwrap().is_empty());
        assert_eq!(
            browser.opened_uris(),
            [
                "https://example.com/#/me/so.tilo.chat",
                "https://tilo.so/pricing",
                stranger.as_str(),
            ]
        );
    }

    #[test]
    fn a_native_link_to_a_strangers_room_opens_its_web_page() {
        let (opener, _, browser) = opener();
        opener
            .open(&format!(
                "tilo://open#/@ada.tilo.example/w/{STRANGER}/so.tilo.chat?invite=AAECAwQF"
            ))
            .unwrap();
        assert_eq!(
            browser.opened_uris(),
            [format!(
                "https://tilo.so/#/@ada.tilo.example/w/{STRANGER}/so.tilo.chat?invite=AAECAwQF"
            )]
        );
    }

    #[test]
    fn a_malformed_native_link_is_refused() {
        let (opener, rooms, browser) = opener();
        assert!(opener.open("tilo://open#/so.tilo.chat").is_err());
        assert!(rooms.opened.lock().unwrap().is_empty());
        assert!(browser.opened_uris().is_empty());
    }
}
