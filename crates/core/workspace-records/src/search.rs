//! The workspace's search: what a caller asks it, what it answers with, and how a provider that
//! keeps one answers.
//!
//! The actions are [`SearchAction`], declared once here: a caller finds by words, possibly
//! narrowed to where and when; the device's image model lists the pictures the search holds and
//! adds the words each was found to show. A provider holding a search answers them with [`serve`],
//! which reads each call once through [`decoded`] and does the work in [`Answers`].
//!
//! [`Answers`] is that provider's search of what its workspace holds: on a desktop the one search
//! it runs — the palette, text typed on the canvas and an agent's `find` — and on a provider with
//! no desktop its ranking of the records it holds, the words' own ranking, best first, each naming
//! the resource a caller addresses. A search may be narrowed to where a thing was said and when
//! ([`Within`]).
//!
//! What this declares is written beside this crate as `search-surface.json`: a crate that does
//! not depend on this one calls the actions through `tilo_resource::bindings!` of that file.

use tilo_actions::remote::{
    ActionCallError, Caller, InvocationReceipt, InvokeArgument, InvokeRequest, Pending,
    ReceiptStatus, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, ActionOutput};
use tilo_resource::Effect as _;
/// The kinds and the weights a search entry is written with, and what a hit answers about it,
/// read here so a caller of the search reads one path to them.
pub use tilo_resource::search_index::{IndexKind, IndexPerson, IndexWeight};

/// The provider the workspace's search is asked under.
pub const PROVIDER: &str = <SearchAction as tilo_resource::Effect>::OPERATION;
/// The resource the search is an action of.
pub const RESOURCE: &str = "search";

/// One picture the search holds: the entry an app published that names a picture's bytes.
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, tilo_resource::JsonSchema,
)]
pub struct Picture {
    /// The resource a result opens.
    pub resource: String,
    /// The app that keeps it.
    pub app: String,
    /// The workspace it is in.
    pub workspace: String,
    /// The record the app publishes it from.
    pub record: String,
    /// The picture's bytes: a content reference in the device's store.
    pub image: String,
}

/// What a picture was found to show: the words it is found by from now on, beside its own.
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, tilo_resource::JsonSchema,
)]
pub struct Description {
    pub picture: Picture,
    /// What the picture reads as where its app published none.
    pub title: String,
    /// The words, each with what a match in it counts.
    pub words: Vec<Word>,
}

/// One word a picture is found by, and what a match in it counts.
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, tilo_resource::JsonSchema,
)]
pub struct Word {
    /// The word.
    pub text: String,
    /// What a match in it counts.
    pub weight: IndexWeight,
}

/// One resource a search found, as every caller of the one search reads a result: the resource it
/// is about, what it is, and what it reads as. It crosses the wire as a provider's answer to a
/// search asked over its API (`tilo.search#find`).
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
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
    /// The record of its app it is in, where the app keeps many records: two PDFs publish
    /// their documents under the same id, and only this says which record a result is of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<String>,
    /// When it was said or happened, in seconds since the Unix epoch, where what its app tells
    /// search says so (a message: when it was written).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<i64>,
    /// The workspace it is in, where the search spans every workspace its caller sees (a
    /// person's own search); `None` where it is the caller's one workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// The search's own name for the result: where its app published it from and its key
    /// there, the same for the same result across searches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The line under its label, as its app says it (an entry's subtitle, else the group it
    /// lists it under).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    /// What it is, where its app keeps something more than a record: an agent, a conversation,
    /// a message, a person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about: Option<IndexKind>,
    /// Who it shows, the one in front first (a message's sender, a conversation's people), each
    /// by what the workspace calls them now; empty where the caller may read no name for them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub people: Vec<IndexPerson>,
    /// Where it is, by what that is called now: the conversation a message sits in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
    /// The named icon its app gave it (a lucide kebab name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// How well it matched, on the ranking's own scale: higher is better.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<i32>,
    /// Whether it opens as the item its app keeps, which the app draws as its own list draws it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub item: bool,
    /// Whether it is a secret its app keeps locked: nothing of it shows but its app, opened to
    /// unlock it ([`Found::app`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
}

/// The kind of a result that is a file of the workspace's folder: its [`Found::resource`] is the
/// file's path there (`legal/contract.txt`), which a computer that logged in has below the
/// workspace's folder.
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
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
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
    /// What one app publishes alone: a window's search of the app it shows, whether or not the
    /// app runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// What it is, where its app says ([`Found::about`]): asked with no words, every one of
    /// these, the most recent first (the people, agents and conversations a message can go to).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub about: Vec<IndexKind>,
}

impl Within {
    /// Whether it narrows nothing.
    #[must_use]
    pub fn is_everything(&self) -> bool {
        self.place.is_none() && self.app.is_none() && self.about.is_empty() && !self.is_timed()
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
        self.holds_in(place, None, when)
    }

    /// [`Self::holds`] for something that lives in a resource as well as being placed: the
    /// item `owner` names (a chapter of a book, a note of a notebook) is inside a search
    /// scoped to that resource, and so is anything whose address lies under the scope.
    #[must_use]
    pub fn holds_in(
        &self,
        place: Option<(&str, &str)>,
        owner: Option<(&str, &str)>,
        when: Option<i64>,
    ) -> bool {
        let placed = self.place.as_deref().is_none_or(|wanted| {
            place.is_some_and(|(published, known)| wanted == published || wanted == known)
                || owner.is_some_and(|(address, holder)| {
                    inside(wanted, address) || inside(wanted, holder)
                })
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

/// Whether `address` is `scope` or lies beneath it in the address path: the id an app publishes
/// an item under goes on from its holder's after a `/`.
///
/// An app hands the id it names a resource by inside itself (`notebook/note`), while the index
/// names every resource with the app it belongs to (`tl:so.tilo.notes:notebook/note`); the two
/// are the same resource, so each side is compared whole and without its app as well.
fn inside(scope: &str, address: &str) -> bool {
    beneath(scope, address)
        || beneath(bare(scope), bare(address))
        || beneath(scope, bare(address))
        || beneath(bare(scope), address)
}

/// `id` without the app it belongs to: `tl:<app>:<id>` as `<id>`.
fn bare(id: &str) -> &str {
    id.strip_prefix("tl:")
        .and_then(|rest| rest.split_once(':'))
        .map_or(id, |(_, id)| id)
}

/// Whether `address` is `scope` or lies under it in the address path.
fn beneath(scope: &str, address: &str) -> bool {
    address
        .strip_prefix(scope)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// The search's actions, each with the parameters its argument carries.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "tilo.search")]
pub enum SearchAction {
    /// Find what the workspace holds by words
    ///
    /// What the words find, best first, as [`Found`] hits: each naming the resource a caller
    /// addresses, and only what the caller may read.
    #[effect(kind = "read", returns = Vec<Found>)]
    Find {
        #[param("The words to find.")]
        needle: String,
        #[param(
            "Where and when to look: a conversation, a stretch of time, one app, or what a result \
             is about. Left out, everything the caller may read."
        )]
        within: Option<Within>,
    },
    /// List the pictures the search holds
    ///
    /// Every picture the caller may read, as [`Picture`]: what the device's image model reads to
    /// say what each shows.
    #[effect(kind = "read", returns = Vec<Picture>)]
    Pictures,
    /// Describe a picture
    ///
    /// Adds the words a picture was found to show to the entry its app published for it, beside
    /// its own.
    #[effect(kind = "outside")]
    Describe {
        #[param("The picture, and the words it was found to show.")]
        description: Description,
    },
}

/// The invocation of `action`, its target the search's and its argument what the action carries.
#[must_use]
pub fn request(action: &SearchAction) -> InvokeRequest {
    InvokeRequest::plain(
        RemoteActionTarget {
            provider: PROVIDER.to_owned(),
            address: ActionAddress::new(RESOURCE, action.id()),
        },
        action.arguments(),
    )
}

/// Whether `target` is one of the search's actions.
#[must_use]
pub fn is_search(target: &RemoteActionTarget) -> bool {
    target.provider == PROVIDER
        && target.address.resource == RESOURCE
        && [
            SearchAction::FIND.id(),
            SearchAction::PICTURES.id(),
            SearchAction::DESCRIBE.id(),
        ]
        .contains(&target.address.action.as_str())
}

/// Whether `target` asks the search what it finds by words ([`SearchAction::Find`]).
#[must_use]
pub fn is_find(target: &RemoteActionTarget) -> bool {
    target.provider == PROVIDER
        && target.address.resource == RESOURCE
        && target.address.action == SearchAction::FIND.id()
}

/// The call `action` with `arguments`, read as its declaration names them. Nothing the
/// declaration does not name is read past: an argument of its caller's own making refuses the
/// call.
///
/// # Errors
/// The action is none of the search's, or an argument is unknown or does not read.
pub fn decoded(
    action: &str,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> Result<SearchAction, String> {
    let declared = SearchAction::declaration(action)
        .ok_or_else(|| format!("the search has no action `{action}`"))?;
    if let Some(unknown) = arguments.keys().find(|name| {
        !declared
            .params
            .iter()
            .any(|param| param.name == name.as_str())
    }) {
        return Err(format!("`{action}` takes no `{unknown}`"));
    }
    SearchAction::decode(action, arguments).map_err(|error| error.to_string())
}

/// What the search answered `receipt`: the hits [`SearchAction::Find`] found.
///
/// # Errors
/// The search refused, or answered with no results.
pub fn found(receipt: InvocationReceipt) -> Result<Vec<Found>, ActionCallError> {
    match receipt.status {
        ReceiptStatus::Completed { output } => found_in(&output),
        ReceiptStatus::Rejected { error } => Err(error),
        _ => Err(ActionCallError::Unavailable {
            reason: "the search did not answer".into(),
        }),
    }
}

/// What the answer `output` holds: the hits [`SearchAction::Find`] found.
///
/// # Errors
/// The answer carries no hits.
pub fn found_in(output: &ActionOutput) -> Result<Vec<Found>, ActionCallError> {
    output
        .value
        .clone()
        .and_then(|value| serde_json::from_value(value).ok())
        .ok_or_else(|| ActionCallError::Unavailable {
            reason: "the search answered no results".into(),
        })
}

/// What a provider's search answers, as the declaration names it, written as the value a receipt
/// carries.
///
/// # Errors
/// The answer cannot be written.
pub fn answered<T: serde::Serialize>(value: T) -> Result<ActionOutput, ActionCallError> {
    let value = serde_json::to_value(value).map_err(|error| ActionCallError::Unavailable {
        reason: format!("the search's answer could not be written: {error}"),
    })?;
    Ok(ActionOutput {
        value: Some(value),
        resources: Vec::new(),
    })
}

/// What `search` answers `request`, a call of one of the actions this declares: the call decoded
/// once, then answered. A provider holding a search answers its calls with this.
pub fn serve<S: Answers + ?Sized>(
    search: &S,
    caller: &Caller,
    request: &InvokeRequest,
) -> Pending<Result<InvocationReceipt, ActionCallError>> {
    let invocation = request.invocation;
    let rejected = |error| {
        Pending::ready(Ok(InvocationReceipt {
            invocation,
            status: ReceiptStatus::Rejected { error },
        }))
    };
    let fields = match &request.argument {
        InvokeArgument::Json(fields) => fields.clone(),
        _ => serde_json::Map::new(),
    };
    let action = match decoded(&request.target.address.action, &fields) {
        Ok(action) => action,
        Err(reason) => {
            return rejected(ActionCallError::InvalidRequest { reason });
        }
    };
    search.answer(caller, action).map(move |answer| {
        Ok(InvocationReceipt {
            invocation,
            status: match answer {
                Ok(output) => ReceiptStatus::Completed { output },
                Err(error) => ReceiptStatus::Rejected { error },
            },
        })
    })
}

/// A provider's search of what its workspace holds, as it offers it to a caller
/// who asks by words. A desktop answers with its one search, the palette's
/// ranking, the same results in the same order; a provider with no desktop (a
/// server's workspace) ranks the records it holds by what they say. Either
/// way the search lives where the records are, so an answer comes back
/// later, as an action's does.
pub trait Answers: Send + Sync {
    /// What `needle` finds, best first, over the workspace `caller` acts in
    /// and what it may read there.
    ///
    /// # Errors
    /// The search could not be run: the provider has none, or it refuses
    /// `caller`.
    fn find(&self, caller: &Caller, needle: &str) -> Pending<Result<Vec<Found>, ActionCallError>>;

    /// [`Self::find`] narrowed to `within`. A search that cannot narrow says so
    /// ([`NOT_NARROWED`]) and never answers with everything instead: what is asked of last week
    /// is not answered with all time.
    ///
    /// # Errors
    /// As [`Self::find`], and where this search cannot be narrowed.
    fn find_within(
        &self,
        caller: &Caller,
        needle: &str,
        within: &Within,
    ) -> Pending<Result<Vec<Found>, ActionCallError>> {
        if within.is_everything() {
            return self.find(caller, needle);
        }
        Pending::ready(Err(ActionCallError::Unavailable {
            reason: NOT_NARROWED.into(),
        }))
    }

    /// The pictures the search holds that `caller` may read ([`SearchAction::Pictures`]).
    ///
    /// # Errors
    /// This search keeps no pictures, or refuses `caller`.
    fn pictures(&self, _caller: &Caller) -> Pending<Result<Vec<Picture>, ActionCallError>> {
        Pending::ready(Err(ActionCallError::Unavailable {
            reason: "this search keeps no pictures".into(),
        }))
    }

    /// Add `description` to its picture's entry ([`SearchAction::Describe`]).
    ///
    /// # Errors
    /// This search keeps no pictures, or `caller` may not read the picture.
    fn describe(
        &self,
        _caller: &Caller,
        _description: Description,
    ) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::Unavailable {
            reason: "this search keeps no pictures".into(),
        }))
    }

    /// What the declared `action` answers, its argument already read: the one place a call
    /// becomes the search's own work.
    fn answer(
        &self,
        caller: &Caller,
        action: SearchAction,
    ) -> Pending<Result<ActionOutput, ActionCallError>> {
        match action {
            SearchAction::Find { needle, within } => self
                .find_within(caller, &needle, &within.unwrap_or_default())
                .map(|found| found.and_then(answered)),
            SearchAction::Pictures => self.pictures(caller).map(|found| found.and_then(answered)),
            SearchAction::Describe { description } => self
                .describe(caller, description)
                .map(|found| found.and_then(answered)),
        }
    }
}

/// The search as a view of another language reads it (`search.search`): its actions, the
/// parameters they take and the records they read and answer.
#[must_use]
pub fn surface() -> tilo_actions::surface::Surface {
    let mut types = tilo_resource::TypeSet::new();
    let verbs = SearchAction::declarations()
        .iter()
        .map(|declaration| tilo_resource::surface::effect_verb(declaration, &mut types))
        .collect();
    tilo_actions::surface::Surface {
        kind: RESOURCE.to_owned(),
        label: "Search".to_owned(),
        version: 1,
        person: false,
        readers: None,
        verbs,
        fields: Vec::new(),
        types: types.into_types(),
        shows: Vec::new(),
        live: None,
        setters: false,
    }
}

#[cfg(test)]
mod tests;
