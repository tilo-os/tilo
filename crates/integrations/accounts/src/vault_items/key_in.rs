//! How a key rides in a request to its service.
//!
//! A service takes its key in one place: most as a bearer token, some in a
//! header of their own, some as the name of a basic sign-in, a few in the
//! address. The item says which, once, when it is kept. Whoever later makes
//! a request with the key by reference names the request and never where
//! the key goes in it.

use super::ItemError;

/// The longest name of a header or of a parameter of the address.
const MAX_NAME_BYTES: usize = 64;
/// The longest word a service wants before its key.
const MAX_SCHEME_BYTES: usize = 32;

/// The shortest key that is sent with a request an agent wrote. What a
/// service answers is handed to the agent with the key struck out of it, and
/// a short key could be found by sending guesses and reading which of them
/// came back struck.
pub const MIN_SENT_KEY_BYTES: usize = 16;

const HOW: &str = "A key is sent as bearer, basic, header:<name>, header:<name>:<word before the key> or query:<name>";

/// Whether `name`, in lower case, is a header a key never rides in: what
/// frames a request, says who it is for or carries a session is not where a
/// service takes its key.
fn never_in(name: &str) -> bool {
    matches!(
        name,
        "accept"
            | "connection"
            | "content-length"
            | "content-type"
            | "cookie"
            | "expect"
            | "forwarded"
            | "host"
            | "keep-alive"
            | "origin"
            | "proxy-authorization"
            | "proxy-connection"
            | "referer"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "user-agent"
    )
}

/// Where a key rides in a request to the service it belongs to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum KeyIn {
    /// `Authorization: Bearer <key>`, what most services take.
    #[default]
    Bearer,
    /// `Authorization: Basic …` of the item's name and the key; of the key
    /// as the name with no password where the item has no name.
    Basic,
    /// A header the service names, such as `X-Api-Key`, with the word the
    /// service wants before the key where it wants one
    /// (`Authorization: Token <key>`).
    Header {
        /// The header's name.
        name: String,
        /// The word before the key; empty for the key alone.
        scheme: String,
    },
    /// A parameter of the address, such as `api_key`.
    Query {
        /// The parameter's name.
        name: String,
    },
}

/// Whether `text` is a name a header or a parameter can have: letters,
/// digits, `-` and `_`, and for a parameter `.` too.
fn is_name(text: &str, dots: bool) -> bool {
    !text.is_empty()
        && text.len() <= MAX_NAME_BYTES
        && text.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || (dots && byte == b'.')
        })
}

impl KeyIn {
    /// What `text` says: `bearer` (or nothing), `basic`, `header:<name>`,
    /// `header:<name>:<word before the key>` or `query:<name>`.
    ///
    /// # Errors
    /// It says none of those, or names a header a key never rides in.
    pub fn parse(text: &str) -> Result<Self, ItemError> {
        let text = text.trim();
        let (how, rest) = text.split_once(':').unwrap_or((text, ""));
        match (how.to_ascii_lowercase().as_str(), rest) {
            ("" | "bearer", "") => Ok(Self::Bearer),
            ("basic", "") => Ok(Self::Basic),
            ("header", named) => {
                let (name, scheme) = named.split_once(':').unwrap_or((named, ""));
                let lower = name.to_ascii_lowercase();
                if !is_name(name, false)
                    || never_in(&lower)
                    || scheme.len() > MAX_SCHEME_BYTES
                    || !scheme
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                {
                    return Err(ItemError::Invalid(HOW));
                }
                Ok(Self::Header {
                    name: name.to_owned(),
                    scheme: scheme.to_owned(),
                })
            }
            ("query", name) if is_name(name, true) => Ok(Self::Query {
                name: name.to_owned(),
            }),
            _ => Err(ItemError::Invalid(HOW)),
        }
    }

    /// The header the key rides in, in lower case; `None` for a key that
    /// rides in the address.
    #[must_use]
    pub fn header(&self) -> Option<String> {
        match self {
            Self::Bearer | Self::Basic => Some("authorization".to_owned()),
            Self::Header { name, .. } => Some(name.to_ascii_lowercase()),
            Self::Query { .. } => None,
        }
    }

    /// As an item keeps it: nothing for a bearer token, which is what an
    /// item that says nothing is.
    pub(super) fn kept(&self) -> String {
        match self {
            Self::Bearer => String::new(),
            other => other.to_string(),
        }
    }
}

impl std::fmt::Display for KeyIn {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bearer => formatter.write_str("bearer"),
            Self::Basic => formatter.write_str("basic"),
            Self::Header { name, scheme } if scheme.is_empty() => {
                write!(formatter, "header:{name}")
            }
            Self::Header { name, scheme } => write!(formatter, "header:{name}:{scheme}"),
            Self::Query { name } => write!(formatter, "query:{name}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{ItemId, ItemKind, NewItem, Place, VaultItems};
    use super::*;

    /// A made-up key: no service ever issued it.
    const KEY: &str = "made-up-key-0123456789-for-tests";

    fn item(kind: ItemKind, place: &str, secret: &str) -> NewItem {
        NewItem::new(
            kind,
            vec![Place::parse(place).unwrap()],
            String::new(),
            secret.to_owned(),
        )
    }

    /// Each way a key rides is read from its own spelling and written back
    /// the same.
    #[test]
    fn where_a_key_rides_reads_and_writes_the_same() {
        for (text, read) in [
            ("bearer", KeyIn::Bearer),
            ("basic", KeyIn::Basic),
            (
                "header:X-Api-Key",
                KeyIn::Header {
                    name: "X-Api-Key".into(),
                    scheme: String::new(),
                },
            ),
            (
                "header:Authorization:Token",
                KeyIn::Header {
                    name: "Authorization".into(),
                    scheme: "Token".into(),
                },
            ),
            (
                "query:api_key",
                KeyIn::Query {
                    name: "api_key".into(),
                },
            ),
        ] {
            assert_eq!(KeyIn::parse(text).unwrap(), read, "{text}");
            assert_eq!(read.to_string(), text);
        }
        assert_eq!(KeyIn::parse("").unwrap(), KeyIn::Bearer);
        assert_eq!(KeyIn::parse(" Bearer ").unwrap(), KeyIn::Bearer);
        assert!(KeyIn::Bearer.kept().is_empty());
        assert_eq!(KeyIn::Basic.header().as_deref(), Some("authorization"));
        assert_eq!(
            KeyIn::parse("header:X-Api-Key")
                .unwrap()
                .header()
                .as_deref(),
            Some("x-api-key")
        );
        assert_eq!(KeyIn::parse("query:key").unwrap().header(), None);
    }

    /// A key rides in no header that frames a request or says who it is
    /// for, and in nothing that is no name.
    #[test]
    fn a_key_rides_in_no_header_that_is_the_senders_own() {
        for text in [
            "header:Host",
            "header:cookie",
            "header:Content-Length",
            "header:User-Agent",
            "header:Referer",
            "header:",
            "header:X Api Key",
            "header:X-Api-Key:two words",
            "header:X-Api-Key:line\nbreak",
            "query:",
            "query:a b",
            "query:a&b",
            "bearer:more",
            "basic:name",
            "somewhere",
        ] {
            assert!(
                matches!(KeyIn::parse(text), Err(ItemError::Invalid(_))),
                "{text:?}"
            );
        }
    }

    /// A key says where it rides when it is kept, and says it again to
    /// whoever sends it. Only a key does, and one too short to be struck
    /// out of an answer is not sent at all.
    #[test]
    fn a_key_says_where_it_rides_and_a_short_one_is_not_sent() {
        let mut items = VaultItems::default();
        let service = "https://api.example.test";
        let plain = items.add(item(ItemKind::Key, service, KEY)).unwrap();
        assert_eq!(items.key_in(&plain.id).unwrap(), KeyIn::Bearer);

        let named = KeyIn::parse("header:X-Api-Key").unwrap();
        let kept = items
            .add_key(item(ItemKind::Key, service, KEY), &named)
            .unwrap();
        assert_eq!(kept.kind, ItemKind::Key);
        assert_eq!(items.key_in(&kept.id).unwrap(), named);
        // Read again from the sealed file.
        let again = VaultItems::of_entries(items.kept());
        assert_eq!(again.key_in(&kept.id).unwrap(), named);
        assert!(
            again.uses(&kept.id).unwrap().is_empty(),
            "saying it uses nothing"
        );

        let login = items
            .add(item(ItemKind::Login, service, "a password"))
            .unwrap();
        assert_eq!(items.key_in(&login.id).err(), Some(ItemError::WrongKind));
        assert!(matches!(
            items.add_key(item(ItemKind::Login, service, "a password"), &named),
            Err(ItemError::Invalid(_))
        ));

        let short = items.add(item(ItemKind::Key, service, "0123")).unwrap();
        assert!(matches!(
            items.key_in(&short.id),
            Err(ItemError::Invalid(_))
        ));
        let nobody = ItemId::parse(&"0".repeat(32)).unwrap();
        assert_eq!(items.key_in(&nobody).err(), Some(ItemError::NotFound));
    }
}
