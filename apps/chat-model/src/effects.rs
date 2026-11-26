//! The effects Chat publishes: showing a conversation, a message or a person,
//! reading a message as a value, sending, sharing a file, and the call controls
//! of the shell's voice service.
//!
//! Each is offered bound on the item it acts on (a conversation, a category, a
//! message, a call), so the palette, an agent or a script reaches the same
//! handler a click does. What an effect changes in the document — the message a
//! shared file becomes — it changes through the resource's verbs and field
//! edits. Sharing a file and the call controls reach the device's files and
//! voice service: outside effects, decided by the person's answers like any
//! other.

use tilo_actions::send_to::Recipient;
use tilo_actions::{ResourceViewRangeSpec, ResourceViewSpec};
use tilo_resource::{self, AbsolutePath, Effect};

use crate::chat::Chat;

/// How many of a conversation's latest messages its row offers effects on;
/// any other message's row is resolved when it is addressed.
pub const RECENT_MESSAGES: usize = 20;

/// The id of the item resource the resource publishes for `id` of `field`.
pub fn item_row_id(field: &str, id: &str) -> String {
    tilo_resource::items::item_resource_id::<Chat>(field, &tilo_resource::Id::from_raw(id))
}

/// The views an item of the chat opens in, each opened by the item's own
/// view effect: a category (its first conversation), and a message (to a
/// range of its text). A conversation's view depends on its kind and is
/// offered by its row.
pub fn item_views(field: &str, _item: &tilo_resource::Id) -> Vec<ResourceViewSpec> {
    match field {
        "categories" => vec![
            ResourceViewSpec::new("category", "Category", ChatEffect::OPEN_CATEGORY.id)
                .with_primary(true),
        ],
        "messages" => vec![
            ResourceViewSpec::new("message", "Message", ChatEffect::REVEAL_MESSAGE.id)
                .with_primary(true)
                .with_range(ResourceViewRangeSpec::new(
                    ChatEffect::REVEAL_MESSAGE_RANGE.id,
                )),
        ],
        _ => Vec::new(),
    }
}

#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "chat.effects")]
pub enum ChatEffect {
    /// Ask the device's model about the latest message.
    #[effect(kind = "outside")]
    #[effect(row, returns, item = channels)]
    #[effect(on = crate::rows::Channel)]
    ModelControl {
        channel: String,
        #[param("Model command", choices = ["ask", "stop", "retry", "continue"])]
        command: String,
    },
    /// Show or hide reasoning actually supplied by the provider, in the
    /// caller's own view.
    #[effect(view, row, item = messages, audience = "agents", presence)]
    #[effect(on = crate::rows::Message)]
    ShowAgentReasoning { message: String, expanded: bool },
    /// Open conversation
    #[effect(view, row, item = channels)]
    #[effect(on = crate::rows::Channel)]
    OpenChannel { channel: String },
    /// Open category
    #[effect(view, row, item = categories)]
    OpenCategory { category: String },
    /// Fold or unfold category
    ///
    /// Hides or shows the category's channels in this person's sidebar.
    #[effect(view, row, item = categories)]
    ToggleCategory { category: String },
    /// Reveal message
    ///
    /// Scrolls the caller's own view to the message: the view its row
    /// discloses through, under the reader's read of the chat's messages.
    #[effect(view, row, item = messages, presence)]
    #[effect(on = crate::rows::Message, on = crate::rows::Report)]
    RevealMessage { message: String },
    /// Reveal message range
    #[effect(view, row, returns, item = messages, audience = "agents")]
    #[effect(on = crate::rows::Message)]
    RevealMessageRange {
        message: String,
        #[param("Start offset in the declared unit")]
        start: usize,
        #[param("End offset in the declared unit")]
        end: usize,
        #[param("Offset unit", choices = ["Chars", "Bytes"])]
        unit: String,
    },
    /// Show view
    ///
    /// Shows the view the message carries, alone, in the caller's own window:
    /// nothing of the conversation stands around it, and the window says how
    /// tall the view is.
    #[effect(view, row, item = messages)]
    #[effect(on = crate::rows::Message)]
    ShowView { message: String },
    /// Open the conversation with an agent: where search's agent hit goes.
    #[effect(view, returns, presence)]
    OpenAgent { agent: String },
    /// Set up the caller's personal assistant and open its direct conversation.
    #[effect(job, idempotent, returns, kind = "undoable")]
    SetUpPersonalAssistant,
    /// Open the conversation with a certified person public key: it changes
    /// only whose view is showing what.
    #[effect(view, audience = "agents", presence, on = crate::rows::DirectConversation)]
    OpenDirectConversation { public_key: String },
    /// Send to the direct conversation with a certified person public key.
    #[effect(kind = "outside")]
    #[effect(job, returns, audience = "agents", on = crate::rows::DirectConversation)]
    SendDirectMessage { public_key: String, text: String },
    /// Send a message
    ///
    /// Sends the words and everything they carry to the conversation with
    /// whom it names — people, agents, or a conversation that is already
    /// there — and answers the conversation it landed in, as the item that
    /// names it. Sent from a person's box, the conversation opens as a
    /// window of the shell where the box stood; sent by anyone else, nothing
    /// opens. The composer's send button is this same action, so what it
    /// does and what an agent's send does cannot drift apart.
    #[effect(returns, kind = "undoable")]
    Send {
        #[param("Who the message goes to")]
        to: Vec<Recipient>,
        #[param("What it carries besides its words")]
        attached: Vec<tilo_reference::Attached>,
        #[param("The message")]
        text: String,
        #[param("The message it answers, when it answers one in a thread")]
        parent: Option<String>,
        #[param("Its paragraphs as written, with their formatting")]
        body: Option<Vec<tilo_document::ClipBlock>>,
        #[param("Open the conversation as a window where the box that sent it stood")]
        opens: Option<bool>,
    },
    /// Export conversation
    ///
    /// Keeps the channel's transcript, oldest first, each line by the name
    /// readers see and its time as RFC 3339, as content, from every part
    /// of the chat's messages this window holds; answers with its content
    /// address. Changes nothing.
    #[effect(kind = "read", returns, idempotent, audience = "agents")]
    ExportContent {
        #[param("Id of the channel")]
        channel: String,
        #[param("md (the default) or json", choices = ["md", "json"])]
        format: Option<String>,
    },
    /// Attach file
    ///
    /// Shares a file from this device in a conversation: its bytes go to
    /// this device's attachment store and the message carries them by
    /// reference. Without a path, the conversation opens and the system's
    /// file chooser asks which files.
    #[effect(kind = "outside")]
    AttachFile {
        #[param("Id of the conversation")]
        channel: String,
        #[param(
            "Absolute path of the file on this device; omit to choose files with the system file chooser"
        )]
        path: Option<AbsolutePath>,
    },
    /// Share live Chat context
    ///
    /// Opt in to publishing the open conversation, thread and scroll position.
    #[effect(kind = "outside")]
    #[effect(idempotent)]
    ShareLiveContext,
    /// Stop sharing live Chat context
    #[effect(kind = "outside")]
    #[effect(idempotent)]
    StopSharingLiveContext,
    /// Live context status
    #[effect(kind = "outside")]
    #[effect(idempotent, returns, audience = "agents")]
    LiveContextStatus,
    /// Open Slack channel in browser
    #[effect(kind = "outside")]
    #[effect(job)]
    OpenSlackChannel { workspace: String, channel: String },
    /// Open Discord channel in browser
    #[effect(kind = "outside")]
    #[effect(job)]
    OpenDiscordChannel { server: String, channel: String },
    /// Join call
    ///
    /// Joins the channel's call muted, listening: being in a workspace's call is
    /// for whoever may read the workspace, as the channel itself is.
    #[effect(row, attended, kind = "read", on = crate::rows::Call)]
    JoinCall { channel: String },
    /// Leave call
    #[effect(row, idempotent, attended, kind = "read", on = crate::rows::Call)]
    LeaveCall { channel: String },
    /// Mute
    #[effect(row, idempotent, attended, kind = "read", on = crate::rows::Call)]
    Mute { channel: String },
    /// Unmute
    ///
    /// Speaks into the call: for whoever may write in the workspace. The
    /// microphone itself is asked for through the device's own prompt.
    #[effect(row, kind = "undoable", on = crate::rows::Call)]
    Unmute { channel: String },
    /// Deafen
    ///
    /// Hear nothing of the call, and be muted, until undeafened.
    #[effect(row, idempotent, attended, kind = "read", on = crate::rows::Call)]
    Deafen { channel: String },
    /// Undeafen
    ///
    /// Hear the call again; the microphone comes back as it was.
    #[effect(row, idempotent, attended, kind = "read", on = crate::rows::Call)]
    Undeafen { channel: String },
    /// Turn camera on
    ///
    /// Shows this person's camera to everyone in the call; asked for
    /// through the normal permission prompt.
    #[effect(row, kind = "undoable", on = crate::rows::Call)]
    CameraOn { channel: String },
    /// Turn camera off
    #[effect(row, idempotent, attended, kind = "read", on = crate::rows::Call)]
    CameraOff { channel: String },
    /// Share screen
    ///
    /// Shows this person's screen to everyone in the call; asked for
    /// through the normal permission prompt.
    #[effect(row, kind = "undoable", on = crate::rows::Call)]
    ShareScreen { channel: String },
    /// Stop sharing screen
    #[effect(row, idempotent, attended, kind = "read", on = crate::rows::Call)]
    StopSharingScreen { channel: String },
    /// Call status
    ///
    /// Requests a refresh and returns the last reported service state.
    /// The response is explicitly a cached report, never microphone consent.
    #[effect(row, idempotent, returns, kind = "read", audience = "agents", on = crate::rows::Call)]
    CallStatus { channel: String },
    /// Start a call
    ///
    /// Joins the workspace's voice channel, making one when the workspace has none,
    /// or reveals the call already running.
    #[effect(idempotent, attended, kind = "read")]
    StartCall,
    /// Start a video call
    ///
    /// Starts a call as Start a call does, with the camera on once it is
    /// live.
    #[effect(idempotent, kind = "undoable")]
    StartVideoCall,
    /// Select microphone
    #[effect(attended, kind = "read")]
    SelectInput {
        #[param("Id of the voice channel")]
        channel: String,
        #[param("Id of the audio input")]
        input: String,
    },
}
