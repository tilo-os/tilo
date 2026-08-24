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
//! ## Which app
//!
//! The focused one. A session checks out one record at a time — the live
//! document *is* the focused tab's document — so that is the app whose tree
//! can be both read and written honestly. This is the same scoping
//! [`SearchScope::ActiveApp`](crate::shell::chrome_state::SearchScope) and
//! `ui::App`'s own resource collection already use: the resources of the
//! thing in front of you. A caller that wants a different app's surface
//! focuses that tab first, by address, and asks again.
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
//! [`refresh`](AppSurfaces::refresh) walks the widget tree through shared
//! references and calls `resources(&self, doc)`, which takes `&self` and
//! `&AutoCommit`. Reading the surface therefore cannot mutate a widget or
//! the document — not by discipline but because no `&mut` is available to
//! it. What a survey returns is inert data, and the invoke callback is
//! built fresh per row rather than handed out by enumeration.

use std::cell::RefCell;
use std::rc::Rc;

use qos_actions::door::{self, ActionAddress, ActionDescriptor, InputSpec};
use qos_actions::{ActionRegistry as ActionTypeRegistry, LocalTransport, Resource};
use ui::record::{RecordId, RecordStore};

use crate::shell::search::action_api::{ActionError, ActionParam, ParamType};
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
        match &inner.document {
            Some((store, id)) => {
                match store.with(id, |doc| f(doc, &inner.resources, &inner.caller)) {
                    Some(r) => r,
                    // The record went away between publish and invoke. Fall
                    // back rather than silently acting on the wrong
                    // document: the action then finds nothing to act on and
                    // says so, which is the honest answer.
                    None => f(fallback, &inner.resources, &inner.caller),
                }
            }
            None => f(fallback, &inner.resources, &inner.caller),
        }
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
    kinds: Vec<ResourceKind>,
}

impl AppResourcesProvider {
    pub fn new(surfaces: AppSurfaces, types: Rc<ActionTypeRegistry>) -> Self {
        Self {
            surfaces,
            types,
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
        for d in self.surfaces.survey() {
            if !matches(ctx.needle, &d) {
                continue;
            }
            let params = params_for(&d.input);
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
                        .map(|_| ())
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
