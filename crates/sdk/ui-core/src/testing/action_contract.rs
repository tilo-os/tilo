//! The contract every application action must honour, as reusable assertions.
//!
//! An app's actions are its public surface: the palette, automation, agents
//! and collaborators all reach it through them. Three guarantees hold for any
//! document action, whichever app it belongs to:
//!
//! * **Replay.** Applied on one replica and replayed from its envelope on
//!   another at the same base, it produces byte-identical changes — the same
//!   change hashes, not merely equal values.
//! * **Undo.** Dispatched through the host path, it records exactly one undo
//!   entry; undo returns the whole document to what it was before, and redo
//!   puts the action back.
//! * **One source of truth.** The [`ActionSpec`] the palette fires encodes the
//!   same action as the builder the on-screen affordance calls.
//!
//! [`assert_every_immediate_action_honours_contract`] sweeps every immediate
//! document action an app publishes for a seed document, so an action added
//! later is covered without anyone writing a test for it. The
//! [`action_contract_tests!`](crate::action_contract_tests) macro generates the
//! test functions from a seed, a registry, the app's resource provider and the
//! builders worth naming individually.

#![allow(
    clippy::panic,
    clippy::expect_used,
    reason = "an assertion library reports a broken contract by panicking the test that called it"
)]

use automerge::{ActorId, AutoCommit, ChangeHash};
use tilo_actions::{
    ActionInput, ActionRegistry, ActionSpec, ArgValue, EncodedAction, OperationKind,
    PROMPTED_PARAM, ParamType, Resource,
};

use crate::action_palette::ActionPalette;
use crate::state::State;

/// Actor ids the two replicas write their own (non-action) changes under.
/// Distinct from each other and from any action's pinned actor, so a replay
/// that fell back to the replica's actor could not accidentally agree.
const PEER_A: &[u8] = b"action-contract-peer-a";
const PEER_B: &[u8] = b"action-contract-peer-b";

fn describe(encoded: &EncodedAction) -> String {
    format!(
        "{} {}",
        encoded.type_id,
        String::from_utf8_lossy(&encoded.payload)
    )
}

fn peer(bytes: &[u8], actor: &[u8]) -> AutoCommit {
    let mut doc = AutoCommit::load(bytes).expect("a saved seed loads");
    doc.set_actor(ActorId::from(actor));
    doc
}

fn raw_changes(doc: &mut AutoCommit, since: &[ChangeHash]) -> Vec<Vec<u8>> {
    doc.get_changes(since)
        .iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect()
}

/// The whole document as a value tree, for comparing content across replicas
/// and across an undo.
pub fn hydrated(doc: &mut AutoCommit) -> automerge::hydrate::Value {
    use automerge::ReadDoc as _;
    use automerge::transaction::Transactable as _;
    // History's own bookkeeping (what is undone, named versions) is not
    // content.
    let mut content = doc.fork();
    let bookkeeping: Vec<String> = content
        .keys(automerge::ROOT)
        .filter(|key| key.starts_with(crate::history_source::steps::BOOKKEEPING_PREFIX))
        .collect();
    for key in bookkeeping {
        let _ = content.delete(automerge::ROOT, key);
    }
    content.commit();
    content.document().hydrate(None)
}

/// What applying one action to a seed did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applied {
    /// The action wrote at least one op.
    Changed,
    /// The action wrote nothing: its effect already held on the seed.
    Unchanged,
}

/// Apply `encoded` on one replica of `seed` and replay its envelope on a
/// second, asserting both land on byte-identical changes and content.
///
/// Returns whether the action changed the document, so a sweep can tell a
/// genuine no-op from a mutation. Use [`assert_replays_byte_identical`] when
/// the action is expected to mutate.
///
/// # Panics
///
/// If `registry` cannot decode `encoded`, if either apply fails, or if the
/// replicas diverge in heads, change bytes or content.
pub fn replay_on_two_peers(
    seed: &AutoCommit,
    registry: &ActionRegistry,
    encoded: &EncodedAction,
) -> Applied {
    let what = describe(encoded);
    let bytes = seed.clone().save();
    let mut origin = peer(&bytes, PEER_A);
    let mut replica = peer(&bytes, PEER_B);
    let base = origin.get_heads();
    assert_eq!(
        base,
        replica.get_heads(),
        "both replicas start from the seed"
    );

    let action = registry
        .decode(encoded)
        .unwrap_or_else(|e| panic!("the registry decodes {what}: {e}"));
    let envelope = apply_signed(&mut origin, action.as_ref(), encoded)
        .unwrap_or_else(|e| panic!("{what} applies on the origin: {e}"));
    tilo_actions::replay(&mut replica, &envelope, registry)
        .unwrap_or_else(|e| panic!("{what} replays on the peer: {e}"));

    assert_eq!(
        origin.get_heads(),
        replica.get_heads(),
        "{what}: replay reproduced the same change hashes"
    );
    let origin_changes = raw_changes(&mut origin, &base);
    assert_eq!(
        origin_changes,
        raw_changes(&mut replica, &base),
        "{what}: replay reproduced byte-identical changes"
    );
    assert!(
        origin_changes.len() <= 1,
        "{what}: one action commits at most one change, got {}",
        origin_changes.len()
    );
    assert_eq!(
        hydrated(&mut origin),
        hydrated(&mut replica),
        "{what}: both replicas hold the same document"
    );
    if origin_changes.is_empty() {
        Applied::Unchanged
    } else {
        Applied::Changed
    }
}

/// The author every action the contract applies is invoked by, as a
/// window's dispatcher signs what its person does.
pub const CONTRACT_AUTHOR: &str = "contract-author";

/// Apply `encoded` as [`CONTRACT_AUTHOR`] invoked it: an action that signs
/// what it writes with its author reads one, as it does in an app.
fn apply_signed(
    doc: &mut AutoCommit,
    action: &dyn tilo_actions::Action,
    encoded: &EncodedAction,
) -> Result<tilo_actions::ActionEnvelope, tilo_actions::ActionError> {
    let context = tilo_actions::ActionContext::default()
        .with_invoker(tilo_actions::ActionInvoker::actor(CONTRACT_AUTHOR));
    tilo_actions::apply_local_with_context(doc, action, encoded.clone(), None, context)
}

/// Assert `encoded`, applied on one replica of `seed` and replayed on another
/// from its envelope, mutates the document and produces byte-identical
/// changes on both.
///
/// # Panics
///
/// See [`replay_on_two_peers`]; also when the action changes nothing on the
/// seed, since a no-op replays identically by construction and proves
/// nothing — seed a document the action actually changes.
pub fn assert_replays_byte_identical(
    seed: &AutoCommit,
    registry: &ActionRegistry,
    encoded: &EncodedAction,
) {
    assert_eq!(
        replay_on_two_peers(seed, registry, encoded),
        Applied::Changed,
        "{} changed nothing on the seed, so its replay proves nothing",
        describe(encoded)
    );
}

/// Dispatch `encoded` against a host [`State`] holding `seed`, record it the
/// way the host records a dispatched action, and check the undo covenant:
/// exactly one undo entry when it changed the document (none when it did
/// not), undo restores the whole pre-action document, redo restores the
/// post-action one.
///
/// # Panics
///
/// If `registry` cannot decode `encoded`, the action fails, or any of the
/// above does not hold.
pub fn undo_round_trip(
    seed: &AutoCommit,
    registry: &ActionRegistry,
    encoded: &EncodedAction,
) -> Applied {
    let what = describe(encoded);
    let mut state = State::from_doc(seed.clone());
    let depth = state.undo_depth();
    let before_doc = hydrated(&mut state.doc);
    let before = state.doc.get_heads();

    let action = registry
        .decode(encoded)
        .unwrap_or_else(|e| panic!("the registry decodes {what}: {e}"));
    apply_signed(&mut state.doc, action.as_ref(), encoded)
        .unwrap_or_else(|e| panic!("{what} applies: {e}"));
    state.record_committed_action(before, None, None);

    let after_doc = hydrated(&mut state.doc);
    if after_doc == before_doc && state.undo_depth() == depth {
        return Applied::Unchanged;
    }
    // Housekeeping and content from outside are nobody's undo step.
    if !action.change_kind().is_step() {
        assert_eq!(
            state.undo_depth(),
            depth,
            "{what}: housekeeping records no undo step"
        );
        return Applied::Changed;
    }
    assert_eq!(
        state.undo_depth().checked_sub(depth),
        Some(1),
        "{what}: the action records exactly one undo boundary"
    );
    assert_ne!(
        after_doc, before_doc,
        "{what}: an action that records an undo entry changes the document's content"
    );

    state
        .undo(None, None)
        .unwrap_or_else(|| panic!("{what}: undo pops the action's entry"));
    assert_eq!(state.undo_depth(), depth, "{what}: undo pops one entry");
    assert_eq!(
        hydrated(&mut state.doc),
        before_doc,
        "{what}: undo restores the whole document to its pre-action content"
    );

    state
        .redo(None, None)
        .unwrap_or_else(|| panic!("{what}: redo re-applies the undone action"));
    assert_eq!(
        hydrated(&mut state.doc),
        after_doc,
        "{what}: redo restores the action's result"
    );
    Applied::Changed
}

/// Assert `encoded` records exactly one undo boundary on a host [`State`]
/// holding `seed`, undo restores the whole pre-action document and redo the
/// post-action one.
///
/// # Panics
///
/// See [`undo_round_trip`]; also when the action changes nothing on the seed.
pub fn assert_one_undo_boundary_reverts(
    seed: &AutoCommit,
    registry: &ActionRegistry,
    encoded: &EncodedAction,
) {
    assert_eq!(
        undo_round_trip(seed, registry, encoded),
        Applied::Changed,
        "{} changed nothing on the seed, so there is nothing to undo",
        describe(encoded)
    );
}

/// The first resource, depth-first, whose id is `resource`, or failing that
/// whose kind is `resource`.
///
/// # Panics
///
/// When no published resource matches.
pub fn find_resource<'a>(resources: &'a [Resource], resource: &str) -> &'a Resource {
    fn walk<'a>(
        resources: &'a [Resource],
        hit: &dyn Fn(&Resource) -> bool,
    ) -> Option<&'a Resource> {
        resources.iter().find_map(|r| {
            if hit(r) {
                Some(r)
            } else {
                walk(&r.children, hit)
            }
        })
    }
    walk(resources, &|r| r.id.as_str() == resource)
        .or_else(|| walk(resources, &|r| r.kind == resource))
        .unwrap_or_else(|| panic!("no published resource has id or kind `{resource}`"))
}

/// The action `action` on the resource [`find_resource`] picks for `resource`.
///
/// # Panics
///
/// When no resource matches or it publishes no such action.
pub fn find_spec<'a>(
    resources: &'a [Resource],
    resource: &str,
    action: &str,
) -> (&'a Resource, &'a ActionSpec) {
    let owner = find_resource(resources, resource);
    let spec = owner
        .actions
        .iter()
        .find(|a| a.id == action)
        .unwrap_or_else(|| {
            let ids: Vec<&str> = owner.actions.iter().map(|a| a.id.as_str()).collect();
            panic!(
                "`{}` publishes no `{action}` action; it has {ids:?}",
                owner.id.as_str()
            )
        });
    (owner, spec)
}

/// The argument the palette binds when a prompted `spec` collects `text`:
/// the first required parameter (else the first), parsed into its declared
/// type. `None` when the text does not parse, as the palette binds nothing.
fn prompted_argument(spec: &ActionSpec, text: &str) -> Option<(String, ArgValue)> {
    let Some(primary) = spec
        .params()
        .iter()
        .find(|p| p.required)
        .or_else(|| spec.params().first())
    else {
        return Some((PROMPTED_PARAM.to_string(), ArgValue::Text(text.to_string())));
    };
    let value = match primary.ty {
        ParamType::Text => Some(ArgValue::Text(text.to_string())),
        ParamType::Integer => text.trim().parse().ok().map(ArgValue::Integer),
        ParamType::Number => text.trim().parse().ok().map(ArgValue::Number),
        ParamType::Boolean => match text.trim() {
            "true" | "yes" | "on" => Some(ArgValue::Boolean(true)),
            "false" | "no" | "off" => Some(ArgValue::Boolean(false)),
            _ => None,
        },
        ParamType::Object => serde_json::from_str(text).ok().map(ArgValue::Object),
        ParamType::Array => serde_json::from_str(text).ok().map(ArgValue::Array),
        ParamType::Resource => Some(ArgValue::Resource(text.to_string())),
        ParamType::DateTime => Some(ArgValue::DateTime(text.to_string())),
        ParamType::Secret => Some(ArgValue::Secret(text.to_string())),
    };
    value.map(|value| (primary.name.to_string(), value))
}

/// What choosing `spec` in the palette dispatches: the pre-encoded action of
/// an immediate spec, or a prompted spec's build of `arg`. The external
/// invocation path (binding arguments against the spec's parameters) must
/// agree, and is checked here too.
///
/// # Panics
///
/// When `arg` is given for an immediate spec, missing for a prompted one, or
/// the external path builds a different action.
pub fn spec_encoding(spec: &ActionSpec, arg: Option<&str>) -> EncodedAction {
    let (palette, supplied) = match (&spec.input, arg) {
        (ActionInput::Immediate(encoded), None) => (encoded.as_ref().clone(), Vec::new()),
        (ActionInput::Deferred { build, .. }, None) => (build(), Vec::new()),
        (ActionInput::Prompted { build, .. }, Some(arg)) => (
            build(arg),
            prompted_argument(spec, arg).into_iter().collect(),
        ),
        (ActionInput::Immediate(_) | ActionInput::Deferred { .. }, Some(_)) => {
            panic!("`{}` is immediate; it takes no argument", spec.id)
        }
        (ActionInput::Prompted { .. }, None) => {
            panic!("`{}` is prompted; pass the argument to build it", spec.id)
        }
    };
    if spec.params().iter().filter(|p| p.required).count() <= supplied.len() {
        let external = spec
            .build_args(supplied)
            .unwrap_or_else(|e| panic!("`{}` binds its own arguments: {e}", spec.id));
        assert_eq!(
            external, palette,
            "`{}`: the external invocation path builds the palette's action",
            spec.id
        );
    }
    palette
}

/// Assert choosing `spec` (with `arg`, for a prompted spec) dispatches
/// exactly `expected` — type and payload — so the palette and the on-screen
/// affordance's builder are one source of truth.
///
/// # Panics
///
/// See [`spec_encoding`]; also when the encodings differ.
pub fn assert_spec_encodes_as(spec: &ActionSpec, arg: Option<&str>, expected: &EncodedAction) {
    let actual = spec_encoding(spec, arg);
    assert_eq!(
        describe(&actual),
        describe(expected),
        "`{}`: the palette spec and the builder encode the same action",
        spec.id
    );
}

/// Sweep the surface published in `resources` (derived from `seed`): it
/// publishes something, passes the protocol validator (unique addresses,
/// registered document types, coherent contracts), and every immediate,
/// available document action replays byte-identically on a second replica
/// and, when it changes the document, records exactly one undo boundary that
/// undo reverts completely.
///
/// Returns how many immediate document actions were swept. An app whose
/// registry decodes anything must publish at least one document action for
/// its seed, so a seed that exercises none of them is refused rather than
/// passing vacuously; an app that publishes only effect and view operations
/// passes an empty registry. Prompted document actions need an argument, so
/// their replay and undo belong in named `replay`/`undo` entries.
///
/// # Panics
///
/// When the surface is empty or invalid, or any action breaks the contract.
pub fn assert_every_immediate_action_honours_contract(
    seed: &AutoCommit,
    registry: &ActionRegistry,
    resources: &[Resource],
) -> usize {
    assert!(
        !tilo_actions::flatten(resources).is_empty(),
        "the seed publishes no action at all"
    );
    let violations = tilo_actions::validate_surface_contracts(resources, registry);
    assert!(
        violations.is_empty(),
        "the published surface breaks the protocol: {violations:#?}"
    );
    let mut swept: usize = 0;
    for flat in tilo_actions::flatten(resources) {
        let spec = flat.action;
        let ActionInput::Immediate(encoded) = &spec.input else {
            continue;
        };
        if spec.kind != OperationKind::Document || !spec.contract.availability.is_available() {
            continue;
        }
        assert!(
            registry.contains(&encoded.type_id),
            "`{}` on `{}` publishes type `{}`, which the registry cannot decode",
            spec.id,
            flat.resource_id.as_str(),
            encoded.type_id
        );
        let replayed = replay_on_two_peers(seed, registry, encoded);
        let undone = undo_round_trip(seed, registry, encoded);
        assert_eq!(
            replayed,
            undone,
            "`{}` on `{}` behaves the same on a replica and in the host",
            spec.id,
            flat.resource_id.as_str()
        );
        swept = swept.saturating_add(1);
    }
    let publishes_documents = tilo_actions::flatten(resources)
        .iter()
        .any(|flat| flat.action.kind == OperationKind::Document);
    assert!(
        publishes_documents || registry.type_ids().is_empty(),
        "the registry decodes {:?} but the seed publishes no document action",
        registry.type_ids()
    );
    swept
}

/// Assert the Ctrl+K palette, loaded with `resources`, offers a row for each
/// of `labels` when the user types it.
///
/// # Panics
///
/// When a label finds no row.
pub fn assert_ctrl_k_publishes(resources: &[Resource], labels: &[&str]) {
    let mut palette = ActionPalette::new();
    palette.load_open_resources(resources);
    palette.open();
    assert!(
        !palette.is_empty(),
        "an empty query lists the published actions"
    );
    for label in labels {
        palette.set_query(label);
        let rows = palette.result_labels();
        assert!(
            rows.iter().any(|row| row.contains(label)),
            "typing `{label}` in Ctrl+K offers it; rows: {rows:?}"
        );
    }
}

/// Generate an app's action-contract tests.
///
/// ```ignore
/// ui::action_contract_tests! {
///     seed: seeded_doc(),
///     registry: registry(),
///     resources: |doc| provide_resources(&ctx(), doc),
///     palette: ["Add function", "Clear history"],
///     replay: [
///         set_formula => |doc| encode(&set_expr_action(first_id(doc), "x^3")),
///     ],
///     undo: [
///         commit_history => |_| encode(&commit_history_action("6*7", "42")),
///     ],
///     encodes: [
///         clear_history => ("calculator.workspace", "clear-history",
///             |doc, _res| encode(&clear_history_ids_action(visible_ids(doc)))),
///         edit_formula => ("calculator.graph", "edit", arg: "cos(x)",
///             |_doc, res| encode(&set_expr_action(res.id.as_str(), "cos(x)"))),
///     ],
/// }
/// ```
///
/// Expands to a module `action_contract` in the calling scope (which must
/// have the names the expressions use in scope, typically via `use super::*`)
/// holding:
///
/// * `every_immediate_action_honours_contract` — the surface sweep of
///   [`assert_every_immediate_action_honours_contract`];
/// * `ctrl_k_publishes` — when `palette` is given;
/// * `replays_byte_identical::<name>` per `replay` entry;
/// * `one_undo_boundary_reverts::<name>` per `undo` entry;
/// * `palette_encodes_as_builder::<name>` per `encodes` entry: the spec
///   `action` on the first resource whose id, else kind, is the first element
///   encodes as the builder does. The builder closure receives the seed
///   document and that resource.
///
/// `seed` is evaluated afresh in every test, and every builder closure
/// receives that test's seed, so ids minted while seeding stay consistent.
/// Sections are optional but must appear in the order shown.
#[macro_export]
macro_rules! action_contract_tests {
    (
        seed: $seed:expr,
        registry: $registry:expr,
        resources: |$rdoc:pat_param| $resources:expr
        $(, palette: [ $( $label:expr ),* $(,)? ] )?
        $(, replay: [ $( $rname:ident => |$rd:pat_param| $rbody:expr ),* $(,)? ] )?
        $(, undo: [ $( $uname:ident => |$ud:pat_param| $ubody:expr ),* $(,)? ] )?
        $(, encodes: [ $(
            $ename:ident => (
                $eres:expr, $eact:expr, $( arg: $earg:expr, )?
                |$edoc:pat_param, $eowner:pat_param| $ebody:expr
            )
        ),* $(,)? ] )?
        $(,)?
    ) => {
        mod action_contract {
            #[allow(unused_imports, reason = "the caller's names, for the expressions it passed")]
            use super::*;

            fn contract_seed() -> $crate::automerge::AutoCommit {
                $seed
            }

            fn contract_registry() -> $crate::tilo_actions::ActionRegistry {
                $registry
            }

            fn contract_resources(
                doc: &$crate::automerge::AutoCommit,
            ) -> ::std::vec::Vec<$crate::tilo_actions::Resource> {
                let $rdoc = doc;
                $resources
            }

            #[test]
            fn every_immediate_action_honours_contract() {
                let seed = contract_seed();
                let resources = contract_resources(&seed);
                $crate::testing::action_contract::assert_every_immediate_action_honours_contract(
                    &seed,
                    &contract_registry(),
                    &resources,
                );
            }

            $(
                #[test]
                fn ctrl_k_publishes() {
                    let seed = contract_seed();
                    $crate::testing::action_contract::assert_ctrl_k_publishes(
                        &contract_resources(&seed),
                        &[$($label),*],
                    );
                }
            )?

            $(
                mod replays_byte_identical {
                    #[allow(unused_imports, reason = "the caller's names, for the expressions it passed")]
                    use super::*;
                    $(
                        #[test]
                        fn $rname() {
                            let seed = contract_seed();
                            let encoded = {
                                let $rd = &seed;
                                $rbody
                            };
                            $crate::testing::action_contract::assert_replays_byte_identical(
                                &seed,
                                &contract_registry(),
                                &encoded,
                            );
                        }
                    )*
                }
            )?

            $(
                mod one_undo_boundary_reverts {
                    #[allow(unused_imports, reason = "the caller's names, for the expressions it passed")]
                    use super::*;
                    $(
                        #[test]
                        fn $uname() {
                            let seed = contract_seed();
                            let encoded = {
                                let $ud = &seed;
                                $ubody
                            };
                            $crate::testing::action_contract::assert_one_undo_boundary_reverts(
                                &seed,
                                &contract_registry(),
                                &encoded,
                            );
                        }
                    )*
                }
            )?

            $(
                mod palette_encodes_as_builder {
                    #[allow(unused_imports, reason = "the caller's names, for the expressions it passed")]
                    use super::*;
                    $(
                        #[test]
                        fn $ename() {
                            let seed = contract_seed();
                            let resources = contract_resources(&seed);
                            let (owner, spec) = $crate::testing::action_contract::find_spec(
                                &resources, $eres, $eact,
                            );
                            let expected = {
                                let $edoc = &seed;
                                let $eowner = owner;
                                $ebody
                            };
                            let arg: ::std::option::Option<&str> = None $( .or(Some($earg)) )?;
                            $crate::testing::action_contract::assert_spec_encodes_as(
                                spec, arg, &expected,
                            );
                        }
                    )*
                }
            )?
        }
    };
}

#[cfg(test)]
mod tests {
    use automerge::ReadDoc;
    use automerge::transaction::Transactable;
    use serde::{Deserialize, Serialize};
    use tilo_actions::{Action, ActionError, Doc};

    use super::*;

    const TYPE: &str = "contract.probe";

    #[derive(Debug, Serialize, Deserialize)]
    enum Probe {
        /// Deterministic: honours the whole contract.
        Set(String),
        /// Writes a fresh random value: diverges on replay.
        Random,
        /// Writes nothing.
        Nothing,
        /// Writes a key and deletes it again: ops, but no visible change.
        Churn,
    }

    impl Action for Probe {
        fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
            match self {
                Self::Set(value) => doc.put(automerge::ROOT, "value", value.as_str())?,
                Self::Random => {
                    doc.put(automerge::ROOT, "value", uuid::Uuid::new_v4().to_string())?
                }
                Self::Nothing => {}
                Self::Churn => {
                    doc.put(automerge::ROOT, "scratch", 1)?;
                    doc.delete(automerge::ROOT, "scratch")?;
                }
            }
            Ok(())
        }
    }

    fn value(doc: &AutoCommit) -> String {
        doc.get(automerge::ROOT, "value")
            .unwrap()
            .and_then(|(v, _)| v.into_string().ok())
            .unwrap_or_default()
    }

    fn registry() -> ActionRegistry {
        let mut registry = ActionRegistry::new();
        registry.register::<Probe>(TYPE);
        registry
    }

    fn seed() -> AutoCommit {
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, "value", "seed").unwrap();
        doc.commit();
        doc
    }

    fn encode(probe: &Probe) -> EncodedAction {
        EncodedAction::encode(TYPE, probe).unwrap()
    }

    fn published(probe: &Probe) -> Vec<Resource> {
        vec![
            Resource::new("probe:doc", "probe.doc", "Probe")
                .with_action(ActionSpec::immediate("act", "Act on probe", TYPE, probe).unwrap()),
        ]
    }

    #[test]
    fn contract_checks_pass_honest_noop_and_prompted_actions() {
        // phase: a_deterministic_action_honours_the_whole_contract
        {
            let set = encode(&Probe::Set("next".into()));
            assert_replays_byte_identical(&seed(), &registry(), &set);
            assert_one_undo_boundary_reverts(&seed(), &registry(), &set);
            let resources = published(&Probe::Set("next".into()));
            let (_, spec) = find_spec(&resources, "probe.doc", "act");
            assert_spec_encodes_as(spec, None, &set);
            assert_eq!(
                assert_every_immediate_action_honours_contract(&seed(), &registry(), &resources),
                1
            );
            assert_ctrl_k_publishes(&resources, &["Act on probe"]);
        }
        // phase: the_sweep_tolerates_a_published_noop
        {
            assert_eq!(
                assert_every_immediate_action_honours_contract(
                    &seed(),
                    &registry(),
                    &published(&Probe::Nothing)
                ),
                1
            );
        }
        // phase: the_sweep_accepts_an_app_whose_document_actions_are_prompted
        {
            let prompted = ActionSpec::prompted_param(
                "rename",
                "Rename probe",
                tilo_actions::ActionParam::required("value", tilo_actions::ParamType::Text, "Name"),
                "",
                tilo_actions::ExecutionLocus::ClientLocal,
                |name| encode(&Probe::Set(name.to_string())),
            );
            let resources =
                vec![Resource::new("probe:doc", "probe.doc", "Probe").with_action(prompted)];
            assert_eq!(
                assert_every_immediate_action_honours_contract(&seed(), &registry(), &resources),
                0
            );
        }
    }

    /// Every contract check fails loudly, with its own message, on the
    /// action shape it exists to catch.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn each_contract_check_fails_the_action_it_exists_to_catch() {
        let spec_disagrees = || {
            let resources = published(&Probe::Set("palette".into()));
            let (_, spec) = find_spec(&resources, "probe:doc", "act");
            assert_spec_encodes_as(spec, None, &encode(&Probe::Set("button".into())));
        };
        let sweep_vacuous = || {
            let effect = ActionSpec::operation(
                "open",
                "Open probe",
                OperationKind::Effect,
                "contract.open",
                &(),
            )
            .unwrap();
            let resources =
                vec![Resource::new("probe:doc", "probe.doc", "Probe").with_action(effect)];
            assert_every_immediate_action_honours_contract(&seed(), &registry(), &resources);
        };
        let sweep_protocol = || {
            let mut resources = published(&Probe::Nothing);
            resources.extend(published(&Probe::Nothing));
            assert_every_immediate_action_honours_contract(&seed(), &registry(), &resources);
        };
        // (case, expected panic message fragment, check)
        let cases: Vec<(&str, &str, Box<dyn Fn()>)> = vec![
            (
                "a nondeterministic action fails replay",
                "replay reproduced the same change hashes",
                Box::new(|| {
                    assert_replays_byte_identical(&seed(), &registry(), &encode(&Probe::Random));
                }),
            ),
            (
                "a noop proves nothing about replay",
                "changed nothing on the seed",
                Box::new(|| {
                    assert_replays_byte_identical(&seed(), &registry(), &encode(&Probe::Nothing));
                }),
            ),
            (
                "a noop proves nothing about undo",
                "nothing to undo",
                Box::new(|| {
                    assert_one_undo_boundary_reverts(
                        &seed(),
                        &registry(),
                        &encode(&Probe::Nothing),
                    );
                }),
            ),
            (
                "a spec that disagrees with its builder fails",
                "encode the same action",
                Box::new(spec_disagrees),
            ),
            (
                "the sweep catches a published nondeterministic action",
                "replay reproduced the same change hashes",
                Box::new(|| {
                    assert_every_immediate_action_honours_contract(
                        &seed(),
                        &registry(),
                        &published(&Probe::Random),
                    );
                }),
            ),
            (
                "the sweep refuses to pass vacuously",
                "publishes no document action",
                Box::new(sweep_vacuous),
            ),
            (
                "the sweep runs the protocol validator",
                "breaks the protocol",
                Box::new(sweep_protocol),
            ),
            (
                "ctrl-k reports a missing label",
                "offers it",
                Box::new(|| {
                    assert_ctrl_k_publishes(&published(&Probe::Nothing), &["Not published"]);
                }),
            ),
        ];
        // A change with no visible effect is no undo step at all.
        assert_eq!(
            undo_round_trip(&seed(), &registry(), &encode(&Probe::Churn)),
            Applied::Unchanged
        );
        for (case, expected, check) in cases {
            let payload =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(check)).expect_err(case);
            let message = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                .unwrap_or_default();
            assert!(
                message.contains(expected),
                "case {case}: expected a panic containing {expected:?}, got {message:?}"
            );
        }
    }

    mod generated {
        use super::*;

        crate::action_contract_tests! {
            seed: seed(),
            registry: registry(),
            resources: |_| published(&Probe::Set("next".into())),
            palette: ["Act on probe"],
            replay: [
                set => |_| encode(&Probe::Set("next".into())),
            ],
            undo: [
                set_from_seed => |doc| encode(&Probe::Set(format!("{}!", value(doc)))),
            ],
            encodes: [
                act => ("probe:doc", "act", |_, _| encode(&Probe::Set("next".into()))),
            ],
        }
    }
}
