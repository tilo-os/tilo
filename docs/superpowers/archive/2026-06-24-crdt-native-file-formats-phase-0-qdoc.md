# CRDT-Native File Formats — Phase 0 (`qdoc` foundation) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the `qdoc` crate: the canonical rich-text CRDT document model (over automerge), the `FormatProvider` trait + registry, and the merge-convergence test bed — with no FS/app wiring yet.

**Architecture:** `qdoc` is a lightweight, GUI-free crate at the workspace root. A `Document` wraps an `automerge::AutoCommit`; the rich-text model is stored as a defined layout of automerge objects (a `blocks` map of block objects ordered by fractional keys, each text block holding an automerge `Text` with marks; a `styles` map; a `preserved_parts` map; a `meta` map). `materialize()` produces a plain-Rust read-only snapshot for rendering/export. Format providers are pure `bytes ↔ changes` transforms registered by extension/MIME.

**Tech Stack:** Rust (edition 2021 to match workspace apps; the crate has no edition-2024 needs), `automerge = "0.8.0"`, `serde`/`serde_json` (snapshot serialization + tests), `proptest` (merge-convergence). No GUI deps — `fs` must be able to depend on this crate.

## Global Constraints

- **automerge = "0.8.0"** exactly (workspace pin; matches `ui`). Text edits via `splice_text(&ObjId, pos: usize, del: isize, &str)`; objects via `put_object(parent, key, ObjType::{Map,Text,List})`.
- **No GUI/heavy deps in `qdoc`** — only `automerge`, `serde`, `serde_json`, `thiserror`, and `proptest` (dev). Rationale: `fs` (a filesystem daemon) depends on `qdoc`.
- **Canonical schema is a public ABI** — the automerge layout (root keys, block fields) and `DocSnapshot` are versioned (`schema_version = 1`). Don't rename fields casually; third-party codecs target them.
- **Block ids must be globally unique across peers** — mint as `"{actor_hex}.{counter}"` (mirrors the existing peer-salt anti-collision approach). Never a bare process-local integer.
- **Verification covenant** (repo-wide): never `#[ignore]` a failing test, never weaken an assertion, always assert on the production path.
- **Build/test command:** `distrobox enter my-distrobox -- bash -c 'cd <worktree> && cargo test -p qdoc'`.

---

### Task 1: Create the `qdoc` crate and wire it into the workspace

**Files:**
- Create: `qdoc/Cargo.toml`
- Create: `qdoc/src/lib.rs`
- Modify: `Cargo.toml` (root workspace `members`)

**Interfaces:**
- Produces: the `qdoc` crate, buildable via `cargo build -p qdoc`. Public modules added in later tasks.

- [ ] **Step 1: Create `qdoc/Cargo.toml`**

```toml
[package]
name = "qdoc"
version = "0.1.0"
edition = "2021"
description = "CRDT-native canonical document model and format-provider API for qOS"

[dependencies]
automerge = "0.8.0"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "1"

[dev-dependencies]
proptest = "1"
```

- [ ] **Step 2: Create `qdoc/src/lib.rs` with a smoke test**

```rust
//! `qdoc` — the CRDT-native canonical document model and format-provider API.
//!
//! Layer 1 (generic): a file is an automerge document; this crate provides the
//! `FormatProvider` contract + registry. Layer 2 (standard schema): the rich-text
//! `Document` model that md/odt/docx map onto. See
//! `docs/superpowers/specs/2026-06-24-crdt-native-file-formats-design.md`.

#[cfg(test)]
mod smoke {
    #[test]
    fn crate_builds() {
        assert_eq!(2 + 2, 4);
    }
}
```

- [ ] **Step 3: Add `qdoc` to the workspace members**

Modify root `Cargo.toml` so `members` reads:

```toml
[workspace]
members = [
    "qdoc",
    "fs",
    "shell",
    "ui",
    "apps/file-explorer",
    "apps/terminal",
    "apps/browser",
]
resolver = "2"
```

- [ ] **Step 4: Build + run the smoke test**

Run: `distrobox enter my-distrobox -- bash -c 'cd <worktree> && cargo test -p qdoc'`
Expected: compiles; `smoke::crate_builds` PASS.

- [ ] **Step 5: Commit**

```bash
git add qdoc/Cargo.toml qdoc/src/lib.rs Cargo.toml
git commit -m "feat(qdoc): scaffold crate + workspace wiring"
```

---

### Task 2: Fractional order keys (`order` module)

Order keys let concurrent block insert/delete/move merge cleanly: a block's position is a value, not an index. Keys are base-256 fractions stored as `Vec<u8>`; the midpoint of two distinct keys is always strictly between them.

**Files:**
- Create: `qdoc/src/order.rs`
- Modify: `qdoc/src/lib.rs` (add `pub mod order;`)

**Interfaces:**
- Produces:
  - `order::key_between(lo: Option<&[u8]>, hi: Option<&[u8]>) -> Vec<u8>`
  - `order::cmp_keys(a: &[u8], b: &[u8]) -> std::cmp::Ordering`

- [ ] **Step 1: Write failing tests**

Create `qdoc/src/order.rs`:

```rust
//! Fractional order keys: base-256 fractions where a midpoint always exists
//! between two distinct keys. Compared as fractions (trailing zeros are
//! insignificant), so block order survives concurrent structural edits.

use std::cmp::Ordering;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn between_empty_bounds_is_mid() {
        let k = key_between(None, None);
        assert_eq!(cmp_keys(&[], &k), Ordering::Less);
    }

    #[test]
    fn append_after_is_greater() {
        let a = key_between(None, None);
        let b = key_between(Some(&a), None);
        assert_eq!(cmp_keys(&a, &b), Ordering::Less);
    }

    #[test]
    fn prepend_before_is_smaller() {
        let a = key_between(None, None);
        let b = key_between(None, Some(&a));
        assert_eq!(cmp_keys(&b, &a), Ordering::Less);
    }

    #[test]
    fn midpoint_is_strictly_between() {
        let a = vec![10u8];
        let b = vec![20u8];
        let m = key_between(Some(&a), Some(&b));
        assert_eq!(cmp_keys(&a, &m), Ordering::Less);
        assert_eq!(cmp_keys(&m, &b), Ordering::Less);
    }

    #[test]
    fn repeated_subdivision_stays_ordered() {
        // Insert 200 keys always between the first two; each must stay ordered.
        let mut lo = key_between(None, None);
        let hi = key_between(Some(&lo), None);
        let mut prev = lo.clone();
        for _ in 0..200 {
            let m = key_between(Some(&lo), Some(&hi));
            assert_eq!(cmp_keys(&lo, &m), Ordering::Less, "m must exceed lo");
            assert_eq!(cmp_keys(&m, &hi), Ordering::Less, "m must precede hi");
            assert_eq!(cmp_keys(&prev, &m), Ordering::Less, "strictly increasing");
            prev = m.clone();
            lo = m;
        }
    }

    #[test]
    fn trailing_zeros_compare_equal() {
        assert_eq!(cmp_keys(&[5], &[5, 0, 0]), Ordering::Equal);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `distrobox enter my-distrobox -- bash -c 'cd <worktree> && cargo test -p qdoc order'`
Expected: FAIL — `key_between` / `cmp_keys` not found.

- [ ] **Step 3: Implement the module**

Prepend to `qdoc/src/order.rs` (above the `tests` module):

```rust
/// Compare two keys as base-256 fractions (missing/trailing digits are 0).
pub fn cmp_keys(a: &[u8], b: &[u8]) -> Ordering {
    let n = a.len().max(b.len());
    for i in 0..n {
        let da = a.get(i).copied().unwrap_or(0);
        let db = b.get(i).copied().unwrap_or(0);
        match da.cmp(&db) {
            Ordering::Equal => continue,
            ord => return ord,
        }
    }
    Ordering::Equal
}

/// `(lo + hi) / 2` as a base-256 fraction. Requires `lo < hi`.
/// Processes most-significant-first, carrying the division remainder down,
/// plus one extra digit to absorb the final remainder — so the result is the
/// exact average and therefore strictly between `lo` and `hi`.
fn midpoint(lo: &[u8], hi: &[u8]) -> Vec<u8> {
    let n = lo.len().max(hi.len()) + 1;
    let mut out = Vec::with_capacity(n);
    let mut rem: u16 = 0;
    for i in 0..n {
        let l = lo.get(i).copied().unwrap_or(0) as u16;
        let h = hi.get(i).copied().unwrap_or(0) as u16;
        let total = rem * 256 + l + h;
        out.push((total / 2) as u8);
        rem = total % 2;
    }
    while out.len() > 1 && *out.last().unwrap() == 0 {
        out.pop();
    }
    out
}

/// Produce a key ordered strictly between `lo` and `hi` (either may be `None`
/// for "before everything" / "after everything").
pub fn key_between(lo: Option<&[u8]>, hi: Option<&[u8]>) -> Vec<u8> {
    match (lo, hi) {
        (None, None) => vec![128],
        (Some(l), None) => {
            let mut v = l.to_vec();
            v.push(128);
            v
        }
        (None, Some(h)) => midpoint(&[], h),
        (Some(l), Some(h)) => midpoint(l, h),
    }
}
```

Add `pub mod order;` to `qdoc/src/lib.rs`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `distrobox enter my-distrobox -- bash -c 'cd <worktree> && cargo test -p qdoc order'`
Expected: all 6 `order::tests::*` PASS.

- [ ] **Step 5: Add a proptest for the ordering invariant**

Append to the `tests` module in `qdoc/src/order.rs`:

```rust
    use proptest::prelude::*;

    proptest! {
        // Insert at random gaps among a growing sorted key list; the list must
        // stay strictly ordered and every gap must remain subdividable.
        #[test]
        fn random_inserts_stay_strictly_ordered(ops in proptest::collection::vec(any::<u8>(), 0..120)) {
            let mut keys: Vec<Vec<u8>> = vec![key_between(None, None)];
            for byte in ops {
                let idx = (byte as usize) % (keys.len() + 1);
                let lo = if idx == 0 { None } else { Some(keys[idx - 1].as_slice()) };
                let hi = if idx == keys.len() { None } else { Some(keys[idx].as_slice()) };
                let k = key_between(lo, hi);
                if let Some(l) = lo { prop_assert_eq!(cmp_keys(l, &k), Ordering::Less); }
                if let Some(h) = hi { prop_assert_eq!(cmp_keys(&k, h), Ordering::Less); }
                keys.insert(idx, k);
            }
            for w in keys.windows(2) {
                prop_assert_eq!(cmp_keys(&w[0], &w[1]), Ordering::Less);
            }
        }
    }
```

- [ ] **Step 6: Run + commit**

Run: `distrobox enter my-distrobox -- bash -c 'cd <worktree> && cargo test -p qdoc order'`
Expected: PASS including `random_inserts_stay_strictly_ordered`.

```bash
git add qdoc/src/order.rs qdoc/src/lib.rs
git commit -m "feat(qdoc): fractional order keys with proptest invariant"
```

---

### Task 3: Document schema + `Document::new`

Defines the automerge layout and document handle.

**Files:**
- Create: `qdoc/src/document.rs`
- Create: `qdoc/src/schema.rs`
- Modify: `qdoc/src/lib.rs`

**Interfaces:**
- Produces:
  - `schema::{SCHEMA_ID, SCHEMA_VERSION, KEY_SCHEMA, KEY_SCHEMA_VERSION, KEY_BLOCKS, KEY_STYLES, KEY_PRESERVED, KEY_META, BLOCK_ORDER, BLOCK_KIND, BLOCK_TEXT, BLOCK_ATTRS, BLOCK_CHILDREN}` (all `&str`/const)
  - `document::Document` with `Document::new() -> Document`, `Document::actor_hex(&self) -> String`, `Document::save(&mut self) -> Vec<u8>`, `Document::load(&[u8]) -> Result<Document, DocError>`
  - `document::DocError` (thiserror)

- [ ] **Step 1: Write `qdoc/src/schema.rs`**

```rust
//! The canonical rich-text automerge layout (schema v1). Public ABI: codecs
//! and the snapshot depend on these field names.

pub const SCHEMA_ID: &str = "qos.richtext";
pub const SCHEMA_VERSION: i64 = 1;

// Root keys
pub const KEY_SCHEMA: &str = "schema";
pub const KEY_SCHEMA_VERSION: &str = "schema_version";
pub const KEY_BLOCKS: &str = "blocks";        // Map<block_id, block>
pub const KEY_STYLES: &str = "styles";        // Map<style_id, style def>
pub const KEY_PRESERVED: &str = "preserved";  // Map<part_name, Bytes>  (preservation layer)
pub const KEY_META: &str = "meta";            // Map (actor->user, etc.)

// Block fields
pub const BLOCK_ORDER: &str = "order";        // Bytes (fractional key)
pub const BLOCK_KIND: &str = "kind";          // Str ("paragraph"|"heading"|"list_item"|"table"|"cell"|"fidelity")
pub const BLOCK_TEXT: &str = "text";          // Text (present for text blocks)
pub const BLOCK_ATTRS: &str = "attrs";        // Map (style_id, level, ...)
pub const BLOCK_CHILDREN: &str = "children";  // Map<block_id, block> (containers)
```

- [ ] **Step 2: Write failing tests in `qdoc/src/document.rs`**

```rust
//! `Document`: a handle over an automerge `AutoCommit` holding the canonical
//! rich-text model.

use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ROOT, ScalarValue, Value};
use automerge::transaction::Transactable;
use crate::schema::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_doc_has_root_layout() {
        let mut d = Document::new();
        // schema scalars
        let (v, _) = d.doc.get(&ROOT, KEY_SCHEMA).unwrap().unwrap();
        assert_eq!(v.to_str(), Some(SCHEMA_ID));
        let (vv, _) = d.doc.get(&ROOT, KEY_SCHEMA_VERSION).unwrap().unwrap();
        assert_eq!(vv.to_i64(), Some(SCHEMA_VERSION));
        // container maps exist
        for k in [KEY_BLOCKS, KEY_STYLES, KEY_PRESERVED, KEY_META] {
            let got = d.doc.get(&ROOT, k).unwrap();
            assert!(matches!(got, Some((Value::Object(ObjType::Map), _))), "missing map {k}");
        }
    }

    #[test]
    fn save_load_roundtrips() {
        let mut d = Document::new();
        let bytes = d.save();
        let mut d2 = Document::load(&bytes).unwrap();
        let (v, _) = d2.doc.get(&ROOT, KEY_SCHEMA).unwrap().unwrap();
        assert_eq!(v.to_str(), Some(SCHEMA_ID));
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `distrobox enter my-distrobox -- bash -c 'cd <worktree> && cargo test -p qdoc document'`
Expected: FAIL — `Document` not found.

- [ ] **Step 4: Implement `Document`** (prepend above the `tests` module)

```rust
#[derive(thiserror::Error, Debug)]
pub enum DocError {
    #[error("automerge error: {0}")]
    Automerge(#[from] automerge::AutomergeError),
    #[error("load error: {0}")]
    Load(String),
    #[error("invalid document: {0}")]
    Invalid(String),
}

pub struct Document {
    pub(crate) doc: AutoCommit,
    pub(crate) blocks: ObjId,
    pub(crate) styles: ObjId,
    pub(crate) preserved: ObjId,
    pub(crate) meta: ObjId,
    next_local: u64,
}

impl Document {
    pub fn new() -> Self {
        let mut doc = AutoCommit::new();
        doc.put(&ROOT, KEY_SCHEMA, SCHEMA_ID).unwrap();
        doc.put(&ROOT, KEY_SCHEMA_VERSION, SCHEMA_VERSION).unwrap();
        let blocks = doc.put_object(&ROOT, KEY_BLOCKS, ObjType::Map).unwrap();
        let styles = doc.put_object(&ROOT, KEY_STYLES, ObjType::Map).unwrap();
        let preserved = doc.put_object(&ROOT, KEY_PRESERVED, ObjType::Map).unwrap();
        let meta = doc.put_object(&ROOT, KEY_META, ObjType::Map).unwrap();
        doc.commit();
        Self { doc, blocks, styles, preserved, meta, next_local: 0 }
    }

    pub fn load(bytes: &[u8]) -> Result<Self, DocError> {
        let doc = AutoCommit::load(bytes).map_err(|e| DocError::Load(e.to_string()))?;
        let blocks = obj_at(&doc, KEY_BLOCKS)?;
        let styles = obj_at(&doc, KEY_STYLES)?;
        let preserved = obj_at(&doc, KEY_PRESERVED)?;
        let meta = obj_at(&doc, KEY_META)?;
        Ok(Self { doc, blocks, styles, preserved, meta, next_local: 0 })
    }

    pub fn save(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    pub fn actor_hex(&self) -> String {
        self.doc.get_actor().to_hex_string()
    }

    /// Mint a globally-unique block id: `{actor_hex}.{counter}`.
    pub(crate) fn mint_id(&mut self) -> String {
        let id = format!("{}.{}", self.actor_hex(), self.next_local);
        self.next_local += 1;
        id
    }
}

fn obj_at(doc: &AutoCommit, key: &str) -> Result<ObjId, DocError> {
    match doc.get(&ROOT, key)? {
        Some((Value::Object(ObjType::Map), id)) => Ok(id),
        _ => Err(DocError::Invalid(format!("root.{key} is not a map"))),
    }
}
```

Add to `qdoc/src/lib.rs`:

```rust
pub mod schema;
pub mod document;
pub use document::{Document, DocError};
```

- [ ] **Step 5: Run + commit**

Run: `distrobox enter my-distrobox -- bash -c 'cd <worktree> && cargo test -p qdoc document'`
Expected: `new_doc_has_root_layout`, `save_load_roundtrips` PASS.

```bash
git add qdoc/src/schema.rs qdoc/src/document.rs qdoc/src/lib.rs
git commit -m "feat(qdoc): document schema layout + Document::new/load/save"
```

---

### Task 4: Block CRUD + ordering

**Files:**
- Modify: `qdoc/src/document.rs`

**Interfaces:**
- Consumes: `order::{key_between, cmp_keys}`, schema consts, `Document`.
- Produces (on `Document`):
  - `insert_block(&mut self, after: Option<&str>, kind: &str) -> String` (returns new block id; `after=None` → at start)
  - `remove_block(&mut self, id: &str)`
  - `move_block(&mut self, id: &str, after: Option<&str>)`
  - `blocks_in_order(&self) -> Vec<String>`
  - `block_kind(&self, id: &str) -> Option<String>`

- [ ] **Step 1: Write failing tests** (append to `tests` in `document.rs`)

```rust
    #[test]
    fn insert_blocks_are_ordered() {
        let mut d = Document::new();
        let a = d.insert_block(None, "paragraph");
        let b = d.insert_block(Some(&a), "paragraph");
        let c = d.insert_block(Some(&a), "paragraph"); // between a and b
        assert_eq!(d.blocks_in_order(), vec![a.clone(), c.clone(), b.clone()]);
        assert_eq!(d.block_kind(&c).as_deref(), Some("paragraph"));
    }

    #[test]
    fn remove_and_move_block() {
        let mut d = Document::new();
        let a = d.insert_block(None, "paragraph");
        let b = d.insert_block(Some(&a), "paragraph");
        let c = d.insert_block(Some(&b), "paragraph");
        d.remove_block(&b);
        assert_eq!(d.blocks_in_order(), vec![a.clone(), c.clone()]);
        d.move_block(&c, None); // to front
        assert_eq!(d.blocks_in_order(), vec![c, a]);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `distrobox enter my-distrobox -- bash -c 'cd <worktree> && cargo test -p qdoc document'`
Expected: FAIL — methods not found.

- [ ] **Step 3: Implement** (add to `impl Document`)

```rust
    pub fn blocks_in_order(&self) -> Vec<String> {
        let mut v: Vec<(Vec<u8>, String)> = self.doc.keys(&self.blocks)
            .filter_map(|id| self.block_order(&id).map(|ord| (ord, id)))
            .collect();
        v.sort_by(|a, b| crate::order::cmp_keys(&a.0, &b.0).then(a.1.cmp(&b.1)));
        v.into_iter().map(|(_, id)| id).collect()
    }

    fn block_obj(&self, id: &str) -> Option<ObjId> {
        match self.doc.get(&self.blocks, id) {
            Ok(Some((Value::Object(ObjType::Map), oid))) => Some(oid),
            _ => None,
        }
    }

    fn block_order(&self, id: &str) -> Option<Vec<u8>> {
        let obj = self.block_obj(id)?;
        match self.doc.get(&obj, BLOCK_ORDER) {
            Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                ScalarValue::Bytes(b) => Some(b.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn block_kind(&self, id: &str) -> Option<String> {
        let obj = self.block_obj(id)?;
        match self.doc.get(&obj, BLOCK_KIND) {
            Ok(Some((Value::Scalar(s), _))) => s.to_str().map(|s| s.to_string()),
            _ => None,
        }
    }

    /// Key that sorts immediately after `after` (or at the very front if None),
    /// and before whatever currently follows it.
    fn order_after(&self, after: Option<&str>) -> Vec<u8> {
        let ordered = self.blocks_in_order();
        let pos = match after {
            None => 0,
            Some(id) => ordered.iter().position(|x| x == id).map(|i| i + 1).unwrap_or(ordered.len()),
        };
        let lo = if pos == 0 { None } else { self.block_order(&ordered[pos - 1]) };
        let hi = if pos >= ordered.len() { None } else { self.block_order(&ordered[pos]) };
        crate::order::key_between(lo.as_deref(), hi.as_deref())
    }

    pub fn insert_block(&mut self, after: Option<&str>, kind: &str) -> String {
        let key = self.order_after(after);
        let id = self.mint_id();
        let obj = self.doc.put_object(&self.blocks, &id, ObjType::Map).unwrap();
        self.doc.put(&obj, BLOCK_ORDER, ScalarValue::Bytes(key)).unwrap();
        self.doc.put(&obj, BLOCK_KIND, kind).unwrap();
        self.doc.put_object(&obj, BLOCK_ATTRS, ObjType::Map).unwrap();
        self.doc.commit();
        id
    }

    pub fn remove_block(&mut self, id: &str) {
        let _ = self.doc.delete(&self.blocks, id);
        self.doc.commit();
    }

    pub fn move_block(&mut self, id: &str, after: Option<&str>) {
        let key = self.order_after(after);
        if let Some(obj) = self.block_obj(id) {
            self.doc.put(&obj, BLOCK_ORDER, ScalarValue::Bytes(key)).unwrap();
            self.doc.commit();
        }
    }
```

- [ ] **Step 4: Run + commit**

Run: `distrobox enter my-distrobox -- bash -c 'cd <worktree> && cargo test -p qdoc document'`
Expected: PASS.

```bash
git add qdoc/src/document.rs
git commit -m "feat(qdoc): block CRUD + fractional ordering"
```

---

### Task 5: Text within blocks

**Files:**
- Modify: `qdoc/src/document.rs`

**Interfaces:**
- Produces (on `Document`):
  - `splice_text(&mut self, block_id: &str, char_idx: usize, del: isize, ins: &str)`
  - `block_text(&self, block_id: &str) -> Option<String>`

- [ ] **Step 1: Write failing tests** (append to `tests`)

```rust
    #[test]
    fn text_splice_and_read() {
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "hello world");
        assert_eq!(d.block_text(&p).as_deref(), Some("hello world"));
        d.splice_text(&p, 5, 0, ",");
        assert_eq!(d.block_text(&p).as_deref(), Some("hello, world"));
        d.splice_text(&p, 0, 5, "HELLO");
        assert_eq!(d.block_text(&p).as_deref(), Some("HELLO, world"));
    }

    #[test]
    fn text_handles_unicode() {
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "héllo");
        assert_eq!(d.block_text(&p).as_deref(), Some("héllo"));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `distrobox enter my-distrobox -- bash -c 'cd <worktree> && cargo test -p qdoc document'`
Expected: FAIL — methods not found.

- [ ] **Step 3: Implement** (add to `impl Document`)

```rust
    fn text_obj(&mut self, block_id: &str) -> Option<ObjId> {
        let obj = self.block_obj(block_id)?;
        match self.doc.get(&obj, BLOCK_TEXT) {
            Ok(Some((Value::Object(ObjType::Text), tid))) => Some(tid),
            _ => {
                // lazily create the Text object on first edit
                self.doc.put_object(&obj, BLOCK_TEXT, ObjType::Text).ok()
            }
        }
    }

    pub fn splice_text(&mut self, block_id: &str, char_idx: usize, del: isize, ins: &str) {
        if let Some(tid) = self.text_obj(block_id) {
            let _ = self.doc.splice_text(&tid, char_idx, del, ins);
            self.doc.commit();
        }
    }

    pub fn block_text(&self, block_id: &str) -> Option<String> {
        let obj = self.block_obj(block_id)?;
        match self.doc.get(&obj, BLOCK_TEXT) {
            Ok(Some((Value::Object(ObjType::Text), tid))) => self.doc.text(&tid).ok(),
            _ => None,
        }
    }
```

- [ ] **Step 4: Run + commit**

Run: `distrobox enter my-distrobox -- bash -c 'cd <worktree> && cargo test -p qdoc document'`
Expected: PASS.

```bash
git add qdoc/src/document.rs
git commit -m "feat(qdoc): per-block automerge Text editing"
```

---

### Task 6: Inline marks (formatting)

Marks are stored via automerge's native mark API over the block `Text`. **At execution, confirm the exact `automerge::marks` API for 0.8** (the `mark`/`marks` method names and `Mark`/`ExpandMark` paths) — adjust the calls below to match the compiler. The behavior asserted by the tests is the contract.

**Files:**
- Modify: `qdoc/src/document.rs`

**Interfaces:**
- Produces (on `Document`):
  - `add_mark(&mut self, block_id: &str, start: usize, end: usize, name: &str, value: bool)`
  - `marks(&self, block_id: &str) -> Vec<MarkSpan>` where `pub struct MarkSpan { pub start: usize, pub end: usize, pub name: String }`

- [ ] **Step 1: Write failing tests** (append to `tests`)

```rust
    #[test]
    fn marks_apply_over_range() {
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "hello world");
        d.add_mark(&p, 0, 5, "strong", true);
        let m = d.marks(&p);
        assert!(m.iter().any(|s| s.name == "strong" && s.start == 0 && s.end == 5),
            "expected strong[0,5], got {m:?}");
    }

    #[test]
    fn mark_survives_later_text_insert() {
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "hello world");
        d.add_mark(&p, 6, 11, "em", true); // "world"
        d.splice_text(&p, 0, 0, "say ");    // shift right by 4
        let m = d.marks(&p);
        assert!(m.iter().any(|s| s.name == "em" && s.start == 10 && s.end == 15),
            "mark must track text shift, got {m:?}");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `distrobox enter my-distrobox -- bash -c 'cd <worktree> && cargo test -p qdoc document'`
Expected: FAIL — `add_mark`/`marks` not found.

- [ ] **Step 3: Implement** (add near the top of `document.rs`, and methods to `impl Document`)

```rust
use automerge::marks::{ExpandMark, Mark};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MarkSpan {
    pub start: usize,
    pub end: usize,
    pub name: String,
}
```

```rust
    pub fn add_mark(&mut self, block_id: &str, start: usize, end: usize, name: &str, value: bool) {
        if let Some(tid) = self.text_obj(block_id) {
            let mark = Mark::new(name.to_string(), value, start, end);
            let _ = self.doc.mark(&tid, mark, ExpandMark::After);
            self.doc.commit();
        }
    }

    pub fn marks(&self, block_id: &str) -> Vec<MarkSpan> {
        let Some(obj) = self.block_obj(block_id) else { return vec![] };
        let tid = match self.doc.get(&obj, BLOCK_TEXT) {
            Ok(Some((Value::Object(ObjType::Text), tid))) => tid,
            _ => return vec![],
        };
        self.doc.marks(&tid).map(|ms| {
            ms.into_iter().map(|m| MarkSpan { start: m.start, end: m.end, name: m.name().to_string() }).collect()
        }).unwrap_or_default()
    }
```

> Execution note: if `Mark::new`/`m.name()`/`m.start` differ in 0.8, fix to the actual API (e.g. `m.name` field vs `m.name()` method). The tests define the required behavior; do not weaken them.

- [ ] **Step 4: Run + commit**

Run: `distrobox enter my-distrobox -- bash -c 'cd <worktree> && cargo test -p qdoc document'`
Expected: PASS.

```bash
git add qdoc/src/document.rs
git commit -m "feat(qdoc): inline marks over block text"
```

---

### Task 7: Materialize → `DocSnapshot`

A read-only, serializable, plain-Rust view used by renderers and exporters.

**Files:**
- Create: `qdoc/src/snapshot.rs`
- Modify: `qdoc/src/document.rs` (add `materialize`), `qdoc/src/lib.rs`

**Interfaces:**
- Produces:
  - `snapshot::{DocSnapshot, BlockView}` (serde-serializable)
  - `Document::materialize(&self) -> DocSnapshot`

```rust
// snapshot.rs shapes:
pub struct DocSnapshot { pub schema: String, pub schema_version: i64, pub blocks: Vec<BlockView> }
pub struct BlockView { pub id: String, pub kind: String, pub text: String, pub marks: Vec<crate::document::MarkSpan>, pub children: Vec<BlockView> }
```

- [ ] **Step 1: Write `qdoc/src/snapshot.rs`**

```rust
//! Read-only materialized view of a `Document` for rendering/export.

use serde::{Serialize, Deserialize};
use crate::document::MarkSpan;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocSnapshot {
    pub schema: String,
    pub schema_version: i64,
    pub blocks: Vec<BlockView>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockView {
    pub id: String,
    pub kind: String,
    pub text: String,
    pub marks: Vec<MarkSpan>,
    pub children: Vec<BlockView>,
}
```

- [ ] **Step 2: Write failing test** (append to `tests` in `document.rs`)

```rust
    #[test]
    fn materialize_reflects_model() {
        let mut d = Document::new();
        let a = d.insert_block(None, "heading");
        d.splice_text(&a, 0, 0, "Title");
        let b = d.insert_block(Some(&a), "paragraph");
        d.splice_text(&b, 0, 0, "Body");
        d.add_mark(&b, 0, 4, "strong", true);

        let snap = d.materialize();
        assert_eq!(snap.schema, SCHEMA_ID);
        assert_eq!(snap.blocks.len(), 2);
        assert_eq!(snap.blocks[0].kind, "heading");
        assert_eq!(snap.blocks[0].text, "Title");
        assert_eq!(snap.blocks[1].text, "Body");
        assert!(snap.blocks[1].marks.iter().any(|m| m.name == "strong"));
    }
```

- [ ] **Step 3: Run to verify failure**

Run: `distrobox enter my-distrobox -- bash -c 'cd <worktree> && cargo test -p qdoc document'`
Expected: FAIL — `materialize` not found.

- [ ] **Step 4: Implement** (add `materialize` to `impl Document`)

```rust
    pub fn materialize(&self) -> crate::snapshot::DocSnapshot {
        let blocks = self.blocks_in_order().into_iter()
            .map(|id| self.materialize_block(&id, &self.blocks))
            .collect();
        crate::snapshot::DocSnapshot {
            schema: SCHEMA_ID.to_string(),
            schema_version: SCHEMA_VERSION,
            blocks,
        }
    }

    fn materialize_block(&self, id: &str, parent: &ObjId) -> crate::snapshot::BlockView {
        let obj = match self.doc.get(parent, id) {
            Ok(Some((Value::Object(ObjType::Map), oid))) => oid,
            _ => return crate::snapshot::BlockView { id: id.into(), kind: String::new(), text: String::new(), marks: vec![], children: vec![] },
        };
        let kind = match self.doc.get(&obj, BLOCK_KIND) {
            Ok(Some((Value::Scalar(s), _))) => s.to_str().unwrap_or("").to_string(),
            _ => String::new(),
        };
        let text = match self.doc.get(&obj, BLOCK_TEXT) {
            Ok(Some((Value::Object(ObjType::Text), tid))) => self.doc.text(&tid).unwrap_or_default(),
            _ => String::new(),
        };
        let marks = self.marks(id);
        // children container, ordered like top-level blocks
        let children = match self.doc.get(&obj, BLOCK_CHILDREN) {
            Ok(Some((Value::Object(ObjType::Map), cid))) => self.children_in_order(&cid)
                .into_iter().map(|c| self.materialize_block(&c, &cid)).collect(),
            _ => vec![],
        };
        crate::snapshot::BlockView { id: id.into(), kind, text, marks, children }
    }

    fn children_in_order(&self, container: &ObjId) -> Vec<String> {
        let mut v: Vec<(Vec<u8>, String)> = self.doc.keys(container).filter_map(|id| {
            let cobj = match self.doc.get(container, &id) { Ok(Some((Value::Object(ObjType::Map), o), )) => o, _ => return None };
            let ord = match self.doc.get(&cobj, BLOCK_ORDER) {
                Ok(Some((Value::Scalar(s), _))) => if let ScalarValue::Bytes(b) = s.as_ref() { b.clone() } else { return None },
                _ => return None,
            };
            Some((ord, id))
        }).collect();
        v.sort_by(|a, b| crate::order::cmp_keys(&a.0, &b.0).then(a.1.cmp(&b.1)));
        v.into_iter().map(|(_, id)| id).collect()
    }
```

> Note: `marks(id)` only reads top-level blocks in Task 6; nested-block marks materialization is fine because `materialize_block` calls `self.marks(id)` which resolves the block's own Text via `block_obj` — extend `block_obj` to search children in a later phase if needed. For Phase 0, marks on nested blocks are out of scope (tables are structural only).

Add to `qdoc/src/lib.rs`:

```rust
pub mod snapshot;
pub use snapshot::{DocSnapshot, BlockView};
```

- [ ] **Step 5: Run + commit**

Run: `distrobox enter my-distrobox -- bash -c 'cd <worktree> && cargo test -p qdoc document'`
Expected: PASS.

```bash
git add qdoc/src/snapshot.rs qdoc/src/document.rs qdoc/src/lib.rs
git commit -m "feat(qdoc): materialize Document to a serializable DocSnapshot"
```

---

### Task 8: `FormatProvider` trait + registry

**Files:**
- Create: `qdoc/src/format.rs`
- Modify: `qdoc/src/lib.rs`

**Interfaces:**
- Produces:
  - `format::{Confidence, CodecError, FormatProvider, ProviderTier, Registry, FormatChange}`
  - `FormatChange = Vec<u8>` (an automerge incremental change blob)
  - `Registry::register(&mut self, ext: &str, tier: ProviderTier, provider: Box<dyn FormatProvider>)`
  - `Registry::provider_for_ext(&self, ext: &str) -> Option<&dyn FormatProvider>` (highest tier wins)

- [ ] **Step 1: Write failing tests** in `qdoc/src/format.rs`

```rust
//! Layer-1 generic format-provider contract + registry. Providers are pure
//! transforms between file bytes and canonical-document changes; in production
//! they run sandboxed (Phase 3). Here we define the contract + registry.

use crate::document::Document;

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyMd;
    impl FormatProvider for DummyMd {
        fn schema_id(&self) -> &str { crate::schema::SCHEMA_ID }
        fn detect(&self, head: &[u8]) -> Confidence {
            if head.starts_with(b"# ") { Confidence::High } else { Confidence::Low }
        }
        fn import(&self, bytes: &[u8], doc: &mut Document) -> Result<(), CodecError> {
            let s = std::str::from_utf8(bytes).map_err(|e| CodecError::Parse(e.to_string()))?;
            let p = doc.insert_block(None, "paragraph");
            doc.splice_text(&p, 0, 0, s.trim());
            Ok(())
        }
        fn export(&self, doc: &Document) -> Result<Vec<u8>, CodecError> {
            let snap = doc.materialize();
            Ok(snap.blocks.iter().map(|b| b.text.clone()).collect::<Vec<_>>().join("\n").into_bytes())
        }
    }

    struct BuiltinMd; // same as DummyMd but registered at a higher tier
    impl FormatProvider for BuiltinMd {
        fn schema_id(&self) -> &str { crate::schema::SCHEMA_ID }
        fn detect(&self, _: &[u8]) -> Confidence { Confidence::High }
        fn import(&self, _: &[u8], _: &mut Document) -> Result<(), CodecError> { Ok(()) }
        fn export(&self, _: &Document) -> Result<Vec<u8>, CodecError> { Ok(b"builtin".to_vec()) }
    }

    #[test]
    fn import_export_roundtrip_via_provider() {
        let p = DummyMd;
        let mut d = Document::new();
        p.import(b"# Hello", &mut d).unwrap();
        let out = p.export(&d).unwrap();
        assert_eq!(out, b"# Hello");
    }

    #[test]
    fn registry_resolves_highest_tier() {
        let mut r = Registry::new();
        r.register("md", ProviderTier::Ephemeral, Box::new(DummyMd));
        r.register("md", ProviderTier::Builtin, Box::new(BuiltinMd));
        let prov = r.provider_for_ext("md").unwrap();
        // Builtin wins → its export returns b"builtin".
        let d = Document::new();
        assert_eq!(prov.export(&d).unwrap(), b"builtin");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `distrobox enter my-distrobox -- bash -c 'cd <worktree> && cargo test -p qdoc format'`
Expected: FAIL — types not found.

- [ ] **Step 3: Implement** (prepend above `tests`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence { None, Low, Medium, High }

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProviderTier { Ephemeral = 0, Installed = 1, Builtin = 2 }

#[derive(thiserror::Error, Debug)]
pub enum CodecError {
    #[error("parse error: {0}")]
    Parse(String),
    #[error("export error: {0}")]
    Export(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
}

/// A format provider: a pure transform between file bytes and a canonical
/// `Document`. In production this trait object lives behind a sandbox IPC
/// boundary (Phase 3); the contract is identical either side.
pub trait FormatProvider: Send + Sync {
    fn schema_id(&self) -> &str;
    fn detect(&self, head: &[u8]) -> Confidence;
    fn import(&self, bytes: &[u8], doc: &mut Document) -> Result<(), CodecError>;
    fn export(&self, doc: &Document) -> Result<Vec<u8>, CodecError>;
}

struct Entry { tier: ProviderTier, provider: Box<dyn FormatProvider> }

#[derive(Default)]
pub struct Registry { by_ext: std::collections::HashMap<String, Vec<Entry>> }

impl Registry {
    pub fn new() -> Self { Self::default() }

    pub fn register(&mut self, ext: &str, tier: ProviderTier, provider: Box<dyn FormatProvider>) {
        self.by_ext.entry(ext.to_lowercase()).or_default().push(Entry { tier, provider });
    }

    /// Highest-tier provider registered for `ext` (Builtin > Installed > Ephemeral).
    pub fn provider_for_ext(&self, ext: &str) -> Option<&dyn FormatProvider> {
        self.by_ext.get(&ext.to_lowercase())?
            .iter().max_by_key(|e| e.tier)
            .map(|e| e.provider.as_ref())
    }
}
```

Add to `qdoc/src/lib.rs`:

```rust
pub mod format;
pub use format::{FormatProvider, Registry, ProviderTier, Confidence, CodecError};
```

- [ ] **Step 4: Run + commit**

Run: `distrobox enter my-distrobox -- bash -c 'cd <worktree> && cargo test -p qdoc format'`
Expected: PASS.

```bash
git add qdoc/src/format.rs qdoc/src/lib.rs
git commit -m "feat(qdoc): FormatProvider trait + tiered registry"
```

---

### Task 9: Property-based merge convergence (the CRDT guarantee)

Two replicas that apply the same set of edits in any interleaving must converge to the same materialized document. This is the core correctness property and the heart of "lots of testing".

**Files:**
- Create: `qdoc/tests/convergence.rs`
- Modify: `qdoc/src/document.rs` (add `merge` + `apply_changes`/`changes_since` helpers)

**Interfaces:**
- Produces (on `Document`):
  - `merge(&mut self, other: &mut Document)` — merge another replica's changes in
  - `fork(&mut self) -> Document` — independent replica with a distinct actor (via save/load + new actor)

- [ ] **Step 1: Add `merge`/`fork`** to `impl Document`, with a unit test (append to `tests` in `document.rs`)

```rust
    #[test]
    fn merge_combines_disjoint_edits() {
        let mut a = Document::new();
        let p = a.insert_block(None, "paragraph");
        a.splice_text(&p, 0, 0, "base");
        let mut b = a.fork();
        a.splice_text(&p, 4, 0, "-A");
        let q = b.insert_block(Some(&p), "paragraph");
        b.splice_text(&q, 0, 0, "B-block");
        a.merge(&mut b);
        b.merge(&mut a);
        assert_eq!(a.materialize(), b.materialize(), "replicas must converge");
        assert_eq!(a.blocks_in_order().len(), 2);
    }
```

Implementation (add to `impl Document`):

```rust
    /// Independent replica with a fresh actor id (distinct edit lineage).
    pub fn fork(&mut self) -> Document {
        let bytes = self.save();
        let mut d = Document::load(&bytes).expect("fork load");
        d.doc.set_actor(automerge::ActorId::random());
        d
    }

    /// Merge another replica's changes into this one.
    pub fn merge(&mut self, other: &mut Document) {
        let _ = self.doc.merge(&mut other.doc);
        self.doc.commit();
    }
```

> Execution note: confirm `AutoCommit::set_actor` / `merge` signatures in 0.8 and adjust. `materialize()` equality is the contract.

- [ ] **Step 2: Run the unit test**

Run: `distrobox enter my-distrobox -- bash -c 'cd <worktree> && cargo test -p qdoc merge_combines'`
Expected: PASS.

- [ ] **Step 3: Write the proptest harness** `qdoc/tests/convergence.rs`

```rust
//! Property: applying the same edit operations on two forked replicas in any
//! order, then cross-merging, yields identical materialized documents.

use proptest::prelude::*;
use qdoc::Document;

#[derive(Debug, Clone)]
enum Op { InsertPara(u8), Type(u8, String), Bold(u8) }

fn ops_strategy() -> impl Strategy<Value = Vec<Op>> {
    let op = prop_oneof![
        any::<u8>().prop_map(Op::InsertPara),
        (any::<u8>(), "[a-z ]{0,6}").prop_map(|(i, s)| Op::Type(i, s)),
        any::<u8>().prop_map(Op::Bold),
    ];
    proptest::collection::vec(op, 0..40)
}

fn apply(doc: &mut Document, ops: &[Op]) {
    for op in ops {
        let blocks = doc.blocks_in_order();
        match op {
            Op::InsertPara(i) => {
                let after = if blocks.is_empty() { None } else { Some(blocks[(*i as usize) % blocks.len()].clone()) };
                doc.insert_block(after.as_deref(), "paragraph");
            }
            Op::Type(i, s) => {
                if !blocks.is_empty() {
                    let b = &blocks[(*i as usize) % blocks.len()];
                    let len = doc.block_text(b).map(|t| t.chars().count()).unwrap_or(0);
                    doc.splice_text(b, len, 0, s);
                }
            }
            Op::Bold(i) => {
                if !blocks.is_empty() {
                    let b = &blocks[(*i as usize) % blocks.len()];
                    let len = doc.block_text(b).map(|t| t.chars().count()).unwrap_or(0);
                    if len > 0 { doc.add_mark(b, 0, len, "strong", true); }
                }
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 200, ..ProptestConfig::default() })]

    #[test]
    fn forked_replicas_converge(a_ops in ops_strategy(), b_ops in ops_strategy()) {
        let mut a = Document::new();
        // shared base so both replicas have a common ancestor
        let base = a.insert_block(None, "paragraph");
        a.splice_text(&base, 0, 0, "base");
        let mut b = a.fork();

        apply(&mut a, &a_ops);
        apply(&mut b, &b_ops);

        // cross-merge both directions → both must reach the same state
        a.merge(&mut b);
        b.merge(&mut a);
        a.merge(&mut b); // settle (merge is monotonic; second pass is a no-op if converged)

        prop_assert_eq!(a.materialize(), b.materialize());
    }
}
```

- [ ] **Step 4: Run the proptest**

Run: `distrobox enter my-distrobox -- bash -c 'cd <worktree> && cargo test -p qdoc --test convergence'`
Expected: PASS (200 cases). If a case fails, proptest shrinks it — fix the model bug it reveals; **do not weaken the assertion**.

- [ ] **Step 5: Full crate test + commit**

Run: `distrobox enter my-distrobox -- bash -c 'cd <worktree> && cargo test -p qdoc'`
Expected: all tests PASS.

```bash
git add qdoc/src/document.rs qdoc/tests/convergence.rs
git commit -m "test(qdoc): property-based merge-convergence harness"
```

---

## Self-Review

**Spec coverage (Phase 0 portion of the design spec):**
- Layer-2 schema (blocks/text/marks/styles/preserved) → Tasks 3–7. ✓ (styles/preserved maps created in Task 3; populated by codecs in later phases.)
- Fractional-key structural ordering → Tasks 2, 4. ✓
- Marks-over-text (not run tree) → Task 6. ✓
- `FormatProvider` trait + tiered registry → Task 8. ✓
- Merge convergence (real-time multi-user core) → Task 9. ✓
- Materialize-for-render/export → Task 7. ✓
- *Deferred to later phases (correctly not here):* FS storage/byte-cache/write-path (Phase 2), sandbox IPC (Phase 3), md/odt codecs (Phases 1/4), history change-blob chain (Phase 2), preservation-layer population (Phase 4). The `preserved` map exists as schema now.

**Placeholder scan:** No "TBD"/"handle later"/"add error handling" steps — every code step is complete. Two explicit *execution notes* (Task 6 marks API, Task 9 actor/merge API) flag the two automerge-0.8 signatures to confirm against the compiler; the asserted behavior is fixed.

**Type consistency:** `Document` methods (`insert_block`/`remove_block`/`move_block`/`blocks_in_order`/`splice_text`/`block_text`/`add_mark`/`marks`/`materialize`/`fork`/`merge`) are referenced consistently across tasks and the convergence harness. `MarkSpan`/`DocSnapshot`/`BlockView` field names match between `snapshot.rs`, Task 6, and Task 7. Registry API (`register`/`provider_for_ext`/`ProviderTier`) matches Task 8 usage.

## Execution Handoff

Phase 0 is self-contained, GUI-free, and fully unit/property tested. Recommended execution: **inline** (the tasks are sequential and share an evolving automerge API surface that two execution notes call out — a single context resolves those once rather than re-discovering per subagent). After Phase 0 is green, the model is proven and Phase 1 (Markdown POC end-to-end) gets its own plan.
