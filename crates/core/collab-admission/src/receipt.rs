//! Deciding a received document op change by change, before it is merged: the decisions that
//! read the changes an op carries, on top of the checks of [`collab_types::receipt`].

use std::collections::BTreeSet;

use collab_types::claim::{ChangeClaim, ClaimedAction, FrameworkChange, Invoker};
use collab_types::receipt::{
    Allows, Receipt, ReceiptRefusal, SurfaceFor, Surfaces, WrittenDocument, WrittenUnits, by_kind,
    check_claimed_changes, check_claims, check_claims_by_kind, check_continuations,
    check_store_kinds, label, moderated_nodes, unread_claims, written_node,
};
use collab_types::{DocumentMutation, OpKind, SignedOp};
use tilo_action_policy::TreePath;
use tilo_action_types::ActionKind;
use tilo_member_types::Acting;

/// Decide a received op on `record`: a compaction as a write of the record
/// (or the workspace's `compact` governance action), a redaction as the workspace's
/// governance action, a change by its claims ([`check_claims`]) against the
/// app surface `surfaces` finds, or, with no surface source, as an edit of
/// the record.
///
/// # Errors
/// The first [`ReceiptRefusal`].
pub fn decide_op(
    op: &SignedOp,
    document: &WrittenDocument<'_>,
    kind: Option<&str>,
    held: Option<automerge::AutoCommit>,
    surfaces: &Surfaces,
    allows: &Allows<'_>,
) -> Result<Receipt, ReceiptRefusal> {
    decide_opened(
        (op, None),
        document,
        (kind, held, None),
        None,
        surfaces,
        allows,
        (None, None),
    )
}

/// The workspace decided the sealed op `op` by its claims without reading
/// it: refuse `mutation`, what it opened to, when a change says other than
/// its claim (another action, another invoker, or another caller's signed
/// call), when the claims do not name exactly its changes, or when a
/// continuation names another invoker than the entry it continues in the same
/// op.
///
/// # Errors
/// [`ReceiptRefusal::Unclaimed`] for changes the claims do not name exactly,
/// [`ReceiptRefusal::Misclaimed`] for a claim its change contradicts.
pub fn check_opened_mutation(
    op: &SignedOp,
    mutation: &DocumentMutation,
) -> Result<(), ReceiptRefusal> {
    check_opened_mutation_of(op, None, mutation)
}

/// [`check_opened_mutation`] for a write to `record`, when the opener knows it. Every change of
/// a resource store's document writes records of the store's kinds, a device's own edit of a
/// record as much as the store's stamp: its claim may name the kinds of the records it writes,
/// which whoever holds the record reads again where it applies the change. A change of any
/// other document names a kind only when it is a store's stamp.
///
/// # Errors
/// As [`check_opened_mutation`].
pub fn check_opened_mutation_of(
    op: &SignedOp,
    record: Option<&str>,
    mutation: &DocumentMutation,
) -> Result<(), ReceiptRefusal> {
    let in_store = record.is_some_and(collab_types::is_store_document);
    let DocumentMutation::Incremental(bytes) = mutation else {
        return Ok(());
    };
    let changes = crate::claim::parse_changes(bytes).map_err(|_| ReceiptRefusal::Unclaimed)?;
    let hashes: Vec<[u8; 32]> = changes.iter().map(|change| change.hash().0).collect();
    check_claimed_changes(&op.claims, Some(&hashes))?;
    let calls = tilo_actions::stamp::KeptCalls::of_changes(&changes);
    for change in &changes {
        let read = ChangeClaim::of_change_in(change.hash().0, change.message(), Vec::new(), &calls)
            .map_err(|error| ReceiptRefusal::Unnamed(error.to_string()))?;
        let claimed = op.claims.iter().find(|claim| claim.change == read.change);
        let made = collab_types::claim::kinds_made_in(change);
        // A store's change names the kinds of the records it writes, read by whoever holds them;
        // any other change names no kind at all, and is what its own message says it is.
        let store = in_store || collab_types::claim::is_store_change(change);
        if claimed.is_none_or(|claimed| {
            !names_what_it_read(&claimed.action, &read.action, &made, store)
                || claimed.invoker != read.invoker
                // The call the claim carries outside the ciphertext is the one
                // the change keeps inside it: the caller's own bytes, not
                // another call the writer substituted for a key-less receiver.
                || claimed.call != read.call
        }) {
            return Err(ReceiptRefusal::Misclaimed(label(&read.action)));
        }
    }
    check_continuations(&changes, None, false)
}

/// Whether `claimed`, a change's signed claim, says what the change it opened to says, `read`:
/// the same action, or, for a resource store's change (`store`), which names no kind of its own,
/// an edit of its records whole of the kinds they are. Every kind of a record the change makes,
/// `made`, must be among them; the kinds of the records made before it are read by whoever holds
/// them, as it receives the change ([`decide_opened`]).
fn names_what_it_read(
    claimed: &ClaimedAction,
    read: &ClaimedAction,
    made: &BTreeSet<String>,
    store: bool,
) -> bool {
    claimed == read
        || (store
            && *read == ClaimedAction::store_write(read.store_verb().unwrap_or_default())
            && claimed.store_verb() == read.store_verb()
            && collab_types::claim::whole_edit_kinds(claimed)
                .is_some_and(|kinds| !kinds.is_empty() && made.is_subset(&kinds)))
}

/// [`decide_op`] for a record whose identity names `pinned`, the surface its changes are decided
/// against: an op naming another surface is refused unless it is the change that names a new one
/// for the record ([`FrameworkChange::Upgrade`]). `holding` is the document as this receiver
/// holds it, when it does: a continuation whose gesture's first entry it does not find there or
/// in the op is refused ([`check_continuations`]).
///
/// `opened` is the changes a device opened a write to, decided rather than what `op` carries: a
/// write larger than one op is written as segments, so no op carries its changes whole. `None`
/// decides `op` itself. `imported` is the person whose devices imported the history `op` is of,
/// when it is: its changes that name no invoker are decided as theirs. `signer` is who signed
/// `op` (the signing device's person, or the agent its key is certified for): the framework's own
/// change of a record that names no one is decided and recorded as theirs
/// ([`crate::claim::Maker::Signer`]). A window's edit of a record names who made it, and one that
/// names no one is refused.
///
/// # Errors
/// The first [`ReceiptRefusal`].
pub fn decide_opened(
    (op, opened): (&SignedOp, Option<&[automerge::Change]>),
    document: &WrittenDocument<'_>,
    (kind, held, pinned): (
        Option<&str>,
        Option<automerge::AutoCommit>,
        Option<[u8; 32]>,
    ),
    holding: Option<&automerge::Automerge>,
    surfaces: &Surfaces,
    allows: &Allows<'_>,
    (imported, signer): (Option<&Acting>, Option<&Acting>),
) -> Result<Receipt, ReceiptRefusal> {
    decide_op_settled(
        (op, opened),
        document,
        (kind, held, pinned),
        holding,
        surfaces,
        allows,
        (false, imported, signer),
    )
}

/// [`decide_opened`] for a write a holder hands on from its store, admitted when it was first
/// made: a change to a resource store's document that names no kind of the records it writes,
/// written before changes claimed them, is decided as it was admitted, against the app's own
/// resource. Kept history is never refused for a claim it was let through under.
///
/// # Errors
/// The first [`ReceiptRefusal`].
pub fn decide_op_replayed(
    (op, opened): (&SignedOp, Option<&[automerge::Change]>),
    document: &WrittenDocument<'_>,
    (kind, held, pinned): (
        Option<&str>,
        Option<automerge::AutoCommit>,
        Option<[u8; 32]>,
    ),
    holding: Option<&automerge::Automerge>,
    surfaces: &Surfaces,
    allows: &Allows<'_>,
    (imported, signer): (Option<&Acting>, Option<&Acting>),
) -> Result<Receipt, ReceiptRefusal> {
    decide_op_settled(
        (op, opened),
        document,
        (kind, held, pinned),
        holding,
        surfaces,
        allows,
        (true, imported, signer),
    )
}

/// The claim each of `changes` makes as a cleartext change names it, with the units `written`
/// reads it to write: decided as the invoker its own message names, the import step's person
/// when it names none, or its signer's when it is the framework's own change of the record and
/// names no one ([`crate::claim::maker_of`]). A window's edit that names no one is refused, as
/// one on every path: a change that names nobody otherwise is too.
fn cleartext_claims(
    changes: &[automerge::Change],
    written: Option<&collab_types::units::ByChange>,
    (imported, signer): (Option<&Acting>, Option<&Acting>),
) -> Result<Vec<ChangeClaim>, ReceiptRefusal> {
    let calls = tilo_actions::stamp::KeptCalls::of_changes(changes);
    changes
        .iter()
        .map(|change| {
            let units = written
                .and_then(|written| written.written.get(&change.hash().0))
                .map_or_else(|| vec![Vec::new()], |written| written.units.clone());
            let mut claim = ChangeClaim::of_imported_change(
                change.hash().0,
                change.message(),
                units,
                imported,
                &calls,
            )
            .map_err(|error| match error {
                collab_types::claim::ClaimError::NoInvoker => ReceiptRefusal::NoInvoker,
                other => ReceiptRefusal::Unnamed(other.to_string()),
            })?;
            match crate::claim::maker_of(change) {
                crate::claim::Maker::Signer => {
                    claim.invoker = signer.cloned().map(collab_types::claim::Invoker::new);
                }
                crate::claim::Maker::Nobody if claim.invoker.is_none() => {
                    return Err(ReceiptRefusal::NoInvoker);
                }
                _ => {}
            }
            Ok(claim)
        })
        .collect()
}

fn decide_op_settled(
    (op, opened): (&SignedOp, Option<&[automerge::Change]>),
    document: &WrittenDocument<'_>,
    (kind, held, pinned): (
        Option<&str>,
        Option<automerge::AutoCommit>,
        Option<[u8; 32]>,
    ),
    holding: Option<&automerge::Automerge>,
    surfaces: &Surfaces,
    allows: &Allows<'_>,
    (replay, imported, signer): (bool, Option<&Acting>, Option<&Acting>),
) -> Result<Receipt, ReceiptRefusal> {
    use tilo_action_policy::roles;
    // A migration is the framework's change, never a history move its app claims: bytes that read
    // as one were written before the step's fields were written whole, and a postcard reader
    // takes the units prefix after the step as its absent bool. Refused by the record it names,
    // whichever surface the record's app publishes, rather than decided under a grant for undo.
    if op.migration_claimed_as_history().is_some() {
        return Err(ReceiptRefusal::MigrationAsHistory {
            app: document.app.to_owned(),
            document: document.document.to_owned(),
        });
    }
    let administered = |action: &str| {
        let node = roles::governance_action(document.workspace, action);
        if allows(None, &node, roles::GOVERNANCE_KIND) {
            Ok(Receipt::Admit)
        } else {
            Err(ReceiptRefusal::NotGranted {
                node: node.to_string(),
                kind: roles::GOVERNANCE_KIND,
            })
        }
    };
    match op.kind {
        OpKind::Compaction => admittable_compaction(op, document, allows, &administered),
        OpKind::Redaction => administered(roles::governance::REDACT),
        OpKind::Change => decide_change_op(
            (op, opened),
            document,
            (kind, held, pinned),
            holding,
            surfaces,
            allows,
            (replay, imported, signer),
        ),
    }
}

/// A change op on `document`: decided as an edit of the record when its app publishes no surface
/// (or the record names none), by the claims its envelope carries when this receiver cannot read
/// it, and otherwise change by change against the surfaces they name
/// ([`decide_cleartext_changes`]).
fn decide_change_op(
    (op, opened): (&SignedOp, Option<&[automerge::Change]>),
    document: &WrittenDocument<'_>,
    (kind, held, pinned): (
        Option<&str>,
        Option<automerge::AutoCommit>,
        Option<[u8; 32]>,
    ),
    holding: Option<&automerge::Automerge>,
    surfaces: &Surfaces,
    allows: &Allows<'_>,
    (replay, imported, signer): (bool, Option<&Acting>, Option<&Acting>),
) -> Result<Receipt, ReceiptRefusal> {
    // An op of an app that publishes no surface (and of a record that
    // names none) is decided as an edit of the record; one naming a
    // surface is decided against it, and held until it is here.
    let Some(source) = surfaces
        .source()
        .filter(|_| op.surface.is_some() || pinned.is_some())
    else {
        return decide_as_document_edit(
            (opened_changes(op, opened)?.as_deref(), &op.claims),
            (document, holding),
            allows,
            (imported, signer),
        );
    };
    // A change of a store's content document that names no kind is of the one resource the
    // document's id names.
    let content = collab_types::claim::store_content_kind(document.document);
    let kind = kind.or(content);
    let surface_for =
        |kind: Option<&str>| source.surface(document, kind.or(content), op.surface.as_ref());
    let Some(changes) = opened_changes(op, opened)? else {
        // A sealed op: the server decides the claims its envelope carries, each as who it names
        // ([`sealed_invoker`]); devices check the changes once they open them.
        let mut claims = op.claims.clone();
        for claim in &mut claims {
            claim.invoker = sealed_invoker(claim, (imported, signer))?.map(Invoker::new);
        }
        return check_claims_by_kind(&claims, document, kind, replay, &surface_for, allows);
    };
    // A cleartext change names its action in itself, under the op's
    // signature: the envelope carries no second copy.
    if !op.claims.is_empty() {
        return Err(ReceiptRefusal::Unclaimed);
    }
    decide_cleartext_changes(
        op,
        &changes,
        document,
        (kind, held, pinned),
        holding,
        &surface_for,
        allows,
        (replay, imported, signer),
    )
}

/// The cleartext `changes` of `op`, each decided against the surface of the kind it names
/// (`surface_for`), as this receiver reads the record: `held` is the record it keeps, which says
/// what each change writes, and a receiver that keeps none decides them by what they carry.
fn decide_cleartext_changes(
    op: &SignedOp,
    changes: &[automerge::Change],
    document: &WrittenDocument<'_>,
    (kind, held, pinned): (
        Option<&str>,
        Option<automerge::AutoCommit>,
        Option<[u8; 32]>,
    ),
    holding: Option<&automerge::Automerge>,
    surface_for: &SurfaceFor<'_>,
    allows: &Allows<'_>,
    (replay, imported, signer): (bool, Option<&Acting>, Option<&Acting>),
) -> Result<Receipt, ReceiptRefusal> {
    // The units each change writes, read from the record as this
    // receiver holds it.
    let reads = held.is_some();
    // A continuation is decided as the invoker its claim names; a
    // receiver holding the document checks that its gesture's first
    // entry names the same.
    check_continuations(changes, holding, holding.is_some())?;
    // A resource store's change names the kinds of the records it writes as this receiver
    // reads them, before the change and after it: never as its writer says.
    let mut held = held;
    let kinds = collab_types::claim::store_record_kinds(
        document.document,
        held.as_mut().map(automerge::AutoCommit::document),
        changes,
    );
    let written = held.and_then(|held| collab_types::units::units_by_change(held, changes));
    let mut claims = cleartext_claims(changes, written.as_ref(), (imported, signer))?;
    collab_types::claim::name_record_kinds(&mut claims, &kinds);
    check_store_kinds(&claims, document, replay)?;
    let hashes: Vec<[u8; 32]> = changes.iter().map(|change| change.hash().0).collect();
    if let Some(pinned) = pinned
        && op.surface != Some(pinned)
        && !claims
            .iter()
            .any(|claim| claim.action == ClaimedAction::Framework(FrameworkChange::Upgrade))
    {
        return Err(ReceiptRefusal::OtherSurface);
    }
    check_claimed_changes(&claims, Some(&hashes))?;
    for (group_kind, claims) in by_kind(kind, claims) {
        let Some(surface) = surface_for(group_kind.as_deref()) else {
            return Ok(Receipt::Hold);
        };
        let claims = unread_claims((!reads).then_some(&*surface), claims);
        // What this receiver read from the changes itself: what says a
        // change queued nothing in an outbox. A record whose index does
        // not name its outboxes (indexed before they were units), or a
        // change this receiver could not apply, is taken as queueing.
        let outboxes = surface.outboxes();
        let told_apart = written
            .as_ref()
            .is_some_and(|written| outboxes.iter().all(|outbox| written.indexes(outbox)));
        let read = |hash: &[u8; 32]| match written
            .as_ref()
            .and_then(|written| written.written.get(hash))
        {
            Some(read) if told_apart => read.clone(),
            read => collab_types::units::Written {
                units: read.map(|read| read.units.clone()).unwrap_or_default(),
                added: outboxes.clone(),
            },
        };
        let read: Option<WrittenUnits<'_>> = written.is_some().then_some(&read as WrittenUnits<'_>);
        check_claims(&claims, document, None, read, Some(&*surface), allows)?;
    }
    // Decided last, so a change refused its grant says so first.
    check_genesis_first(changes, holding)?;
    Ok(Receipt::Admit)
}

/// The changes `op` carries in the clear: those a device handed it on whole (`opened`), or the
/// ones its payload holds.
fn opened_changes(
    op: &SignedOp,
    opened: Option<&[automerge::Change]>,
) -> Result<Option<Vec<automerge::Change>>, ReceiptRefusal> {
    match opened {
        Some(opened) => Ok(Some(opened.to_vec())),
        None => cleartext_changes(op),
    }
}

/// Refuse a change named the record's genesis that is not the record's first: one that builds on
/// a change someone made, or that arrives at a record whose copy here, `copy`, already has such a
/// change. Only what every member derives alike (the founding identity, a derived structure)
/// may come before it. A genesis this receiver already has is no new write and passes, as a
/// history handed on whole carries it again.
///
/// A genesis is decided by the grant to create the record, never against the app's published
/// edits; without this, any writer could name a whole-record rewrite of a record with content the
/// record's genesis and so pass by the edits the app publishes.
fn check_genesis_first(
    changes: &[automerge::Change],
    copy: Option<&automerge::Automerge>,
) -> Result<(), ReceiptRefusal> {
    let derived =
        |change: &automerge::Change| crate::claim::maker_of(change) == crate::claim::Maker::Derived;
    // The record's changes as this receiver keeps them, read whole only when a genesis it does
    // not have yet arrives: whether a kept change is nobody's is read from what it writes, as for
    // one received.
    let kept_changes = std::cell::OnceCell::new();
    let kept_derived = |record: &automerge::Automerge, hash: &automerge::ChangeHash| {
        kept_changes
            .get_or_init(|| record.get_changes(&[]))
            .iter()
            .find(|change: &&automerge::Change| change.hash() == *hash)
            .is_some_and(derived)
    };
    let mut has_content = None;
    for (at, change) in changes.iter().enumerate() {
        let named = change.message().and_then(tilo_actions::stamp::Label::of);
        if named
            != Some(tilo_actions::stamp::Label::Framework(
                FrameworkChange::Genesis,
            ))
            || derived(change)
        {
            continue;
        }
        if copy.is_some_and(|record| record.has_change(&change.hash())) {
            continue;
        }
        let made_before = change.deps().iter().any(|dep| {
            match changes
                .iter()
                .take(at)
                .find(|earlier| earlier.hash() == *dep)
            {
                Some(earlier) => !derived(earlier),
                None => {
                    copy.is_some_and(|record| record.has_change(dep) && !kept_derived(record, dep))
                }
            }
        });
        let content = *has_content.get_or_insert_with(|| {
            copy.is_some_and(|record| {
                record
                    .get_heads()
                    .iter()
                    .any(|head| !kept_derived(record, head))
            })
        });
        if made_before || content {
            return Err(ReceiptRefusal::GenesisAfterContent);
        }
    }
    Ok(())
}

/// A change of an app that publishes no surface (and of a document that names
/// none), decided as an edit of the document by whoever made each of its
/// changes: as the receiver reads them, or, where it reads none (a sealed op it has not
/// opened), as the op's signed `claims` name them. A genesis among them is decided last against
/// the record as this receiver keeps it, `holding`, so a change refused its grant says so first.
fn decide_as_document_edit(
    (changes, claims): (Option<&[automerge::Change]>, &[ChangeClaim]),
    (document, holding): (&WrittenDocument<'_>, Option<&automerge::Automerge>),
    allows: &Allows<'_>,
    (imported, signer): (Option<&Acting>, Option<&Acting>),
) -> Result<Receipt, ReceiptRefusal> {
    let record = written_node(document);
    // Each change is decided as who made it ([`crate::claim::maker_of`]) at the record, or a
    // store's change at the verb it was written under, which admits no other edit.
    let at_verb = |verb: Option<&str>| match verb {
        Some(verb) => record.clone().action(verb),
        None => record.clone(),
    };
    let mut invokers: Vec<(Option<Acting>, TreePath)> = Vec::new();
    match changes {
        Some(changes) => {
            let calls = tilo_actions::stamp::KeptCalls::of_changes(changes.iter());
            for change in changes {
                let action = ChangeClaim::of_change_in(
                    change.hash().0,
                    change.message(),
                    Vec::new(),
                    &calls,
                )
                .ok()
                .map(|claim| claim.action);
                let verb = action
                    .as_ref()
                    .and_then(|action| action.store_verb().map(str::to_owned));
                let invoker = match (crate::claim::maker_of(change), imported) {
                    (crate::claim::Maker::Named, _) => collab_types::claim::invoker_of(change),
                    (crate::claim::Maker::Derived, _) => None,
                    (crate::claim::Maker::Signer, _) => signer.cloned(),
                    (crate::claim::Maker::Nobody, Some(person)) => Some(person.clone()),
                    (crate::claim::Maker::Nobody, None) => return Err(ReceiptRefusal::NoInvoker),
                };
                for node in action
                    .iter()
                    .flat_map(|action| moderated_nodes(&record, action))
                {
                    invokers.push((invoker.clone(), node));
                }
                invokers.push((invoker, at_verb(verb.as_deref())));
            }
        }
        // A write this receiver cannot read is decided by its signed claims ([`sealed_invoker`]).
        None => {
            for claim in claims {
                let node = at_verb(claim.action.store_verb());
                let invoker = sealed_invoker(claim, (imported, signer))?;
                for moderated in moderated_nodes(&record, &claim.action) {
                    invokers.push((invoker.clone(), moderated));
                }
                invokers.push((invoker, node));
            }
        }
    }
    if invokers.is_empty() {
        invokers.push((None, record.clone()));
    }
    if let Some((_, node)) = invokers
        .iter()
        .find(|(invoker, node)| !allows(invoker.as_ref(), node, ActionKind::Undoable))
    {
        Err(ReceiptRefusal::NotGranted {
            node: node.to_string(),
            kind: ActionKind::Undoable,
        })
    } else {
        if let Some(changes) = changes {
            check_genesis_first(changes, holding)?;
        }
        Ok(Receipt::Admit)
    }
}

/// Who made a sealed op's change, by its `claim`: who it names; no one for a founding every device
/// derives; the signer for the framework's own change; the importer for an imported history.
/// Any other claim naming no one is refused, never decided as the signing device's.
fn sealed_invoker(
    claim: &ChangeClaim,
    (imported, signer): (Option<&Acting>, Option<&Acting>),
) -> Result<Option<Acting>, ReceiptRefusal> {
    Ok(match (&claim.invoker, &claim.action) {
        (Some(invoker), _) => Some(Acting::clone(invoker)),
        (None, ClaimedAction::Framework(FrameworkChange::Genesis | FrameworkChange::Structure)) => {
            None
        }
        (None, ClaimedAction::Framework(_)) => signer.cloned(),
        (None, _) => Some(imported.ok_or(ReceiptRefusal::NoInvoker)?.clone()),
    })
}

/// A sealed compaction folds a record's sealed history into one snapshot of
/// what it already held, and only a device holding the record's channel keys
/// can make one. Whoever the record admits as a writer of it holds those
/// keys, so compacting is part of writing it; the workspace's host keeps its
/// governance action as well. A compaction in the clear sets the workspace's
/// history horizon for everyone and stays the host's to make, which `administered`
/// decides.
fn admittable_compaction(
    op: &SignedOp,
    document: &WrittenDocument<'_>,
    allows: &Allows<'_>,
    administered: &dyn Fn(&str) -> Result<Receipt, ReceiptRefusal>,
) -> Result<Receipt, ReceiptRefusal> {
    if op.encryption.is_some() && allows(None, &written_node(document), ActionKind::Undoable) {
        Ok(Receipt::Admit)
    } else {
        administered(tilo_action_policy::roles::governance::COMPACT)
    }
}

/// Who the changes of `op` name as having made them, as its claims or its
/// cleartext changes say, each once and in order: what a refusal reports, so
/// whoever reads it knows whose change was refused.
#[must_use]
pub fn claimed_invokers(op: &SignedOp) -> Vec<Acting> {
    let named: Vec<Option<Acting>> = match cleartext_changes(op) {
        Ok(Some(changes)) => changes
            .iter()
            .map(collab_types::claim::invoker_of)
            .collect(),
        _ => op
            .claims
            .iter()
            .map(|claim| claim.invoker.as_deref().cloned())
            .collect(),
    };
    let mut invokers: Vec<Acting> = Vec::new();
    for invoker in named.into_iter().flatten() {
        if !invokers.contains(&invoker) {
            invokers.push(invoker);
        }
    }
    invokers
}

/// The hashes of the changes a cleartext op carries; `None` for a sealed
/// op or one segment of a larger mutation, whose changes are checked once
/// readable.
fn cleartext_changes(op: &SignedOp) -> Result<Option<Vec<automerge::Change>>, ReceiptRefusal> {
    if op.encryption.is_some() {
        return Ok(None);
    }
    match DocumentMutation::decode(&op.payload) {
        Ok(DocumentMutation::Incremental(bytes)) => crate::claim::parse_changes(&bytes)
            .map(Some)
            .map_err(|_| ReceiptRefusal::Unclaimed),
        // A snapshot is a checkpoint; segments are decided once whole.
        Ok(DocumentMutation::Snapshot(_)) | Err(_) => Ok(None),
    }
}

#[cfg(test)]
mod tests;
