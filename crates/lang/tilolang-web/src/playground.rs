//! The playground: one source text compiled as a package of one file, run by two people (`alice`,
//! `bob`) whose workspaces are two Automerge stores that sync when they are online, each with the
//! package's first view open. Every export answers both views' trees as JSON, with the actions
//! each person's history holds; going offline, undoing and syncing replay what each person did.

use std::cell::RefCell;

use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use tilolang::{Compiled, IdsLock, SeededEntropy, compile_package_with};
use tilolang_runtime::ids::MemberId;
use tilolang_runtime::{Actor, AmStore, InvokeOutcome, ResourceRef, Runtime, Value, ViewInstance};
use tilolang_tilo::authority::TiloAuthority;
use ui::widget::code_editor::{Highlighter as _, TilolangHighlighter, TokenKind};
use wasm_bindgen::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum PeerId {
    Alice,
    Bob,
}

impl PeerId {
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "alice" | "a" | "A" => Ok(Self::Alice),
            "bob" | "b" | "B" => Ok(Self::Bob),
            other => Err(format!(
                "unknown peer `{other}` (expected `alice` or `bob`)"
            )),
        }
    }

    fn actor(self) -> Actor {
        match self {
            Self::Alice => Actor::from(MemberId::person("alice")),
            Self::Bob => Actor::from(MemberId::person("bob")),
        }
    }
}

#[derive(Clone)]
enum Op {
    Fire {
        key: String,
        event: String,
        value: Option<Json>,
    },
    Edit {
        key: String,
        value: Json,
    },
    Invoke {
        action: String,
        args: Vec<Json>,
    },
}

#[derive(Clone)]
enum TimelineEvent {
    Op {
        peer: PeerId,
        op: Op,
        mutated_store: bool,
    },
    Sync,
}

struct Peer {
    actor: Actor,
    store: AmStore,
    rt: Runtime,
    view: ViewInstance,
}

struct Session {
    compiled: Compiled,
    root: ResourceRef,
    alice: Peer,
    bob: Peer,
    online: bool,
    synced_history_len: usize,
    events: Vec<TimelineEvent>,
}

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
}

#[derive(Serialize)]
struct HighlightSpan {
    start: usize,
    end: usize,
    kind: &'static str,
}

/// A run's kind as a page's stylesheet names it.
fn kind_name(kind: TokenKind) -> &'static str {
    match kind {
        TokenKind::Keyword => "keyword",
        TokenKind::Type => "type",
        TokenKind::Function => "function",
        TokenKind::Number => "number",
        TokenKind::StringLit | TokenKind::CharLit => "string",
        TokenKind::Comment => "comment",
        TokenKind::Operator | TokenKind::Punctuation => "punct",
        TokenKind::Attribute => "ref",
        TokenKind::Constant => "constant",
        _ => "text",
    }
}

/// `source`'s coloured runs as the toolkit's code editor colours tilolang
/// ([`TilolangHighlighter`]), as JSON spans with UTF-16 offsets, as a page's editor counts.
#[wasm_bindgen]
pub fn tokenize(source: &str) -> String {
    let mut spans = Vec::new();
    // UTF-16 units before the line, and the line's byte offsets in UTF-16 units.
    let mut before = 0usize;
    for line in source.split('\n') {
        let mut units = Vec::with_capacity(line.len().saturating_add(1));
        let mut at = 0usize;
        for ch in line.chars() {
            units.extend(std::iter::repeat_n(at, ch.len_utf8()));
            at = at.saturating_add(ch.len_utf16());
        }
        units.push(at);
        let unit = |byte: usize| before.saturating_add(units.get(byte).copied().unwrap_or(at));
        for span in TilolangHighlighter.spans(line, 0).spans {
            spans.push(HighlightSpan {
                start: unit(span.range.start),
                end: unit(span.range.end),
                kind: kind_name(span.kind),
            });
        }
        // The line and its `\n`.
        before = before.saturating_add(at).saturating_add(1);
    }
    serde_json::to_string(&spans).unwrap_or_else(|_| "[]".to_owned())
}

fn open_default_view(
    compiled: &Compiled,
    rt: &Runtime,
    root: &ResourceRef,
    actor: &Actor,
) -> Result<ViewInstance, String> {
    let mut view = if let Ok(v) = rt.open_resource(actor, root, None) {
        v
    } else {
        let first_view = compiled
            .meta
            .views
            .first()
            .ok_or_else(|| "package declares no views".to_owned())?;
        let args = if first_view.params.is_empty() {
            Vec::new()
        } else {
            vec![Value::resource(root)]
        };
        rt.open_view(actor, &first_view.id, args)
            .map_err(|e| e.to_string())?
    };
    view.set_lazy_rows(false).map_err(|e| e.to_string())?;
    Ok(view)
}

fn history_count(peer: &Peer) -> usize {
    peer.store
        .doc_ids()
        .iter()
        .map(|doc| peer.rt.history(doc).map_or(0, |v| v.len()))
        .sum()
}

fn boot_peers(compiled: &Compiled) -> Result<(ResourceRef, Peer, Peer, usize), String> {
    let mut store_a = AmStore::new();
    let mut store_b = AmStore::new();
    // Each peer is one person with no workspace around them: the platform's authority decides
    // by the code's authorship alone.
    let solo = |peer: PeerId| TiloAuthority::new(peer.actor(), || None);
    let rt_a = Runtime::new(store_a.clone(), solo(PeerId::Alice)).map_err(|e| e.to_string())?;
    let rt_b = Runtime::new(store_b.clone(), solo(PeerId::Bob)).map_err(|e| e.to_string())?;

    rt_a.load_compiled(compiled).map_err(|e| e.to_string())?;
    rt_b.load_compiled(compiled).map_err(|e| e.to_string())?;

    let root_type_name = compiled
        .meta
        .roots
        .first()
        .and_then(|root_id| {
            compiled
                .meta
                .types
                .iter()
                .find(|t| &t.id == root_id)
                .map(|t| t.name.clone())
        })
        .or_else(|| compiled.meta.types.first().map(|t| t.name.clone()))
        .ok_or_else(|| "package declares no types".to_owned())?;

    let root = if compiled.meta.roots.is_empty() {
        rt_a.create(&root_type_name).map_err(|e| e.to_string())?
    } else {
        rt_a.create_root(&root_type_name)
            .map_err(|e| e.to_string())?
    };

    // Initial Automerge sync so both peers start from the exact same root resource.
    store_a.sync_with(&mut store_b).map_err(|e| e.to_string())?;

    let actor_a = PeerId::Alice.actor();
    let actor_b = PeerId::Bob.actor();

    let view_a = open_default_view(compiled, &rt_a, &root, &actor_a)?;
    let view_b = open_default_view(compiled, &rt_b, &root, &actor_b)?;

    let alice = Peer {
        actor: actor_a,
        store: store_a,
        rt: rt_a,
        view: view_a,
    };
    let bob = Peer {
        actor: actor_b,
        store: store_b,
        rt: rt_b,
        view: view_b,
    };
    let synced_len = history_count(&alice);
    Ok((root, alice, bob, synced_len))
}

fn json_to_value(j: &Json) -> Value {
    match j {
        Json::Null => Value::none(),
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else {
                Value::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        Json::String(s) => Value::Text(s.clone()),
        Json::Array(items) => Value::List(items.iter().map(json_to_value).collect()),
        Json::Object(_) => Value::Json(Box::new(j.clone())),
    }
}

fn actor_label(invoker: Option<&Actor>) -> &'static str {
    let alice = PeerId::Alice.actor();
    let bob = PeerId::Bob.actor();
    match invoker {
        Some(a) if a == &alice => "alice",
        Some(a) if a == &bob => "bob",
        _ => "init",
    }
}

fn collect_peer_history(compiled: &Compiled, peer: &Peer) -> Vec<(String, &'static str, usize)> {
    let mut out = Vec::new();
    for doc in peer.store.doc_ids() {
        let notes = peer.rt.history(&doc).unwrap_or_default();
        for n in notes {
            let action_name = compiled
                .meta
                .actions
                .iter()
                .find(|a| a.id == n.action)
                .map_or_else(|| n.action.clone(), |a| a.name.clone());
            let actor = actor_label(n.invoker.as_ref());
            out.push((action_name, actor, n.touched.len()));
        }
    }
    out
}

fn serialize_history(session: &Session) -> Json {
    let hist_a = collect_peer_history(&session.compiled, &session.alice);
    let hist_b = collect_peer_history(&session.compiled, &session.bob);
    let shared_len = session
        .synced_history_len
        .min(hist_a.len())
        .min(hist_b.len());

    let mut items: Vec<Json> = Vec::new();

    for (action, actor, touched) in hist_a.iter().take(shared_len) {
        items.push(json!({
            "action": action,
            "actor": actor,
            "touched": touched,
            "synced": true,
        }));
    }

    for (action, actor, touched) in hist_a.iter().skip(shared_len) {
        items.push(json!({
            "action": action,
            "actor": actor,
            "touched": touched,
            "synced": false,
        }));
    }

    for (action, actor, touched) in hist_b.iter().skip(shared_len) {
        items.push(json!({
            "action": action,
            "actor": actor,
            "touched": touched,
            "synced": false,
        }));
    }

    Json::Array(items)
}

fn pending_counts(session: &Session) -> (usize, usize) {
    let mut alice = 0usize;
    let mut bob = 0usize;
    for ev in session.events.iter().rev() {
        match ev {
            TimelineEvent::Sync => break,
            TimelineEvent::Op {
                peer,
                mutated_store: true,
                ..
            } => match peer {
                PeerId::Alice => alice = alice.saturating_add(1),
                PeerId::Bob => bob = bob.saturating_add(1),
            },
            TimelineEvent::Op {
                mutated_store: false,
                ..
            } => {}
        }
    }
    (alice, bob)
}

fn can_undo_peer(session: &Session, peer: PeerId) -> bool {
    session.events.iter().any(|ev| {
        matches!(
            ev,
            TimelineEvent::Op {
                peer: p,
                mutated_store: true,
                ..
            } if *p == peer
        )
    })
}

fn session_snapshot(session: &Session) -> Json {
    let (pending_alice, pending_bob) = pending_counts(session);
    json!({
        "ok": true,
        "wasmBytes": session.compiled.wasm.len(),
        "app": {
            "id": session.compiled.meta.app,
            "title": session.compiled.meta.title,
        },
        "online": session.online,
        "pendingAlice": pending_alice,
        "pendingBob": pending_bob,
        "alice": {
            "tree": session.alice.view.tree().to_json(),
            "canUndo": can_undo_peer(session, PeerId::Alice),
        },
        "bob": {
            "tree": session.bob.view.tree().to_json(),
            "canUndo": can_undo_peer(session, PeerId::Bob),
        },
        "history": serialize_history(session),
    })
}

fn refresh_view(view: &mut ViewInstance) -> Result<(), String> {
    let _ = view.poll_patches().map_err(|e| e.to_string())?;
    view.set_lazy_rows(false).map_err(|e| e.to_string())?;
    Ok(())
}

fn sync_stores_and_views(session: &mut Session) -> Result<(), String> {
    session
        .alice
        .store
        .sync_with(&mut session.bob.store)
        .map_err(|e| e.to_string())?;
    refresh_view(&mut session.alice.view)?;
    refresh_view(&mut session.bob.view)?;
    session.synced_history_len = history_count(&session.alice);
    Ok(())
}

fn apply_op_on_peer(
    compiled: &Compiled,
    root: &ResourceRef,
    peer: &mut Peer,
    op: &Op,
) -> Result<bool, String> {
    let before = history_count(peer);
    match op {
        Op::Fire { key, event, value } => {
            let val = value.as_ref().map(json_to_value);
            let outcome = peer
                .view
                .fire_with(key, event, val.as_ref())
                .map_err(|e| e.to_string())?;
            if matches!(outcome, InvokeOutcome::Denied) {
                return Err("action denied".to_owned());
            }
        }
        Op::Edit { key, value } => {
            let val = json_to_value(value);
            peer.view.edit(key, &val).map_err(|e| e.to_string())?;
        }
        Op::Invoke { action, args } => {
            let meta = compiled
                .meta
                .actions
                .iter()
                .find(|a| a.name == *action || a.id == *action)
                .ok_or_else(|| format!("unknown action `{action}`"))?;
            let mut call_args: Vec<Value> = args.iter().map(json_to_value).collect();
            if let Some(first_param) = meta.params.first() {
                let root_type_name = compiled
                    .meta
                    .types
                    .iter()
                    .find(|t| t.id == root.ty)
                    .map(|t| t.name.as_str());
                let needs_root = call_args.len() < meta.params.len()
                    && (Some(first_param.ty.as_str()) == root_type_name
                        || first_param
                            .ty
                            .ends_with(&format!(".{}", root_type_name.unwrap_or(""))));
                if needs_root {
                    call_args.insert(0, Value::resource(root));
                }
            }
            let outcome = peer
                .rt
                .invoke_action(&peer.actor, (meta.id.as_str(), call_args))
                .map_err(|e| e.to_string())?;
            if matches!(outcome, InvokeOutcome::Denied) {
                return Err("action denied".to_owned());
            }
        }
    }
    refresh_view(&mut peer.view)?;
    let after = history_count(peer);
    Ok(after > before)
}

fn run_peer_op(session: &mut Session, peer_id: PeerId, op: Op) -> Result<(), String> {
    let mutated_store = match peer_id {
        PeerId::Alice => {
            apply_op_on_peer(&session.compiled, &session.root, &mut session.alice, &op)?
        }
        PeerId::Bob => apply_op_on_peer(&session.compiled, &session.root, &mut session.bob, &op)?,
    };
    session.events.push(TimelineEvent::Op {
        peer: peer_id,
        op,
        mutated_store,
    });
    if mutated_store && session.online {
        sync_stores_and_views(session)?;
        session.events.push(TimelineEvent::Sync);
    }
    Ok(())
}

/// Compile tilolang source once, boot two independent Automerge peers (Alice & Bob),
/// run initial sync, and return both rendered views + compiled byte size + history.
#[wasm_bindgen]
pub fn compile_and_run(source: &str) -> String {
    let compiled =
        match compile_package_with(source, &IdsLock::default(), &[], &mut SeededEntropy(1)) {
            Ok(c) => c,
            Err(mut diags) => {
                for d in &mut diags {
                    if d.line == 0 {
                        d.locate(source);
                    }
                }
                let list: Vec<Json> = diags.iter().map(tilolang::Diagnostic::to_json).collect();
                return json!({
                    "ok": false,
                    "diagnostics": list,
                })
                .to_string();
            }
        };

    let (root, alice, bob, synced_history_len) = match boot_peers(&compiled) {
        Ok(quad) => quad,
        Err(err) => {
            return json!({
                "ok": false,
                "diagnostics": [{
                    "code": "RUNTIME",
                    "message": err,
                    "line": 1,
                    "col": 1,
                    "start": 0,
                    "end": 0,
                }],
            })
            .to_string();
        }
    };

    let session = Session {
        compiled,
        root,
        alice,
        bob,
        online: true,
        synced_history_len,
        events: Vec::new(),
    };
    let out = session_snapshot(&session).to_string();
    SESSION.with(|slot| {
        *slot.borrow_mut() = Some(session);
    });
    out
}

/// Fire a view event (e.g. button `press`, input `submit`) on `peer` ("alice" | "bob").
#[wasm_bindgen]
pub fn fire_peer(peer: &str, key: &str, event: &str, value_json: Option<String>) -> String {
    let peer_id = match PeerId::parse(peer) {
        Ok(p) => p,
        Err(err) => return json!({ "ok": false, "error": err }).to_string(),
    };
    let parsed_val: Option<Json> = match value_json {
        Some(raw) if !raw.is_empty() => match serde_json::from_str(&raw) {
            Ok(v) => Some(v),
            Err(e) => return json!({ "ok": false, "error": e.to_string() }).to_string(),
        },
        _ => None,
    };

    SESSION.with(|slot| {
        let mut borrow = slot.borrow_mut();
        let Some(session) = borrow.as_mut() else {
            return json!({ "ok": false, "error": "no active session" }).to_string();
        };
        let op = Op::Fire {
            key: key.to_owned(),
            event: event.to_owned(),
            value: parsed_val,
        };
        match run_peer_op(session, peer_id, op) {
            Ok(()) => session_snapshot(session).to_string(),
            Err(err) => json!({ "ok": false, "error": err }).to_string(),
        }
    })
}

/// Edit a bound control (`bind:`) on `peer` ("alice" | "bob").
#[wasm_bindgen]
pub fn edit_peer(peer: &str, key: &str, value_json: &str) -> String {
    let peer_id = match PeerId::parse(peer) {
        Ok(p) => p,
        Err(err) => return json!({ "ok": false, "error": err }).to_string(),
    };
    let value: Json = match serde_json::from_str(value_json) {
        Ok(v) => v,
        Err(e) => return json!({ "ok": false, "error": e.to_string() }).to_string(),
    };

    SESSION.with(|slot| {
        let mut borrow = slot.borrow_mut();
        let Some(session) = borrow.as_mut() else {
            return json!({ "ok": false, "error": "no active session" }).to_string();
        };
        let op = Op::Edit {
            key: key.to_owned(),
            value,
        };
        match run_peer_op(session, peer_id, op) {
            Ok(()) => session_snapshot(session).to_string(),
            Err(err) => json!({ "ok": false, "error": err }).to_string(),
        }
    })
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ArgsInput {
    List(Vec<Json>),
    Single(Json),
}

/// Invoke a named action directly on `peer`'s runtime with JSON arguments.
#[wasm_bindgen]
pub fn invoke_peer(peer: &str, action: &str, args_json: &str) -> String {
    let peer_id = match PeerId::parse(peer) {
        Ok(p) => p,
        Err(err) => return json!({ "ok": false, "error": err }).to_string(),
    };
    let args: Vec<Json> = if args_json.trim().is_empty() {
        Vec::new()
    } else {
        match serde_json::from_str::<ArgsInput>(args_json) {
            Ok(ArgsInput::List(v)) => v,
            Ok(ArgsInput::Single(Json::Null)) => Vec::new(),
            Ok(ArgsInput::Single(v)) => vec![v],
            Err(e) => return json!({ "ok": false, "error": e.to_string() }).to_string(),
        }
    };

    SESSION.with(|slot| {
        let mut borrow = slot.borrow_mut();
        let Some(session) = borrow.as_mut() else {
            return json!({ "ok": false, "error": "no active session" }).to_string();
        };
        let op = Op::Invoke {
            action: action.to_owned(),
            args,
        };
        match run_peer_op(session, peer_id, op) {
            Ok(()) => session_snapshot(session).to_string(),
            Err(err) => json!({ "ok": false, "error": err }).to_string(),
        }
    })
}

/// Toggle online/offline sync mode. Switching back online immediately syncs both Automerge stores.
#[wasm_bindgen]
pub fn set_online(online: bool) -> String {
    SESSION.with(|slot| {
        let mut borrow = slot.borrow_mut();
        let Some(session) = borrow.as_mut() else {
            return json!({ "ok": false, "error": "no active session" }).to_string();
        };
        session.online = online;
        if online {
            if let Err(err) = sync_stores_and_views(session) {
                return json!({ "ok": false, "error": err }).to_string();
            }
            session.events.push(TimelineEvent::Sync);
        }
        session_snapshot(session).to_string()
    })
}

/// Run real bidirectional `AmStore::sync_with` between Alice and Bob and re-render both views.
#[wasm_bindgen]
pub fn sync_peers() -> String {
    SESSION.with(|slot| {
        let mut borrow = slot.borrow_mut();
        let Some(session) = borrow.as_mut() else {
            return json!({ "ok": false, "error": "no active session" }).to_string();
        };
        if let Err(err) = sync_stores_and_views(session) {
            return json!({ "ok": false, "error": err }).to_string();
        }
        session.events.push(TimelineEvent::Sync);
        session_snapshot(session).to_string()
    })
}

/// Undo the last CRDT-mutating operation by `peer` ("alice" | "bob") while preserving the other peer's work.
#[wasm_bindgen]
pub fn undo_peer(peer: &str) -> String {
    let peer_id = match PeerId::parse(peer) {
        Ok(p) => p,
        Err(err) => return json!({ "ok": false, "error": err }).to_string(),
    };

    SESSION.with(|slot| {
        let mut borrow = slot.borrow_mut();
        let Some(session) = borrow.as_mut() else {
            return json!({ "ok": false, "error": "no active session" }).to_string();
        };

        let Some(idx) = session.events.iter().rposition(|ev| {
            matches!(
                ev,
                TimelineEvent::Op {
                    peer: p,
                    mutated_store: true,
                    ..
                } if *p == peer_id
            )
        }) else {
            return session_snapshot(session).to_string();
        };

        session.events.remove(idx);

        let (root, mut alice, mut bob, mut synced_len) = match boot_peers(&session.compiled) {
            Ok(quad) => quad,
            Err(err) => return json!({ "ok": false, "error": err }).to_string(),
        };

        for ev in &session.events {
            match ev {
                TimelineEvent::Op { peer, op, .. } => {
                    let target = match peer {
                        PeerId::Alice => &mut alice,
                        PeerId::Bob => &mut bob,
                    };
                    // Best-effort replay: if a dependent item no longer exists, skip cleanly.
                    let _ = apply_op_on_peer(&session.compiled, &root, target, op);
                }
                TimelineEvent::Sync => {
                    if let Err(err) = alice.store.sync_with(&mut bob.store) {
                        return json!({ "ok": false, "error": err.to_string() }).to_string();
                    }
                    let _ = refresh_view(&mut alice.view);
                    let _ = refresh_view(&mut bob.view);
                    synced_len = history_count(&alice);
                }
            }
        }

        if session.online {
            if let Err(err) = alice.store.sync_with(&mut bob.store) {
                return json!({ "ok": false, "error": err.to_string() }).to_string();
            }
            let _ = refresh_view(&mut alice.view);
            let _ = refresh_view(&mut bob.view);
            synced_len = history_count(&alice);
        }

        session.root = root;
        session.alice = alice;
        session.bob = bob;
        session.synced_history_len = synced_len;
        session_snapshot(session).to_string()
    })
}

/// Reset both peers back to their initial synced state.
#[wasm_bindgen]
pub fn reset() -> String {
    SESSION.with(|slot| {
        let mut borrow = slot.borrow_mut();
        let Some(session) = borrow.as_mut() else {
            return json!({ "ok": false, "error": "no active session" }).to_string();
        };
        let (root, alice, bob, synced_len) = match boot_peers(&session.compiled) {
            Ok(quad) => quad,
            Err(err) => return json!({ "ok": false, "error": err }).to_string(),
        };
        session.root = root;
        session.alice = alice;
        session.bob = bob;
        session.online = true;
        session.synced_history_len = synced_len;
        session.events.clear();
        session_snapshot(session).to_string()
    })
}
