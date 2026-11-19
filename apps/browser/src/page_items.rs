//! A vault item's text on its way into a page.
//!
//! An agent that signs in to a site holds a reference to an item of its person's vault and
//! never what the item keeps. `fill-page-item` names a field and that reference. The window
//! says where its page is, by the address its engine committed, and asks whoever keeps the
//! vault for the item's text for that one site ([`ItemSource`]). What comes back is the text
//! in a type that prints nothing and is held once ([`tilo_webview::ReleasedText`]), or that its
//! person is asked first, or a refusal. The window hands a released text to its engine with
//! the site it was released for, and the engine writes it only into that site's own page.
//!
//! A window has a source only where something reaches its person's vault for it. Without one
//! it does not offer the action.

use tilo_webview::{FillInto, ReleasedText};
use ui::tilo_actions::{ActionInvoker, InvocationId};

/// The failure code of a fill that was not made because its person is asked first: the code a
/// use of a vault item by reference ends with wherever its person must allow it.
pub const ASKED_FIRST: &str = "asked_person_first";

/// What a window asks of whoever keeps its person's vault.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemAsk {
    /// The page action that asks: an answer is for this one and no other.
    pub invocation: InvocationId,
    /// The item, by its reference.
    pub item: String,
    /// The site the window's page is at: its origin, as the engine committed it.
    pub origin: String,
    /// Why whoever asks says it needs the item, in its own words; empty when it gave none.
    pub reason: String,
    /// Who the action was admitted for.
    pub by: Option<ActionInvoker>,
}

/// What a source answers an [`ItemAsk`].
#[derive(Debug)]
pub enum ItemAnswer {
    /// The item's text for that site, and the kind of field it is for.
    Released { text: ReleasedText, into: FillInto },
    /// Its person is asked first and nothing was released: what whoever asked is told.
    Asked { words: String },
    /// The item is not released there, and why, in words that name no secret.
    Refused { reason: String },
}

/// Whoever keeps a person's vault, as a window reaches it.
pub trait ItemSource {
    /// Ask for `ask`. The answer is read with [`Self::answered`].
    fn ask(&self, ask: ItemAsk);

    /// The answer to what `invocation` asked, handed out once; `None` while there is none.
    fn answered(&self, invocation: InvocationId) -> Option<ItemAnswer>;

    /// Whoever asked as `invocation` no longer waits: an answer for it is wanted no more.
    fn forget(&self, invocation: InvocationId);
}

/// The origin of `address`, when a text kept for one site may be written into a page there:
/// a page that arrived over https, or from this computer. `None` for any other address. The
/// engine decides again in the step that writes; this keeps the vault from being asked for a
/// page it would refuse.
#[must_use]
pub fn released_origin(address: &str) -> Option<String> {
    let origin = url::Url::parse(address).ok()?.origin();
    let url::Origin::Tuple(scheme, name, _) = &origin else {
        return None;
    };
    let name = name.to_string();
    let this_computer = name == "localhost"
        || name.ends_with(".localhost")
        || name
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    match scheme.as_str() {
        "https" => {}
        "http" if this_computer => {}
        _ => return None,
    }
    Some(origin.ascii_serialization())
}
