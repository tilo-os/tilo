//! A framework app's resource type, by the kind its records name.

use std::sync::Mutex;

use tilo_resource::{Doc, Kinds, Resource, ResourceKind};

use super::app::{ResourceApp, SeedCx, apply_seed};
use tilo_lock::MutexRecover as _;

/// `A`'s resource type as a kind a host can look up: hosted by `A`, and an
/// open resource named as `A` names it ([`ResourceApp::label`]). Every
/// framework app gets its kind from here, so a host working with whatever a
/// room holds reads, publishes and replays it without any of the app's own
/// code.
#[must_use]
pub const fn resource_kind<A: ResourceApp>() -> ResourceKind {
    ResourceKind::of::<A::Resource>()
        .hosted_by(A::ID, A::TITLE)
        .named(name::<A>)
}

fn name<A: ResourceApp>(state: &dyn std::any::Any) -> Option<String> {
    state
        .downcast_ref::<<A::Resource as Resource>::State>()
        .map(A::label)
}

/// Applies an app's seed to a new record, created by `me` on `device`.
type Seed = fn(&mut Doc, &str, &str, Option<&str>) -> Result<(), String>;

/// Every framework app's kind whose definition this process created, and
/// its seed.
static HOSTED: Mutex<(Kinds, Vec<(&'static str, Seed)>)> = Mutex::new((Kinds::new(), Vec::new()));

/// Remember `A`'s kind and seed; its definition is being created.
pub(crate) fn register<A: ResourceApp>() {
    // The parts of the chains it keeps beside its record are its kinds too:
    // a host reads a part (a chat's messages) as the app's window does.
    let mut declared = super::Records::declaring();
    A::records(&mut declared);
    // A runtime that runs a package hosts the kinds under the package's app.
    let app = super::app::hooks::<A>();
    let mut hosted = HOSTED.lock_recover();
    hosted.0.insert(
        ResourceKind::of::<A::Resource>()
            .hosted_by(app.id(), app.title())
            .named(name::<A>),
    );
    for chain in declared.into_chains() {
        hosted.0.insert(chain.hosted_by(app.id(), app.title()));
    }
    let kind = <A::Resource as Resource>::KIND;
    hosted.1.retain(|(known, _)| *known != kind);
    hosted.1.push((kind, seed::<A>));
}

fn seed<A: ResourceApp>(
    doc: &mut Doc,
    me: &str,
    device: &str,
    room: Option<&str>,
) -> Result<(), String> {
    let seed = A::seed(&SeedCx {
        me,
        device,
        host: None,
        room,
    });
    apply_seed::<A>(doc, me, &seed)
}

/// Seed a record of `kind` its genesis just created, as `me` creating it on
/// `device` without a window: the app's [`ResourceApp::seed`] with no host,
/// applied as in a window. A kind no app registered has nothing to seed.
///
/// # Errors
/// Returns why a seed action did not apply.
pub fn seed_record(kind: &str, doc: &mut Doc, me: &str, device: &str) -> Result<(), String> {
    seed_scoped(kind, doc, me, device, None)
}

/// Seed a new room record in its host-selected creation scope, without lending
/// ambient device services. The route supplies context, not permission.
///
/// # Errors
/// Returns why a seed action did not apply.
pub fn seed_record_in_room(
    kind: &str,
    doc: &mut Doc,
    me: &str,
    device: &str,
    room: &str,
) -> Result<(), String> {
    seed_scoped(kind, doc, me, device, Some(room))
}

fn seed_scoped(
    kind: &str,
    doc: &mut Doc,
    me: &str,
    device: &str,
    room: Option<&str>,
) -> Result<(), String> {
    let seed = HOSTED
        .lock_recover()
        .1
        .iter()
        .find(|(known, _)| *known == kind)
        .map(|(_, seed)| *seed);
    seed.map_or(Ok(()), |seed| seed(doc, me, device, room))
}

/// The resource type of every framework app this process has the definition
/// of ([`definition`](super::definition)): what a host that holds records
/// of those apps without mounting them — an agent, a room service — reads,
/// publishes and edits them by. Look one up by the kind a record names
/// ([`Kinds::get`]) or by its app ([`Kinds::hosted_by`]).
#[must_use]
pub fn hosted_kinds() -> Kinds {
    HOSTED.lock_recover().0.clone()
}
