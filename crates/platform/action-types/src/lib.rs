//! Plain names the action model and the toolkit share: which action of which resource
//! ([`ActionAddress`]), a control's request to run it ([`ControlInvocation`]), one invocation
//! of it ([`InvocationId`]) and the change it made ([`ActionId`]), whose view state it is
//! ([`PresenceId`]), which resource ([`ResourceId`]) and which history entry
//! ([`HistoryEntryId`]).
//!
//! They are data alone. The action model (`tilo-actions`) builds on them, and the
//! toolkit's widgets carry them without knowing that model.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use uuid::timestamp::{Timestamp, context::ContextV7};

pub mod history;
pub use history::HistoryEntryId;

/// Field separator inside a flattened composite key — a unit-separator
/// control char never appears in a resource or action id.
pub const KEY_SEP: char = '\u{1f}';

/// The stable address of one exported operation: which resource, which
/// action. Both halves are the app's own ids — no screen position, no
/// list index — so the same address names the same operation across
/// layouts, themes, resolutions, and a caller with no display can work
/// from ids and labels alone.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ActionAddress {
    /// The id the app publishes the one resource the operation acts on under:
    /// a control that stands for one row among many names its row, so the
    /// second task's Complete is not the first's.
    pub resource: String,
    /// The id the resource publishes the action under. A label, a declared
    /// id or a kind never stands in for it: the API resolves this pair and
    /// nothing else.
    pub action: String,
}

impl ActionAddress {
    pub fn new(resource: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            resource: resource.into(),
            action: action.into(),
        }
    }

    /// The address of `resource` and `action`, or `None` when either half
    /// cannot survive [`Self::encode`]: an empty id or one containing
    /// [`KEY_SEP`] would let two different pairs name one string.
    #[must_use]
    pub fn checked(resource: impl AsRef<str>, action: impl AsRef<str>) -> Option<Self> {
        let (resource, action) = (resource.as_ref(), action.as_ref());
        (!resource.is_empty()
            && !action.is_empty()
            && !resource.contains(KEY_SEP)
            && !action.contains(KEY_SEP))
        .then(|| Self::new(resource, action))
    }

    /// The single-string form, for transports that carry one id. [`KEY_SEP`]
    /// never appears in either half, so [`parse`](Self::parse) is unambiguous.
    pub fn encode(&self) -> String {
        format!("{}{KEY_SEP}{}", self.resource, self.action)
    }

    /// Recover an address from [`encode`](Self::encode). `None` when the string
    /// is not one, so a caller that made an address up is refused here rather
    /// than looked up against a half-parsed id.
    pub fn parse(s: &str) -> Option<Self> {
        let (resource, action) = s.split_once(KEY_SEP)?;
        Self::checked(resource, action)
    }
}

/// Stable identity of a resource within the publishing app instance
/// (e.g. `tube.video:dQw4w9WgXcQ`, `ebook.annotation:<uuid>`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ResourceId(pub String);

impl ResourceId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Stable key carried from admission through completion and used to
/// deduplicate retry-safe calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InvocationId(pub Uuid);

impl InvocationId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

/// An invocation id as a caller writes it: the uuid it is, which [`std::str::FromStr`] reads back.
impl std::fmt::Display for InvocationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Read an invocation id a caller keeps in its own words (a page's dialog, a script's file) so
/// its next ask is the same invocation: the server answers it with what the first did.
impl std::str::FromStr for InvocationId {
    type Err = uuid::Error;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(text.trim()).map(Self)
    }
}

impl Default for InvocationId {
    fn default() -> Self {
        Self::new()
    }
}

/// Upper bound on a [`PresenceId`] in bytes. Presences key bounded
/// per-view maps in app workers; they are never free text.
pub const MAX_PRESENCE_ID_BYTES: usize = 128;

/// One presence in a shared record: a person's window or a host-minted
/// execution. Each presence owns its own view state (caret, selection,
/// Find), and the app builds that presence's resource tree from it
/// (the toolkit's `PublishesResources::resources_for`).
///
/// Opaque, non-empty, ASCII-printable and at most
/// [`MAX_PRESENCE_ID_BYTES`] bytes. It is a routing key for view state,
/// never authority: the authenticated actor is the invoker's `acting`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PresenceId(String);

impl PresenceId {
    /// Validate and wrap a presence id.
    pub fn new(id: impl Into<String>) -> Result<Self, String> {
        let id = id.into();
        if id.is_empty()
            || id.len() > MAX_PRESENCE_ID_BYTES
            || !id.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(format!(
                "presence id must be 1..={MAX_PRESENCE_ID_BYTES} printable ASCII bytes"
            ));
        }
        Ok(Self(id))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// One host-minted execution's private view. This key is subordinate to the
    /// admitted actor and confers no authority. Hosts derive it from their
    /// execution lease, never a client-supplied run label or context.
    pub fn for_execution(execution: uuid::Uuid) -> Self {
        Self(format!("execution:{execution}"))
    }
}

impl TryFrom<String> for PresenceId {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<PresenceId> for String {
    fn from(value: PresenceId) -> Self {
        value.0
    }
}

impl std::fmt::Display for PresenceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A globally-unique id minted per dispatched action. Its 16 bytes
/// double as the deterministic Automerge actor id, making replay
/// content-addressed identical across replicas.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ActionId(pub Uuid);

/// The counter that keeps a minted id after every id minted here before it:
/// one millisecond never goes backwards, and within a millisecond the
/// counter orders them.
static MINTED: Mutex<ContextV7> = Mutex::new(ContextV7::new());

impl ActionId {
    /// Mint a fresh id, ordered after every id this device minted before it.
    ///
    /// Its bytes are the actor of the action's change, and a document keeps its actors sorted:
    /// an actor that sorts before others renumbers the actor of every op the document holds.
    /// A time-ordered id (`UUIDv7`: milliseconds, then random bits) sorts after the actors a
    /// document already has, so taking an action's change in costs what the change holds.
    ///
    /// The time is [`tilo_clock`]'s, which is the reading this process was given: an app model
    /// runs as a WebAssembly module with no clock of its own, and what it stamps an action's
    /// change with is the host's time.
    pub fn new() -> Self {
        let now_ms = tilo_clock::unix_now_ms();
        let seconds = now_ms / 1_000;
        let nanos = u32::try_from(now_ms % 1_000).map_or(0, |rest| rest.saturating_mul(1_000_000));
        Self(Uuid::new_v7(Timestamp::from_unix(&MINTED, seconds, nanos)))
    }

    /// The 16 raw bytes, used verbatim as the Automerge actor id.
    pub fn actor_bytes(&self) -> [u8; 16] {
        *self.0.as_bytes()
    }
}

impl Default for ActionId {
    fn default() -> Self {
        Self::new()
    }
}

/// A UI control's inert request to invoke one advertised operation.
///
/// This deliberately carries an address and typed JSON arguments, not an
/// encoded action. The app process resolves the address against its
/// *fresh* resource tree before opening the action API, so a control cannot
/// retain stale authority when availability or policy changes.
#[derive(Clone, Debug, PartialEq)]
pub struct ControlInvocation {
    pub address: ActionAddress,
    pub arguments: serde_json::Map<String, serde_json::Value>,
}

impl ControlInvocation {
    /// An invocation of `action` on `resource`, with its arguments, or `None`
    /// when either half is empty or contains the address separator.
    ///
    /// This checks the format only. The app process resolves the address
    /// against its fresh resource tree and revalidates the arguments;
    /// constructing a value grants no authority.
    #[must_use]
    pub fn from_parts(
        resource: impl AsRef<str>,
        action: impl AsRef<str>,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Option<Self> {
        Some(Self {
            address: ActionAddress::checked(resource, action)?,
            arguments,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resource ids carry colons, so the address separator has to be something
    /// they cannot contain; and a string that is not an address is refused
    /// rather than half-parsed into a lookup. An address is a resource and an
    /// action, and nothing else: a third part is no address.
    #[test]
    fn an_address_survives_its_string_form() {
        let address = ActionAddress::new("notes.note:1", "star");
        assert_eq!(ActionAddress::parse(&address.encode()), Some(address));
        let one = ActionAddress::new("notes.note:1", "edit");
        assert_eq!(ActionAddress::parse(&one.encode()), Some(one));
        assert_eq!(ActionAddress::parse("notes.note:1"), None);
        assert_eq!(ActionAddress::parse(&format!("{KEY_SEP}star")), None);
        assert_eq!(
            ActionAddress::parse(&format!("notes{KEY_SEP}{KEY_SEP}edit")),
            None
        );
        assert_eq!(
            ActionAddress::parse(&format!("notes.note{KEY_SEP}notes.note:1{KEY_SEP}edit")),
            None
        );
        assert_eq!(
            ActionAddress::checked("notes.note:1", "edit"),
            Some(ActionAddress::new("notes.note:1", "edit"))
        );
        assert_eq!(ActionAddress::checked("", "edit"), None);
        assert_eq!(ActionAddress::checked("notes.note:1", ""), None);
        assert_eq!(
            ActionAddress::checked(format!("notes{KEY_SEP}note"), "edit"),
            None
        );
        assert_eq!(
            ActionAddress::checked("notes.note:1", format!("edit{KEY_SEP}title")),
            None
        );
    }
}
