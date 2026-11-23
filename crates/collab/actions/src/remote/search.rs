//! What a word a caller searches with matches: the word itself and its
//! inflections, and one resource a search found.
//!
//! An agent describes what it wants to do in its own words, and an app
//! describes the same gesture in its own: "draw" against "drawing a
//! signature", "scrolling" against "Scroll down with `PageDown`". Reducing
//! both to a stem is what makes those the same word.
//!
//! [`ResourceSearch`] is a provider's search of what its workspace holds: on a
//! desktop the one search it runs — the palette, text typed on the canvas
//! and an agent's `find` — and on a provider with no desktop its ranking of the
//! records it holds. Its results are the ranking's own, best first, each
//! naming the resource a caller addresses. A search may be narrowed to where
//! a thing was said and when ([`Within`]).

use super::{
    ActionCallError, ActionProvider, Caller, InvocationReceipt, InvokeArgument, InvokeRequest,
    Pending, ReceiptStatus, RemoteActionTarget,
};
use crate::api::ActionAddress;

/// The provider a workspace's search is asked under: what a provider answers from its own
/// search ([`ActionProvider::resource_search`]), as any other action it serves.
pub const PROVIDER: &str = "tilo.search";
/// The resource the search is an action of: the workspace's search.
pub const RESOURCE: &str = "search";
/// Find what the workspace holds by words, best first.
pub const FIND: &str = "find";

/// What [`FIND`] is invoked on.
#[must_use]
pub fn target() -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, FIND),
    }
}

/// Whether `target` asks the workspace's search.
#[must_use]
pub fn is_find(target: &RemoteActionTarget) -> bool {
    target.provider == PROVIDER
        && target.address.resource == RESOURCE
        && target.address.action == FIND
}

/// What [`FIND`] is asked with: the words, and what the search is narrowed to.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub needle: String,
    /// What the search is narrowed to; left out, everything the caller may read.
    #[serde(default, skip_serializing_if = "Within::is_everything")]
    pub within: Within,
}

impl Query {
    /// The invocation `invocation` that asks the search for `needle` within `within`.
    #[must_use]
    pub fn request(
        invocation: super::InvocationId,
        needle: &str,
        within: &Within,
    ) -> InvokeRequest {
        let query = Self {
            needle: needle.to_owned(),
            within: within.clone(),
        };
        let argument = match serde_json::to_value(query) {
            Ok(serde_json::Value::Object(fields)) => InvokeArgument::Json(fields),
            _ => InvokeArgument::None,
        };
        InvokeRequest {
            invocation,
            target: target(),
            argument,
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }
    }

    /// What `request` asks the search.
    ///
    /// # Errors
    /// Its argument is no query.
    pub fn of(request: &InvokeRequest) -> Result<Self, ActionCallError> {
        let InvokeArgument::Json(fields) = &request.argument else {
            return Err(ActionCallError::InvalidRequest {
                reason: "a search is asked with its words: `needle`".into(),
            });
        };
        serde_json::from_value(serde_json::Value::Object(fields.clone())).map_err(|error| {
            ActionCallError::InvalidRequest {
                reason: format!("a search is asked with `needle` and `within`: {error}"),
            }
        })
    }
}

/// The receipt that answers invocation `invocation` of [`FIND`] with what the search found.
#[must_use]
pub fn receipt(
    invocation: super::InvocationId,
    found: Result<Vec<Found>, ActionCallError>,
) -> InvocationReceipt {
    let status = match found.and_then(|found| {
        serde_json::to_value(found).map_err(|error| ActionCallError::Unavailable {
            reason: format!("the search's answer could not be written: {error}"),
        })
    }) {
        Ok(value) => ReceiptStatus::Completed {
            output: crate::ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            },
        },
        Err(error) => ReceiptStatus::Rejected { error },
    };
    InvocationReceipt { invocation, status }
}

/// What the search found, as `receipt` ([`receipt`]) answers it.
///
/// # Errors
/// The search refused, or answered with no results.
pub fn found(receipt: InvocationReceipt) -> Result<Vec<Found>, ActionCallError> {
    match receipt.status {
        ReceiptStatus::Completed { output } => output
            .value
            .and_then(|value| serde_json::from_value(value).ok())
            .ok_or_else(|| ActionCallError::Unavailable {
                reason: "the search answered no results".into(),
            }),
        ReceiptStatus::Rejected { error } => Err(error),
        _ => Err(ActionCallError::Unavailable {
            reason: "the search did not answer".into(),
        }),
    }
}

/// What `request` of [`FIND`] finds in `caller`'s workspace by `provider`'s own search
/// ([`ActionProvider::resource_search`]).
pub fn searched<H: ActionProvider + ?Sized>(
    provider: &H,
    caller: &Caller,
    request: &InvokeRequest,
) -> Pending<InvocationReceipt> {
    let invocation = request.invocation;
    let answer = move |found| receipt(invocation, found);
    let query = match Query::of(request) {
        Ok(query) => query,
        Err(error) => return Pending::ready(answer(Err(error))),
    };
    match provider.resource_search() {
        Some(search) => search
            .find_within(caller, &query.needle, &query.within)
            .map(answer),
        None => Pending::ready(answer(Err(ActionCallError::Unavailable {
            reason: "this provider holds no records to search".into(),
        }))),
    }
}

/// One resource a search found, as every caller of the one search reads a
/// result: the resource it is about, what it is, and what it reads as. It
/// crosses the wire as a provider's answer to a search asked over its API
/// ([`FIND`]).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Found {
    /// The resource's published id: what a caller addresses it by.
    pub resource: String,
    /// Its kind (`tl.so.tilo.notes.Notebook`), as its app publishes it.
    pub kind: String,
    /// Its label, as the person reads it.
    pub label: String,
    /// The action a caller runs to open it, when the result is one a caller
    /// opens: the resource's own (`open` for one that opens itself, a page's
    /// `open-pane`); `None` for a result that is not a resource of an app
    /// (a setting, a file, a command) or one reached another way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<String>,
    /// The arguments that open it, by parameter name.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub arguments: serde_json::Map<String, serde_json::Value>,
    /// The app publishing it, when one does: what a caller names it as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// The provider of the record it is in, where the app keeps many records
    /// (`record:<app>:<record>`): two PDFs publish their documents under the
    /// same id, and only this says which record a result is of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// When it was said or happened, in seconds since the Unix epoch, where what its app tells
    /// search says so (a message: when it was written).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<i64>,
}

/// The kind of a result that is a file of the workspace's folder: its
/// [`Found::resource`] is the file's path there (`legal/contract.txt`), which
/// a computer that logged in has below the workspace's folder.
pub const FOUND_FILE: &str = "file";

/// What a search that cannot be narrowed answers one that asks it to.
pub const NOT_NARROWED: &str =
    "this search cannot be narrowed to a conversation or a time: search by words alone";

/// What a search is narrowed to, beside its words: where a thing was said or is kept, and
/// when. Empty, it narrows nothing.
///
/// What an app tells search about a resource says both, where it has them (a message: the
/// conversation it is in and when it was written). So a search narrowed to one conversation or
/// to last week is answered from what search already holds, and nothing is read through for
/// it.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Within {
    /// Where: what a result is placed in (a conversation), by its published id or by the id it
    /// goes by among its kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
    /// From this moment on, in seconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<i64>,
    /// Before this moment, in seconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<i64>,
}

impl Within {
    /// Whether it narrows nothing.
    #[must_use]
    pub const fn is_everything(&self) -> bool {
        self.place.is_none() && !self.is_timed()
    }

    /// Whether it narrows to a stretch of time.
    #[must_use]
    pub const fn is_timed(&self) -> bool {
        self.after.is_some() || self.before.is_some()
    }

    /// Whether something placed at `place` (the published id of where it is, and the id that
    /// goes by) at `when` is inside it. What names no place is outside a search narrowed to
    /// one, and what names no time outside a search narrowed to a stretch of it.
    #[must_use]
    pub fn holds(&self, place: Option<(&str, &str)>, when: Option<i64>) -> bool {
        let placed = self.place.as_deref().is_none_or(|wanted| {
            place.is_some_and(|(published, known)| wanted == published || wanted == known)
        });
        let timed = !self.is_timed()
            || when.is_some_and(|when| {
                self.after.is_none_or(|after| when >= after)
                    && self.before.is_none_or(|before| when < before)
            });
        placed && timed
    }

    /// The moment `text` names, in seconds since the Unix epoch: a day (`2026-11-10`, its first
    /// second in UTC) or a moment with its offset (`2026-11-10T14:00:00Z`).
    #[must_use]
    pub fn moment(text: &str) -> Option<i64> {
        let text = text.trim();
        if let Ok(moment) = text.parse::<jiff::Timestamp>() {
            return Some(moment.as_second());
        }
        let day = jiff::civil::Date::strptime("%Y-%m-%d", text).ok()?;
        Some(
            day.to_zoned(jiff::tz::TimeZone::UTC)
                .ok()?
                .timestamp()
                .as_second(),
        )
    }

    /// How the moment `when` reads in an answer, as [`Self::moment`] reads it back:
    /// `2026-11-10T14:02:00Z`.
    #[must_use]
    pub fn said_at(when: i64) -> Option<String> {
        jiff::Timestamp::from_second(when)
            .ok()
            .map(|moment| format!("{moment:.0}"))
    }
}

/// A provider's search of what its workspace holds, as it offers it to a caller
/// who asks by words. A desktop answers with its one search, the palette's
/// ranking, the same results in the same order; a provider with no desktop (a
/// server's workspace) ranks the records it holds by what they say. Either
/// way the search lives where the records are, so an answer comes back
/// later, as an action's does, and a caller across the wire asks the same
/// search ([`FIND`]).
pub trait ResourceSearch: Send + Sync {
    /// What `needle` finds, best first, over the workspace `caller` acts in
    /// and what it may read there.
    ///
    /// # Errors
    /// The search could not be run: the provider has none, or it refuses
    /// `caller`.
    fn find(
        &self,
        caller: &super::Caller,
        needle: &str,
    ) -> super::Pending<Result<Vec<Found>, ActionCallError>>;

    /// [`Self::find`] narrowed to `within`. A search that cannot narrow says so
    /// ([`NOT_NARROWED`]) and never answers with everything instead: what is asked of last week
    /// is not answered with all time.
    ///
    /// # Errors
    /// As [`Self::find`], and where this search cannot be narrowed.
    fn find_within(
        &self,
        caller: &super::Caller,
        needle: &str,
        within: &Within,
    ) -> super::Pending<Result<Vec<Found>, ActionCallError>> {
        if within.is_everything() {
            return self.find(caller, needle);
        }
        super::Pending::ready(Err(ActionCallError::Unavailable {
            reason: NOT_NARROWED.into(),
        }))
    }
}

/// The stem an English word is matched by: its plural, its participle and
/// its "-ing" form reduce to the word a description most likely spells
/// ("drawing" and "draw", "dragging" and "drag", "scrolling" and "scroll",
/// "signed" and "sign"). An inflection is only undone when what is left is
/// still a word of reasonable length, so "ring" and "thing" keep their own
/// letters rather than becoming "r" and "th".
#[must_use]
pub fn stem_word(word: &str) -> String {
    let letters = word.chars().count();
    let strip = |ending: &str, shortest: usize| -> Option<String> {
        let kept = word.strip_suffix(ending)?;
        (word.chars().count() >= shortest && kept.chars().count() >= 3).then(|| kept.to_owned())
    };
    if letters <= 3 {
        return word.to_owned();
    }
    // An "-ing" and an "-ed" are where a word of a single syllable doubles
    // its last consonant ("dragging", "dragged"), so those undo the
    // doubling; a plural never does.
    let inflected = strip("ing", 4)
        .or_else(|| strip("ed", 5))
        .map(|stemmed| (stemmed, true))
        .or_else(|| strip("ies", 5).map(|stemmed| (format!("{stemmed}y"), false)))
        .or_else(|| strip("es", 5).map(|stemmed| (stemmed, false)))
        // "press" is not "pres": a word already ending in a double s is the
        // form "-ing", "-ed" and "-es" reduce to.
        .or_else(|| {
            (!word.ends_with("ss"))
                .then(|| strip("s", 4))
                .flatten()
                .map(|stemmed| (stemmed, false))
        });
    let Some((mut stemmed, doubled)) = inflected else {
        return word.to_owned();
    };
    if doubled && doubles_its_last_letter(&stemmed) {
        stemmed.pop();
    }
    stemmed
}

/// Whether a stem's last two letters are one consonant doubled, as "-ing"
/// and "-ed" leave it in "dragging" and "dragged": the "e" of "agreed", the
/// "l" of "called" and the "s" of "passed" are not doubled ones.
fn doubles_its_last_letter(stem: &str) -> bool {
    let mut doubled = stem.chars().rev().take(2);
    matches!(
        (doubled.next(), doubled.next()),
        (Some(last), Some(previous))
            if last == previous && !matches!(last, 'e' | 'l' | 's' | 'z')
    )
}

#[cfg(test)]
mod tests {
    use super::{Within, stem_word};

    #[test]
    fn a_search_is_narrowed_by_where_a_thing_is_and_when() {
        let conversation = ("pad/threads/t1", "t1");
        let last_week = Within {
            place: Some("pad/threads/t1".into()),
            after: Within::moment("2026-11-10"),
            before: Within::moment("2026-11-17T00:00:00Z"),
        };
        assert_eq!(last_week.after, Some(1_794_268_800));
        let inside = Within::moment("2026-11-12T09:30:00+01:00");
        assert!(last_week.holds(Some(conversation), inside));
        // By the id the place goes by, too.
        let by_id = Within {
            place: Some("t1".into()),
            ..Within::default()
        };
        assert!(by_id.holds(Some(conversation), None));
        // The stretch holds its first second and not its last.
        assert!(last_week.holds(Some(conversation), last_week.after));
        assert!(!last_week.holds(Some(conversation), last_week.before));
        // Elsewhere, earlier, and what says neither where nor when.
        assert!(!last_week.holds(Some(("pad/threads/t2", "t2")), inside));
        assert!(!last_week.holds(Some(conversation), Within::moment("2026-11-09")));
        assert!(!last_week.holds(None, inside));
        assert!(!last_week.holds(Some(conversation), None));
        // Nothing narrows nothing.
        assert!(Within::default().is_everything() && Within::default().holds(None, None));
        assert!(!last_week.is_everything());
        // A moment reads back as it is written, and words that name none name none.
        assert_eq!(
            Within::said_at(1_794_268_800).as_deref(),
            Some("2026-11-10T00:00:00Z")
        );
        assert_eq!(Within::moment("next week"), None);
    }

    #[test]
    fn a_stem_undoes_an_inflection_and_nothing_else() {
        for (word, stem) in [
            ("drawing", "draw"),
            ("dragging", "drag"),
            ("dragged", "drag"),
            ("scrolling", "scroll"),
            ("scrolled", "scroll"),
            ("scrolls", "scroll"),
            ("signing", "sign"),
            ("signed", "sign"),
            ("signs", "sign"),
            ("presses", "press"),
            ("pressing", "press"),
            ("copies", "copy"),
        ] {
            assert_eq!(stem_word(word), stem, "{word}");
        }
        // A word that is already a stem keeps its letters.
        for word in ["press", "ring", "thing", "id", "use", "scroll", "drag"] {
            assert_eq!(stem_word(word), word, "{word}");
        }
    }
}
