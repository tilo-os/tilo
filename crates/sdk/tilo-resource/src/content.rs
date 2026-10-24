//! The content store of the window whose code is running.
//!
//! A record names its binaries (a picture, an attachment) by hash; the bytes
//! live in the content store the host lends the window, joined to the
//! record's route. Every window has its own: two windows of one app in two
//! rooms — in one process, as the shell mounts apps — keep their content
//! apart. The host registers a window's store against the window's wake
//! scope ([`register`]), the scope the toolkit enters whenever it runs the
//! window's code; [`current`] answers the store of the scope entered now.
//!
//! That is how code no view reaches gets it: an action applying
//! ([`ActionCx::content`](crate::ActionCx::content)), an action's arguments
//! being built, the window's views and handlers. Work that leaves the
//! window's thread captures the store when it starts, as it captures its
//! wake handle.

use std::cell::RefCell;

use tilo_content::ContentStore;
use tilo_wake::WakeHandle;

thread_local! {
    static STORES: RefCell<Vec<(WakeHandle, ContentStore)>> = const { RefCell::new(Vec::new()) };
}

/// Keep `store` as the content store of the window whose wake scope
/// `window` names, in place of any it had. Windows that closed are
/// forgotten.
pub fn register(window: &WakeHandle, store: ContentStore) {
    STORES.with(|stores| {
        let mut stores = stores.borrow_mut();
        stores.retain(|(handle, _)| handle.is_active() && !handle.same_open_scope(window));
        stores.push((window.clone(), store));
    });
}

/// The content store of the window whose code is running: the one
/// registered for the wake scope entered now. `None` outside a window, or
/// for one whose host lent no store.
#[must_use]
pub fn current() -> Option<ContentStore> {
    let here = WakeHandle::new();
    STORES.with(|stores| {
        stores
            .borrow()
            .iter()
            .find(|(handle, _)| handle.same_open_scope(&here))
            .map(|(_, store)| store.clone())
    })
}

/// Why a content reference an action was given does not resolve here.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContentRefusal {
    /// The value is no `tilo.content:<hash>` reference.
    #[error(
        "{0:?} is not a content reference: give the tilo.content:<hash> a put or another step answered"
    )]
    NotAReference(String),
    /// No window runs, or its host lent it no store.
    #[error("this window has no content store to read {0} from")]
    NoStore(String),
    /// The window's store is not a room's, so a hash would name content from
    /// anywhere on this machine.
    #[error("{0} cannot be read here: this window's content store is not a room's")]
    NotInRoom(String),
    /// The room does not reference the content, or this device does not
    /// hold it.
    #[error("{0} is not readable in this room: put the content into this room first")]
    NotReadable(String),
}

/// The hash a `tilo.content:<hash>` reference names, when `value` is one.
#[must_use]
pub fn reference_hash(value: &str) -> Option<&str> {
    tilo_actions::content_hash(value.trim()).filter(|hash| tilo_content::valid_hash(hash))
}

/// The bytes the content reference `address` names, read from the store of
/// the window whose code is running.
///
/// A hash is never a capability by itself: only a store that answers for
/// one room ([`tilo_content::ContentStore::room_scoped`]) resolves a
/// reference, and it serves an address only when that room references it.
/// So a caller can name only content readable in the room it acts in.
///
/// # Errors
///
/// A [`ContentRefusal`] saying why, in words for whoever passed it.
pub fn read_reference(address: &str) -> Result<Vec<u8>, ContentRefusal> {
    let hash =
        reference_hash(address).ok_or_else(|| ContentRefusal::NotAReference(address.to_owned()))?;
    read_hash(hash)
}

/// [`read_reference`] for the hash itself, for a caller that accepted it in
/// another spelling.
///
/// # Errors
///
/// As [`read_reference`].
pub fn read_hash(hash: &str) -> Result<Vec<u8>, ContentRefusal> {
    let named = tilo_actions::content_resource(hash).unwrap_or_else(|| hash.to_owned());
    if !tilo_content::valid_hash(hash) {
        return Err(ContentRefusal::NotAReference(named));
    }
    let store = current().ok_or_else(|| ContentRefusal::NoStore(named.clone()))?;
    if !store.room_scoped() {
        return Err(ContentRefusal::NotInRoom(named));
    }
    store
        .read(hash)
        .filter(|bytes| tilo_content::verify(hash, bytes))
        .ok_or(ContentRefusal::NotReadable(named))
}

/// The schema of a byte parameter: the bytes inline, as integers, or a
/// reference to content readable in the room.
#[must_use]
pub fn bytes_schema() -> serde_json::Value {
    serde_json::json!({
        "oneOf": [
            {"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 255}},
            reference_schema(),
        ]
    })
}

/// The schema of a list of byte parameters, each as [`bytes_schema`].
#[must_use]
pub fn byte_list_schema() -> serde_json::Value {
    serde_json::json!({"type": "array", "items": bytes_schema()})
}

fn reference_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "string",
        tilo_actions::CONTENT_ANNOTATION: true,
        "pattern": "^tilo\\.content:[0-9a-f]{64}$",
    })
}

/// Bytes as a byte parameter takes them: inline, or by a content reference
/// resolved through [`read_reference`] where the action is built.
struct ByteArgument(Vec<u8>);

impl<'de> serde::Deserialize<'de> for ByteArgument {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visit;

        impl<'de> serde::de::Visitor<'de> for Visit {
            type Value = ByteArgument;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("bytes, or a tilo.content:<hash> reference")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<ByteArgument, E> {
                read_reference(value).map(ByteArgument).map_err(E::custom)
            }

            fn visit_bytes<E: serde::de::Error>(self, value: &[u8]) -> Result<ByteArgument, E> {
                Ok(ByteArgument(value.to_vec()))
            }

            fn visit_byte_buf<E: serde::de::Error>(
                self,
                value: Vec<u8>,
            ) -> Result<ByteArgument, E> {
                Ok(ByteArgument(value))
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<ByteArgument, A::Error> {
                let mut bytes = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(1 << 20));
                while let Some(byte) = seq.next_element::<u8>()? {
                    bytes.push(byte);
                }
                Ok(ByteArgument(bytes))
            }
        }

        deserializer.deserialize_any(Visit)
    }
}

/// Decode a byte parameter: its bytes inline, or a `tilo.content:<hash>`
/// reference read through [`read_reference`]. Actions carry intent and
/// references; bytes travel by session transport into the content store,
/// and are read from there where the action is built. A recorded action
/// keeps the bytes it was built with, so its replay reads no reference.
///
/// # Errors
///
/// Neither shape, or a reference that does not resolve here.
pub fn bytes<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
    <ByteArgument as serde::Deserialize>::deserialize(deserializer).map(|bytes| bytes.0)
}

/// Decode a list of byte parameters, each as [`bytes`].
///
/// # Errors
///
/// As [`bytes`], for any item.
pub fn byte_lists<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Vec<u8>>, D::Error> {
    <Vec<ByteArgument> as serde::Deserialize>::deserialize(deserializer)
        .map(|items| items.into_iter().map(|bytes| bytes.0).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_window_reads_its_own_store() {
        let (one, two) = (tilo_wake::WakeScope::new(), tilo_wake::WakeScope::new());
        let (first, second) = (ContentStore::memory(), ContentStore::memory());
        register(&one.handle(), first.clone());
        register(&two.handle(), second);
        let hash = first.put(b"in the first window").unwrap();
        assert!(current().is_none(), "no window runs");
        {
            let _entered = one.enter();
            assert!(current().unwrap().has(&hash));
        }
        {
            let _entered = two.enter();
            assert!(!current().unwrap().has(&hash));
        }
    }

    #[derive(Debug, serde::Deserialize)]
    struct Picture {
        #[serde(deserialize_with = "bytes")]
        bytes: Vec<u8>,
        #[serde(default, deserialize_with = "byte_lists")]
        more: Vec<Vec<u8>>,
    }

    fn decode(value: serde_json::Value) -> Result<Picture, String> {
        serde_json::from_value(value).map_err(|error| error.to_string())
    }

    /// A byte parameter takes its bytes inline or by a reference to content
    /// the window's room holds, and refuses any other hash.
    #[test]
    fn a_byte_parameter_reads_a_reference_only_from_the_rooms_store() {
        let window = tilo_wake::WakeScope::new();
        let room = ContentStore::memory_in_room();
        register(&window.handle(), room.clone());
        let held = room.put(b"a picture the room holds").unwrap();
        let reference = tilo_actions::content_resource(&held).unwrap();
        let elsewhere = tilo_content::hash_bytes(b"content kept elsewhere");
        let _entered = window.enter();

        let inline = decode(serde_json::json!({"bytes": [1, 2, 3]})).unwrap();
        assert_eq!(inline.bytes, [1, 2, 3]);
        let named =
            decode(serde_json::json!({"bytes": reference, "more": [[4], reference]})).unwrap();
        assert_eq!(named.bytes, b"a picture the room holds");
        assert_eq!(
            named.more,
            [b"\x04".to_vec(), b"a picture the room holds".to_vec()]
        );

        let missing = format!("tilo.content:{elsewhere}");
        let refused = decode(serde_json::json!({"bytes": missing})).unwrap_err();
        assert!(
            refused.contains(&ContentRefusal::NotReadable(missing).to_string()),
            "{refused}"
        );
        assert!(
            decode(serde_json::json!({"bytes": held})).is_err(),
            "a bare hash is no reference"
        );
    }

    /// A store that is not a room's resolves no reference, even to content it
    /// holds: a hash is never a capability by itself.
    #[test]
    fn a_store_outside_a_room_resolves_no_reference() {
        let window = tilo_wake::WakeScope::new();
        let machine = ContentStore::memory();
        register(&window.handle(), machine.clone());
        let held = machine.put(b"on this machine").unwrap();
        let reference = tilo_actions::content_resource(&held).unwrap();
        assert_eq!(
            read_reference(&reference),
            Err(ContentRefusal::NoStore(reference.clone())),
            "outside the window"
        );
        let _entered = window.enter();
        assert_eq!(
            read_reference(&reference),
            Err(ContentRefusal::NotInRoom(reference.clone()))
        );
    }
}
