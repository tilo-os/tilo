//! "Add a device": the link that makes a new device of the person one of theirs, kept current while
//! its sheet is up.
//!
//! The link is the workspace link to the workspace in front, at the host that keeps it, carrying
//! the person's root and a fresh invite ([`crate::pairing_link`]). One invite is open at a time:
//! asking while it works answers the same link, so a sheet left on screen keeps showing the code
//! that admits the person's device, and asking once it ran out makes a new one, so the code on
//! screen is never one that admits nothing. While a link works, this device waits at its host for
//! the device that opens it ([`crate::certification::watch_host`]). A workspace no host on the web
//! keeps uses this device’s native enrollment endpoint.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use tilo_sheets::SheetState as _;
use tilo_sheets::pair_device::{AddDevice, AddDeviceState, PairDeviceEffect};

use crate::pairing_link::Invites;
use crate::platform::clipboard::Clipboard;

/// How long a link the sheet copied stays on the clipboard: long enough to open it on the new
/// device or paste it into a message to yourself, and no longer, because the link is what admits
/// the device.
const ON_CLIPBOARD_FOR: Duration = Duration::from_secs(300);

/// Where a pairing link goes: the page it opens on, the host that keeps the workspace in front, that
/// workspace, and the person whose device a newcomer becomes.
pub(crate) struct Place {
    pub site: String,
    pub origin: String,
    pub workspace: String,
    /// Where the host that keeps the workspace in front is dialled, as its site publishes it.
    pub host: tilo_server_client::ServerAddress,
    pub person: [u8; 32],
}

/// When the site link needs a native fallback, or pairing is unavailable.
pub(crate) enum NoLink {
    /// No web host keeps the workspace in front. Try native enrollment.
    Nearby,
    /// The catalogue key saying why.
    Because(&'static str),
}

/// The host a link shown now sends a new device to, and until when.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Waiting {
    pub origin: String,
    /// Where the host the link names is dialled.
    pub host: tilo_server_client::ServerAddress,
    pub person: [u8; 32],
    pub until_ms: u64,
}

struct LocalOffer {
    person: [u8; 32],
    link: String,
    expires_at_ms: u64,
}

/// The link the sheet shows, and what its presses do.
pub(crate) struct Adding {
    invites: RefCell<Invites>,
    local: RefCell<Option<LocalOffer>>,
    clipboard: Rc<dyn Clipboard>,
    /// The link last put on the clipboard, while it is still the one shown: what the copy button
    /// says it did.
    copied: RefCell<Option<String>>,
    /// Where the link last shown sends a new device.
    waiting: RefCell<Option<Waiting>>,
    /// The machines that asked to join, as the person's store holds them, and
    /// what tells the person of each once.
    machines: RefCell<Vec<crate::enrollment::Waiting>>,
    told: RefCell<std::collections::BTreeSet<String>>,
    /// The question put to the person for each machine still waiting, by ask:
    /// the sheet's own resource for the same ask, which an agent answers
    /// through the question actions.
    parked: RefCell<std::collections::BTreeMap<String, tilo_service::Asked>>,
}

impl Adding {
    #[must_use]
    pub(crate) fn new(clipboard: Rc<dyn Clipboard>) -> Self {
        Self {
            invites: RefCell::new(Invites::default()),
            local: RefCell::default(),
            clipboard,
            copied: RefCell::default(),
            waiting: RefCell::default(),
            machines: RefCell::default(),
            told: RefCell::default(),
            parked: RefCell::default(),
        }
    }

    /// Where the link last shown sends a new device, and until when.
    pub(crate) fn waiting(&self) -> Option<Waiting> {
        self.waiting.borrow().clone()
    }

    /// The sheet at `now_ms`: the link that admits a device, or a fresh one once the one shown ran
    /// out, or why no link can be made here.
    #[must_use]
    pub(crate) fn device(&self, place: Result<Place, NoLink>, now_ms: u64) -> AddDevice {
        let place = match place {
            Ok(place) => place,
            Err(NoLink::Nearby) => {
                let mut device = AddDevice::nearby();
                device.waiting = self.waiting_machines();
                return device;
            }
            Err(NoLink::Because(key)) => {
                let mut device = AddDevice::refusing(&tilo_sheets::word(key));
                device.waiting = self.waiting_machines();
                return device;
            }
        };
        match self.invites.borrow_mut().at(
            &place.site,
            &place.origin,
            &place.workspace,
            &place.person,
            now_ms,
        ) {
            Ok(shown) => {
                let mut device =
                    AddDevice::of(&shown.link, minutes_left(shown.expires_at_ms, now_ms));
                if self.copied.borrow().as_deref() == Some(shown.link.as_str()) {
                    device.copy = device.copied.clone();
                }
                *self.waiting.borrow_mut() = Some(Waiting {
                    origin: place.origin,
                    host: place.host,
                    person: place.person,
                    until_ms: shown.expires_at_ms,
                });
                device.waiting = self.waiting_machines();
                device
            }
            Err(reason) => {
                tracing::warn!(%reason, "no pairing link could be made");
                let mut device = AddDevice::refusing(&tilo_sheets::word("pair-device-no-link"));
                device.waiting = self.waiting_machines();
                device
            }
        }
    }

    fn local(
        &self,
        links: &crate::enrollment::LocalLinks,
        person: [u8; 32],
        now_ms: u64,
    ) -> AddDevice {
        let mut local = self.local.borrow_mut();
        if local
            .as_ref()
            .is_none_or(|offer| offer.person != person || offer.expires_at_ms <= now_ms)
        {
            *local = links
                .issue(person, now_ms)
                .map(|(link, expires_at_ms)| LocalOffer {
                    person,
                    link,
                    expires_at_ms,
                });
        }
        let Some(offer) = local.as_ref() else {
            return self.device(Err(NoLink::Nearby), now_ms);
        };
        let mut device = AddDevice::of(&offer.link, minutes_left(offer.expires_at_ms, now_ms));
        device.in_tilo = tilo_sheets::word("pair-device-local-link");
        if self.copied.borrow().as_deref() == Some(offer.link.as_str()) {
            device.copy = device.copied.clone();
        }
        device.waiting = self.waiting_machines();
        device
    }

    /// What the person's store holds now: the asks no device answered yet.
    pub(crate) fn holding(&self, machines: Vec<crate::enrollment::Waiting>) {
        *self.machines.borrow_mut() = machines;
    }

    /// The machines waiting for the person's answer, as the sheet lists them.
    fn waiting_machines(&self) -> Vec<tilo_sheets::pair_device::WaitingMachine> {
        self.machines
            .borrow()
            .iter()
            .map(|machine| tilo_sheets::pair_device::WaitingMachine {
                ask: machine.ask.clone(),
                label: machine.label.clone(),
                code: machine.code.clone(),
            })
            .collect()
    }

    /// Let in the machine whose own `tilo://enroll?…` line the person pasted: this device dials
    /// it and approves it there, answering as either kind of machine.
    pub(crate) fn scan(link: &str) {
        let Ok(paths) = tilo_identity::device_admin::PersonPaths::from_env() else {
            return;
        };
        if let Err(error) = crate::enrollment::approve_line(link, paths.root, paths.dir) {
            tracing::warn!(%error, "a machine's link was not approved");
        }
    }

    /// Note that the person was told of the machine whose ask this is; `false` when they were
    /// already.
    pub(crate) fn note_told(&self, ask: &str) -> bool {
        self.told.borrow_mut().insert(ask.to_owned())
    }

    /// Whether the machine whose ask this is already has its question up.
    fn parked(&self, ask: &str) -> bool {
        self.parked.borrow().contains_key(ask)
    }

    /// Put the machine's ask on the question sheet under `asked`.
    pub(crate) fn park(&self, ask: String, asked: tilo_service::Asked) {
        self.parked.borrow_mut().insert(ask, asked);
    }

    /// The answers the person gave on the question sheet since the last pass,
    /// by ask: `None` is the question's cancel.
    pub(crate) fn answers(&self) -> Vec<(String, Option<tilo_service::Answer>)> {
        let now = tilo_clock::now();
        let mut parked = self.parked.borrow_mut();
        let mut resolved = Vec::new();
        parked.retain(|ask, asked| match asked.until(Some(now)) {
            Some(answer) => {
                resolved.push((ask.clone(), answer));
                false
            }
            None => true,
        });
        resolved
    }

    /// Withdraw retired questions and any notices the open sheet now presents inline.
    fn retire(&self, waiting: &[crate::enrollment::Waiting], inline: bool) -> Vec<String> {
        let pending = |ask: &str| waiting.iter().any(|machine| machine.ask == ask);
        self.parked.borrow_mut().retain(|ask, _| pending(ask));
        let mut withdrawn = Vec::new();
        self.told.borrow_mut().retain(|ask| {
            if inline || !pending(ask) {
                withdrawn.push(ask.clone());
                false
            } else {
                true
            }
        });
        withdrawn
    }

    /// Carry out what the person pressed in the sheet, and say the link is on the clipboard when it
    /// is.
    pub(crate) fn take(&self, state: &AddDeviceState, answer: impl Fn(&str, bool)) {
        for effect in state.take_asked() {
            match effect {
                PairDeviceEffect::Copy { link } => {
                    self.clipboard.copy(link.clone(), ON_CLIPBOARD_FOR);
                    *self.copied.borrow_mut() = Some(link);
                }
                PairDeviceEffect::Approve { ask } => answer(&ask, true),
                PairDeviceEffect::Decline { ask } => answer(&ask, false),
                PairDeviceEffect::Link { link } => Self::scan(&link),
                // Its own close: the sheet took itself down as it asked.
                PairDeviceEffect::Done => {}
            }
        }
    }
}

/// The minutes a link that stops working at `until_ms` still works at `now_ms`, as the sheet
/// counts them: a part of a minute counts whole, so the last minute reads "the next minute".
fn minutes_left(until_ms: u64, now_ms: u64) -> u32 {
    u32::try_from(until_ms.saturating_sub(now_ms).div_ceil(MINUTE_MS)).unwrap_or(u32::MAX)
}

/// How long after `now_ms` the count of a link stopping at `until_ms` goes down by one: at its
/// last minute, the instant it stops working and a new link takes its place.
fn next_count_in(until_ms: u64, now_ms: u64) -> Duration {
    let into_minute = until_ms.saturating_sub(now_ms).saturating_sub(1) % MINUTE_MS;
    Duration::from_millis(into_minute.saturating_add(1))
}

const MINUTE_MS: u64 = 60_000;

impl crate::Shared {
    /// Add a device: the sheet showing the link a new device of this person's becomes one of
    /// theirs, up over the shell.
    pub(crate) fn open_pair_device(&self) {
        let Some(state) = self.pair_device.as_ref() else {
            return;
        };
        state.presentation().open(false);
        self.follow_pair_device();
    }

    pub(crate) fn pair_device_open(&self) -> bool {
        self.pair_device
            .as_ref()
            .is_some_and(|state| state.presentation().is_open())
    }

    /// Keep the sheet current while it is up: the link that admits a device, renewed once the one
    /// shown ran out, and what its presses do.
    pub(crate) fn follow_pair_device(&self) {
        if let Some(state) = self.pair_device.as_ref() {
            self.adding.take(state, |ask, approve| {
                if let Some(questions) = self.asker.questions() {
                    let handle = crate::enrollment::question_handle(ask);
                    if approve {
                        questions.answer(&handle, tilo_service::Answer::default());
                    } else {
                        questions.decline(&handle);
                    }
                }
            });
        }
        // The store's wake must put the request on screen in this pass, even when the
        // owner leaves the sheet idle and no further input wakes the shell.
        self.follow_enrolling();
        if let Some(state) = self.pair_device.as_ref()
            && state.presentation().is_open()
        {
            state.show(self.pairing_device(ui::time::unix_now_ms()));
        }
        self.wait_for_new_device();
    }

    /// Put each machine that asks to join to the person once, and take back the asks that are
    /// gone: what the person's own workspace holds is what every one of their devices
    /// shows, and an ask any of them answered leaves them all at once. Each ask is also a question resource while it
    /// waits, so an agent answers the machine through the same sheet as a person does.
    pub(crate) fn follow_enrolling(&self) {
        let Some(store) = self.sync.as_ref().and_then(|sync| sync.person_resources()) else {
            return;
        };
        let held = crate::enrollment::held(&store);
        let inline = self.pair_device_open();
        for ask in self.adding.retire(&held.waiting, inline) {
            self.telling().withdraw(&crate::enrollment::tag(&ask));
        }
        self.adding.holding(held.waiting.clone());
        if let Some(questions) = self.asker.questions() {
            for machine in &held.waiting {
                if !self.adding.parked(&machine.ask) {
                    self.adding.park(
                        machine.ask.clone(),
                        questions.ask(crate::enrollment::question(machine)),
                    );
                }
            }
        }
        if !inline {
            for machine in &held.waiting {
                if self.adding.note_told(&machine.ask) {
                    self.telling().show(&crate::enrollment::notice(machine));
                }
            }
        }
        for (ask, answer) in self.adding.answers() {
            self.answer_enrolling(&ask, crate::enrollment::admits(answer.as_ref()));
        }
    }

    /// The person answered a machine that asked to join: approving admits it. The answer is
    /// written to their own workspace, and whichever of their devices serves the machine
    /// carries it out.
    pub(crate) fn answer_enrolling(&self, ask: &str, approve: bool) {
        let Some(store) = self.sync.as_ref().and_then(|sync| sync.person_resources()) else {
            return;
        };
        crate::enrollment::answer(&store, ask, approve);
        self.telling().withdraw(&crate::enrollment::tag(ask));
    }

    /// When the sheet next changes on its own while it is up: its minutes count down, and the link
    /// that ran out gives way to a new one.
    pub(crate) fn pair_device_deadline(&self) -> Option<std::time::Instant> {
        let state = self.pair_device.as_ref()?;
        if !state.presentation().is_open() {
            return None;
        }
        let now_ms = ui::time::unix_now_ms();
        let after = if matches!(self.pairing_place(), Err(NoLink::Nearby)) {
            self.adding
                .local
                .borrow()
                .as_ref()
                .map_or(Duration::from_millis(250), |offer| {
                    next_count_in(offer.expires_at_ms, now_ms)
                })
        } else {
            next_count_in(self.adding.waiting()?.until_ms, now_ms)
        };
        std::time::Instant::now().checked_add(after)
    }

    /// Wait at the host the link shown names for the device that opens it, until the link runs
    /// out: what asks the person here.
    fn wait_for_new_device(&self) {
        if let (Some(fabric), Some(waiting)) = (self.fabric.as_ref(), self.adding.waiting()) {
            crate::certification::watch_host(
                fabric.as_ref(),
                &waiting.origin,
                waiting.host,
                waiting.person,
                waiting.until_ms,
            );
        }
    }

    fn pairing_device(&self, now_ms: u64) -> AddDevice {
        let place = self.pairing_place();
        if matches!(place, Err(NoLink::Nearby))
            && let Some(engine) = self.engine.as_ref()
            && let Some(links) = self.sync.as_ref().and_then(|sync| sync.enrollment_links())
        {
            return self
                .adding
                .local(links, engine.identity().device_cert.user_public_key, now_ms);
        }
        self.adding.device(place, now_ms)
    }

    /// Where the link goes: the workspace in front at the web host that keeps it,
    /// and the person whose device a newcomer becomes. Without a web keeper,
    /// use the native enrollment endpoint.
    fn pairing_place(&self) -> Result<Place, NoLink> {
        // Only a device of the person's answers the new device, on their own devices' fabric.
        let engine = self
            .engine
            .as_ref()
            .filter(|_| self.fabric.is_some())
            .ok_or(NoLink::Because("pair-device-not-yours"))?;
        let workspace = self
            .handle
            .read(|tree| {
                tree.active_workspace()
                    .map(|workspace| workspace.id().to_string())
            })
            .ok_or(NoLink::Because("pair-device-no-workspace"))?;
        let listed = self.directory_store.listing(&workspace);
        let keeper = listed
            .as_ref()
            .and_then(|listed| listed.entry_url.as_ref())
            .map(|place| place.origin().ascii_serialization());
        let (site, origin) =
            place_of(crate::platform::directory::community_site(), keeper).ok_or(NoLink::Nearby)?;
        // Where that host is reached, as the site or directory that names the entry published it.
        let host = listed
            .and_then(|listed| listed.entry_host)
            .and_then(|host| host.address)
            .map(|address| tilo_server_client::ServerAddress::published(&address.into()))
            .ok_or(NoLink::Because("pair-device-no-host"))?;
        Ok(Place {
            site,
            origin,
            workspace,
            host,
            person: engine.identity().device_cert.user_public_key,
        })
    }
}

/// The page opens at the community site or at the workspace's own web host.
/// A community can provide the page when it is known, while the keeper
/// alone determines the host that holds the workspace.
fn place_of(community: Option<url::Url>, keeper: Option<String>) -> Option<(String, String)> {
    let origin = keeper?;
    let site = community.map_or_else(|| format!("{origin}/"), |site| site.to_string());
    Some((site, origin))
}

#[cfg(test)]
mod tests {

    use std::cell::RefCell;

    use super::*;

    const SITE: &str = "https://tilo.coffee/";
    const ORIGIN: &str = "https://eu1.tilo.coffee";
    const WORKSPACE: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";
    const START: u64 = 1_700_000_000_000;

    /// A clipboard that keeps what it was given.
    #[derive(Default)]
    struct Sheets(RefCell<Vec<String>>);

    impl Clipboard for Sheets {
        fn copy(&self, text: String, _kept_for: Duration) {
            self.0.borrow_mut().push(text);
        }
    }

    fn place() -> Place {
        Place {
            site: SITE.to_owned(),
            origin: ORIGIN.to_owned(),
            workspace: WORKSPACE.to_owned(),
            host: host(),
            person: [0xab; 32],
        }
    }

    fn host() -> tilo_server_client::ServerAddress {
        tilo_server_client::ServerAddress::at(iroh::EndpointAddr::new(
            iroh::SecretKey::from_bytes(&[9; 32]).public(),
        ))
    }

    fn adding() -> (Adding, Rc<Sheets>) {
        let clipboard = Rc::new(Sheets::default());
        let adding = Adding::new(Rc::clone(&clipboard) as Rc<dyn Clipboard>);
        (adding, clipboard)
    }

    tilo_test_env::scenario! {
        fn the_pairing_sheet_shows_one_link_and_counts_it_down;

        phase the_sheet_shows_one_link_until_it_runs_out;
        // The sheet shows the link the session made, and the same one on every pass until it runs
        // out: a sheet that redrew a fresh invite each pass would hand out a code that never works.
        {
            let (adding, _) = adding();
            let first = adding.device(Ok(place()), START);
            assert!(
                first
                    .link
                    .starts_with("https://tilo.coffee/#/@eu1.tilo.coffee/w/")
            );
            assert!(first.link.contains("?pair=abab"), "{}", first.link);
            let again = adding.device(Ok(place()), START + 60_000);
            assert_eq!(again.link, first.link);
            let renewed = adding.device(Ok(place()), START + 600_000);
            assert_ne!(renewed.link, first.link);
        }

        phase without_a_host_on_the_web_the_sheet_offers_nearby;
        // A workspace no host on the web keeps: no link, and the sheet says to add the device nearby
        // rather than showing a code that admits nobody.
        {
            let (adding, _) = adding();
            let device = adding.device(Err(NoLink::Nearby), START);
            assert_eq!(device.link, "");
            assert_eq!(device.refusal, AddDevice::nearby().refusal);
            assert!(adding.waiting().is_none(), "nothing to wait for");
        }

        phase the_link_names_the_host_that_keeps_the_workspace;
        // The web link needs the workspace's actual keeper. A configured
        // community may provide the page even when another host keeps the workspace.
        {
            let community = url::Url::parse(SITE).ok();
            assert_eq!(
                place_of(community.clone(), Some("https://site.example".to_owned())),
                Some((SITE.to_owned(), "https://site.example".to_owned()))
            );
            assert_eq!(
                place_of(community, None),
                None
            );
            assert_eq!(
                place_of(None, Some("https://site.example".to_owned())),
                Some((
                    "https://site.example/".to_owned(),
                    "https://site.example".to_owned()
                ))
            );
            assert_eq!(place_of(None, None), None);
        }

        phase a_shown_link_is_waited_for_until_it_runs_out;
        // A link shown is one this device waits at its host for, until it runs out.
        {
            let (adding, _) = adding();
            let shown = adding.device(Ok(place()), START);
            assert!(shown.refusal.is_none());
            assert_eq!(
                adding.waiting(),
                Some(Waiting {
                    origin: ORIGIN.to_owned(),
                    host: host(),
                    person: [0xab; 32],
                    until_ms: START + 600_000,
                })
            );
        }

        phase the_sheet_counts_down_to_a_new_link;
        // The minutes on the sheet count down while it is up: a pass is due the instant the count
        // drops, and the last minute ends when the link does, where a new one takes its place.
        {
            let until = START + 600_000;
            for (now, minutes, next) in [(0, 10, 60), (30, 10, 30), (60, 9, 60), (590, 1, 10)] {
                assert_eq!(
                    minutes_left(until, START + now * 1_000),
                    minutes,
                    "at {now} s"
                );
                let next_in = next_count_in(until, START + now * 1_000);
                assert_eq!(next_in, Duration::from_secs(next), "at {now} s");
            }
            let (adding, _) = adding();
            let first = adding.device(Ok(place()), START);
            assert_eq!(first.lasting, AddDevice::of(&first.link, 10).lasting);
            let later = adding.device(Ok(place()), START + 60_000);
            assert_eq!(later.lasting, AddDevice::of(&first.link, 9).lasting);
        }
    }

    /// The person approves the machine they planned, and the session carries the
    /// approval into the machine's entry: the entry the plan made under the
    /// plan's id becomes the machine's, under its device key, in the Servers
    /// workspace the session's own host holds. Nothing here is a fixture of the
    /// service: the session asks its own host the same `machine.*` operations a
    /// window of theirs does.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one test walks an identity, a session, its own workspace service and the person's approval"
    )]
    fn the_persons_approval_takes_a_planned_machine_into_its_entry() {
        let _settled = ui::motion::testing::settled();
        let root = tempfile::tempdir().expect("a directory for the identity");
        let xdg = root.path().join("config");
        let system = root.path().join("system");
        let state = root.path().join("state");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_CONFIG_HOME", &xdg)
            .set("TILO_IDENTITY_ROOT", &system)
            .set(tilo_identity::provisioning::FILE_SECRETS_ENV, "1");
        tilo_identity::provisioning::ensure_collaboration_identity(
            &xdg.join("tilo/identity"),
            &tilo_device_identity::IdentityPaths::under(&system),
            tilo_identity::provisioning::IdentityStorage::Files,
        )
        .expect("this computer's identity");
        let mut session = crate::Session::headless(
            crate::Options {
                presentation: ui::headless::fixture_runner_inputs,
                collab: true,
                shell: true,
                state: Some(state),
                ..crate::Options::default()
            },
            &[tilo_compositor::OutputConfig::new("HEADLESS-1", 1280, 800)],
        )
        .expect("a session");
        let engine = session.shared.engine.clone().expect("this device's engine");
        let person = engine.identity().device_cert.user_public_key;
        let admin = tilo_server_protocol::servers::person_workspace_id(&person);

        // The machine's own key, as it enrolled with the token the plan names.
        let machine = "cd".repeat(32);
        let nonce = "ab".repeat(16);
        let plan_id = "0123456789abcdef0123456789abcdef";
        let plan = serde_json::json!({
            "schema": 1,
            "plan": plan_id,
            "revision": 1,
            "destination": "tilo-server",
            "name": "cupboard",
            "network": { "addressing": "dhcp" },
            "disk": { "kind": "empty" },
            "unlock": "tpm",
            "updates": "stable",
            "hosting": [],
            "open": [],
            "token": { "nonce": nonce, "expires_at_ns": u64::MAX },
        })
        .to_string();
        let runtime = tilo_rt::try_app().expect("the app runtime");
        let planned = runtime.block_on(
            engine
                .ask_own_host(
                    "machine.plan",
                    serde_json::Map::from_iter([
                        ("plan".to_owned(), plan.into()),
                        ("label".to_owned(), "cupboard".into()),
                        ("token_file".to_owned(), "/tmp/cupboard.token".into()),
                        ("installer_url".to_owned(), "https://tilo.sh".into()),
                    ]),
                )
                .expect("this device holds its own Servers workspace"),
        );
        assert!(planned.is_ok(), "the plan is written: {planned:?}");

        // The person's approval, as their device keeps it: the machine's ask
        // carries the plan's nonce, so the session knows which entry it is.
        let ask = "ef".repeat(16);
        session.shared.open_pair_device();
        for _ in 0..5 {
            session
                .step(Duration::from_millis(16))
                .expect("the sheet opens");
        }
        let idle = tilo_sheets::word("pair-device-waiting-nothing");
        assert!(session.accessible_labels().contains(&idle));
        {
            let store = session
                .shared
                .sync
                .as_ref()
                .and_then(|sync| sync.person_resources())
                .expect("the person's own workspace");
            crate::enrollment::enter(
                &store,
                &tilo_enrollment::waiting::WaitingMachine {
                    ask,
                    device: machine.clone(),
                    service: "ef".repeat(32),
                    label: "cupboard".into(),
                    hardware: String::new(),
                    code: "123-456".into(),
                    nonce,
                },
            )
            .expect("the ask is kept in the person's own workspace");
        }
        // The person answers the machine through the same question surface an
        // agent uses: the ask is a question while it waits, and answering it
        // approves the machine.
        session.shared.follow_pair_device();
        session
            .step(Duration::from_millis(16))
            .expect("a pass without input");
        let controls = session.controls();
        assert!(
            controls
                .iter()
                .any(|control| control.name == "Approve cupboard with code 123-456"),
            "the request appears on the idle sheet: {controls:?}",
        );
        assert!(!session.accessible_labels().contains(&idle));
        let questions = session
            .shared
            .asker
            .questions()
            .expect("the session publishes the questions it asks");
        let question = questions
            .pending()
            .into_iter()
            .find(|question| question.kind == crate::enrollment::KIND)
            .expect("the machine asking to join is a question");
        assert_eq!(question.label, "cupboard");
        assert!(questions.answer(&question.handle, tilo_service::Answer::default()));
        session.shared.follow_pair_device();

        // The entry is the machine's under its device key once the session
        // carried the approval out: the plan's own id names the entry before
        // that, and the machine's key only after.
        let deadline = tilo_test_env::hang_deadline();
        while std::time::Instant::now() < deadline {
            session.step(std::time::Duration::from_millis(1)).ok();
            let entry = runtime.block_on(
                engine
                    .ask_own_host(
                        "machine.show",
                        serde_json::Map::from_iter([("plan".to_owned(), machine.clone().into())]),
                    )
                    .expect("this device holds its own Servers workspace"),
            );
            if entry.is_ok() {
                assert!(
                    runtime
                        .block_on(
                            engine
                                .ask_own_host(
                                    "machine.show",
                                    serde_json::Map::from_iter([(
                                        "plan".to_owned(),
                                        plan_id.into(),
                                    )]),
                                )
                                .expect("this device holds its own Servers workspace"),
                        )
                        .is_ok(),
                    "the plan still reads under its own id: the entry keeps it"
                );
                assert_eq!(
                    admin,
                    tilo_server_protocol::servers::person_workspace_id(&person)
                );
                return;
            }
        }
        panic!("the approval did not take the machine into its entry");
    }
}
