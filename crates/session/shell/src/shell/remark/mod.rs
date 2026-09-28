//! The system's remark surface: say something about any part of any app.
//!
//! Posting feedback to Tilo and making a remark to a colleague about a
//! paragraph are one gesture with different destinations
//! (`docs/architecture/feedback.md`). This module is that gesture's state:
//! what the remark is about (the pick), what it says, where it goes, and the
//! recording of what happened that may travel with it. It holds no pixels
//! and no widgets; the composer panel and the pick overlay draw it, and every
//! step is also a shell action (`remark.*`), so a screen-reader user, an
//! agent and `tilo-control` reach exactly what the pointer does.
//!
//! A remark leaves through chat's own actions: [`plan_delivery`] validates it
//! with [`Remark::to_chat_action`] / [`Remark::to_direct_action`], which fail
//! closed, and addresses the conversation it must land in. A private remark
//! is only ever written into the two-member room of its author and recipient.

pub mod act;
/// Replays a session awaiting review out of sight, capturing its surfaces
/// at the offsets and scales its frames were recorded at, so review can
/// check the recording against it. The replay runs as the frame loop
/// drives it; review asks after it from its own pump.
pub trait OffscreenReplay {
    /// Start replaying `record` against `frames`.
    ///
    /// # Errors
    ///
    /// When this device cannot replay the session.
    fn start(
        &self,
        record: &ui::record::RecordId,
        frames: &session_trace::frames::FrameStrip,
    ) -> Result<u64, String>;

    /// The replay's frames once every offset was captured; `None` while it
    /// runs.
    fn take(
        &self,
        check: u64,
    ) -> Option<Result<Box<dyn session_trace::frames::FrameProvider + Send>, String>>;
}

pub mod fixture;
pub mod pick;
pub mod review;
pub mod view;

use serde::Serialize;
use tilo_reference::{
    Destination, DirectRoom, MentionRef, ROOM_CONVERSATION, RecordAttachment, Remark, ResourceRef,
    RoomMember, Undeliverable, resolve_mentions,
};

use pick::{NodeKey, PickScene, Resolved, Selection};

/// Destination id of the private conversation with the Tilo team.
pub const TEAM: &str = "team";
/// Destination id of the public `#feedback` channel of the Tilo Community.
pub const COMMUNITY: &str = "community";
/// Prefix of a person destination id: `person:<id>`.
pub const PERSON_PREFIX: &str = "person:";
/// Prefix of a shared room destination id: `room:<room id>`.
pub const ROOM_PREFIX: &str = "room:";

/// Resource kind of a UI control reference, shared with the `ui:` resources
/// the semantic tree already publishes, so a remark about a control and an
/// action on it name the same thing.
pub const UI_CONTROL_KIND: &str = "tilo.ui.control";
/// Resource kind of a whole app, when nothing narrower was picked.
pub const APP_KIND: &str = "tilo.app";

/// Where a remark can go.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DestinationKind {
    /// The author's direct room with the Tilo team principal.
    Team { team: String },
    /// The public `#feedback` channel of the community room.
    Community {
        room: String,
        channel: String,
        /// The standing invite has been redeemed.
        joined: bool,
    },
    /// Privately, to one person or agent the author shares a room with.
    Person { person: String, is_agent: bool },
    /// The conversation of a room the author is in.
    Room { room: String },
}

/// One choice in the destination selector.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DestinationOption {
    pub id: String,
    pub label: String,
    #[serde(flatten)]
    pub kind: DestinationKind,
}

impl DestinationOption {
    /// Only the author and the addressee can read it.
    pub fn is_private(&self) -> bool {
        matches!(
            self.kind,
            DestinationKind::Team { .. } | DestinationKind::Person { .. }
        )
    }

    /// Anyone who joins can read it.
    pub fn is_public(&self) -> bool {
        matches!(self.kind, DestinationKind::Community { .. })
    }

    /// Leaves every room the author works in, so personal values in a
    /// reference must be masked first.
    pub fn leaves_the_workspace(&self) -> bool {
        matches!(
            self.kind,
            DestinationKind::Team { .. } | DestinationKind::Community { .. }
        )
    }
}

/// What the destination selector is built from. Gathered by the host from
/// the community configuration, the workspace tree and the roster; kept as
/// plain data so the selector's rules are testable without either.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Directory {
    /// The Tilo team principal's person id and label.
    pub team: Option<(String, String)>,
    /// The community room, its `#feedback` channel, and whether it is joined.
    pub community: Option<CommunityRoom>,
    /// People and agents the author shares a room with: `(id, label, agent)`.
    pub people: Vec<RoomMember>,
    /// Rooms with someone else in them: `(room id, name)`.
    pub rooms: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommunityRoom {
    pub room: String,
    pub channel: String,
    pub label: String,
    pub joined: bool,
}

impl Directory {
    /// What the selector starts on: the Tilo team, privately, when there is
    /// one, since a remark with no one in mind is feedback; else the first.
    pub fn default_destination(&self) -> Option<String> {
        self.options().into_iter().next().map(|option| option.id)
    }

    /// Every destination, in the order the selector lists them: Tilo first
    /// (private, then public), then rooms, then people.
    pub fn options(&self) -> Vec<DestinationOption> {
        let mut options = Vec::new();
        if let Some((team, label)) = &self.team {
            options.push(DestinationOption {
                id: TEAM.into(),
                label: label.clone(),
                kind: DestinationKind::Team { team: team.clone() },
            });
        }
        if let Some(community) = &self.community {
            options.push(DestinationOption {
                id: COMMUNITY.into(),
                label: community.label.clone(),
                kind: DestinationKind::Community {
                    room: community.room.clone(),
                    channel: community.channel.clone(),
                    joined: community.joined,
                },
            });
        }
        for (room, name) in &self.rooms {
            options.push(DestinationOption {
                id: format!("{ROOM_PREFIX}{room}"),
                label: name.clone(),
                kind: DestinationKind::Room { room: room.clone() },
            });
        }
        let team = self.team.as_ref().map(|(id, _)| id.as_str());
        for member in &self.people {
            if Some(member.id.as_str()) == team {
                continue;
            }
            options.push(DestinationOption {
                id: format!("{PERSON_PREFIX}{}", member.id),
                label: member.name.clone(),
                kind: DestinationKind::Person {
                    person: member.id.clone(),
                    is_agent: member.is_agent,
                },
            });
        }
        options
    }
}

/// One picked thing, resolved at the moment it was picked so the composer
/// can show it after the tree it came from has moved on.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Pick {
    pub surface: String,
    pub node: String,
    pub app_id: String,
    pub app_label: String,
    /// Room the picked surface runs in.
    pub room: String,
    pub role: String,
    pub label: String,
    /// The address the remark carries.
    pub reference: ResourceRef,
    /// The label is a personal value: shown here, masked when it leaves.
    pub personal: bool,
    /// Screen bounds at pick time, for anchoring the composer.
    #[serde(skip)]
    pub screen: vello::kurbo::Rect,
}

impl Pick {
    pub fn from_resolved(resolved: &Resolved<'_>) -> Self {
        let node = resolved.node;
        let surface = resolved.surface;
        let label = node
            .label
            .clone()
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| surface.app_label.clone());
        let reference = node
            .reference
            .clone()
            .unwrap_or_else(|| ui_control_reference(&node.id, &label));
        Self {
            surface: surface.key.clone(),
            node: node.id.clone(),
            app_id: surface.app_id.clone(),
            app_label: surface.app_label.clone(),
            room: surface.room.clone(),
            role: node.role.clone(),
            label,
            reference,
            personal: node.personal,
            screen: resolved.screen_rect(),
        }
    }

    pub fn key(&self) -> NodeKey {
        NodeKey::new(self.surface.clone(), self.node.clone())
    }

    /// The reference as it may leave the device: a personal label is masked
    /// to its shape, the address is kept.
    pub fn outbound_reference(&self, masked: bool) -> ResourceRef {
        let mut reference = self.reference.clone();
        if masked && self.personal {
            reference.label = mask(&reference.label);
        }
        reference
    }
}

fn rect_json(rect: vello::kurbo::Rect) -> serde_json::Value {
    serde_json::json!({ "x": rect.x0, "y": rect.y0, "width": rect.width(), "height": rect.height() })
}

/// What `state-dump` reports under `chrome.remark`: everything the
/// composer shows, in the terms its actions take.
/// Whether replaying the session under review reproduced its frames: the
/// layer found and, for each region replay does not vouch for, where it is
/// and why (the evidence crops stay on the device).
fn verdict_json(verdict: &review::Verdict) -> serde_json::Value {
    match verdict {
        review::Verdict::Unchecked => serde_json::json!({ "state": "unchecked" }),
        review::Verdict::Unavailable => serde_json::json!({ "state": "unavailable" }),
        review::Verdict::Checking => serde_json::json!({ "state": "checking" }),
        review::Verdict::Failed(reason) => {
            serde_json::json!({ "state": "failed", "reason": reason })
        }
        review::Verdict::Checked(report) => serde_json::json!({
            "state": "checked",
            "layer": report.layer,
            "compared": report.compared,
            "regions": report.steps.iter().flat_map(|step| step.regions.iter().map(move |region| {
                serde_json::json!({
                    "at_ms": step.at.as_millis(),
                    "app": step.route.app_id,
                    "element": region.element,
                    "bounds": region.bounds,
                    "divergence": region.divergence,
                })
            })).collect::<Vec<_>>(),
        }),
    }
}

pub fn status_json(st: &crate::shell::chrome_state::ChromeState) -> serde_json::Value {
    let composer = &st.remark;
    let surfaces: Vec<serde_json::Value> = st
        .pick
        .scene
        .surfaces
        .iter()
        .map(|surface| {
            serde_json::json!({
                "surface": surface.key,
                "app": surface.app_id,
                "room": surface.room,
                "screen": rect_json(surface.screen),
                "nodes": surface.nodes.iter().map(|node| serde_json::json!({
                    "id": node.id,
                    "role": node.role,
                    "label": node.label,
                    "personal": node.personal,
                    "screen": rect_json(surface.to_screen(node.bounds)),
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let review = composer.review.as_ref().map(|review| {
        serde_json::json!({
            "record": review.record,
            "model": review.model,
            "selected": review.selected,
            "redactions": review.redaction_count(),
            "verdict": verdict_json(&review.verdict),
            // The replays checked out of sight for it, and how far each got.
            "checks": st.session_view().replay.as_ref().map(|replay| replay.checks.clone()),
            "steps": review.steps.iter().map(|step| serde_json::json!({
                "step": step.index,
                "title": review.title(step.index),
                "app": step.app_id,
                "failed": step.failed(),
                "cut": review.is_cut(step.index),
                "texts": review.texts(step.index).iter().map(|text| serde_json::json!({
                    "node": text.text.node,
                    "sent": review.redacted(step.index, &text.text.node),
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })
    });
    serde_json::json!({
        "open": composer.is_open(),
        "stage": composer.stage,
        "opening": composer.opening,
        "text": composer.text,
        "picks": composer.picks,
        "context": composer.context,
        "destination": composer.destination,
        "destinations": st.remark_directory().options(),
        "flow": composer.flow,
        "include_frames": composer.include_frames,
        "sending": composer.sending,
        "blocked": composer.send_blocker(&st.remark_directory().options()),
        "review": review,
        "pick": {
            "surfaces": surfaces,
            "selection": st.pick.selection.keys(),
            "focus": st.pick.focus,
        },
        "last": st.last_remark,
    })
}

/// The notification an app that quit unexpectedly leaves: its default
/// action opens the composer about that app, with the last two minutes
/// when the recent buffer kept them.
pub fn crash_notification(
    display_name: &str,
    package_id: &str,
    buffered: bool,
) -> crate::shell::notification::Notification {
    use crate::shell::i18n::{t, t_args, text};
    crate::shell::notification::Notification::new(
        t_args("remark-crash-title", &[("app", text(display_name))]),
        t(if buffered {
            "remark-crash-body"
        } else {
            "remark-crash-body-no-buffer"
        }),
    )
    .with_severity(crate::shell::notification::NotificationSeverity::Error)
    .with_app_name(display_name.to_string())
    .with_shell_action(
        t("remark-crash-action"),
        "remark.crash",
        serde_json::json!({ "app": package_id, "label": display_name }),
    )
}

/// Whether the shell action `id` is offered while the composer is in
/// `composer`'s state: opening always, every other step only while a
/// remark is being written.
pub fn offered(id: &str, composer: &Composer) -> bool {
    !id.starts_with("remark.")
        || matches!(
            id,
            "remark.open" | "remark.crash" | "remark.conversation.open"
        )
        || composer.is_open()
}

/// The app a tab shows, as `(app id, label)`: what a remark opened with
/// nothing picked, or about a whole window, is about.
pub fn tab_app(tab: &crate::shell::state::Tab) -> Option<(String, String)> {
    use crate::shell::state::TileContent;
    match &tab.tile {
        TileContent::Foreign { app_id, .. }
        | TileContent::LocalApp { app_id, .. }
        | TileContent::PeerWindow { app_id, .. } => {
            let label = if tab.title.trim().is_empty() {
                app_id.clone()
            } else {
                tab.title.clone()
            };
            Some((app_id.clone(), label))
        }
        TileContent::AppPlaceholder { kind, label } | TileContent::Stub { kind, label } => {
            Some((format!("{kind:?}"), label.clone()))
        }
        TileContent::WorkspaceActivity => None,
    }
}

/// A UI control's reference: the same kind and id `a11y_tree` publishes the
/// control's `ui:` resource under.
pub fn ui_control_reference(node: &str, label: &str) -> ResourceRef {
    let encoded = percent_encoding::utf8_percent_encode(node, percent_encoding::NON_ALPHANUMERIC);
    ResourceRef::whole(UI_CONTROL_KIND, format!("ui:{encoded}"), label)
}

/// A whole app, the reference of a remark opened with nothing picked.
pub fn app_reference(app_id: &str, label: &str) -> ResourceRef {
    ResourceRef::whole(APP_KIND, format!("app:{app_id}"), label)
}

/// A value's shape without its content, so a same-length name still shows a
/// truncation bug: every non-space character becomes the review mask.
pub fn mask(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_whitespace() {
                c
            } else {
                tilo_feedback_review::MASK
            }
        })
        .collect()
}

/// Where the composer stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Nothing is open.
    #[default]
    Closed,
    /// Writing, choosing where it goes, deciding what to attach.
    Compose,
    /// Pick mode is over the desktop; the composer waits behind it.
    Picking,
    /// Walking through the recording before it is sent.
    Review,
    /// Handed to chat; waiting for the destination to accept it.
    Sending,
    /// Recording what happens: the composer steps aside so the person can
    /// show it, and the Recorder's pill, with Stop, is all that remains.
    Recording,
}

/// Why the composer was opened.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Opening {
    /// The gesture, Quick Settings or Ctrl+K.
    #[default]
    Gesture,
    /// An app exited abnormally; the remark is about it.
    Crash { app_id: String, app_label: String },
}

/// The recording that may travel with the remark.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Flow {
    /// Nothing recorded.
    #[default]
    None,
    /// The recorder is running for this remark.
    Recording { rooms: Vec<String> },
    /// The last two minutes are being sealed.
    Capturing,
    /// A sealed session waits for review; its record id and summary.
    Attached {
        record: String,
        summary: tilo_reference::SessionSummary,
    },
}

/// Work the composer hands the shell host, drained once per frame: what
/// needs the broker, the gateway or the record store, none of which a shell
/// action may block on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Ask every surface on screen for its tree again.
    RefreshPick,
    /// Build the walk-through of the attached session and ask the review
    /// model about it.
    OpenReview,
    /// Offer to set up the review model in place, as the hub offers it.
    SetUpReviewModel,
    /// Set up the offered choice the person confirmed, through the hub's
    /// own action.
    ConfirmReviewModel { action: String },
    /// Deliver the remark.
    Send,
}

/// Where sending stands, once asked.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Sending {
    #[default]
    Idle,
    /// Opening the conversation and handing the remark to chat.
    InFlight {
        step: String,
    },
    /// Delivered: the room and channel it landed in.
    Sent {
        room: String,
        channel: String,
        record: Option<String>,
    },
    Failed {
        reason: String,
    },
}

/// Something the composer has to tell the person, until they act on it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Notice {
    pub key: &'static str,
    pub detail: Option<String>,
}

/// The composer: one remark being written.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Composer {
    pub stage: Stage,
    pub opening: Opening,
    pub text: String,
    pub picks: Vec<Pick>,
    /// Chosen destination id; `None` until a directory offered one.
    pub destination: Option<String>,
    pub flow: Flow,
    /// Keep the recorded surfaces' frames with the session.
    pub include_frames: bool,
    #[serde(skip)]
    pub review: Option<review::Review>,
    pub notice: Option<Notice>,
    /// A fallback reference when nothing was picked: the focused app.
    pub context: Option<Pick>,
    /// Sessions already waiting for review when the composer opened, so the
    /// one this remark records is the next to appear.
    #[serde(skip)]
    pub known_sessions: Vec<String>,
    pub sending: Sending,
}

impl Composer {
    pub fn is_open(&self) -> bool {
        self.stage != Stage::Closed
    }

    /// Open on what is picked or selected. `context` is the focused app (or
    /// the crashed one), used when nothing narrower is picked.
    pub fn open(&mut self, opening: Opening, context: Option<Pick>, default: Option<String>) {
        if self.is_open() {
            // The gesture again while open goes back to picking: the
            // quickest way to add another thing to the same remark.
            if self.stage == Stage::Compose {
                self.stage = Stage::Picking;
            }
            return;
        }
        *self = Self {
            stage: if context.is_some() && matches!(opening, Opening::Crash { .. }) {
                Stage::Compose
            } else {
                Stage::Picking
            },
            opening,
            destination: default,
            context,
            ..Self::default()
        };
    }

    pub fn close(&mut self) {
        *self = Self::default();
    }

    /// Pick mode's result: the selection becomes the remark's reference.
    pub fn finish_picking(&mut self, scene: &PickScene, selection: &Selection) {
        let picks: Vec<Pick> = selection
            .keys()
            .iter()
            .filter_map(|key| scene.resolve(key))
            .map(|resolved| Pick::from_resolved(&resolved))
            .collect();
        for pick in picks {
            if !self.picks.iter().any(|known| known.key() == pick.key()) {
                self.picks.push(pick);
            }
        }
        self.stage = Stage::Compose;
    }

    /// Leave pick mode without adding anything. With nothing picked and no
    /// context the remark has nothing to be about, so the composer closes.
    pub fn cancel_picking(&mut self) {
        if self.picks.is_empty() && self.context.is_none() {
            self.close();
        } else {
            self.stage = Stage::Compose;
        }
    }

    /// The reference the remark carries: the first pick, else the context.
    pub fn primary(&self) -> Option<&Pick> {
        self.picks.first().or(self.context.as_ref())
    }

    pub fn remove_pick(&mut self, index: usize) -> bool {
        if index < self.picks.len() {
            self.picks.remove(index);
            true
        } else {
            false
        }
    }

    /// Whether Send can do anything yet, and if not, why.
    pub fn send_blocker(&self, options: &[DestinationOption]) -> Option<&'static str> {
        if self.text.trim().is_empty() {
            return Some("remark-blocked-empty");
        }
        if self.primary().is_none() {
            return Some("remark-blocked-nothing-picked");
        }
        let Some(destination) = self.destination.as_deref() else {
            return Some("remark-blocked-no-destination");
        };
        if !options.iter().any(|option| option.id == destination) {
            return Some("remark-blocked-no-destination");
        }
        match self.flow {
            Flow::Recording { .. } => Some("remark-blocked-recording"),
            Flow::Capturing => Some("remark-blocked-capturing"),
            Flow::Attached { .. } if self.review.is_none() => Some("remark-blocked-review"),
            Flow::None | Flow::Attached { .. } => None,
        }
    }

    /// Build the remark as it will leave: text, the outbound reference, the
    /// attachment. `members` resolves `@name` mentions for a room.
    pub fn remark(
        &self,
        option: &DestinationOption,
        members: &[RoomMember],
        attachment: Option<RecordAttachment>,
    ) -> Option<Remark> {
        let primary = self.primary()?;
        let masked = option.leaves_the_workspace();
        let destination = match &option.kind {
            DestinationKind::Team { team } => Destination::participant(team, &option.label, false),
            DestinationKind::Person { person, is_agent } => {
                Destination::participant(person, &option.label, *is_agent)
            }
            DestinationKind::Community { channel, .. } => {
                Destination::channel(channel, &option.label)
            }
            DestinationKind::Room { .. } => Destination::channel(ROOM_CONVERSATION, &option.label),
        };
        let mentions: Vec<MentionRef> = if option.is_private() {
            Vec::new()
        } else {
            resolve_mentions(&self.text, members)
        };
        let mut remark = Remark::new(
            self.text.trim(),
            primary.outbound_reference(masked),
            destination,
        )
        .with_mentions(mentions);
        if let Some(attachment) = attachment {
            remark = remark.with_attachment(attachment);
        }
        Some(remark)
    }
}

/// `remark` with every value review hid masked in the labels of what it is
/// about, as it is in the recording: a picked field is labelled by its
/// content.
pub fn hide_reviewed(mut remark: Remark, masker: &session_trace::redact::Masker) -> Remark {
    remark.reference.label = masker.mask(&remark.reference.label);
    for reference in &mut remark.also {
        reference.label = masker.mask(&reference.label);
    }
    remark
}

/// A remark addressed: which room's conversation it is written into, and the
/// arguments chat's `remark` action takes there.
#[derive(Clone, Debug, PartialEq)]
pub struct Delivery {
    /// The room the chat conversation belongs to.
    pub room: String,
    /// The id the message is posted under, named before anything is
    /// written so the records that travel with it can be named after it.
    pub message: String,
    /// The chat channel within it.
    pub channel: String,
    pub private: bool,
    pub public: bool,
    pub remark: Remark,
}

impl Delivery {
    /// Chat's `remark` action arguments: the remark's own fields.
    pub fn arguments(&self) -> serde_json::Value {
        let mut args = serde_json::json!({
            "text": self.remark.text,
            "reference": self.remark.reference,
            "message": self.message,
        });
        if let Some(attachment) = &self.remark.attachment
            && let Some(object) = args.as_object_mut()
        {
            object.insert(
                "attachment".into(),
                serde_json::to_value(attachment).unwrap_or(serde_json::Value::Null),
            );
        }
        if !self.remark.also.is_empty()
            && let Some(object) = args.as_object_mut()
        {
            object.insert(
                "references".into(),
                serde_json::to_value(&self.remark.also).unwrap_or(serde_json::Value::Null),
            );
        }
        if !self.remark.mentions.is_empty()
            && let Some(object) = args.as_object_mut()
        {
            object.insert(
                "mentions".into(),
                serde_json::to_value(&self.remark.mentions).unwrap_or(serde_json::Value::Null),
            );
        }
        args
    }
}

/// Address `remark` to `option`, refusing anything the chat encoders refuse.
///
/// A private remark goes to the author's direct room with the recipient and
/// nowhere else; a public or room remark goes to its channel. `id` and `ts`
/// are minted by the caller, as every Tilo action requires.
///
/// # Errors
///
/// [`Undeliverable`] when the remark's delivery and the option disagree, or
/// the direct room is not the author's and the recipient's.
/// The record the message `message` is addressed by once chat posts it
/// where `option` says: this device's own comment record on the feedback
/// channel in the community room, or the message's place in a room's
/// conversation.
pub fn message_record(
    option: &DestinationOption,
    message: &str,
    device: [u8; 32],
) -> Option<ui::record::RecordId> {
    match &option.kind {
        DestinationKind::Community { channel, .. } => ui::record::RecordId::for_comment(
            device,
            message,
            &ui::record::RecordId::parse(&tilo_reference::chat_channel_record(channel))?,
        ),
        DestinationKind::Team { .. }
        | DestinationKind::Person { .. }
        | DestinationKind::Room { .. } => {
            ui::record::RecordId::parse(&tilo_reference::chat_conversation_message_record(message))
        }
    }
}

/// The local name of a report's recording on its message.
pub const RECORDING_LOCAL: &str = "recording";

/// The record a remark's reviewed recording is written as: this device's
/// own comment record on the message it travels with. The reporter owns
/// their report everywhere, and a comment record is what a Commenter may
/// write, so it can be served in any room the message goes to.
pub fn recording_record(
    option: &DestinationOption,
    message: &str,
    device: [u8; 32],
) -> Option<ui::record::RecordId> {
    ui::record::RecordId::for_comment(
        device,
        RECORDING_LOCAL,
        &message_record(option, message, device)?,
    )
}

/// # Errors
///
/// When the remark cannot be delivered where `option` says.
pub fn plan_delivery(
    remark: &Remark,
    option: &DestinationOption,
    me: &str,
    id: &str,
    ts: i64,
) -> Result<Delivery, Undeliverable> {
    let (room, channel) = match &option.kind {
        DestinationKind::Team { team: to } | DestinationKind::Person { person: to, .. } => {
            let direct =
                DirectRoom::between(me, to).ok_or_else(|| Undeliverable::NotThisPairsRoom {
                    to: to.clone(),
                    room: String::new(),
                })?;
            remark.to_direct_action(&direct, id, me, ts)?;
            (direct.room_id(), ROOM_CONVERSATION.to_string())
        }
        DestinationKind::Community { room, channel, .. } => {
            remark.to_chat_action(id, me, ts)?;
            (room.clone(), channel.clone())
        }
        DestinationKind::Room { room } => {
            remark.to_chat_action(id, me, ts)?;
            (room.clone(), ROOM_CONVERSATION.to_string())
        }
    };
    Ok(Delivery {
        room,
        message: id.to_string(),
        channel,
        private: option.is_private(),
        public: option.is_public(),
        remark: remark.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_reference::Delivery as ChatDelivery;

    const ME: &str = "u_1f4c3a9b2d7e5061";
    const TEAM_ID: &str = "u_7ea7ea7ea7ea7ea7";
    const ADA: &str = "u_9a0b1c2d3e4f5061";

    fn directory() -> Directory {
        Directory {
            team: Some((TEAM_ID.into(), "Tilo team".into())),
            community: Some(CommunityRoom {
                room: "community-room".into(),
                channel: "c_feedback".into(),
                label: "Tilo Community".into(),
                joined: true,
            }),
            people: vec![
                RoomMember::new(ADA, "Ada", false),
                RoomMember::new(TEAM_ID, "Tilo team", false),
            ],
            rooms: vec![("room-1".into(), "Launch plan".into())],
        }
    }

    fn pick(personal: bool) -> Pick {
        Pick {
            surface: "tab:1".into(),
            node: "send".into(),
            app_id: "so.tilo.mail".into(),
            app_label: "Mail".into(),
            room: "room-1".into(),
            role: "button".into(),
            label: "Ada Lovelace".into(),
            reference: ui_control_reference("send", "Ada Lovelace"),
            personal,
            screen: vello::kurbo::Rect::ZERO,
        }
    }

    fn composer(text: &str, destination: &str) -> Composer {
        Composer {
            stage: Stage::Compose,
            text: text.into(),
            picks: vec![pick(true)],
            destination: Some(destination.into()),
            ..Composer::default()
        }
    }

    fn option(id: &str) -> DestinationOption {
        directory()
            .options()
            .into_iter()
            .find(|option| option.id == id)
            .expect("listed")
    }

    #[test]
    fn tilo_comes_first_and_the_team_is_not_listed_twice() {
        let ids: Vec<String> = directory().options().into_iter().map(|o| o.id).collect();
        assert_eq!(
            ids,
            vec![
                TEAM.to_string(),
                COMMUNITY.to_string(),
                "room:room-1".to_string(),
                format!("person:{ADA}"),
            ]
        );
        assert!(option(TEAM).is_private());
        assert!(option(COMMUNITY).is_public());
        assert!(!option("room:room-1").is_private());
    }

    #[test]
    fn a_private_remark_to_the_team_lands_only_in_the_pair_room() {
        let composer = composer("Send does nothing", TEAM);
        let team = option(TEAM);
        let remark = composer.remark(&team, &[], None).expect("a remark");
        let delivery = plan_delivery(&remark, &team, ME, "m_1", 42).expect("deliverable");
        assert_eq!(
            delivery.room,
            DirectRoom::between(ME, TEAM_ID).unwrap().room_id()
        );
        assert_eq!(delivery.channel, ROOM_CONVERSATION);
        assert!(delivery.private);
        assert_eq!(
            remark.delivery(),
            ChatDelivery::Private { to: TEAM_ID.into() }
        );
    }

    #[test]
    fn a_personal_label_is_masked_when_it_leaves_and_kept_for_a_room() {
        let composer = composer("look", TEAM);
        let to_team = composer.remark(&option(TEAM), &[], None).unwrap();
        assert_eq!(to_team.reference.label, "▮▮▮ ▮▮▮▮▮▮▮▮");
        assert_eq!(
            to_team.reference.id,
            ui_control_reference("send", "").id,
            "the address survives the mask"
        );
        let to_room = composer.remark(&option("room:room-1"), &[], None).unwrap();
        assert_eq!(to_room.reference.label, "Ada Lovelace");
    }

    #[test]
    fn a_public_remark_goes_to_the_feedback_channel_of_the_community_room() {
        let composer = composer("Crash when pasting", COMMUNITY);
        let community = option(COMMUNITY);
        let remark = composer.remark(&community, &[], None).unwrap();
        let delivery = plan_delivery(&remark, &community, ME, "m_2", 42).unwrap();
        assert_eq!(delivery.room, "community-room");
        assert_eq!(delivery.channel, "c_feedback");
        assert!(delivery.public && !delivery.private);
    }

    /// A report's recording is the reporter's own comment record on the
    /// message it travels with, whether that goes to the community's
    /// feedback channel or to a direct room: named before anything is
    /// written, after the message chat posts it as.
    #[test]
    fn a_recording_is_named_as_the_reporters_comment_on_its_message() {
        let device = [0x42; 32];
        for (destination, message_record) in [
            (
                COMMUNITY,
                ui::record::RecordId::for_comment(
                    device,
                    "m_3",
                    &ui::record::RecordId::parse(&tilo_reference::chat_channel_record(
                        "c_feedback",
                    ))
                    .unwrap(),
                )
                .unwrap(),
            ),
            (
                TEAM,
                ui::record::RecordId::parse(&tilo_reference::chat_conversation_message_record(
                    "m_3",
                ))
                .unwrap(),
            ),
        ] {
            let option = option(destination);
            let recording = recording_record(&option, "m_3", device).expect("a name");
            assert_eq!(recording.comment_author(), Some(device), "{destination}");
            assert_eq!(
                recording.comment_parent(),
                Some(message_record),
                "{destination}: parented on the message chat posts"
            );
            assert_eq!(
                collab_types::document_op_class_for_record(recording.as_str()),
                collab_types::OpClass::Comment,
                "{destination}: writable by a Commenter"
            );
            let remark = composer("It froze", destination)
                .remark(&option, &[], None)
                .unwrap();
            let delivery = plan_delivery(&remark, &option, ME, "m_3", 42).unwrap();
            assert_eq!(
                delivery.arguments()["message"],
                "m_3",
                "{destination}: chat posts it under the id the recording is named after"
            );
        }
    }

    #[test]
    fn mentions_ping_in_a_room_and_never_widen_a_private_remark() {
        let members = [RoomMember::new(ADA, "Ada", false)];
        let composer = composer("@Ada is this right?", "room:room-1");
        let room = composer
            .remark(&option("room:room-1"), &members, None)
            .unwrap();
        assert_eq!(room.mentions.len(), 1);
        let private = composer.remark(&option(TEAM), &members, None).unwrap();
        assert!(private.mentions.is_empty());
        assert!(private.delivery().is_private());
    }

    #[test]
    fn send_explains_what_it_is_waiting_for() {
        let options = directory().options();
        let mut composer = composer("", TEAM);
        assert_eq!(
            composer.send_blocker(&options),
            Some("remark-blocked-empty")
        );
        composer.text = "It froze".into();
        assert_eq!(composer.send_blocker(&options), None);
        composer.flow = Flow::Recording {
            rooms: vec!["room-1".into()],
        };
        assert_eq!(
            composer.send_blocker(&options),
            Some("remark-blocked-recording")
        );
        composer.flow = Flow::None;
        composer.destination = Some("person:nobody".into());
        assert_eq!(
            composer.send_blocker(&options),
            Some("remark-blocked-no-destination")
        );
    }

    #[test]
    fn cancelling_the_first_pick_with_nothing_to_be_about_closes() {
        let mut composer = Composer::default();
        composer.open(Opening::Gesture, None, Some(TEAM.into()));
        assert_eq!(composer.stage, Stage::Picking);
        composer.cancel_picking();
        assert!(!composer.is_open());

        composer.open(Opening::Gesture, Some(pick(false)), Some(TEAM.into()));
        composer.cancel_picking();
        assert_eq!(composer.stage, Stage::Compose);
        assert_eq!(composer.primary().map(|p| p.node.as_str()), Some("send"));
    }

    #[test]
    fn a_crash_opens_straight_on_the_composer_about_that_app() {
        let mut composer = Composer::default();
        let context = Pick {
            reference: app_reference("so.tilo.mail", "Mail"),
            ..pick(false)
        };
        composer.open(
            Opening::Crash {
                app_id: "so.tilo.mail".into(),
                app_label: "Mail".into(),
            },
            Some(context),
            Some(TEAM.into()),
        );
        assert_eq!(composer.stage, Stage::Compose);
        assert_eq!(
            composer.primary().map(|p| p.reference.kind.as_str()),
            Some(APP_KIND)
        );
    }
}
