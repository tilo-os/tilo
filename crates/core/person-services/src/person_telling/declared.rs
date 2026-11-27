//! The actions of `tilo.telling`, declared once: what an agent of a person's
//! asks of the log of what they were told ([`ToldAction`]).
//!
//! The host that keeps the person's workspace answers them, and every caller
//! reads them from here: the catalogue rows an agent is offered, the parameters
//! a call is checked against, and `telling-surface.json`, which a tilolang view
//! compiles against and a crate without this one binds its calls to. The
//! records they take and answer are beside them ([`super::records`]); labels are
//! this crate's, kept in the catalogue every `tilo.*` action's words are in.

use tilo_actions::ActionAddress;
use tilo_actions::surface::Surface;

use super::records::{About, Cause, Offered, Stretch, TaskRef, Told, ToldList};

/// Whether an agent of a person's is offered the action `action` of the log:
/// telling its person, and marking what it told as not to be missed.
#[must_use]
pub fn offered_to_agent(action: &str) -> bool {
    action == ToldAction::TELL.id() || action == ToldAction::INSIST.id()
}

/// What a person's assistant tells them, and what they read of it: the actions
/// of the log, each thing marked by what kind of thing it is.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(
    operation = "tilo.telling",
    catalogue = "../tilo-workspace-service/locales"
)]
#[allow(
    clippy::large_enum_variant,
    reason = "a call is built once and matched once, so a wider variant costs no copy"
)]
pub enum ToldAction {
    /// Tell the person
    ///
    /// Answers it as it was kept, with its mark. An agent of the person's asks this; it never
    /// says what kind of thing it is. What it tells is news whatever cause it names, and a
    /// button it offers is an action it may invoke itself.
    #[effect(kind = "undoable", returns = Told)]
    Tell {
        #[param("What to tell the person, as plain text, in a sentence or two.")]
        words: String,
        #[param("A few words over it.")]
        title: Option<String>,
        #[param(
            "What caused it: { kind, label, key }, in your own words; the key is your own id for it, and one cause is told once. What you tell is news whatever kind you name."
        )]
        cause: Option<Cause>,
        #[param("Where it is: { workspace, app, record, resource }, what opening it shows.")]
        about: Option<About>,
        #[param("The task it belongs to: { record, issue }.")]
        task: Option<TaskRef>,
        #[param(
            "Its buttons: [{ id, label, main, target: { app, record, resource, action }, arguments }], each a typed action the person's device invokes as the person. Offer only an action you may invoke yourself here: anything else is refused, and a workspace's own administration always."
        )]
        actions: Option<Vec<Offered>>,
        #[param("Whether it must not be missed, when its person said so of this thing.")]
        insists: Option<bool>,
    },
    /// What was told
    ///
    /// What was told after a place in the log, oldest first. The person's alone, and every ask
    /// counts as a device of theirs looking.
    #[effect(kind = "read", returns = ToldList)]
    List {
        #[param("The place read up to; everything the log keeps when omitted.")]
        after: Option<u64>,
        #[param(
            "How long to wait for the log to move when nothing is newer, in milliseconds.",
            name = "waitMs"
        )]
        wait_ms: Option<u64>,
        #[param("The device asking, as it names itself.")]
        device: Option<String>,
    },
    /// Say a thing was seen
    ///
    /// The person saw one thing or acted on it. The person's alone.
    #[effect(kind = "undoable", returns = Told)]
    Acknowledge {
        #[param("The thing, by its id.")]
        told: String,
        #[param("The device of theirs it happened on, as that device names itself.")]
        device: Option<String>,
        #[param("How: `opened`, `dismissed`, `answered`, or the id of the button pressed.")]
        by: Option<String>,
    },
    /// Mark as not to be missed
    ///
    /// It is then raised again until they acknowledge it.
    #[effect(kind = "undoable", returns = Told)]
    Insist {
        #[param("The thing, by its id.")]
        told: String,
        #[param("Whether it must not be missed: raised again, until they acknowledge it.")]
        insists: bool,
    },
    /// What happened while they were away
    ///
    /// What came in, what the assistant did alone and what waits, for the message at the top of
    /// a new stretch. The person's alone.
    #[effect(kind = "read", returns = Stretch)]
    SinceYouLeft,
}

/// The target of the action `action` of `resource` ([`super::records::TOLD`]).
#[must_use]
pub fn target(resource: &str, action: &str) -> tilo_actions::remote::RemoteActionTarget {
    tilo_actions::remote::RemoteActionTarget {
        provider: super::records::PROVIDER.to_owned(),
        address: ActionAddress::new(resource, action),
    }
}

/// What this declaration publishes, as the file beside this crate keeps it: the
/// log, as a view of another language reads it and calls its actions.
#[must_use]
pub fn surfaces() -> Vec<Surface> {
    vec![tilo_resource::surface::row_surface::<Told, ToldAction>()]
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;

    /// The file a tilolang view compiles what a person's log offers against,
    /// and a crate that does not depend on this one binds its calls to, is what
    /// this crate declares. `UPDATE_SNAPSHOTS=1` writes it.
    #[test]
    fn the_telling_surface_file_is_what_this_crate_publishes() {
        let text = tilo_surface_compile::canonical(&serde_json::json!({ "resources": surfaces() }));
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("telling-surface.json");
        if std::env::var_os("UPDATE_SNAPSHOTS").is_some_and(|value| value == "1") {
            std::fs::write(&path, &text).expect("the surface file is written");
            return;
        }
        assert_eq!(
            std::fs::read_to_string(&path).ok().as_deref(),
            Some(text.as_str()),
            "crates/core/person-services/telling-surface.json is not what this crate \
             publishes; regenerate it with UPDATE_SNAPSHOTS=1"
        );
    }

    /// What the action registry knows of the provider without this crate names
    /// the same provider this service answers under.
    #[test]
    fn the_registry_names_the_provider() {
        assert_eq!(
            super::super::records::PROVIDER,
            tilo_actions::remote::TELLING_PROVIDER
        );
    }
}
