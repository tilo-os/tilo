//! Root widget for the chat app.
//!
//! [`ChatApp`] owns the CRDT-derived snapshot (channels / categories /
//! users / messages / drafts), the per-peer view state (active channel,
//! open thread, call state, composer caret), and one real
//! [`ui::widget::VideoPlayer`] used as the host call tile. It does its own
//! manual layout: the conversation (timeline and composer) beside the
//! shell's channel sidebar, plus floating layers
//! (thread panel, emoji picker, edit-history popover, call huddle /
//! expanded overlay) are computed from `self.rect` each frame, and every
//! clickable area is recorded into `self.hits` for the next event.
//!
//! Convergent state lives in the Automerge document; this widget is a
//! projection + an input router, in keeping with the toolkit's
//! "state lives in the CRDT" contract.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{Receiver, SyncSender};
use ui::widget::TaffyTreeExt;
use ui::widget::paint_damage::PaintParts;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::percent;
use taffy::{LengthPercentageAuto, NodeId, Position, Size, Style, TaffyTree};
use tilo_content::ContentStore;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use ui::Widget;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Collaborative, Continuable, NavFeed, SidebarDrawer, VideoPlayer, WindowBarLink};

use crate::call::CallState;
use crate::geometry::Chrome;
use crate::identity::LocalIdentity;
use crate::model::{self, Channel, Id, Message, User};
use crate::permissions::ChatPermissions;

// --- Floating call huddle home placement (fractions of the window) --------
// The docked huddle's resting video rect before the user drags it. The
// VideoPlayer's taffy node and the surrounding chrome derive from this so
// they stay aligned; the drag offset is applied on top in `apply_layout`.
// (The expanded overlay instead tiles the host into a responsive grid — see
// `overlay_tiles` — so it has no fixed fraction.)
pub(crate) const HUDDLE_VIDEO: (f64, f64, f64, f64) = (0.505, 0.300, 0.250, 0.230);

/// What the room's private conversations and drafts are keyed by. Chat runs
/// in one Tilo workspace, its room, and holds no workspaces of its own: an
/// agent conversation, a private draft and a call continuation name the
/// room they belong to by this one scope.
pub(crate) const ROOM_SCOPE: &str = "room";

/// Which composer currently owns keystrokes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Focus {
    Main,
    Thread,
}

/// The bounded worker's result. File I/O and header sniffing happen before
/// this crosses back to the UI thread; the only UI-thread write is the
/// canonical hash-only descriptor + `SendMessage` action in `sync`.
pub(crate) enum AttachmentImportResult {
    Ready {
        token: u64,
        name: String,
        blob: String,
        size: u64,
        width: u32,
        height: u32,
        video: bool,
    },
    Failed {
        token: u64,
        reason: String,
    },
}

/// The picker enumerates files off-thread too: even metadata on a mounted or
/// hostile workspace must not stall the composer click. A workspace that
/// cannot be read lists nothing: this device's files are still offered.
pub(crate) struct AttachmentPickerResult {
    pub generation: u64,
    pub entries: Vec<crate::picker::PickEntry>,
}

#[derive(Clone, Debug)]
pub(crate) struct AttachmentImportPending {
    pub token: u64,
    pub channel: Id,
}

#[derive(Clone, Debug)]
pub(crate) struct AgentConversationDrag {
    pub(crate) peer: String,
    pub(crate) channel: Id,
    pub(crate) origin: Point,
}

/// How many of `messages`' last `unread` top-level messages mention one of
/// `me` and were written by someone else.
pub(super) fn unread_mentions(messages: &[Message], unread: usize, me: &[&str]) -> u32 {
    let top: Vec<&Message> = messages
        .iter()
        .filter(|message| message.is_top_level())
        .collect();
    let count = top
        .iter()
        .skip(top.len().saturating_sub(unread))
        .filter(|message| !me.contains(&message.author.as_str()))
        .filter(|message| {
            message
                .mentions
                .iter()
                .any(|mention| me.contains(&mention.user.as_str()))
        })
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// The whole app's mutable state.
#[allow(
    clippy::struct_excessive_bools,
    reason = "the bools split across unrelated concerns (attachment-picker/emoji-popover open flags, layout/render bookkeeping, and --shots-only staging), so grouping them would need a real design pass rather than a mechanical lint fix"
)]
pub struct ChatApp {
    pub(crate) host_model: crate::host_model::HostModel,
    pub(crate) expanded_reasoning: std::collections::HashSet<String>,
    // CRDT projection (rebuilt every sync / after a mutation).
    pub(crate) channels: Vec<Channel>,
    pub(crate) categories: Vec<crate::model::Category>,
    /// People evidenced in the room's chat: the local person plus authors,
    /// reactors, mentioned people, and voice participants in its channels.
    pub(crate) users: Vec<User>,
    pub(crate) messages: Vec<Message>,
    /// Whole-document snapshots used by global search. The visible `messages`
    /// are the open channel's.
    pub(crate) search_messages: Vec<Message>,
    pub(crate) search_users: Vec<User>,
    /// The reviewed feedback reports filed here: each one's id and text.
    pub(crate) reports: Vec<(Id, String)>,
    /// The items a search hit or an agent's `open` asked to show, by their
    /// published ids, shown on the next sync.
    pub(crate) opened: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    /// Voice membership per channel of the room (cached from the
    /// CRDT each reload so the sidebar can show presence without re-reading).
    pub(crate) voice_members: HashMap<Id, Vec<crate::model::VoiceMember>>,
    /// Cached plain text of the focused drafts, mirrored from the composers.
    /// Kept only so the composer's auto-grow height (`composer_height`) and
    /// the tests can read the draft without reaching into the editor; the
    /// source of truth is the draft in the document.
    pub(crate) draft_main: String,
    pub(crate) draft_thread: String,

    /// The message composers: the SDK rich-text editor over each surface's
    /// shared draft. Enter sends, Shift+Enter breaks the line.
    pub(crate) composer_main: crate::composer::DraftComposer,
    /// Why the last message was not sent, while its text waits in the
    /// composer to be sent again.
    pub(crate) not_sent: Option<String>,
    /// A message of the person's own they just deleted, while their Undo
    /// can take it back: the part of the chat it was in, and the channel
    /// it was deleted from.
    /// Any later step of theirs in the chat takes the offer down: Undo
    /// would take back that step instead.
    pub(crate) deleted: std::cell::RefCell<Option<(u64, Id)>>,
    /// A message of the person's own whose deletion they were asked to
    /// confirm, with the part it is in and its channel: once it is gone,
    /// their Undo is offered as for one deleted at once.
    pub(crate) asked_delete: Option<(ui::tilo_resource::Id, Option<u64>, Id)>,
    /// The message the pointer is over, which shows its action bar.
    pub(crate) hovered_message: Option<Id>,
    /// Where each message row was painted this frame, whole: what the
    /// pointer is over when it is over a message.
    pub(crate) message_rows: Vec<(Rect, Id)>,
    /// The action bar's arrival: which message it is over, and its fade.
    pub(crate) message_bar: crate::app::message_bar::BarMotion,
    /// The last message posted from the main composer, by channel: put back
    /// in the composer if the room refuses it after it was applied here.
    pub(crate) last_post: Option<(Id, String)>,
    /// The open sheet that makes a channel or a category, or changes a
    /// channel.
    pub(crate) channel_sheet: Option<crate::channel_sheet::ChannelSheet>,
    /// The detail drawer shows the open channel's pinned messages.
    pub(crate) pins_open: bool,
    /// The top of the main composer as last drawn, `None` when none was. A
    /// detail drawer floating over the conversation ends above it, so the
    /// composer stays whole and usable while pins or people are open.
    pub(crate) composer_top: Option<f64>,
    pub(crate) composer_thread: crate::composer::DraftComposer,

    // Per-peer view state.
    pub(crate) active_channel: Id,
    pub(crate) focus: Focus,
    /// Offset from the newest edge of the timeline. Zero means the reader is
    /// looking at the newest message; negative values move back through
    /// history. The renderer owns its exact bounds because wrapping and media
    /// cards determine them.
    pub(crate) scroll: f64,
    /// Independent, top-origin thread offset and production paint metrics.
    pub(crate) thread_scroll: ui::widget::Scrollable,
    pub(crate) thread_follow: bool,
    pub(crate) thread_body: Rect,
    /// A durable read watermark is only advanced while the reader can actually
    /// see the newest edge. This flag is local presentation state, refreshed
    /// from the measured timeline during paint.
    pub(crate) timeline_at_end: bool,
    /// A person is looking at this instance: a runner laid it out in a real
    /// viewport. A hosted/headless worker only pumps the model and never lays
    /// out, so it must not author read receipts under the host identity —
    /// read state belongs to the person's own device.
    pub(crate) presented: bool,
    /// The first layout just happened, or the timeline just arrived at its
    /// newest edge: request one more model frame so the channel in view is
    /// marked read without waiting for input.
    pub(crate) read_on_present: bool,
    /// Last measured timeline height. Keeps a reader's visual anchor stable if
    /// new messages arrive while they are reading older messages.
    pub(crate) timeline_total_height: f64,
    pub(crate) scroll_content: f64,
    pub(crate) scroll_viewport: f64,
    pub(crate) thread_open: Option<Id>,
    pub(crate) revealed_message: Option<Id>,
    pub(crate) revealed_range: Option<(Id, String, usize, usize)>,
    /// A search result needs one initial scroll into view. Keeping this
    /// separate from `revealed_message` lets its visual accent remain while
    /// the reader subsequently scrolls through nearby context.
    pub(crate) reveal_scroll_pending: bool,
    pub(crate) revealed_person: Option<Id>,
    pub(crate) emoji_open: bool,
    pub(crate) emoji_target: Option<Id>,
    pub(crate) emoji_anchor: Rect,
    /// The toolkit's shared picker, placed at `emoji_anchor` while
    /// `emoji_open`.
    pub(crate) emoji_picker: ui::widget::EmojiPicker,
    /// Each message's reaction pills, in the timeline (`false`) and the
    /// thread (`true`), with their arrival and count motion.
    pub(crate) reaction_strips:
        std::cell::RefCell<HashMap<(bool, Id), ui::widget::reaction_strip::ReactionStrip>>,
    pub(crate) history_open: Option<Id>,
    pub(crate) history_anchor: Point,
    /// Your own message currently being edited. The shared multiline input
    /// owns its working text, caret, selection and IME composition until Save
    /// submits the record's `edit-message`.
    pub(crate) editing: Option<Id>,
    pub(crate) edit_input: crate::composer::MessageEditor,

    // Attachment picker (browses the shared workspace for a file to share).
    pub(crate) attach_open: bool,
    pub(crate) attach_entries: Vec<crate::picker::PickEntry>,
    /// Exactly one import may run at a time. This is a deliberate resource
    /// bound: selecting another file while one is reading is refused rather
    /// than creating unbounded reader threads and byte buffers.
    pub(crate) attachment_pending: Option<AttachmentImportPending>,
    /// The platform file chooser, while it is open: the files chosen on
    /// this device arrive here with their bytes.
    pub(crate) device_pick: Option<ui::portal::ContentsChannel>,
    /// Files chosen, pasted or dropped that wait for the one import at a
    /// time: their names and bytes.
    pub(crate) device_files: VecDeque<(String, Vec<u8>)>,
    pub(crate) attachment_next_token: u64,
    pub(crate) attachment_result_tx: SyncSender<AttachmentImportResult>,
    pub(crate) attachment_result_rx: Receiver<AttachmentImportResult>,
    /// A settled import outcome for the picker/status UI and accessibility.
    pub(crate) attachment_status: Option<String>,
    /// The attachment the person opened over the conversation, and the copy
    /// they asked to save.
    pub(crate) attachment_view: Option<crate::attachment_view::AttachmentView>,
    pub(crate) attachment_save: Option<crate::attachment_view::AttachmentSave>,
    pub(crate) attachment_picker_pending: Option<u64>,
    pub(crate) attachment_picker_generation: u64,
    pub(crate) attachment_picker_result_tx: SyncSender<AttachmentPickerResult>,
    pub(crate) attachment_picker_result_rx: Receiver<AttachmentPickerResult>,

    // Structure editing.
    pub(crate) menu: Option<ContextMenu>,
    /// The channel or category whose name a menu asked to change, until
    /// the event that asked hands the sidebar the keyboard.
    pub(crate) sidebar_rename: Option<String>,

    // Call state.
    pub(crate) call: CallState,
    pub(crate) native_voice: crate::local_voice::LocalVoice,
    pub(crate) resource_navigation: crate::shared_resources::Navigation,
    /// Windows shared live into the room ([`crate::live_share`]).
    pub(crate) live_shares: crate::live_share::LiveShares,
    pub(crate) moderation: crate::moderation::Moderation,
    /// Measured control/header sizes of the expanded call overlay, refreshed
    /// by the paint pass and read by the renderer-less layout functions.
    pub(crate) call_overlay_metrics: crate::render_call::CallOverlayMetrics,
    /// `(renderer scale, theme scale, catalogue revision)` the overlay metrics
    /// were measured at; unchanged means the measurement is skipped.
    pub(crate) call_overlay_metrics_key: Option<(u32, u32, u64)>,
    pub(crate) realtime: crate::continuity::ChatRealtimeContinuity,
    pub(crate) call_chrome_motion: CallChromeMotion,
    /// The speaking ring on each face in the call.
    pub(crate) speaking: crate::call::SpeakingRings,
    /// How many rows the floating call window's participant chips take,
    /// measured by the paint pass.
    pub(crate) huddle_chip_rows: usize,
    pub(crate) pending_floating: Option<ui::floating_surface::FloatingSurfaceRequest>,
    /// Authoritative combined-output bounds supplied by the shell. Standalone
    /// app/test hosts leave this empty and fall back to the app rect.
    pub(crate) floating_surface_bounds: Rect,

    /// Opens the platform search when the header's search button is used.
    pub(crate) search_opener: ui::search::SearchOpener,

    /// The shell's sidebar: the channels this window shows in it and what
    /// the person does there ([`crate::sidebar`]).
    pub(crate) places: NavFeed,
    /// The shell's own controls in the window bar, and its sidebar's
    /// state; `None` outside a shell (a floated conversation, a test).
    pub(crate) bar: Option<WindowBarLink>,
    /// The detail drawer: people, pins, or the open thread.
    pub(crate) right_flap: SidebarDrawer,
    /// The chrome metrics of the last painted theme.
    pub(crate) chrome: Chrome,
    /// Local text preference also changes usable layout width; it is never
    /// shared with collaborators.
    pub(crate) layout_text_scale: f32,

    // The local peer's identity: how this peer authors messages, reactions,
    // voice membership, and its own read watermark in the shared document.
    // Distinct + stable per real peer (see [`crate::identity`]) so genuine
    // multi-participant chat attributes and de-duplicates correctly.
    pub(crate) me: LocalIdentity,

    // The room's answer for who everybody is, refreshed every frame from
    // presence (see `adopt_room`). A peer never writes its own name into
    // this map — only what the room says about a participant lands here.
    pub(crate) room_names: HashMap<Id, String>,
    /// The person each of the room's devices belongs to, where presence
    /// says: a call's roster names people, presence names their devices,
    /// and `room_names` carries both so either reads as the same name.
    pub(crate) room_persons: HashMap<Id, Id>,
    /// Who the room's presence places in each voice channel.
    pub(crate) room_voice: HashMap<Id, Vec<Id>>,
    /// The numbers people who share a name are told apart by, as the room
    /// resolves them, so every screen in it numbers the same people alike.
    pub(crate) room_labels: ui::presence::ParticipantLabels,
    /// What the room calls this window's person, numbered like anyone else.
    pub(crate) own_label: Option<String>,
    pub(crate) room_colors: HashMap<Id, (u8, u8, u8)>,
    /// Exact bare Agent ids from the authenticated room roster.
    pub(crate) room_agents: Vec<Id>,
    /// Explicit shot fixture; never set on the live launch path.
    capture_agent_presence: Option<PresenceCtx>,
    pub(crate) direct_person: Option<String>,
    pub(crate) person_subscription: Option<uuid::Uuid>,
    pub(crate) person_contacts: Vec<ui::person_chat::Contact>,
    pub(crate) person_messages: Vec<ui::person_chat::Message>,
    pub(crate) person_error: Option<String>,
    /// The conversation being opened, until the room answers.
    pub(crate) person_opening: Option<crate::person_conversation::Opening>,
    /// Opens another one replaced: each lane they open is released.
    pub(crate) person_abandoned: Vec<crate::person_conversation::Opening>,
    /// The message the composer is sending, until the room accepts it.
    pub(crate) person_sending: Option<crate::person_conversation::Sending>,
    pub(crate) person_body_views: HashMap<String, (String, crate::composer::MessageEditor)>,
    /// Retained direct conversations currently owned by the shared
    /// floating-surface host, keyed by their deterministic conversation id.
    /// The person's reduced-motion setting, applied to every floating
    /// placement this app owns (`Widget::set_motion_reduced`).
    pub(crate) motion_reduced: bool,
    pub(crate) agent_surfaces:
        HashMap<Id, crate::agent_conversation_surface::AgentConversationSurface>,
    pub(crate) agent_surface_requests: VecDeque<ui::floating_surface::FloatingSurfaceRequest>,
    pub(crate) focused_agent_surface: Option<String>,
    pub(crate) input_agent_surface: Option<String>,
    pub(crate) conversation_surface: bool,
    pub(crate) conversation_return_requested: bool,
    pub(crate) agent_conversation_drag: Option<AgentConversationDrag>,

    // Agent display names of agent-authored messages (`agent_authors` in the
    // document): what every member renders as "<name> (agent)".
    pub(crate) agent_authors: HashMap<Id, String>,
    /// What the room's host keeps of the files members post, as it says:
    /// a file it would not take is refused here, in plain words.
    pub(crate) room_files: crate::record::RoomFiles,
    /// Each shown message body, read-only in the SDK editor, by body size
    /// and message id, with the mention colours it was painted with.
    pub(crate) body_views:
        std::cell::RefCell<HashMap<(u32, Id), (u64, tilo_rich_text::RichTextView)>>,
    /// How far the body view sets its text inside the rect it is laid out
    /// in, as last read back from a laid-out paragraph. Bodies are laid out
    /// that far to the leading side, so their text starts on the message
    /// column's edge.
    pub(crate) body_text_inset: std::cell::Cell<f64>,
    /// The colours mentions are painted in, and what they were built from.
    pub(crate) mention_paint_cache:
        std::cell::RefCell<Option<(u64, ui::widget::marks::MentionPaint)>>,
    // Agent task participants this (headless host) worker serves, with the
    // display name the host labelled each with. A delivered `post` resolved
    // for one of them is authored by that participant.
    pub(crate) participant_labels: HashMap<Id, String>,

    // Other spellings of this window's person the room uses. See
    // `adopt_own_participant`: what was said under one of these is still
    // this person, so their lines go on reading as theirs.
    pub(crate) former_ids: Vec<Id>,

    // The OS permission gate: routes channel participation + file-embed
    // hydration through the toolkit's permission model. Refreshed from the
    // `PermissionContext` the compositor scopes around `draw`.
    pub(crate) perms: ChatPermissions,

    // The resource's dispatcher: every doc-mutating affordance (Send, a
    // reaction, the channel/message context menus) applies its verb or
    // field edit through it, permission-checked, attributed to this
    // window's person, recorded in history and replicated. See
    // [`crate::record`].
    pub(crate) dispatch: Option<ui::framework::Dispatcher<crate::resource::Chat>>,
    /// The read receipt last sent on its own and not answered yet, by
    /// channel, and the one the door refused: a receipt the door does not
    /// offer this person (a viewer) is not sent again until they read
    /// further.
    pub(crate) read_asked: Option<(String, crate::parts::ReadMark)>,
    pub(crate) read_refused: Option<(String, crate::parts::ReadMark)>,
    /// The chat's messages, kept in parts beside the record
    /// ([`crate::parts`]): what the window reads them from and posts, edits
    /// and reacts through. `None` where the window has none (a past
    /// version).
    pub(crate) chain: Option<ui::framework::Chain<crate::parts::ChatPart>>,
    /// The chain's revision the window last read its messages at.
    pub(crate) chain_seen: Option<u64>,
    /// The messages the chat record kept in itself, read once for the
    /// object holding them ([`crate::record::kept_messages`]).
    pub(crate) kept_messages: Option<(
        automerge::ObjId,
        std::rc::Rc<ui::tilo_resource::Collection<crate::record::Message>>,
    )>,
    /// What the chat record kept as the names of the people in it
    /// ([`crate::record::kept_names`]), read with the kept messages.
    pub(crate) kept_names: std::collections::BTreeMap<String, String>,
    /// The first part a screenshot scenario starts with, until written:
    /// messages by several people under the ids the scenario names.
    pub(crate) staged_start: Option<crate::parts::ChatPart>,
    /// A screenshot scenario's messages still to stage, each with who
    /// wrote it: staged once the room's records take a write (a frame's
    /// read of the record holds them).
    pub(crate) staged_parts: std::collections::VecDeque<(
        &'static str,
        ui::tilo_resource::Invoke<crate::parts::ChatPart>,
    )>,
    /// The scenario those messages are of.
    pub(crate) staged_scenario: Option<String>,
    /// The chat record as the window last read it: what a test's detached
    /// chain reads where its actions are invoked.
    #[cfg(test)]
    pub(crate) last_read: std::rc::Rc<std::cell::RefCell<std::rc::Rc<crate::record::Chat>>>,
    /// The voice channel a started call joins once it is in the document.
    pub(crate) pending_call: Option<Id>,
    /// Ephemeral opt-in to publishing Chat navigation through viewport presence.
    pub(crate) sharing_context: bool,
    pub(crate) following_context: bool,
    pub(crate) context_follow_requested: bool,
    /// This person's Chat preferences, following their other devices; `None`
    /// where the host keeps none (a past version, a test).
    pub(crate) preferences: Option<ui::framework::Setting<crate::preferences::ChatPreferences>>,
    /// The categories this person folded away, as last read or toggled.
    pub(crate) collapsed: std::collections::BTreeSet<Id>,
    /// Every action an affordance applied, for tests that compare them with
    /// the published ones.
    #[cfg(test)]
    pub(crate) dispatched: std::cell::RefCell<Vec<crate::record::ChatAction>>,
    /// Every verb on the chat's messages an affordance applied, likewise.
    #[cfg(test)]
    pub(crate) dispatched_parts: std::cell::RefCell<Vec<crate::messages::ChatPartAction>>,

    // Persistence (None for ephemeral / `--shots` instances).

    // Background image loader/cache for attachment thumbnails (decodes off
    // the UI thread; real bytes come from `content`).
    pub(crate) images: ui::ImageStore,
    /// Where attachment bytes live. Messages carry only content hashes; the
    /// import worker and the image fetcher each hold a clone.
    pub(crate) content: ContentStore,
    /// The attachments' bytes as reads ([`ui::arriving`]): here, on their
    /// way from the room, or with no device that has them.
    pub(crate) content_reads: ui::collaboration::record_content::ContentReads,
    /// When the next attachment on its way steps its placeholder on, as the
    /// last frame painted them.
    pub(crate) arriving_wake: std::cell::Cell<Option<ui::time::Instant>>,

    // Layout + render caches.
    pub(crate) rect: Rect,
    root_node: Option<NodeId>,
    video_node: Option<NodeId>,
    pub(crate) video: VideoPlayer,
    /// What the call tile's layout node was last placed from: whether the
    /// call fills the window, and where the tile stands. Chat's only laid-out
    /// node is that tile; everything else it places itself as it paints, so
    /// a scroll or a keystroke re-solves no layout.
    laid_video: Option<(bool, Rect)>,
    pub(crate) hits: Vec<(Rect, Hit)>,
    /// Ordinal in `keyboard_targets()`. Derived from the current hit inventory
    /// so Tab and assistive focus traverse the same controls a pointer can use.
    pub(crate) keyboard_target: Option<usize>,
    pub(crate) last_mouse: Point,
    /// The live call control beneath the pointer. Kept as render-only state so
    /// attached and shell-owned copies share the same hover treatment without
    /// duplicating a second native-window control implementation.
    pub(crate) call_hovered: Option<Hit>,
    relayout: bool,
    /// True while either flap is mid-slide — drives `needs_animation_frame`
    /// so the loop keeps ticking only during the transition, then idles.
    flap_animating: bool,
    /// This window's person still has to be announced into the shared
    /// roster under the name this device knows them by.
    announce: Option<crate::identity::LocalIdentity>,
    /// The person's own drafts record, so the sidebar can say where the
    /// unsent words are.
    pub(crate) drafts_record: Option<ui::framework::PersonRecord<crate::drafts::ChatDrafts>>,

    /// Latest decoded camera picture per person, shown in their call tile
    /// (this person's own among them, in the host tile). Only ever holds a
    /// picture the call delivered and [`Self::call_pictures`] decoded; a tile
    /// without one shows the person, never a made-up picture.
    pub(crate) remote_frames: HashMap<String, vello::peniko::ImageData>,

    /// Latest decoded **screen-share** picture per person, kept separate
    /// from their camera picture so a person can share their screen and keep
    /// their camera on at once: the screen fills the featured stage tile, the
    /// camera stays in their filmstrip cell. Until the first picture arrives
    /// the stage says whose screen it waits for.
    pub(crate) remote_screens: HashMap<String, vello::peniko::ImageData>,
    /// Decodes the call's pictures, off the UI thread.
    pub(crate) call_pictures: ui::call_pictures::CallPictures,
    /// The picture tiles the last frame drew: whose camera or screen, and
    /// their size in device pixels. What the call is asked to deliver, each
    /// at the layer that fits its tile.
    pub(crate) picture_tiles: std::cell::RefCell<Vec<PictureTile>>,
    /// Where each message shown last frame drew its own texts, by message
    /// id: what its accessible row's children are read and picked by.
    pub(crate) painted_messages: HashMap<Id, PaintedMessage>,
    /// The conversation's rows, measured and painted, kept between frames.
    pub(crate) timeline: std::cell::RefCell<crate::timeline::Timeline>,
    /// Counts each load of [`Self::messages`], so what is kept of them knows
    /// when to look again.
    pub(crate) messages_revision: u64,
    /// A video call was started: the camera turns on once the call is live.
    pub(crate) camera_on_live: bool,

    /// The latest call/media problem to show in the call chrome (a refused
    /// control, a native voice issue, or no connected media transport).
    pub(crate) media_error: Option<String>,
    pub(crate) canonical_dirty: bool,
    /// The regions the last draws painted, compared to say which changed:
    /// a typed character repaints the composer, not the window.
    paint_damage: ui::widget::paint_damage::PaintDamage,
    /// Document heads and pending-operation count at the last reload,
    /// so an unchanged document is not reloaded and repainted every frame.
    synced: Option<(Vec<automerge::ChangeHash>, usize)>,
    pub(crate) host_focused: bool,
    pub(crate) external_services: bool,
}

/// A picture tile a frame drew: whose camera or screen, and its size in
/// device pixels.
pub(crate) type PictureTile = (String, ui::native_media::PictureSource, (f64, f64));

/// Native external services are explicit; browser peer transport is installed later.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatRuntime {
    Native,
    Unavailable,
}

/// Whether this host imports attachments from native paths. Voice support
/// is reported independently through the host's local media lane.
pub(crate) fn native_file_access() -> bool {
    tilo_files::available(tilo_files::Feature::NativePaths)
}

impl ChatApp {
    /// The chat surface for `identity`, keeping attachment bytes in
    /// `content`; the document arrives with its first sync.
    pub fn new(identity: LocalIdentity, runtime: ChatRuntime, content: ContentStore) -> Self {
        let mut app = Self::bare(content);
        app.external_services = runtime == ChatRuntime::Native;
        app.me = identity;
        app.active_channel.clear();
        app.thread_open = None;
        app.media_error =
            (!app.external_services).then(|| crate::i18n::t("chat-voice-unavailable"));
        app
    }

    /// Announce this window's person into the shared roster on the next
    /// sync, under the name this device knows them by.
    pub(crate) fn announcing(mut self) -> Self {
        self.announce = Some(self.me.clone());
        self
    }

    /// A demo-reading instance for tests: the demo's own person and its
    /// default room, attachment bytes in memory.
    #[cfg(test)]
    pub(crate) fn new_ephemeral() -> Self {
        let mut app = Self::bare(ContentStore::memory());
        app.attach_framework(ui::framework::Dispatcher::detached(
            "so.tilo.chat",
            crate::model::ME,
        ));
        app.attach_detached_chain(crate::model::ME);
        app
    }

    /// Hold the chat's messages in a chain of this window's own, as `me`,
    /// its actions reading the record as the window last read it: a test's
    /// stand-in for the room's.
    #[cfg(test)]
    pub(crate) fn attach_detached_chain(&mut self, me: &str) {
        let last_read = std::rc::Rc::clone(&self.last_read);
        if let Some(chain) = ui::framework::Chain::detached("so.tilo.chat", me, move || {
            std::rc::Rc::clone(&last_read.borrow())
        }) {
            self.attach_chain(chain);
        }
    }

    /// Show `room`, a test's chat, reading and writing its messages through
    /// its chain and refreshing the record its actions read.
    #[cfg(test)]
    pub(crate) fn attach_room(&mut self, room: &crate::demo::Room) {
        self.last_read = room.base();
        if let Some(chain) = room.chain() {
            let me = self.me.id.clone();
            self.attach_chain(chain.as_reader(&me));
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the complete app state is deliberately initialized together so bounded channels and their lifecycles cannot drift"
    )]
    fn bare(content: ContentStore) -> Self {
        // The shared picker and reaction strip name and search emoji through
        // Chat's catalogue.
        crate::i18n::offer_emoji_catalogue();
        // The host call tile is a real VideoPlayer with no frame until a live
        // capture feeds one; there is no synthesised stand-in. Its honest
        // "camera off" state is painted over the empty surface.
        let video = VideoPlayer::new();
        let (attachment_result_tx, attachment_result_rx) = std::sync::mpsc::sync_channel(1);
        let (attachment_picker_result_tx, attachment_picker_result_rx) =
            std::sync::mpsc::sync_channel(1);
        Self {
            channels: Vec::new(),
            categories: Vec::new(),
            users: Vec::new(),
            messages: Vec::new(),
            search_messages: Vec::new(),
            search_users: Vec::new(),
            reports: Vec::new(),
            opened: std::rc::Rc::default(),
            voice_members: HashMap::new(),
            draft_main: String::new(),
            draft_thread: String::new(),
            composer_main: crate::composer::DraftComposer::shared(),
            not_sent: None,
            deleted: std::cell::RefCell::default(),
            asked_delete: None,
            hovered_message: None,
            message_rows: Vec::new(),
            message_bar: crate::app::message_bar::BarMotion::default(),
            last_post: None,
            channel_sheet: None,
            pins_open: false,
            composer_top: None,
            composer_thread: crate::composer::DraftComposer::shared(),
            active_channel: "c_crit".into(),
            focus: Focus::Main,
            scroll: 0.0,
            thread_scroll: ui::widget::Scrollable::new_vertical().always_visible(),
            thread_follow: false,
            thread_body: Rect::ZERO,
            timeline_at_end: true,
            presented: false,
            read_on_present: false,
            timeline_total_height: 0.0,
            scroll_content: 0.0,
            scroll_viewport: 0.0,
            thread_open: Some("m_concept".into()),
            revealed_message: None,
            revealed_range: None,
            reveal_scroll_pending: false,
            revealed_person: None,
            emoji_open: false,
            emoji_target: None,
            emoji_anchor: Rect::ZERO,
            emoji_picker: ui::widget::EmojiPicker::framed(),
            reaction_strips: std::cell::RefCell::default(),
            history_open: None,
            history_anchor: Point::ZERO,
            editing: None,
            edit_input: crate::composer::MessageEditor::local(),
            attach_open: false,
            attach_entries: Vec::new(),
            attachment_pending: None,
            device_pick: None,
            device_files: VecDeque::new(),
            attachment_next_token: 0,
            attachment_result_tx,
            attachment_result_rx,
            attachment_status: None,
            attachment_view: None,
            attachment_save: None,
            attachment_picker_pending: None,
            attachment_picker_generation: 0,
            attachment_picker_result_tx,
            attachment_picker_result_rx,
            menu: None,
            sidebar_rename: None,
            call: CallState::default(),
            native_voice: crate::local_voice::LocalVoice::default(),
            resource_navigation: crate::shared_resources::Navigation::default(),
            live_shares: crate::live_share::LiveShares::default(),
            moderation: crate::moderation::Moderation::default(),
            call_overlay_metrics: crate::render_call::CallOverlayMetrics::default(),
            call_overlay_metrics_key: None,
            realtime: crate::continuity::ChatRealtimeContinuity::default(),
            call_chrome_motion: CallChromeMotion::default(),
            speaking: crate::call::SpeakingRings::default(),
            huddle_chip_rows: 0,
            pending_floating: None,
            floating_surface_bounds: Rect::ZERO,
            search_opener: ui::search::SearchOpener::new(),
            places: NavFeed::new(),
            bar: None,
            right_flap: Self::detail_drawer(),
            chrome: Chrome::default(),
            layout_text_scale: 1.0,
            me: LocalIdentity::demo(),
            room_names: HashMap::new(),
            room_persons: HashMap::new(),
            room_voice: HashMap::new(),
            room_labels: ui::presence::ParticipantLabels::default(),
            own_label: None,
            room_colors: HashMap::new(),
            room_agents: Vec::new(),
            capture_agent_presence: None,
            direct_person: None,
            person_subscription: None,
            person_contacts: Vec::new(),
            person_messages: Vec::new(),
            person_error: None,
            person_opening: None,
            person_abandoned: Vec::new(),
            person_sending: None,
            person_body_views: HashMap::new(),
            motion_reduced: false,
            agent_surfaces: HashMap::new(),
            agent_surface_requests: VecDeque::new(),
            focused_agent_surface: None,
            input_agent_surface: None,
            conversation_surface: false,
            conversation_return_requested: false,
            agent_conversation_drag: None,
            agent_authors: HashMap::new(),
            room_files: crate::record::RoomFiles::default(),
            body_views: std::cell::RefCell::new(HashMap::new()),
            body_text_inset: std::cell::Cell::new(0.0),
            mention_paint_cache: std::cell::RefCell::new(None),
            participant_labels: HashMap::new(),
            former_ids: Vec::new(),
            // Default to the deterministic local bus; the live binary
            // (`ChatApp::new`) upgrades to the real transport. This keeps
            // `--shots`/tests from binding a real iroh endpoint per instance.
            perms: ChatPermissions::permissive(),
            host_model: crate::host_model::HostModel::default(),
            expanded_reasoning: std::collections::HashSet::new(),
            dispatch: None,
            read_asked: None,
            read_refused: None,
            chain: None,
            chain_seen: None,
            kept_messages: None,
            kept_names: std::collections::BTreeMap::new(),
            staged_start: None,
            staged_parts: std::collections::VecDeque::new(),
            staged_scenario: None,
            #[cfg(test)]
            last_read: std::rc::Rc::default(),
            pending_call: None,
            sharing_context: false,
            following_context: false,
            context_follow_requested: false,
            preferences: None,
            collapsed: std::collections::BTreeSet::new(),
            #[cfg(test)]
            dispatched: std::cell::RefCell::default(),
            #[cfg(test)]
            dispatched_parts: std::cell::RefCell::default(),
            images: ui::ImageStore::new(&crate::images::fetcher(content.clone())),
            content_reads: ui::collaboration::record_content::ContentReads::new(content.clone()),
            arriving_wake: std::cell::Cell::new(None),
            content,
            rect: Rect::ZERO,
            root_node: None,
            video_node: None,
            video,
            laid_video: None,
            hits: Vec::new(),
            keyboard_target: None,
            last_mouse: Point::ZERO,
            call_hovered: None,
            relayout: false,
            flap_animating: false,
            announce: None,
            drafts_record: None,
            remote_frames: HashMap::new(),
            remote_screens: HashMap::new(),
            call_pictures: ui::call_pictures::CallPictures::new(),
            picture_tiles: std::cell::RefCell::new(Vec::new()),
            painted_messages: HashMap::new(),
            timeline: std::cell::RefCell::default(),
            messages_revision: 0,
            camera_on_live: false,
            media_error: None,
            canonical_dirty: true,
            paint_damage: ui::widget::paint_damage::PaintDamage::default(),
            synced: None,
            host_focused: true,
            external_services: true,
        }
    }

    /// Whether a call action is permitted, routed through the real OS
    /// permission gate for the active workspace. A View-only / ungranted peer
    /// is blocked from join/speak/camera/screen-share; leaving is always
    /// allowed so a downgraded peer can drop out.
    pub(crate) fn call_allows(&self, action: crate::call::CallAction) -> bool {
        match action {
            crate::call::CallAction::Leave => true,
            crate::call::CallAction::Camera | crate::call::CallAction::ShareScreen => false,
            _ => self.voice_service_available() && self.perms.allows(action),
        }
    }

    pub(crate) fn voice_service_available(&self) -> bool {
        self.external_services
            || self.native_voice.snapshot.state != ui::native_media::State::Unavailable
    }

    /// Whether the local peer may post / react / start a call in the active
    /// workspace (an Edit grant). The permissive default allows; a restricted
    /// context gates writes while still letting the peer read.
    pub(crate) fn can_participate(&self) -> bool {
        self.perms.can_participate()
    }

    /// Whether the local peer keeps the room's layout: makes, renames,
    /// moves, archives and deletes its channels and categories. A member
    /// who may not is not offered it.
    pub(crate) fn keeps_layout(&self) -> bool {
        self.perms.can_moderate()
    }

    /// `items` as the local peer may use them: a member still collapses a
    /// category, and the rest of the layout is kept by those who keep it.
    pub(crate) fn layout_menu(&self, mut items: Vec<MenuAction>) -> Vec<MenuAction> {
        if !self.keeps_layout() {
            items.retain(|action| *action == MenuAction::ToggleCollapse);
        }
        items
    }

    /// Whether the local peer may hydrate + render an attachment blob. An
    /// ungranted embed shows a locked card and never requests shared bytes.
    pub(crate) fn embed_allowed(&self, blob: &str) -> bool {
        self.perms.can_embed(blob)
    }

    /// The poster frame of the video behind `blob`, once the image store has
    /// it; `None` for a clip with no poster to show.
    pub(crate) fn video_poster(&self, blob: &str) -> Option<vello::peniko::ImageData> {
        crate::images::poster_key(blob).and_then(|key| self.images.get(&key))
    }

    /// Stage the messages a screenshot of `scenario` shows in the chat's
    /// parts, once: the first part as the scenario starts it, then what is
    /// done with its messages, each by its author. A chain that holds
    /// messages already is left as it is.
    pub(crate) fn stage_parts(&mut self, scenario: Option<&str>) {
        let Some(chain) = &self.chain else {
            return;
        };
        if chain.head().is_some() {
            return;
        }
        let (start, steps) = crate::resource::part_scenario(scenario);
        self.staged_start = start;
        self.staged_parts = steps.into();
        self.staged_scenario = scenario.map(str::to_owned);
        self.flush_staged_parts();
    }

    /// Stage what is left of a scenario's messages, until the room's
    /// records refuse a write for now.
    pub(crate) fn flush_staged_parts(&mut self) {
        let Some(chain) = self.chain.clone() else {
            return;
        };
        let mut staged = false;
        if chain.takes_changes()
            && let Some(start) = self.staged_start.take()
        {
            // Missing messages show in the picture's comparison.
            let _ = chain.write_start(0, &start);
            staged = true;
        }
        while chain.takes_changes()
            && let Some((author, invoke)) = self.staged_parts.pop_front()
        {
            // A message that does not stage is missing from the picture,
            // which its comparison shows.
            let _ = chain.stage_as(&invoke, author);
            staged = true;
        }
        if staged {
            self.reselect();
            // The view state the scenario stages names its messages: staged
            // again over them once they are all there.
            if self.staged_parts.is_empty()
                && let Some(scenario) = self.staged_scenario.take()
            {
                self.stage_scenario(&scenario);
            }
            self.request_relayout();
        }
    }

    /// The parts of the chat's messages this window holds.
    pub fn held_messages(&self) -> crate::messages::Messages {
        self.chain
            .as_ref()
            .map(|chain| crate::messages::Messages::new(chain.parts()))
            .unwrap_or_default()
            .with_kept(
                self.kept_messages
                    .as_ref()
                    .map(|(_, kept)| kept.clone())
                    .unwrap_or_default(),
            )
    }

    /// Read the messages the chat record `doc` kept in itself, unless they
    /// were read from the same object already.
    fn read_kept_messages(&mut self, doc: &AutoCommit) {
        let holding = ui::tilo_resource::evolution::map_at(doc, &automerge::ROOT, "messages")
            .ok()
            .flatten();
        if self.kept_messages.as_ref().map(|(id, _)| id) == holding.as_ref() {
            return;
        }
        self.kept_names = if holding.is_some() {
            crate::record::kept_names(doc)
        } else {
            std::collections::BTreeMap::new()
        };
        self.kept_messages =
            holding.map(|id| (id, std::rc::Rc::new(crate::record::kept_messages(doc))));
    }

    /// Apply a verb on the chat's messages now, through the chain's door:
    /// to the part it changes, permission-checked and attributed to this
    /// window's person. One that needs confirming goes to the door, which
    /// asks first. The window reads the change on its next sync.
    pub(crate) fn apply_part(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        submit: impl ui::framework::Submit<crate::parts::ChatPart>,
    ) {
        // A refusal changes nothing; the dispatcher reports it.
        let _ = self.try_apply_part(doc, submit);
    }

    /// [`Self::apply_part`], saying why when the chat refused it.
    pub(crate) fn try_apply_part(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        submit: impl ui::framework::Submit<crate::parts::ChatPart>,
    ) -> Result<(), String> {
        let chain = self
            .chain
            .as_ref()
            .ok_or_else(|| "this window holds no messages of the chat".to_owned())?;
        let invoke = submit
            .into_invoke()
            .map_err(|_| "the message could not be put together".to_owned())?;
        #[cfg(test)]
        if let Ok(action) = invoke.build(0) {
            self.dispatched_parts.borrow_mut().push(action.operation);
        }
        self.deleted.take();
        chain
            .dispatcher()
            .submit_now(doc, invoke)
            .map_err(|error| error.to_string())
    }

    /// Submit a verb on the chat's messages to the door, applied after this
    /// pass: from a pass that only reads the document.
    pub(crate) fn submit_part(&self, submit: impl ui::framework::Submit<crate::parts::ChatPart>) {
        let Some(chain) = &self.chain else {
            return;
        };
        let Ok(invoke) = submit.into_invoke() else {
            return;
        };
        #[cfg(test)]
        if let Ok(action) = invoke.build(0) {
            self.dispatched_parts.borrow_mut().push(action.operation);
        }
        self.deleted.take();
        chain.dispatcher().submit(invoke);
    }

    /// Edit through `dispatcher` from now on.
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Chat>,
    ) {
        self.dispatch = Some(dispatcher);
    }

    /// Keep the composers' drafts in the person's own `record`: theirs
    /// alone, on each of their devices, never in the room.
    #[must_use]
    pub(crate) fn with_drafts(
        mut self,
        record: Option<ui::framework::PersonRecord<crate::drafts::ChatDrafts>>,
    ) -> Self {
        if let Some(record) = record {
            self.composer_main.attach_drafts(record.clone());
            self.composer_thread.attach_drafts(record.clone());
            // The sidebar shows where the unsent words are, so it holds the
            // record too.
            self.drafts_record = Some(record);
        }
        self
    }

    /// Follow this person's preferences from now on.
    pub(crate) fn follow_preferences(
        &mut self,
        preferences: ui::framework::Setting<crate::preferences::ChatPreferences>,
    ) {
        self.collapsed = preferences.get().collapsed_categories;
        self.preferences = Some(preferences);
        self.reselect();
    }

    /// Fold category `id` away in this person's sidebar, or unfold it.
    pub(crate) fn toggle_category(&mut self, id: &str) {
        if !self.collapsed.remove(id) {
            self.collapsed.insert(id.to_owned());
        }
        if let Some(preferences) = &self.preferences {
            let collapsed = self.collapsed.clone();
            // Unsaved, the fold still holds in this window.
            let _ = preferences.update(|stored| stored.collapsed_categories = collapsed);
        }
        for category in &mut self.categories {
            category.collapsed = self.collapsed.contains(&category.id);
        }
        self.canonical_dirty = true;
    }

    /// The document moved under an unchanged selection, or the selection
    /// moved: read it again on the next sync.
    pub(crate) fn reselect(&mut self) {
        self.synced = None;
        self.canonical_dirty = true;
    }

    /// React to `msg` with `emoji`, or take the reaction back: on a filed
    /// report's message the edit is the report's own reactions field in the
    /// chat record, which is where its message is read from; on any other
    /// message it is [`Self::reaction_toggle_action`] on the message's part.
    pub(crate) fn toggle_reaction(&self, doc: &mut ui::widget::Edits<'_>, msg: &str, emoji: &str) {
        let report = crate::feedback::report_of_message(msg)
            .filter(|report| self.reports.iter().any(|(id, _)| id.as_str() == *report));
        let Some(report) = report else {
            self.apply_part(doc, self.reaction_toggle_action(msg, emoji));
            return;
        };
        let reacting = self.message(msg).is_some_and(|m| {
            m.reactions
                .iter()
                .any(|r| r.emoji == emoji && r.includes(&self.me.id))
        });
        let reactions = crate::resource::Chat::paths()
            .feedback
            .item(&ui::tilo_resource::Id::from_raw(report))
            .reactions;
        self.apply_now(
            doc,
            if reacting {
                reactions.unreact(emoji)
            } else {
                reactions.react(emoji)
            },
        );
    }

    /// Build the reaction a click on `msg`/`emoji` should dispatch, deciding
    /// add-vs-remove from the currently-loaded state at authoring time. A
    /// toggle is a presentation decision; the result is the framework's
    /// idempotent `react` or `unreact` field edit of the message — the single
    /// source of truth both the reaction pill and the emoji picker resolve to.
    pub(crate) fn reaction_toggle_action(
        &self,
        msg: &str,
        emoji: &str,
    ) -> ui::tilo_resource::Edit<crate::parts::ChatPart> {
        use crate::parts::ChatPart;
        let reacting = self.message(msg).is_some_and(|m| {
            m.reactions
                .iter()
                .any(|r| r.emoji == emoji && r.includes(&self.me.id))
        });
        let reactions = ChatPart::paths()
            .messages
            .item(&ui::tilo_resource::Id::from_raw(msg))
            .reactions;
        if reacting {
            reactions.unreact(emoji)
        } else {
            reactions.react(emoji)
        }
    }

    /// Where a message's header and body were last painted, if they were.
    fn message_rect(&self, id: &str) -> Option<Rect> {
        self.hits.iter().find_map(|(rect, hit)| match hit {
            Hit::MessageMenu { msg } if msg.as_str() == id => Some(*rect),
            _ => None,
        })
    }

    // --- model lookups ----------------------------------------------------
    pub(crate) fn user(&self, id: &str) -> Option<&User> {
        self.users.iter().find(|u| u.id == id)
    }

    /// Whether `id` is this session's own participant, under its own id or
    /// a spelling the room uses for it.
    pub(crate) fn is_me(&self, id: &str) -> bool {
        id == self.me.id || self.former_ids.iter().any(|f| f == id)
    }

    pub(crate) fn agent_display_name(&self, agent: &str) -> String {
        self.room_names
            .get(&format!("agent:{agent}"))
            .or_else(|| self.room_names.get(agent))
            .filter(|name| !name.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| crate::i18n::t("chat-agent-kind"))
    }

    pub(crate) fn main_composer_can_edit(&self) -> bool {
        if let Some(peer) = &self.direct_person {
            return self
                .person_contacts
                .iter()
                .any(|contact| &contact.public_key == peer)
                && self.person_error.is_none();
        }
        self.can_participate()
    }

    pub(crate) fn channel(&self, id: &str) -> Option<&Channel> {
        self.channels.iter().find(|c| c.id == id)
    }
    pub(crate) fn message(&self, id: &str) -> Option<&Message> {
        self.messages.iter().find(|m| m.id == id)
    }

    /// The room's channels, ordered. Archived channels are hidden from the
    /// sidebar (they stay in the document and are reachable via Ctrl+K).
    pub(crate) fn ws_channels(&self) -> Vec<&Channel> {
        let mut v: Vec<&Channel> = self.channels.iter().filter(|c| !c.archived).collect();
        v.sort_by_key(|c| c.order);
        v
    }

    /// The room's categories, ordered.
    pub(crate) fn ws_categories(&self) -> Vec<&crate::model::Category> {
        let mut v: Vec<&crate::model::Category> = self.categories.iter().collect();
        v.sort_by_key(|c| c.order);
        v
    }

    /// The categories the sidebar lists: each one with a channel in it.
    /// Someone who keeps the room's layout sees the empty ones too, since
    /// that is where they add a category's first channel; to everyone else
    /// an empty heading is only noise between the channels.
    pub(crate) fn sidebar_categories(&self) -> Vec<&crate::model::Category> {
        let keeps_layout = self.keeps_layout();
        self.ws_categories()
            .into_iter()
            .filter(|category| keeps_layout || !self.category_channels(&category.id).is_empty())
            .collect()
    }

    /// Cached voice members of `channel` (empty when nobody is connected).
    pub(crate) fn voice_in(&self, channel: &str) -> &[crate::model::VoiceMember] {
        self.voice_members
            .get(channel)
            .map_or(&[], std::vec::Vec::as_slice)
    }

    /// Channels in `category`, ordered. Archived channels are hidden from the
    /// sidebar.
    pub(crate) fn category_channels(&self, category: &str) -> Vec<&Channel> {
        let mut v: Vec<&Channel> = self
            .channels
            .iter()
            .filter(|c| c.category == category && !c.archived)
            .collect();
        v.sort_by_key(|c| c.order);
        v
    }

    pub(crate) fn reveal_category_resource(&mut self, category: &str) -> Result<(), String> {
        let Some(category_id) = self
            .categories
            .iter()
            .find(|item| item.id == category)
            .map(|item| item.id.clone())
        else {
            return Err("category no longer exists".into());
        };
        self.active_channel = self
            .channels
            .iter()
            .filter(|channel| {
                channel.category == category_id && channel.kind == model::ChannelKind::Text
            })
            .min_by_key(|channel| channel.order)
            .map_or_else(String::new, |channel| channel.id.clone());
        self.thread_open = None;
        self.revealed_message = None;
        self.reveal_scroll_pending = false;
        self.revealed_person = None;
        self.reset_timeline_to_latest();
        self.focus = Focus::Main;
        Ok(())
    }

    /// Keep the active channel and thread pointing at something that exists
    /// — used after seeding a fresh (real) room whose ids differ from the
    /// demo defaults, and after any structural delete. Returns true when the
    /// active channel changed (the caller must reload so the message list
    /// refreshes).
    pub(crate) fn ensure_selection(&mut self) -> bool {
        let channel_changed = self.direct_person.is_none()
            && !self.active_channel.is_empty()
            && self.channel(&self.active_channel).is_none();
        if channel_changed {
            self.active_channel.clear();
        }
        // Drop a stale open thread whose parent isn't loaded.
        if let Some(t) = self.thread_open.clone()
            && self.message(&t).is_none()
        {
            self.thread_open = None;
            if self.focus == Focus::Thread {
                self.focus = Focus::Main;
            }
        }
        channel_changed
    }

    /// The decoded avatar `m`'s bot showed for it, requesting it from the
    /// content store (and so from the room) until it is there.
    pub(crate) fn shown_avatar(
        &self,
        m: &crate::model::Message,
    ) -> Option<vello::peniko::ImageData> {
        let avatar = m.appearance.as_ref()?.avatar.as_deref()?;
        self.images.get(&crate::images::blob_key(avatar))
    }

    /// Warm the attachment-image cache for the current channel and an open
    /// picker, so the frame shows decoded thumbnails as soon as the
    /// background loader has them.
    pub(crate) fn request_visible_images(&self) {
        // `self.messages` already holds every message in the active channel,
        // including threaded replies and the thread parent, so one pass covers
        // the timeline and the open thread panel.
        for m in &self.messages {
            let _ = self.shown_avatar(m);
            match &m.attachment {
                // Never hydrate the bytes of an embed this peer isn't granted.
                Some(crate::model::Attachment::Image { blob, .. }) if self.embed_allowed(blob) => {
                    let _ = self.images.get(&crate::images::blob_key(blob));
                }
                Some(crate::model::Attachment::Video { blob, .. }) if self.embed_allowed(blob) => {
                    let _ = self.video_poster(blob);
                }
                _ => {}
            }
        }
        // Also warm any thumbnails visible in an open attachment picker.
        for e in &self.attach_entries {
            let _ = self.images.get(&e.key());
        }
    }
}

/// Compute an absolute rect from window-relative fractions.
pub(crate) fn frac_rect(win: Rect, f: (f64, f64, f64, f64)) -> Rect {
    let x0 = win.x0 + f.0 * win.width();
    let y0 = win.y0 + f.1 * win.height();
    Rect::new(x0, y0, x0 + f.2 * win.width(), y0 + f.3 * win.height())
}

impl ui::semantic_document_read::SemanticReaders for ChatApp {
    fn semantic_readers(&self) -> &'static [ui::semantic_document_read::RegisteredSemanticReader] {
        &crate::feedback::READERS
    }
}

impl Continuable for ChatApp {
    fn continuation_metadata(
        &self,
        _state: &AutoCommit,
        _path: &ObjId,
    ) -> Option<ui::collaboration::ContinuationMetadata> {
        let continuation = self.call_continuation()?;
        let bytes = serde_json::to_vec(&continuation).ok()?;
        Some(ui::collaboration::ContinuationMetadata {
            schema_version: crate::continuity::CHAT_CONTINUATION_SCHEMA,
            bytes,
        })
    }

    fn restore_continuation_metadata(
        &mut self,
        metadata: &ui::collaboration::ContinuationMetadata,
        state: &mut ui::widget::Edits<'_>,
        _path: &ObjId,
    ) -> bool {
        self.restore_call_continuation(metadata, state)
    }
}

impl Collaborative for ChatApp {
    fn take_collab_requests(&mut self) -> Vec<ui::collaboration::CollabRequest> {
        self.queue_realtime_registration();
        if self.realtime.target_media_ready && self.realtime_target_media_ready() {
            self.realtime.target_media_ready = false;
        }
        let mut requests = std::mem::take(&mut self.realtime.requests);
        self.settle_resource_navigation();
        requests.extend(self.resource_navigation.remote_mut().take_collab_requests());
        requests.extend(self.moderation_requests());
        self.native_voice.video.wanted = self.wanted_pictures();
        let before = self.native_voice.snapshot.state;
        requests.extend(self.native_voice.take_requests(ui::time::now()));
        if before != self.native_voice.snapshot.state {
            self.project_native_voice();
        }
        requests
    }

    fn handle_collab_event(&mut self, event: &ui::collaboration::CollabEvent) {
        match event {
            ui::collaboration::CollabEvent::RemoteAction { message } => {
                self.complete_moderation(message);
                self.resource_navigation
                    .remote_mut()
                    .handle_collab_event(event);
                self.settle_resource_navigation();
            }
            ui::collaboration::CollabEvent::FollowChanged { peer_id } => {
                self.context_follow_requested = peer_id.is_some();
                self.following_context = false;
                self.request_relayout();
            }
            ui::collaboration::CollabEvent::NativeMediaCommandResult { request_id, result } => {
                if self.native_voice.complete(*request_id, result) {
                    self.project_native_voice();
                }
                let arrived = std::mem::take(&mut self.native_voice.video.arrived);
                if !arrived.is_empty() {
                    self.call_pictures.receive(arrived);
                }
            }
            ui::collaboration::CollabEvent::RealtimeCommandResult { result, .. } => match result {
                Ok(ui::realtime_continuity::RealtimeReply::Registered { session }) => {
                    self.realtime.registration_pending = false;
                    self.realtime.session = Some(session.clone());
                }
                Ok(ui::realtime_continuity::RealtimeReply::Accepted) => {
                    self.realtime.accepted_target = true;
                }
                Ok(ui::realtime_continuity::RealtimeReply::Rejected { .. }) | Err(_) => {
                    self.realtime.registration_pending = false;
                }
                _ => {}
            },
            ui::collaboration::CollabEvent::Realtime(
                ui::realtime_continuity::RealtimeEvent::Offer {
                    session, intent, ..
                },
            ) if intent.provider == "tilo.chat" => {
                let Some(workspace_id) = intent.semantic_position.clone() else {
                    return;
                };
                let offered = crate::continuity::ChatCallContinuation {
                    workspace_id,
                    channel_id: intent.room_id.clone(),
                    media: intent.media,
                    controls: intent.controls,
                };
                if offered.valid() {
                    self.realtime.offer = Some((session.clone(), offered));
                }
            }
            ui::collaboration::CollabEvent::Realtime(
                ui::realtime_continuity::RealtimeEvent::CaptureAuthorized {
                    session,
                    capture_authorizations,
                },
            ) if self
                .realtime
                .offer
                .as_ref()
                .is_some_and(|(offered_session, _)| offered_session == session) =>
            {
                let _ = self.accept_realtime_offer(capture_authorizations.clone());
            }
            ui::collaboration::CollabEvent::Realtime(
                ui::realtime_continuity::RealtimeEvent::StopSource { session },
            ) if self.realtime.session.as_ref() == Some(session) => {
                self.realtime.stop_source = Some(session.clone());
            }
            ui::collaboration::CollabEvent::Realtime(
                ui::realtime_continuity::RealtimeEvent::RolledBack { session },
            ) if self.realtime.session.as_ref() == Some(session) => {
                // Source deliberately remains live. Only discard the pending
                // target edge; no call/media mutation occurs here.
                self.realtime.offer = None;
            }
            ui::collaboration::CollabEvent::Realtime(
                ui::realtime_continuity::RealtimeEvent::Ended { session },
            ) if self.realtime.session.as_ref() == Some(session) => {
                self.realtime.session = None;
            }
            _ => {}
        }
    }
}

impl ui::widget::controls::ActionControls for ChatApp {
    /// The header's search button asks the platform to open search over
    /// Chat; everything else comes from the children.
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.search_opener.take().or_else(|| {
            self.children_mut()
                .into_iter()
                .find_map(ui::widget::controls::take_control_invocation)
        })
    }
}

impl Widget for ChatApp {
    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, _path: &ObjId) {
        // The record is lent to the settle pass: the room's other records
        // take writes now.
        self.flush_staged_parts();
        self.apply_window_toolbar(edits);
        self.take_sidebar_events(edits);
        self.follow_window_shares(edits);
    }

    ui::provides!(
        dyn ui::widget::Collaborative,
        dyn ui::widget::Continuable,
        dyn ui::widget::FloatingSurfaceProvider,
        dyn ui::collaboration::participants::Participants,
        dyn ui::presence::capability::PublishesPresence,
        dyn ui::application_view::ApplicationViewState,
        dyn ui::view::capability::ViewState,
        dyn ui::semantic_document_read::SemanticReaders
    );

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        self.motion_reduced = reduced;
        self.call.placement.set_reduce_motion(reduced);
        for surface in self.agent_surfaces.values_mut() {
            surface.placement.set_reduce_motion(reduced);
        }
        for child in self.children_mut() {
            child.set_motion_reduced(reduced);
        }
    }

    fn primary_vertical_scroll(&self) -> Option<ui::widget::PrimaryScrollRegion> {
        if self.thread_open.is_some()
            && (self.thread_is_page() || matches!(self.focus, Focus::Thread))
        {
            return Some(ui::widget::PrimaryScrollRegion {
                key: "thread",
                owner_generation: None,
                metrics: self.thread_scroll.primary_vertical_scroll()?.metrics,
            });
        }
        Some(ui::widget::PrimaryScrollRegion {
            key: "primary",
            owner_generation: None,
            metrics: ui::widget::ScrollMetrics::new(
                self.scroll_content,
                self.scroll_viewport,
                (self.scroll_content - self.scroll_viewport).max(0.0)
                    - self
                        .scroll
                        .clamp(0.0, (self.scroll_content - self.scroll_viewport).max(0.0)),
            ),
        })
    }
    fn apply_primary_vertical_scroll(
        &mut self,
        action: ui::widget::PrimaryScrollAction,
    ) -> ui::widget::PrimaryScrollResult {
        action.apply_to(self, Self::primary_vertical_scroll, |app, region, next| {
            if region.key == "thread" {
                app.thread_scroll.set_offset_y(ui::num::f32_of_f64(next));
            } else {
                app.scroll = region.metrics.max_scroll() - next;
                app.timeline_at_end = app.scroll <= 0.5;
            }
            app.canonical_dirty = true;
        })
    }

    fn is_focusable(&self) -> bool {
        true
    }
    fn is_focused(&self) -> bool {
        self.host_focused
    }
    fn set_focus(&mut self, focused: bool) {
        self.host_focused = focused;
        if !focused {
            self.right_flap.set_resize_focused(false);
        }
        self.canonical_dirty = true;
        self.set_composer_focus();
    }
    fn ime_content(&self) -> Option<ui::ime::InputContent> {
        if let Some(id) = self.focused_agent_surface.as_deref()
            && let Some(surface) = self
                .agent_surfaces
                .values()
                .find(|surface| surface.id == id)
        {
            return surface.view.ime_content();
        }
        self.host_focused.then_some(ui::ime::InputContent::TEXT)
    }
    fn ime_cursor_rect(&self) -> Option<Rect> {
        if let Some(id) = self.focused_agent_surface.as_deref()
            && let Some(surface) = self
                .agent_surfaces
                .values()
                .find(|surface| surface.id == id)
        {
            return surface.view.ime_cursor_rect();
        }
        if !self.host_focused {
            return None;
        }
        if self.editing.is_some() {
            self.edit_input.ime_cursor_rect()
        } else {
            self.focused_composer().ime_cursor_rect()
        }
    }

    fn sync(&mut self, doc: &AppModel, path: &ModelPath) {
        if self.thread_open.is_some() {
            self.thread_scroll.sync(doc, path);
        }
        let (doc, _) = ui::doc::document_ref(doc, path);
        self.apply_realtime_stop();
        self.establish_realtime_target();
        self.announce_me(doc);
        // A fold made on another of this person's devices.
        if let Some(preferences) = &self.preferences {
            let collapsed = preferences.get().collapsed_categories;
            if collapsed != self.collapsed {
                self.collapsed = collapsed;
                self.reselect();
            }
        }
        self.settle_attachment_picker();
        self.settle_device_files();
        self.settle_refused_post();
        self.settle_attachment_import();
        self.refresh_person_conversation();
        {
            use automerge::transaction::Transactable as _;
            // Reload and repaint only when the document moved: committed heads
            // differ, or a transaction is or was open. Service completions run
            // before this comparison so their canonical changes are projected
            // in the same frame without making idle sync reload continuously.
            let heads = doc.base_heads();
            let pending = doc.pending_ops();
            // The messages move on their own, in the parts beside the
            // record.
            let parts_moved = self
                .chain
                .as_ref()
                .is_some_and(|chain| Some(chain.revision()) != self.chain_seen);
            let unchanged = !parts_moved
                && self
                    .synced
                    .as_ref()
                    .is_some_and(|(synced_heads, synced_pending)| {
                        *synced_heads == heads && *synced_pending == 0 && pending == 0
                    });
            if !unchanged {
                self.canonical_dirty = true;
                ui::tilo_resource::profiling::measure("chat.sync.reload", || self.reload(doc));
                self.synced = Some((heads, pending));
            }
        }
        if self.ensure_selection() {
            // Re-picking the channel changes which messages apply.
            self.canonical_dirty = true;
            self.reload(doc);
        }
        self.join_pending_call();
        self.open_requested();
        // The channel in view is read: advance its watermark (no-op when
        // nothing new arrived) so its unread badge stays cleared. Only a
        // presented instance has a viewer; a headless host worker never
        // authors read receipts.
        if self.presented {
            self.mark_active_read(doc);
        }
        self.sync_composers(doc);
        self.request_visible_images();
        self.sync_call_pictures();
        // Native capture/transport belongs to the shell's lease-bound service.
    }

    fn composition_follows_allocation(&self) -> bool {
        true
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The host video tile is an absolute leaf. In the docked huddle it
        // sits at a *fractional* home (sized correctly even before `self.rect`
        // is known); the dragged overlay offset is applied as an origin shift
        // in `apply_layout`. In the expanded overlay it instead tracks the
        // first cell of the responsive participant grid so the surface fills
        // its share at every width. That cell depends on the window size, so
        // it is expressed in absolute px derived from the last known
        // `self.rect`; before any layout pass the rect is empty, in which case
        // the node collapses to zero size (nothing to paint) and the next
        // frame — once the real size is known — positions it exactly.
        let video_style = if self.call.expanded {
            let host = self.call_video_rect();
            let (left, top, w, h) = if host.width() > 1.0 && host.height() > 1.0 {
                (
                    (host.x0 - self.rect.x0) as f32,
                    (host.y0 - self.rect.y0) as f32,
                    host.width() as f32,
                    host.height() as f32,
                )
            } else {
                (0.0, 0.0, 0.0, 0.0)
            };
            Style {
                position: Position::Absolute,
                inset: taffy::Rect {
                    left: taffy::style_helpers::length(left),
                    top: taffy::style_helpers::length(top),
                    right: LengthPercentageAuto::auto(),
                    bottom: LengthPercentageAuto::auto(),
                },
                size: Size::from_lengths(w, h),
                ..Default::default()
            }
        } else {
            let f = HUDDLE_VIDEO;
            Style {
                position: Position::Absolute,
                inset: taffy::Rect {
                    left: percent(f.0 as f32),
                    top: percent(f.1 as f32),
                    right: LengthPercentageAuto::auto(),
                    bottom: LengthPercentageAuto::auto(),
                },
                size: Size::from_percent(f.2 as f32, f.3 as f32),
                ..Default::default()
            }
        };
        let video_node = taffy.widget_leaf(video_style, node_map, &mut self.video);
        self.video_node = Some(video_node);

        // `new_with_children` only fails when a child belongs to a different
        // tree; `video_node` was just built in this one above.
        #[allow(
            clippy::unwrap_used,
            reason = "video_node was just created in this same tree"
        )]
        let root = taffy
            .new_with_children(
                Style {
                    size: Size::from_percent(1.0_f32, 1.0_f32),
                    ..Default::default()
                },
                &[video_node],
            )
            .unwrap();
        node_map.insert(root, std::ptr::from_mut::<dyn Widget>(self));
        self.root_node = Some(root);
        root
    }

    /// The host call tile has a node of its own; Chat routes and paints it.
    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        visitor(&self.video);
        true
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        if !self.presented {
            self.presented = true;
            self.read_on_present = true;
        }
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        if !self.call.expanded {
            self.call
                .placement
                .set_preferred_size(frac_rect(self.rect, HUDDLE_VIDEO).size());
            self.call
                .placement
                .set_attached_chrome(self.huddle_attached_chrome());
        }
        if let Some(vn) = self.video_node {
            // The docked tile's taffy node retains its responsive fractional
            // size, while the shared floating placement owns its actual
            // origin. Offset the node's origin so its resolved local location
            // lands exactly on that shared rect.
            let off = if self.call.expanded {
                Point::new(self.rect.x0, self.rect.y0)
            } else {
                let desired = self.call_video_rect();
                let video_layout = taffy.layout(vn).copied().unwrap_or_default();
                Point::new(
                    desired.x0 - f64::from(video_layout.location.x),
                    desired.y0 - f64::from(video_layout.location.y),
                )
            };
            self.video.apply_layout(taffy, vn, off);
        }
        self.laid_video = Some((self.call.expanded, self.call_video_rect()));
    }

    /// Chat paints what it shows itself; under it only the call's video
    /// tile is laid out. So a frame elsewhere on the desktop (another window
    /// opening, an animation, a peer's pointer) re-solves nothing here: the
    /// layout is dirty only while the expanded call's tile follows the
    /// participant grid, or when the tile's node would be placed elsewhere
    /// than it was laid, because the call closed or the tile moved.
    fn is_layout_dirty(&self) -> bool {
        self.call.expanded
            || self
                .laid_video
                .is_some_and(|laid| laid != (self.call.expanded, self.call_video_rect()))
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // The attachments painted below say when they step on next.
        let _draw_phase = ui::tilo_resource::profiling::Phase::start("chat.draw");
        self.arriving_wake.set(None);
        // Adopt this theme's chrome and advance the drawers for this frame.
        self.layout_text_scale = theme.text_scale;
        self.advance_flaps(theme);
        self.reconcile_huddle_motion(tr, theme);
        self.reconcile_speaking(theme);

        let mut hits: Vec<(Rect, Hit)> = Vec::new();
        // The picture tiles this frame draws note themselves again, and so
        // do the messages it shows.
        self.picture_tiles.borrow_mut().clear();
        self.painted_messages.clear();
        // Every region is drawn as a part of its own, compared with the
        // last frame's: what Chat reports as damage is the regions whose
        // drawing changed.
        let mut parts = PaintParts::new(scene);
        let window = self.rect;

        // The conversation stands on the window ground the framework painted.
        // A pushed thread owns the narrow page, including its input targets.
        if !self.thread_is_page() {
            self.draw_conversation(&mut parts, scene, tf, tr, theme, presence, &mut hits);
        }
        let drawers_phase = ui::tilo_resource::profiling::Phase::start("chat.draw.drawers");
        self.draw_drawers(&mut parts, scene, tf, tr, theme, presence, &mut hits);
        drawers_phase.end();
        self.declare_window_toolbar();

        // Call layers. The host tile is a real VideoPlayer, painted only once
        // a real capture has fed it a frame; otherwise the honest camera-off
        // placeholder covers the host tile.
        let host_has_frame = self.video.latest_frame().is_some();
        if self.call.active && self.call.expanded {
            // Measure the controls and header with this frame's text scale
            // before anything reads their geometry.
            self.measure_call_overlay(tr, theme);
            parts.draw(scene, "chat-call", window, |scene| {
                self.draw_call_overlay_bg(scene, tf, tr, theme);
                if host_has_frame {
                    self.video.draw(scene, tf, tr, theme, presence);
                }
                self.draw_host_camera_off(scene, tf, tr, theme);
                self.draw_call_overlay_fg(scene, tf, tr, theme, &mut hits);
            });
        } else if self.app_draws_huddle() {
            // Settled, the call window paints its panel and shadow; moving,
            // it may be anywhere in the window.
            let placement = &self.call.placement;
            let bounds = if placement.is_dragging() || placement.is_handoff_animating() {
                window
            } else {
                ui::widget::floating_surface::painted_bounds(self.huddle_panel(), theme)
                    .intersect(window)
            };
            parts.draw(scene, "chat-call", bounds, |scene| {
                self.draw_live_huddle(scene, tf, tr, theme, presence, &mut hits);
            });
        }

        // Popovers over the conversation; the context menu paints in the
        // top layer, from `draw_overlay`.
        if self.emoji_open {
            parts.draw(scene, "chat-emoji", window, |scene| {
                self.draw_emoji_picker(scene, tf, tr, theme, presence, &mut hits);
            });
        }
        if self.history_open.is_some() {
            parts.draw(scene, "chat-history", window, |scene| {
                self.draw_history_popover(scene, tf, tr, theme, &mut hits);
            });
        }
        if self.attach_open {
            parts.draw(scene, "chat-attach", window, |scene| {
                self.draw_attach_picker(scene, tf, tr, theme, &mut hits);
            });
        }
        if self.live_shares.panel.is_some() {
            parts.draw(scene, "chat-share", window, |scene| {
                self.draw_share_panel(scene, tf, tr, theme, &mut hits);
            });
        }
        self.declare_share_embeds();

        // Immediate-mode keyboard focus needs a visible counterpart. The ring
        // is painted last so it remains legible on sidebar and popovers.
        let semantic = Self::deduplicate_semantic_hits(self.semantic_hits_from(&hits));
        if self.host_focused
            && let Some(index) = self.keyboard_target
        {
            if let Some((rect, ..)) = semantic.get(index) {
                parts.draw(scene, "chat-focus", window, |scene| {
                    ui::widget::paint_focus_ring(
                        scene,
                        tf,
                        *rect,
                        f64::from(theme.tokens.radii.item),
                        theme,
                    );
                });
            } else {
                self.keyboard_target = None;
            }
        }

        let frame = parts.finish(scene, vec![window], tf, theme);
        self.paint_damage.record(window, frame);
        self.hits = hits;
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // A focused composer's editor floats its menus over the timeline.
        for composer in [&mut self.composer_main, &mut self.composer_thread] {
            if composer.is_focused() && composer.rect().width() > 0.0 {
                composer.draw_overlay(scene, tf, tr, theme, presence);
            }
        }
        if let Some(menu) = self.menu.as_mut() {
            menu.widget.draw_overlay(scene, tf, tr, theme, presence);
        }
        if self.channel_sheet.is_some() {
            let categories: Vec<(Id, String)> = self
                .ws_categories()
                .into_iter()
                .map(|category| (category.id.clone(), category.name.clone()))
                .collect();
            let window = self.rect;
            let focused = self.host_focused;
            if let Some(sheet) = self.channel_sheet.as_mut() {
                sheet.paint(
                    ui::paint::PaintCtx::new(scene, tf, tr, theme),
                    presence,
                    window,
                    &categories,
                    focused,
                );
            }
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let _phase = ui::tilo_resource::profiling::Phase::start("chat.handle_event");
        // What is typed belongs to the control that has the keyboard. A
        // desktop where no window holds it offers each key to every window
        // in turn, and Chat must not write into its composer what was not
        // typed into it.
        if !self.host_focused
            && matches!(
                event,
                WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
            )
        {
            return;
        }
        let secondary = self
            .input_agent_surface
            .as_deref()
            .or(self.focused_agent_surface.as_deref());
        if let Some(id) = secondary
            && let Some(surface) = self
                .agent_surfaces
                .values_mut()
                .find(|surface| surface.id == id)
        {
            surface.view.handle_event(event, ctx);
            self.canonical_dirty |= surface.view.canonical_dirty;
            self.settle_agent_surface_commands();
            return;
        }
        // Embedded inputs request focus with a temporary layout node. Map
        // that actual request to Chat's retained keyboard/IME owner.
        let was_handled = *ctx.handled;
        // The detail drawer takes its show/hide shortcut, Escape and a press
        // on its scrim, unless a popover over the conversation is open and
        // dismisses first.
        if self.menu.is_none()
            && self.channel_sheet.is_none()
            && !self.attach_open
            && self.live_shares.panel.is_none()
            && !self.emoji_open
            && self.history_open.is_none()
            && !self.thread_is_page()
        {
            self.reconcile_flaps();
            if self.right_flap.handle_event(event, ctx) {
                *ctx.handled = true;
                self.canonical_dirty = true;
                return;
            }
        }
        self.canonical_dirty |= self.right_flap.take_resize_redraw();
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Pressed,
                ..
            }
        ) {
            self.right_flap.set_resize_focused(false);
        }
        let requested = self.dispatch_event(
            event,
            ctx.scale_factor,
            *ctx.modifiers,
            &mut ctx.state,
            ctx.handled,
        );
        // Pointer motion that neither Chat nor its inputs consumed changes
        // nothing visible; every other event may. Marking every event dirty
        // made each pointer move anywhere on the desktop repaint everything.
        if !matches!(event, WindowEvent::CursorMoved { .. }) || *ctx.handled || requested {
            self.canonical_dirty = true;
        }
        if !was_handled && requested && ctx.focus_target.is_none() {
            *ctx.focus_target = Some(ctx.node_id);
        }
        // A menu's Rename names the row in the sidebar, which takes the
        // keyboard for it.
        if let Some(place) = self.sidebar_rename.take() {
            self.places.begin_rename(&place, ctx.focus_target);
        }
    }

    /// Chat's painted controls and elided header text take their tooltips
    /// through the toolkit's one mechanism, which shows them after a dwell
    /// and takes them away when the person acts or the pointer leaves.
    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        self.hover_tooltip_target(point)
    }

    /// The flap's resize edge, then the painted control under the pointer:
    /// a text field's I-beam, a handle's grab, a pointing hand on what a
    /// press activates. A popover's scrim asks for nothing.
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        use winit::window::CursorIcon;
        if let Some(resize) = self.right_flap.resize_cursor(point) {
            return Some(resize);
        }
        let (_, hit) = self
            .hits
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(point))?;
        match hit {
            Hit::Dismiss => None,
            Hit::FocusEmojiSearch => ui::Widget::cursor_shape(&self.emoji_picker, point),
            Hit::AgentDragHandle | Hit::CallDragHandle => Some(CursorIcon::Grab),
            hit if self.text_field_access(hit).is_some() => Some(CursorIcon::Text),
            _ => Some(CursorIcon::Pointer),
        }
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, Role};

        #[cfg(test)]
        crate::component_tests::ACCESSIBILITY_BUILDS
            .set(crate::component_tests::ACCESSIBILITY_BUILDS.get() + 1);
        let controls = self.control_accessibility();

        let conversation = self.conversation_accessibility();
        let mut children: Vec<AccessNode> = controls;
        children.extend(self.right_flap.resize_accessibility());
        if let Some(peer) = &self.direct_person {
            children.push(
                AccessNode::new(Role::Heading { level: 1 })
                    .with_name(self.person_display_name(peer))
                    .with_description(crate::i18n::t("chat-person-chat-private"))
                    .with_hashed_key(("person-conversation-heading", &self.active_channel)),
            );
        }
        if let Some(channel) = self.channel(&self.active_channel) {
            let mut heading = AccessNode::new(Role::Heading { level: 1 })
                .with_name(channel.kind.label(&channel.name))
                .with_hashed_key(("conversation-heading", &channel.id));
            if !channel.topic.trim().is_empty() {
                // The desktop header deliberately elides the topic before the
                // member/call cluster, and narrow layouts may omit it entirely.
                // Keep the complete string available to screen readers.
                heading = heading.with_description(channel.topic.clone());
            }
            children.push(heading);
        }
        children.push(conversation);
        // The call panel's count is painted in its title bar: read where it
        // is drawn, and no stop for the keyboard. Its status (what is
        // wrong, else how it is connected) is read with it.
        if self.call.active
            && let Some((title_bar, _)) = self
                .hits
                .iter()
                .find(|(_, hit)| matches!(hit, Hit::CallDragHandle))
        {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(self.call_count_label())
                    .with_description(self.call_status_label())
                    .with_bounds(*title_bar)
                    .with_hashed_key(("call-count", &self.call.channel)),
            );
        }
        children.extend(self.emoji_notice_accessibility());
        if let Some(menu) = self.menu.as_ref()
            && let Some(menu_node) = menu.widget.accessibility()
        {
            children.push(menu_node);
        }
        // Existing Chat controls are exported by their typed domain resources,
        // which carry device and destructive-operation metadata. Keep their
        // synthetic accessibility subtrees outside generic action projection.
        // Only the shared separators use the generic widget effect contract.
        for child in &mut children {
            if !matches!(
                child.action_target.as_deref(),
                Some("sidebar-resize" | "inspector-resize")
            ) {
                child.semantic_key = None;
            }
        }
        Some(
            AccessNode::new(Role::Group)
                .with_semantic_key("chat-surface")
                .with_name(crate::i18n::t("chat-surface"))
                .with_interface_name(true)
                .with_description(crate::i18n::t("chat-surface-detail"))
                .with_children(children),
        )
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let [index] = path else {
            return None;
        };
        self.semantic_hits()
            .get(*index)
            .map(|(rect, ..)| *rect)
            .or_else(|| {
                let offset = index.checked_sub(self.semantic_hits().len())?;
                self.right_flap
                    .resize_accessibility()
                    .filter(|_| offset == 0)
                    .and_then(|node| node.bounds.map(ui::a11y::AccessBounds::to_rect))
            })
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        let [index] = path else {
            return false;
        };
        if *index >= self.semantic_hits().len() {
            let offset = index.saturating_sub(self.semantic_hits().len());
            let target = self
                .right_flap
                .resize_accessibility()
                .filter(|_| offset == 0)
                .and_then(|node| node.action_target);
            return target.is_some_and(|target| {
                self.perform_drawer_resize(&target, &ui::a11y::Action::Focus)
            });
        }
        self.right_flap.set_resize_focused(false);
        self.keyboard_target = Some(*index);
        true
    }

    /// Semantic targets are the same inventory used by pointer and Tab/Enter
    /// input. Keeping this adapter at the widget boundary prevents an
    /// assistive-technology activation from taking a separate, untracked
    /// mutation path: `on_hit` submits the same verb or field edit through
    /// the dispatcher, for history and collaboration.
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ui::widget::ModelPath,
    ) -> bool {
        if self.perform_drawer_resize(target, action) {
            return true;
        }
        let (state, path) = ui::doc::document(state, path);
        if target.starts_with("context-menu:item:") {
            return self.perform_menu_semantic_action(target, action, state, path);
        }
        let hit = self
            .semantic_hits()
            .into_iter()
            .find_map(|(_, hit, ..)| (hit.semantic_id() == target).then_some(hit));
        let Some(hit) = hit else {
            return false;
        };
        // These immediate-mode editors are real TextInputs, even though their
        // chrome is painted by Chat. Preserve their truthful text semantics:
        // focus goes through the same focus route as a click, values remain a
        // local draft until Send/Save, and selections stay inside the toolkit
        // editor rather than becoming a second text model.
        if matches!(hit, Hit::FocusMain | Hit::FocusThread | Hit::FocusEdit) {
            return self.perform_editor_semantic_action(hit, action, state, path);
        }
        if !matches!(
            action,
            ui::a11y::Action::Activate | ui::a11y::Action::ToggleCheck
        ) {
            return false;
        }
        if matches!(hit, Hit::Send) && self.direct_person.is_some() {
            return self.send_person_focused();
        }
        let messages_before = matches!(hit, Hit::Send).then(|| {
            crate::projection::read(state)
                .message_rows(&self.held_messages(), &self.active_channel)
                .len()
        });
        self.on_hit(hit, state);
        messages_before.is_none_or(|before| {
            crate::projection::read(state)
                .message_rows(&self.held_messages(), &self.active_channel)
                .len()
                == before.saturating_add(1)
        })
    }

    fn name(&self) -> &'static str {
        "ChatApp"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn traps_focus(&self) -> bool {
        self.menu
            .as_ref()
            .is_some_and(|menu| menu.widget.traps_focus())
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.menu
            .as_ref()
            .and_then(|menu| menu.widget.modal_capture())
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.relayout)
    }

    /// Keep the loop ticking only while something is actually moving: a flap
    /// mid-slide, an overlay being dragged, or attachment images still
    /// decoding in the background. A live-but-static call does **not** force
    /// continuous redraws (the roster/rings are static), so an idle call
    /// no longer pins a core.
    fn update(&mut self, _doc: &AppModel, _path: &ModelPath) -> bool {
        let model_changed = self.pump_model();
        let room_changed = self.take_room();
        self.settle_attachment_save();
        // Re-run `sync` once after the first layout so the channel already in
        // view acquires its read receipt on open.
        std::mem::take(&mut self.read_on_present) || model_changed || room_changed
    }

    fn needs_animation_frame(&self) -> bool {
        self.read_on_present
            || self.host_model.needs_frame()
            || self.resource_navigation.pending()
            || (self.thread_open.is_some() && self.thread_scroll.needs_animation_frame())
            || self.flap_animating
            || self.call_chrome_motion.animating
            || self.message_bar.animating
            || self.speaking.animating
            || self.call.placement.is_dragging()
            || self.call.placement.is_handoff_animating()
            || self.agent_surfaces.values().any(|surface| {
                surface.placement.is_dragging() || surface.placement.is_handoff_animating()
            })
            || !self.images.is_idle()
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        // An attachment on its way steps its placeholder on at its
        // deadline; its bytes arriving wake the window themselves.
        self.native_voice
            .next_deadline(ui::time::now())
            .into_iter()
            .chain(self.arriving_wake.get())
            .min()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.right_flap.is_resizing()
            || self.call.placement.is_dragging()
            || self.agent_conversation_drag.is_some()
            || (self.thread_open.is_some() && self.thread_scroll.wants_pointer_capture())
    }

    /// Attachment thumbnails or call pictures still decoding: a capture
    /// waits for them.
    fn has_pending_visual_work(&self) -> bool {
        !self.images.is_idle() || self.call_pictures.pending() > 0
    }

    fn needs_redraw(&self) -> bool {
        self.canonical_dirty
            || (self.thread_open.is_some() && self.thread_scroll.needs_redraw())
            || self
                .arriving_wake
                .get()
                .is_some_and(|at| at <= ui::motion::frame_now())
    }
    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if self.needs_redraw() || self.paint_damage.is_recorded() {
            self.paint_damage.collect(self.rect, regions);
        }
    }
    fn damage_covers_own_box(&self) -> bool {
        self.paint_damage.covers_own_bounds()
    }
    fn release_scene_cache(&mut self) {
        self.paint_damage.release_scene_cache();
        for child in self.children_mut() {
            child.release_scene_cache();
        }
    }
    fn mark_drawn(&mut self) {
        self.canonical_dirty = false;
        self.paint_damage.mark_drawn();
        self.composer_main.mark_drawn();
        self.composer_thread.mark_drawn();
        self.thread_scroll.mark_drawn();
    }
}

impl ChatApp {
    fn perform_editor_semantic_action(
        &mut self,
        hit: Hit,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ObjId,
    ) -> bool {
        use ui::a11y::Action;

        // `keyboard_target` is the immediate-mode Tab ring. A semantic
        // editor action moves focus into the actual TextInput instead, so
        // let that editor be the single focused accessibility child.
        self.keyboard_target = None;

        match action {
            Action::Focus | Action::Activate => {
                self.on_hit(hit, state);
                return true;
            }
            Action::SetValue(_) | Action::SetSelection { .. } => match hit {
                Hit::FocusMain => {
                    self.focus = Focus::Main;
                    self.composer_main.ensure_draft(state);
                    if !self.composer_main.perform_action(action, state, path) {
                        return false;
                    }
                    self.reload(state);
                }
                Hit::FocusThread => {
                    if !self.perform_thread_editor_action(action, state, path) {
                        return false;
                    }
                }
                Hit::FocusEdit => {
                    if self.editing.is_none() {
                        return false;
                    }
                    if !self.edit_input.perform_action(action, state, path) {
                        return false;
                    }
                }
                _ => return false,
            },
            _ => return false,
        }
        self.set_composer_focus();
        true
    }

    fn perform_thread_editor_action(
        &mut self,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &automerge::ObjId,
    ) -> bool {
        if self.thread_draft_key().is_none() {
            return false;
        }
        self.focus = Focus::Thread;
        self.composer_thread.ensure_draft(state);
        if !self.composer_thread.perform_action(action, state, path) {
            return false;
        }
        self.reload(state);
        true
    }

    fn perform_menu_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ObjId,
    ) -> bool {
        let Some(mut menu) = self.menu.take() else {
            return false;
        };
        let menu_handled = menu
            .widget
            .perform_semantic_action(target, action, state, path);
        let selection = menu.widget.take_selection();
        let selected = selection.and_then(|index| menu.items.get(index).copied());
        let menu_target = menu.target.clone();
        if menu.widget.is_open() {
            self.menu = Some(menu);
        }
        if let Some(selected) = selected {
            if self.can_participate() {
                self.apply_menu_action(menu_target, selected, state);
            }
            return true;
        }
        menu_handled
    }

    fn perform_drawer_resize(&mut self, target: &str, action: &ui::a11y::Action) -> bool {
        if matches!(action, ui::a11y::Action::Focus) {
            self.right_flap.set_resize_focused(false);
        }
        let handled = self.right_flap.perform_resize_action(target, action);
        if handled {
            self.keyboard_target = None;
            self.reconcile_flaps();
            self.canonical_dirty = true;
        }
        handled
    }
}

impl ui::presence::capability::PublishesPresence for ChatApp {
    /// That its person is writing in a composer, else which voice channel
    /// they are in: what the others' sidebars show under it.
    fn presence_payload(&self) -> Option<(ui::presence::PresenceKey, ui::PresencePayload)> {
        self.composer_presence().or_else(|| {
            (self.in_call() && !self.call.channel.is_empty()).then(|| {
                (
                    Self::voice_presence_key(&self.call.channel),
                    ui::PresencePayload::Caret { pos: 0 },
                )
            })
        })
    }
}

impl ui::collaboration::participants::Participants for ChatApp {
    fn join_participant(&mut self, participant: &tilo_actions::ParticipantId) -> bool {
        if participant.agent_task().is_some() {
            self.participant_labels
                .entry(participant.as_str().to_owned())
                .or_default();
        }
        false
    }
    fn leave_participant(&mut self, participant: &tilo_actions::ParticipantId) {
        self.participant_labels.remove(participant.as_str());
    }
    fn set_participant_label(&mut self, participant: &tilo_actions::ParticipantId, label: &str) {
        if participant.agent_task().is_some() {
            let label = label.trim();
            self.participant_labels
                .insert(participant.as_str().to_owned(), label.to_owned());
            // Every member names the agent's messages as the host labelled
            // the task.
            if !label.is_empty()
                && self
                    .agent_authors
                    .get(participant.as_str())
                    .map(String::as_str)
                    != Some(label)
            {
                self.submit(crate::resource::Chat::NAME_AGENT.with(
                    crate::record::chat::NameAgent {
                        participant: participant.as_str().to_owned(),
                        name: label.to_owned(),
                    },
                ));
            }
            // A host acting as this agent: it is also the local participant.
            if participant.as_str() == self.me.id && !label.is_empty() {
                label.clone_into(&mut self.me.name);
                self.me.initials = crate::identity::initials_for(label);
            }
        }
    }
}

impl ChatApp {
    /// The one rendering path for a docked or shell-presented huddle. The
    /// shell calls this exact method after ownership transfer, so video,
    /// participants, controls and hit regions remain live without duplication.
    fn draw_live_huddle(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        // The app copy, transfer proxy and native shell surface all pass
        // through this one presentation transform. Applying the handoff
        // flourish in an ownership-specific caller made the first app-owned
        // frame lose its soft deformation even though its geometry matched.
        let transform = self.call.placement.presentation_transform(
            transform,
            self.huddle_panel(),
            theme.reduce_motion,
        );
        self.draw_huddle_bg(scene, transform, text_renderer, theme, hits);
        // The large tile shows someone else once anyone is; this person's
        // own camera only while alone in the call.
        if self.huddle_features_me() {
            if self.video.latest_frame().is_some() {
                self.video
                    .draw(scene, transform, text_renderer, theme, presence);
            }
            self.draw_host_camera_off(scene, transform, text_renderer, theme);
        } else {
            self.draw_huddle_featured(scene, transform, text_renderer, theme);
        }
        self.draw_huddle_fg(scene, transform, text_renderer, theme, hits);
    }

    /// The detail drawer over (or beside) the conversation: the open thread,
    /// the pins or the workspace's people, over its scrim when it floats.
    fn draw_drawers(
        &mut self,
        parts: &mut PaintParts,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        // The detail drawer: the open thread, or the people in the workspace
        // (so the people button always reveals a real panel).
        if self.right_flap_open() || self.right_flap.reveal() > 0.001 {
            if self.thread_is_page() {
                let page = self.thread_rect();
                parts.draw(scene, "chat-thread", self.rect, |scene| {
                    ui::surface::region(scene, tf, page, ui::surface::Region::Content, theme);
                    ChatApp::push_hit(hits, page, Hit::FocusThread);
                    self.draw_thread(scene, tf, tr, theme, presence, hits);
                });
            } else {
                let scrim = self.right_flap.scrim_rect();
                if scrim.width() > 0.5 {
                    ChatApp::push_hit(hits, scrim, Hit::Dismiss);
                }
                let panel = self.right_flap.sidebar_rect();
                if panel.width() > 0.5 {
                    // Clipped to its panel, shadow and all.
                    parts.draw(scene, "chat-detail", panel, |scene| {
                        crate::paint::push_clip(scene, tf, panel);
                        let local = self.right_flap.paint_panel(scene, tf, theme);
                        if self.thread_open.is_some() {
                            self.draw_thread(scene, tf, tr, &local, presence, hits);
                        } else if self.pins_open {
                            self.draw_pins(scene, tf, tr, &local, hits);
                        } else {
                            self.draw_members(scene, tf, tr, &local, hits);
                        }
                        scene.pop_layer();
                    });
                }
            }
        }
        if self.thread_is_page() {
            return;
        }
        parts.draw(scene, "chat-dividers", self.rect, |scene| {
            self.right_flap.paint_resize(scene, tf, theme);
        });
    }

    /// Flag a taffy rebuild (kept for parity; the video rect is now set in
    /// `apply_layout` so most state changes don't need it, but call expand
    /// still requests a clean relayout).
    pub(crate) fn request_relayout(&mut self) {
        self.relayout = true;
        self.canonical_dirty = true;
    }
}

#[cfg(test)]
mod identity_on_screen_tests;

mod a11y;
mod agent_route;
mod call_chrome_motion;
mod composer_focus;
mod document;
mod floating_surfaces;
mod hits;
mod layout;
#[cfg(test)]
mod live_draft_presence_tests;
pub(crate) mod menus;
pub(crate) mod message_bar;
mod own_delete;
mod people;
mod room;
mod scenarios;
mod submission;
pub(crate) use call_chrome_motion::CallChromeMotion;
pub(crate) use hits::{Hit, PaintedMessage};
pub(crate) use menus::{ContextMenu, MenuAction, MenuTarget};
