//! The triage agent in a room: Chat, run headless as the agent's own
//! participant, and the triage decision driving it.
//!
//! The connection is an agent route: the broker admits it only for an agent
//! the room has admitted, launched from a device of one of the room's
//! administrators, and decides everything it may do from the agent's own
//! grants. Chat then writes what the agent asks for as the agent's own
//! comment records, which the broker checks like any member's. When the
//! room removes the agent, the broker says so, and the agent stops.

use std::collections::HashMap;
use std::os::fd::IntoRawFd as _;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use chat::hand::ChatHand;
use tilo_actions::ActionAddress;
use tilo_reference::ResourceRef;
use ui::collaboration::{CertifiedEndpointIdentity, CollaborationManager};
use ui::permissions::Verb;

use super::forge::{self, FiledIssue, Forge, Intents};
use super::lease;
use super::think::Think;
use super::{ISSUE_KIND, Move, Scope, Triage};

/// Chat's package id: the app whose route the agent joins.
pub const CHAT_APP: &str = "so.tilo.chat";

/// How long a reply the agent asked for may take to show in the room before
/// it is asked for again.
const REPLY_WAIT: Duration = Duration::from_secs(30);

/// How long the agent waits before trying a failed filing again.
const FILING_RETRY: Duration = Duration::from_secs(60);

/// How the agent is joining.
pub struct Config {
    /// The room (workspace) it is a member of.
    pub room: uuid::Uuid,
    /// The agent's name as the room admitted it.
    pub name: String,
    /// The channel it triages.
    pub scope: Scope,
    /// An explicit broker socket for a headless run outside the launcher;
    /// `None` inherits the launcher's `TILO_SVC_COLLAB_FD`.
    pub endpoint: Option<PathBuf>,
    /// The device credentials for an explicit endpoint.
    pub identity: Option<CertifiedEndpointIdentity>,
    /// Where the agent keeps its own comment records.
    pub records: ui::record::RecordStore,
    /// The filings it set out to make and has not seen answered.
    pub intents: Intents,
    /// How the one-session-per-room lease is timed.
    pub lease: lease::Timing,
}

/// Where the agent stands after a step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// The broker has not yet said the agent may comment here.
    Waiting,
    /// Admitted; it acted or had nothing to do.
    Working,
    /// It was admitted and no longer is. It does nothing more.
    Revoked,
    /// Admitted, and waiting to learn whether it holds the room's lease.
    Standby,
    /// Another device's session holds the room. It does nothing more; its
    /// desktop tries again later, and takes the room over once that
    /// session's claim expires.
    Yielded,
}

/// The agent's participant id in Chat.
#[must_use]
pub fn participant_id(name: &str) -> String {
    format!("agent:{name}")
}

pub struct Participant {
    app: ui::App,
    hand: ChatHand,
    triage: Triage,
    think: Box<dyn Think>,
    forge: Box<dyn Forge>,
    /// Moves asked for and not yet shown in the room, by key, with when.
    busy: HashMap<String, (Instant, Duration)>,
    intents: Intents,
    admitted: bool,
    lease_timing: lease::Timing,
    /// The session's side of the room's lease, once it knows its device.
    lease: Option<lease::Lease>,
}

impl Participant {
    /// Join the room as the agent.
    ///
    /// # Errors
    ///
    /// When the broker cannot be reached or refuses the connection.
    pub fn connect(cfg: Config, think: Box<dyn Think>, forge: Box<dyn Forge>) -> Result<Self> {
        let manager = match (&cfg.endpoint, cfg.identity) {
            (Some(path), Some(identity)) => {
                let stream = UnixStream::connect(path)
                    .with_context(|| format!("connect to {}", path.display()))?;
                CollaborationManager::for_agent_route_fd_with_identity(
                    stream.into_raw_fd(),
                    CHAT_APP,
                    cfg.room,
                    None,
                    cfg.name.clone(),
                    identity,
                )
            }
            (Some(path), None) => {
                let stream = UnixStream::connect(path)
                    .with_context(|| format!("connect to {}", path.display()))?;
                CollaborationManager::for_agent_route_fd(
                    stream.into_raw_fd(),
                    CHAT_APP,
                    cfg.room,
                    None,
                    cfg.name.clone(),
                )
            }
            (None, _) => {
                CollaborationManager::for_agent_route(CHAT_APP, cfg.room, None, cfg.name.clone())
            }
        }
        .context("join the room as the triage agent")?;
        let me = participant_id(&cfg.name);
        let hand = ChatHand::new();
        let chat =
            chat::ChatApp::for_participant(&me, &cfg.name, cfg.records).with_hand(hand.clone());
        let mut app =
            ui::App::with_collaboration_manager(ui::State::new(), Box::new(chat), manager);
        chat::actions::register(app.action_registry_mut());
        let app = app.with_records(
            ui::RecordSet::in_memory().with_home(cfg.room.to_string()),
            automerge::AutoCommit::new(),
        );
        tracing::info!(room = %cfg.room, agent = %cfg.name, channel = %cfg.scope.channel, "triage: joined");
        Ok(Self {
            app,
            hand,
            triage: Triage {
                me,
                room: cfg.room.to_string(),
                scope: cfg.scope,
            },
            think,
            forge,
            busy: HashMap::new(),
            intents: cfg.intents,
            admitted: false,
            lease_timing: cfg.lease,
            lease: None,
        })
    }

    /// The agent's participant id in Chat.
    #[must_use]
    pub fn me(&self) -> &str {
        &self.triage.me
    }

    /// What the agent's Chat holds, for a caller watching it work.
    #[must_use]
    pub fn hand(&self) -> &ChatHand {
        &self.hand
    }

    /// Run one pass of Chat and take at most one step of triage.
    pub fn step(&mut self) -> Standing {
        self.app.step_headless();
        if !self.hand.permits(&ui::permissions::PermissionScope::action(
            CHAT_APP,
            "remark",
            Verb::Comment,
        )) {
            if self.admitted {
                tracing::info!("triage: removed from the room; stopping");
                return Standing::Revoked;
            }
            return Standing::Waiting;
        }
        self.admitted = true;
        match self.hold_lease() {
            lease::Verdict::Hold => {}
            lease::Verdict::Wait => return Standing::Standby,
            lease::Verdict::Yield => {
                tracing::info!(room = %self.triage.room, "triage: another device's session holds this room; ending");
                return Standing::Yielded;
            }
        }
        for (address, reason) in self.hand.take_refused() {
            tracing::warn!(%address.resource, %address.action, %reason, "triage: Chat refused a request");
        }
        let now = Instant::now();
        self.busy
            .retain(|_, (since, wait)| now.duration_since(*since) < *wait);
        let messages = self.hand.messages(&self.triage.scope.channel);
        let me = self.triage.me.clone();
        self.intents.settle(&|draft| {
            messages
                .iter()
                .find(|m| m.id == draft)
                .and_then(|m| m.parent.as_deref())
                .is_some_and(|report| super::filed(&messages, &me, report))
        });
        let busy = self.busy.keys().cloned().collect();
        if let Some(next) = self.triage.next(&messages, self.think.as_ref(), &busy) {
            self.perform(next);
        }
        Standing::Working
    }

    /// Triage until the agent is removed from the room, or another device's
    /// session holds it.
    pub fn run(&mut self) {
        loop {
            match self.step() {
                Standing::Revoked | Standing::Yielded => return,
                Standing::Waiting | Standing::Standby | Standing::Working => {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }

    /// Read the claims in the room's presence, decide, and restate this
    /// session's claim when it is due.
    fn hold_lease(&mut self) -> lease::Verdict {
        let now = Instant::now();
        let wall = std::time::SystemTime::now();
        let lease = self.lease.get_or_insert_with(|| {
            let device = self
                .app
                .collaboration_device()
                .map_or_else(|| uuid::Uuid::new_v4().simple().to_string(), hex::encode);
            lease::Lease::new(&device, self.lease_timing, now)
        });
        // The same agent run from another team device is the same
        // participant, so its claims arrive as this participant's siblings.
        let presence = self.app.presence_ctx();
        let heard: Vec<(lease::Claim, Duration)> = presence
            .statuses()
            .map(|(_, status)| (status.text.as_str(), status.updated_at))
            .chain(presence.sibling_statuses())
            .filter_map(|(text, heard)| {
                let claim = lease::Claim::parse(text)?;
                Some((claim, wall.duration_since(heard).unwrap_or_default()))
            })
            .collect();
        let verdict = lease.decide(&heard, now, lease::unix_ms(wall));
        tracing::trace!(heard = heard.len(), ?verdict, "triage: lease");
        if verdict != lease::Verdict::Yield
            && let Some(text) = lease.due(now)
        {
            let key = ui::presence::PresenceKey::Workspace {
                id: self.triage.room.clone(),
            };
            self.app.broadcast_status(
                key,
                Some(&ui::LiveStatus {
                    text,
                    origin: ui::StatusOrigin::Manual,
                    updated_at: wall,
                }),
            );
        }
        verdict
    }

    /// Whether this session has claimed its room's lease.
    #[must_use]
    pub fn claimed_lease(&self) -> bool {
        self.lease.as_ref().is_some_and(lease::Lease::claimed)
    }

    fn perform(&mut self, next: Move) {
        let key = next.key();
        let mut wait = REPLY_WAIT;
        match next {
            Move::SuggestDuplicate { report, of, text } => {
                tracing::info!(%report, original = %of.id, "triage: suggesting a duplicate");
                self.reply(&report, &text, &of, None);
            }
            Move::Draft {
                report,
                about,
                draft,
                text,
            } => {
                tracing::info!(%report, "triage: drafting an issue");
                self.reply(&report, &text, &about, Some(&draft));
            }
            Move::File {
                report,
                draft_message,
                draft,
            } => match self.file(&draft_message, &draft) {
                Ok(filed) => {
                    tracing::info!(%report, %draft_message, number = filed.number, "triage: filed an approved issue");
                    self.reply(
                        &report,
                        &format!("Filed as issue #{}.", filed.number),
                        &ResourceRef::whole(ISSUE_KIND, filed.url, format!("#{}", filed.number)),
                        None,
                    );
                }
                Err(reason) => {
                    tracing::warn!(%report, %draft_message, %reason, "triage: could not file an approved issue");
                    wait = FILING_RETRY;
                }
            },
        }
        self.busy.insert(key, (Instant::now(), wait));
    }

    /// File the approved draft on `draft_message`, at most once whatever
    /// happens to this process: the intent is on disk before the forge is
    /// asked, and an intent left by an earlier run is answered by the issue
    /// its marker finds before anything is filed again.
    fn file(
        &mut self,
        draft_message: &str,
        draft: &chat::feedback::IssueDraft,
    ) -> Result<FiledIssue, String> {
        let marker = forge::marker(&self.triage.room, draft_message);
        if self.intents.contains(draft_message) {
            if let Some(found) = self.forge.find(&marker)? {
                tracing::info!(%draft_message, number = found.number, "triage: found the issue an earlier run filed");
                return Ok(found);
            }
        } else {
            self.intents
                .record(draft_message)
                .map_err(|error| format!("could not record the intent to file: {error}"))?;
        }
        self.forge.file(draft, &marker)
    }

    /// Reply in `report`'s thread through Chat's `remark` action: the text,
    /// the address it is about, and a drafted issue when there is one.
    fn reply(
        &self,
        report: &str,
        text: &str,
        about: &ResourceRef,
        issue: Option<&chat::feedback::IssueDraft>,
    ) {
        let mut arguments = serde_json::Map::new();
        arguments.insert("text".into(), text.into());
        arguments.insert(
            "reference".into(),
            serde_json::to_value(about).unwrap_or_default(),
        );
        arguments.insert("parent".into(), report.into());
        if let Some(issue) = issue {
            arguments.insert(
                "issue".into(),
                serde_json::to_value(issue).unwrap_or_default(),
            );
        }
        self.hand.ask(
            ActionAddress::new(
                format!("chat.channel:{}", self.triage.scope.channel),
                "remark",
            ),
            arguments,
        );
    }
}
