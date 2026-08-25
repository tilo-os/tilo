//! The app half of the action surface: what the app in front of you has
//! open, and what can be done to it.
//!
//! Every other provider in this registry answers for something the *shell*
//! owns — its chrome actions, its notes, the filesystem. An app's own
//! resources were the gap: apps publish a
//! [`qos_actions::Resource`] tree through
//! [`Widget::resources`](ui::widget::Widget::resources), the Ctrl+K palette
//! inside the app process reads it, and nothing outside that process could.
//! So an agent or a script could open a terminal but not add a row to the
//! table you were both looking at.
//!
//! This provider closes that by putting the app's tree into the *same*
//! registry the rest of the surface already comes from. There is no second
//! action API: one catalogue, one address shape, one capability gate, and
//! `actions_enumerate` / `actions_invoke` reach an app's operations exactly
//! as they reach the shell's.
//!
//! ## Two halves, one catalogue
//!
//! An app compiled into the compositor is a widget, and its tree is read
//! straight off it. Every *packaged* qOS app is a separate process, and a
//! widget walk finds nothing at all in one — which is why, on a real desktop,
//! this provider published an empty tree every time and the palette's hundred
//! offered operations included none from a running application.
//!
//! The out-of-process half arrives over the socket the app already holds and
//! is held by [`RemoteAppActions`](crate::shell::app_action_wire::RemoteAppActions).
//! Both halves are surveyed into the same [`ActionDescriptor`], carry the same
//! address shape and pass the same capability gate. What differs is only who
//! runs the operation: a widget's action is dispatched here against the
//! document this pass carries, and a packaged app's is carried back to the app,
//! which performs it on its own document. See that module for why it must be.
//!
//! ## Which app
//!
//! For the in-process half, the focused one, read against its **own**
//! document: a hosted tab is
//! redirected onto its record, so the document a frame carries is not the
//! one the app reads, and a survey taken against that one would describe an
//! empty document belonging to nobody. This is the same scoping
//! [`SearchScope::ActiveApp`](crate::shell::chrome_state::SearchScope) and
//! `ui::App`'s own resource collection already use: the resources of the
//! thing in front of you. A caller that wants a different app's surface
//! focuses that tab first, by address, and asks again.
//!
//! The out-of-process half is every connected application at once, not the
//! focused one. There is no widget to walk and so no notion of "in front"
//! here, and the addresses are app-namespaced anyway — which makes answering
//! for all of them the honest reading of "what can be done on this desktop"
//! rather than a widening of scope. It is also the case the whole thing is
//! for: a caller reaching across two applications should not have to focus
//! each in turn to find out what they offer.
//!
//! ## Addresses
//!
//! `"<resource id>#<action id>"` — both minted by the app, joined by a
//! character neither contains. A resource id is already app-namespaced by
//! convention (`word-processor.document`, `files.entry:/var/workspace/plan.md`),
//! which is what lets the capability gate name the app being acted on
//! without the shell keeping a second table of who owns what.
//!
//! Nothing here is positional. The same address keeps naming the same
//! operation across layouts, themes and resolutions, and a caller with no
//! display can work entirely from ids and labels.
//!
//! ## Enumeration cannot act
//!
//! The host's refresh walks the widget tree through shared references and
//! calls `resources(&self, doc)`, which takes `&self` and
//! `&AutoCommit`. Reading the surface therefore cannot mutate a widget or
//! the document — not by discipline but because no `&mut` is available to
//! it. What a survey returns is inert data, and the invoke callback is
//! built fresh per row rather than handed out by enumeration.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use qos_actions::door::{self, ActionAddress, ActionDescriptor, InputSpec};
use qos_actions::{ActionRegistry as ActionTypeRegistry, LocalTransport, Resource};
use ui::record::{RecordId, RecordStore};

use crate::shell::app_action_wire::RemoteAppActions;

use crate::shell::search::action_api::{ActionError, ActionParam, ParamType};
use crate::shell::state::ResourceRef;
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

/// The provider id every app-exported operation is addressed under.
pub const PROVIDER_ID: &str = "app";

/// The one [`ResourceKind`] this provider mints, so the palette gets a
/// single chip for "things this app can do" rather than one per app-defined
/// resource kind (which no fixed chip strip could know in advance). The
/// app's own kind travels in the row's subtitle, where a caller reading
/// descriptors still sees it.
pub fn app_action_kind() -> ResourceKind {
    ResourceKind::Custom("App action".to_string())
}

/// Separator between a resource id and an action id in an address.
///
/// `#` because a resource id may contain `.`, `:` and `/` — a file entry's
/// id is an absolute path — but an action id is a plain identifier and
/// neither half ever contains `#`. Splitting on the last occurrence is
/// therefore unambiguous, and unlike the unit separator the whole address
/// survives a trip through a JSON reply legibly enough for a person
/// debugging one to read it.
pub const ADDRESS_SEP: char = '#';

/// Encode an address into the provider-local id a [`SearchResult`] carries.
pub fn encode_address(address: &ActionAddress) -> String {
    format!("{}{}{}", address.resource, ADDRESS_SEP, address.action)
}

/// Recover an address from a provider-local id.
pub fn decode_address(id: &str) -> Option<ActionAddress> {
    let (resource, action) = id.rsplit_once(ADDRESS_SEP)?;
    if resource.is_empty() || action.is_empty() {
        return None;
    }
    Some(ActionAddress::new(resource, action))
}

/// The app a resource id belongs to.
///
/// Resource ids are app-namespaced by convention across every app in the
/// tree (`tube.video:…`, `ebook.book`, `word-processor.document`,
/// `files.entry:/…`), so the segment before the first `.` names the app
/// whose operation is being invoked. The capability gate asks about *that*
/// app rather than lumping every app's actions under the shell's own
/// namespace, which is what lets a room grant someone edit rights in one
/// app and not another.
pub fn app_of(resource_id: &str) -> &str {
    resource_id
        .split_once('.')
        .map(|(head, _)| head)
        .unwrap_or(resource_id)
}

/// The focused app's published surface, refreshed from the live widget
/// tree and read by [`AppResourcesProvider`].
///
/// Shared rather than owned because the provider lives inside the search
/// registry and the widget tree does not: the host refreshes one side, the
/// registry reads the other, and neither needs a borrow of the other.
/// [`crate::shell::room_records::RoomRecords`] is shared with the records
/// provider the same way.
#[derive(Default)]
pub struct AppSurface {
    /// The tree the focused app published on the last refresh.
    pub resources: Vec<Resource>,
    /// Who is asking. Their bytes become the Automerge actor an invoked
    /// action commits under, so the edit is attributed in history exactly
    /// like one they typed.
    pub caller: String,
    /// Where the app's document actually lives.
    ///
    /// A hosted tab is redirected onto its own record, so the document
    /// the shell passes down a frame is *not* the one the app reads —
    /// enumerating or acting against that one would describe and edit an
    /// empty document belonging to nobody. `None` for an app with no
    /// record, whose document is the one the pass carries.
    pub document: Option<(RecordStore, RecordId)>,
}

/// Shared handle to the focused app's surface.
#[derive(Clone, Default)]
pub struct AppSurfaces(Rc<RefCell<AppSurface>>);

impl AppSurfaces {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the published surface. Called by the host once per
    /// enumeration, from the live widget tree.
    pub fn publish(
        &self,
        caller: impl Into<String>,
        resources: Vec<Resource>,
        document: Option<(RecordStore, RecordId)>,
    ) {
        let mut inner = self.0.borrow_mut();
        inner.caller = caller.into();
        inner.resources = resources;
        inner.document = document;
    }

    /// Describe every operation the focused app currently exports.
    pub fn survey(&self) -> Vec<ActionDescriptor> {
        door::survey(&self.0.borrow().resources)
    }

    /// Perform `f` against the app's own document.
    ///
    /// `fallback` is the document the pass carried, used only when the app
    /// has no record of its own. Every borrow the store hands out is
    /// closed before this returns, so an action cannot keep the loan.
    fn on_app_document<R>(
        &self,
        fallback: &mut automerge::AutoCommit,
        f: impl FnOnce(&mut automerge::AutoCommit, &[Resource], &str) -> R,
    ) -> R {
        let inner = self.0.borrow();
        // Held in an `Option` so the fallback can still run it: the store
        // only calls the closure when the record is there, and a closure
        // that was never called has not been consumed.
        let mut f = Some(f);
        if let Some((store, id)) = &inner.document {
            let mut out = None;
            store.with(id, |doc| {
                let run = f.take().expect("the closure runs at most once");
                out = Some(run(doc, &inner.resources, &inner.caller));
            });
            if let Some(r) = out {
                return r;
            }
        }
        // No record, or it went away between publish and invoke. Falling
        // back is better than acting on the wrong document: the action
        // then finds nothing to act on and says so.
        let run = f.take().expect("the closure runs at most once");
        run(fallback, &inner.resources, &inner.caller)
    }
}

/// Serves the focused app's own resources into the shell's search +
/// action registry.
pub struct AppResourcesProvider {
    surfaces: AppSurfaces,
    /// Decoders for every action type the in-process apps registered.
    ///
    /// The door decodes through this before running anything, so an action
    /// whose type the app never registered is refused rather than applied
    /// on trust — the same decoder a replaying peer would use is the one
    /// that reconstructs it here.
    types: Rc<ActionTypeRegistry>,
    /// What the applications that are not in this process currently offer.
    ///
    /// Empty in a shell with no broker behind it, which is the honest answer
    /// there: no app has connected, so none has published anything.
    remote: Arc<RemoteAppActions>,
    kinds: Vec<ResourceKind>,
}

impl AppResourcesProvider {
    pub fn new(surfaces: AppSurfaces, types: Rc<ActionTypeRegistry>) -> Self {
        Self::with_remote(surfaces, types, Arc::new(RemoteAppActions::new()))
    }

    /// The same provider, serving the connected applications' surfaces too.
    pub fn with_remote(
        surfaces: AppSurfaces,
        types: Rc<ActionTypeRegistry>,
        remote: Arc<RemoteAppActions>,
    ) -> Self {
        Self {
            surfaces,
            types,
            remote,
            kinds: vec![app_action_kind()],
        }
    }
}

/// What one descriptor's invocation must carry.
///
/// An action that collects text advertises it, so a caller that cannot see
/// the screen is never left guessing an argument from a label. The
/// parameter is bound by [`ActionArgs`](crate::shell::search::action_api::ActionArgs)
/// before the action runs, so a call that does not fit is refused with the
/// document untouched.
fn params_for(input: &InputSpec) -> Vec<ActionParam> {
    match input {
        InputSpec::None => Vec::new(),
        InputSpec::Text { .. } => vec![ActionParam::required(
            "text",
            ParamType::Text,
            "the text this action collects before it runs",
        )],
    }
}

fn matches(needle: &str, d: &ActionDescriptor) -> bool {
    if needle.is_empty() {
        return true;
    }
    let n = needle.to_lowercase();
    d.action_label.to_lowercase().contains(&n)
        || d.resource_label.to_lowercase().contains(&n)
        || d.address.resource.to_lowercase().contains(&n)
        || d.resource_kind.to_lowercase().contains(&n)
}

/// Build one row for an operation a connected application published.
///
/// The gate runs inside the row's own callback rather than when the row is
/// built, for the reason invocation re-resolves at all: a catalogue is built
/// once and acted on later, and a capability revoked in between must take
/// effect on the act, not on the last survey.
fn remote_row(
    remote: &Arc<RemoteAppActions>,
    entry: crate::shell::app_action_wire::RemoteAction,
    kind: ResourceKind,
) -> SearchResult {
    let d = entry.descriptor;
    let params = params_for(&d.input);
    let collects_content = d.input.wants_content();
    let address = d.address.clone();
    let app = app_of(&address.resource).to_string();
    let remote = Arc::clone(remote);
    SearchResult {
        kind,
        provider_id: PROVIDER_ID.to_string(),
        id: encode_address(&d.address),
        title: d.action_label.clone(),
        subtitle: Some(format!("{} — {}", d.resource_label, d.resource_kind)),
        score: 0,
        fetch_source: None,
        params,
        on_open: Rc::new(move |_doc, _root, chrome, args| {
            let argument = args.text("text");
            // The capability check. Nothing below this line can be reached
            // without passing it: `invoke` performs no check of its own and
            // this is the only caller of it in the tree.
            let caller = {
                let st = chrome.borrow();
                let tree = st.tree();
                let Some(workspace) = tree.active_workspace() else {
                    // A session that cannot resolve a workspace cannot resolve
                    // a capability either, and a gate that opens when it
                    // cannot find its policy is not a gate.
                    return Err(ActionError::Refused(
                        "no active workspace to resolve capabilities against".into(),
                    ));
                };
                let actor = workspace.actor_for_person(&st.local_user_id);
                let target = ResourceRef::Action {
                    app: app.clone(),
                    action: address.action.clone(),
                };
                let outcome = remote.check(workspace, &actor, &app, &address.action, &target);
                if !outcome.is_permitted() {
                    // A caller who may not act never gets this far: the
                    // catalogue's own visibility gate leaves the row out, so
                    // the address resolves to nothing and the refusal is
                    // `unknown_action` before anything is built. Reaching here
                    // means the answer changed between that build and this
                    // act -- a capability revoked, or the app republished --
                    // and saying so plainly is right, because this caller was
                    // shown the address a moment ago and learns nothing from
                    // being told it has since lost it.
                    return Err(ActionError::Refused(format!(
                        "({}, {}) is no longer permitted to {}",
                        PROVIDER_ID,
                        encode_address(&address),
                        st.local_user_id
                    )));
                }
                st.local_user_id.clone()
            };
            // Sent, not waited for. The thread running this is the one that
            // serves the application's own connection, so waiting here for the
            // application to answer would be waiting for something this thread
            // is preventing. The answer is picked up by
            // `RemoteAppActions::settle` from the loop that paints, and a
            // caller waiting on the control socket has its reply held until
            // then -- see `shell::app_action_wire`.
            remote
                .dispatch(&address, argument, &caller)
                .map(|_id| {
                    // Noted on the ask rather than on the answer. The
                    // crossing is what a person is shown, and the operation
                    // that carries it is one this session already decided may
                    // happen; withholding the animation until an out-of-process
                    // reply lands would show it a frame or two after the
                    // picture it is about.
                    if collects_content {
                        if let Some(hash) = argument {
                            crate::shell::action_flight::note_flight(
                                &caller,
                                hash.trim(),
                                app_of(&address.resource),
                            );
                        }
                    }
                })
                .map_err(|e| match e.code() {
                    "invalid_arguments" => ActionError::Arguments(
                        crate::shell::search::action_api::ArgError::Missing {
                            name: e.to_string(),
                        },
                    ),
                    _ => ActionError::Refused(e.to_string()),
                })
        }),
    }
}

impl SearchProvider for AppResourcesProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    fn name(&self) -> &str {
        "App"
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &self.kinds
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        let kind = app_action_kind();
        if !ctx.wanted.is_empty() && !ctx.wanted.contains(&kind) {
            return Vec::new();
        }
        let mut out = Vec::new();
        for entry in self.remote.survey() {
            if !matches(ctx.needle, &entry.descriptor) {
                continue;
            }
            out.push(remote_row(&self.remote, entry, kind.clone()));
        }
        for d in self.surfaces.survey() {
            if !matches(ctx.needle, &d) {
                continue;
            }
            let params = params_for(&d.input);
            // Whether this operation's one argument is the address of
            // content rather than words somebody says. The app declared it,
            // and it is the only thing that distinguishes an operation that
            // can have carried an object out of another application from one
            // that merely takes a string.
            let collects_content = d.input.wants_content();
            let address = d.address.clone();
            let surfaces = self.surfaces.clone();
            let types = Rc::clone(&self.types);
            out.push(SearchResult {
                kind: kind.clone(),
                provider_id: PROVIDER_ID.to_string(),
                id: encode_address(&d.address),
                title: d.action_label.clone(),
                // The resource this acts on, and its app-defined kind. A
                // caller reading descriptors needs both: two sections
                // export the same action under different labels, and the
                // kind is what says which app's vocabulary applies.
                subtitle: Some(format!("{} — {}", d.resource_label, d.resource_kind)),
                score: 0,
                fetch_source: None,
                params,
                on_open: Rc::new(move |doc, _root, _chrome, args| {
                    let argument = args.text("text");
                    surfaces.on_app_document(doc, |doc, resources, caller| {
                        let mut transport = LocalTransport::new();
                        door::invoke(
                            doc,
                            resources,
                            &address,
                            argument,
                            caller.as_bytes(),
                            &types,
                            &mut transport,
                        )
                        .map(|_| {
                            // The operation succeeded, so if it carried
                            // content here from somewhere else there is now
                            // something worth showing. Noted *after* the
                            // dispatch and never before: an action that was
                            // refused must leave no trace, and a record made
                            // in advance would animate a crossing that did
                            // not happen.
                            //
                            // A push onto a queue and nothing more — no
                            // layout, no window lookup. The compositor
                            // resolves the geometry on a frame it was going
                            // to render anyway, so this cannot slow the
                            // operation down.
                            if collects_content {
                                if let Some(hash) = argument {
                                    crate::shell::action_flight::note_flight(
                                        caller,
                                        hash.trim(),
                                        app_of(&address.resource),
                                    );
                                }
                            }
                        })
                        // The door's own refusal codes line up with the
                        // action API's, so a caller sees the same
                        // vocabulary whether it addressed a shell action
                        // or an app's.
                        .map_err(|e| match e.code() {
                            "invalid_arguments" => ActionError::Arguments(
                                crate::shell::search::action_api::ArgError::Missing {
                                    name: e.to_string(),
                                },
                            ),
                            _ => ActionError::Refused(e.to_string()),
                        })
                    })
                }),
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qos_actions::{Action, ActionError as ActError, ActionSpec, Doc, ExecutionLocus};
    use serde::{Deserialize, Serialize};

    const TEST_TYPE: &str = "test.app.action";

    #[derive(Debug, Clone, Serialize, Deserialize)]
    enum Probe {
        Put { key: String, value: String },
    }

    impl Action for Probe {
        fn apply(&self, doc: &mut Doc) -> Result<(), ActError> {
            use automerge::transaction::Transactable;
            let Probe::Put { key, value } = self;
            doc.put(&automerge::ROOT, key, value.as_str())?;
            Ok(())
        }
        fn locus(&self) -> ExecutionLocus {
            ExecutionLocus::ClientLocal
        }
    }

    fn types() -> Rc<ActionTypeRegistry> {
        let mut r = ActionTypeRegistry::new();
        r.register::<Probe>(TEST_TYPE);
        Rc::new(r)
    }

    fn tree() -> Vec<Resource> {
        let put = Probe::Put {
            key: "hit".into(),
            value: "yes".into(),
        };
        vec![
            Resource::new("probe.document", "probe.document", "Document").with_action(
                ActionSpec::immediate("mark", "Mark it", TEST_TYPE, &put).unwrap(),
            ),
        ]
    }

    /// An address survives the round trip through the id a search result
    /// carries — including a resource id full of the punctuation a file
    /// path brings with it, which is the case a naive `split('.')` or
    /// `split(':')` would lose.
    #[test]
    fn a_path_shaped_address_round_trips_through_a_result_id() {
        let addr = ActionAddress::new("files.entry:/var/workspace/a.b/plan.md", "rename");
        let id = encode_address(&addr);
        assert_eq!(decode_address(&id), Some(addr));
    }

    /// The app the capability model is asked about comes from the resource
    /// id's own namespace, for both shapes of id in the tree.
    #[test]
    fn the_app_is_read_off_the_resource_namespace() {
        assert_eq!(app_of("word-processor.document"), "word-processor");
        assert_eq!(app_of("files.entry:/var/workspace/plan.md"), "files");
    }

    /// Querying the provider builds rows and touches nothing: the surface
    /// is the same afterwards, and the document is untouched.
    #[test]
    fn querying_the_provider_leaves_the_document_alone() {
        let surfaces = AppSurfaces::new();
        surfaces.publish("reviewer", tree(), None);
        let provider = AppResourcesProvider::new(surfaces.clone(), types());
        let mut doc = automerge::AutoCommit::new();
        let before = doc.get_heads();
        let chrome = {
            use crate::shell::state::{WorkspaceTree, WorkspaceTreeHandle};
            use parking_lot::RwLock;
            use std::sync::Arc;
            let handle: WorkspaceTreeHandle = Arc::new(RwLock::new(WorkspaceTree::default()));
            crate::shell::chrome_state::ChromeState::new_with_handle(handle)
        };
        let rows = provider.query(SearchContext {
            needle: "",
            wanted: &[app_action_kind()],
            chrome: &chrome.borrow(),
            doc: &doc,
            doc_root: &automerge::ROOT,
        });
        assert_eq!(rows.len(), 1, "the tree's one action became one row");
        assert_eq!(rows[0].id, "probe.document#mark");
        assert_eq!(
            doc.get_heads(),
            before,
            "building rows must not write to the document"
        );
    }
}
