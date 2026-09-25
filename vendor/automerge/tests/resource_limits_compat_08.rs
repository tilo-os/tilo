//! qOS automerge 0.8 compatibility corpus on automerge 0.12.
//!
//! `tests/fixtures/qos_08/` was produced by the qOS-patched automerge 0.8.0
//! (Cloud alignment branch vendor) with a small standalone generator; the
//! generator's canonical dump code is copied verbatim below so the expected
//! values in `expected.json` are compared structurally, not by Debug text.
//! Covered: maps, lists, text (incl. unicode), marks (all expand modes,
//! unmark, concurrent marks), counters with increments, populated Table
//! objects written through map keys, multiple actors, conflicts, deletions,
//! compressed and uncompressed saves, change chunks (plain and compressed),
//! orphan change chunks, and a document saved with a queued orphan.
use automerge::resource_limits::{
    NativeAllocationSet, NativeAllowance, NativeDependencyPolicy, NativeIncomingOutcome,
    NativeLimits,
};
use automerge::{
    AutoCommit, Change, ObjId, ObjType, ROOT, ReadDoc, SaveOptions, ScalarValue, Value, hydrate,
};
use serde_json::{Map as JMap, Value as J, json};
use std::path::PathBuf;

// ---- canonical dump (verbatim from the 0.8 generator) ----------------------
fn scalar_json(value: &ScalarValue) -> J {
    match value {
        ScalarValue::Bytes(b) => json!({ "bytes": hex::encode(b) }),
        ScalarValue::Str(s) => json!({ "str": s.as_str() }),
        ScalarValue::Int(i) => json!({ "int": i }),
        ScalarValue::Uint(u) => json!({ "uint": u }),
        ScalarValue::F64(f) => json!({ "f64": f }),
        ScalarValue::Counter(c) => json!({ "counter": i64::from(c) }),
        ScalarValue::Timestamp(t) => json!({ "ts": t }),
        ScalarValue::Boolean(b) => json!({ "bool": b }),
        ScalarValue::Unknown { type_code, bytes } => {
            json!({ "unknown": { "type_code": type_code, "bytes": hex::encode(bytes) } })
        }
        ScalarValue::Null => json!({ "null": true }),
    }
}

fn entry_json(doc: &AutoCommit, value: Value<'_>, id: ObjId) -> J {
    let v = match value {
        Value::Object(kind) => walk(doc, &id, kind),
        Value::Scalar(s) => scalar_json(&s),
    };
    json!({ "id": id.to_string(), "v": v })
}

fn walk(doc: &AutoCommit, obj: &ObjId, kind: ObjType) -> J {
    match kind {
        ObjType::Map | ObjType::Table => {
            let mut props = JMap::new();
            for key in doc.keys(obj) {
                let all = doc.get_all(obj, key.as_str()).unwrap();
                let entries: Vec<J> = all
                    .into_iter()
                    .map(|(v, id)| entry_json(doc, v, id))
                    .collect();
                props.insert(key, J::Array(entries));
            }
            let t = if kind == ObjType::Map { "map" } else { "table" };
            json!({ "type": t, "props": props })
        }
        ObjType::List => {
            let len = doc.length(obj);
            let items: Vec<J> = (0..len)
                .map(|i| {
                    let all = doc.get_all(obj, i).unwrap();
                    J::Array(
                        all.into_iter()
                            .map(|(v, id)| entry_json(doc, v, id))
                            .collect(),
                    )
                })
                .collect();
            json!({ "type": "list", "items": items })
        }
        ObjType::Text => {
            let marks: Vec<J> = doc
                .marks(obj)
                .unwrap()
                .into_iter()
                .map(|m| json!({ "name": m.name.as_str(), "value": scalar_json(&m.value), "start": m.start, "end": m.end }))
                .collect();
            json!({ "type": "text", "text": doc.text(obj).unwrap(), "len": doc.length(obj), "marks": marks })
        }
    }
}

fn hydrate_json(value: &hydrate::Value) -> J {
    match value {
        hydrate::Value::Scalar(s) => scalar_json(s),
        hydrate::Value::Map(m) => {
            let mut entries: Vec<(&String, &hydrate::MapValue)> = m.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            let mut out = JMap::new();
            for (k, v) in entries {
                out.insert(
                    k.clone(),
                    json!({ "v": hydrate_json(&v.value), "conflict": v.conflict }),
                );
            }
            json!({ "map": out })
        }
        hydrate::Value::List(l) => {
            let items: Vec<J> = l
                .iter()
                .map(|item| {
                    let mut marks: Vec<(&String, &ScalarValue)> = item.marks.iter().collect();
                    marks.sort_by(|a, b| a.0.cmp(b.0));
                    let marks: JMap<String, J> = marks.into_iter().map(|(k, v)| (k.clone(), scalar_json(v))).collect();
                    json!({ "v": hydrate_json(&item.value), "conflict": item.conflict, "marks": marks })
                })
                .collect();
            json!({ "list": items })
        }
        hydrate::Value::Text(t) => json!({ "text": t.to_string() }),
    }
}

fn dump(doc: &AutoCommit) -> J {
    walk(doc, &ROOT, ObjType::Map)
}
fn dump_hydrated(doc: &AutoCommit) -> J {
    hydrate_json(&doc.hydrate(ROOT, None).unwrap())
}
// ---------------------------------------------------------------------------

fn hashes(mut h: Vec<automerge::ChangeHash>) -> Vec<String> {
    h.sort();
    h.iter().map(|h| h.to_string()).collect()
}

fn change_meta(changes: &[Change]) -> Vec<J> {
    let mut v: Vec<J> = changes
        .iter()
        .map(|c| {
            json!({
                "hash": c.hash().to_string(),
                "actor": c.actor_id().to_hex_string(),
                "seq": c.seq(),
                "deps": hashes(c.deps().to_vec()),
                "message": c.message(),
            })
        })
        .collect();
    v.sort_by(|a, b| a["hash"].as_str().cmp(&b["hash"].as_str()));
    v
}

fn dir() -> PathBuf {
    // Built either as the vendored crate's own test or from the workspace
    // wrapper in crates/doc/tilo-automerge/tests/am012_compat_08.rs.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let own = root.join("tests/fixtures/qos_08");
    if own.is_dir() {
        return own;
    }
    root.join("../../../vendor/automerge/tests/fixtures/qos_08")
}
fn read(name: &str) -> Vec<u8> {
    std::fs::read(dir().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}
fn manifest() -> Vec<J> {
    let text = std::fs::read_to_string(dir().join("expected.json")).unwrap();
    let parsed: J = serde_json::from_str(&text).unwrap();
    parsed["fixtures"].as_array().unwrap().clone()
}
fn fixtures(kind: &str) -> Vec<J> {
    manifest()
        .into_iter()
        .filter(|f| f["kind"] == kind)
        .collect()
}
fn s(v: &J) -> &str {
    v.as_str().unwrap()
}

/// Compare the full observable state of `doc` against a manifest entry.
fn check_state(ctx: &str, doc: &mut AutoCommit, expected: &J, changes_key: &str) {
    assert_eq!(
        json!(hashes(doc.get_heads())),
        expected["heads"],
        "{ctx}: heads"
    );
    assert_eq!(
        json!(hashes(doc.get_missing_deps(&[]))),
        expected["missing_deps"],
        "{ctx}: missing deps"
    );
    assert_eq!(dump(doc), expected["dump"], "{ctx}: canonical dump");
    assert_eq!(dump_hydrated(doc), expected["hydrate"], "{ctx}: hydrate");
    assert_eq!(
        json!(change_meta(&doc.get_changes(&[]))),
        expected[changes_key],
        "{ctx}: changes"
    );
}

/// Runs `check` for every fixture, isolating panics so one incompatible
/// fixture does not hide the others; fails with the complete list.
fn each(fixtures: Vec<J>, mut check: impl FnMut(&J)) {
    let mut failures = Vec::new();
    for f in &fixtures {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(f)));
        if let Err(panic) = result {
            let msg = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            let short: String = msg.chars().take(300).collect();
            failures.push(format!("{}: {short}", f["name"]));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} fixtures failed:\n{}",
        failures.len(),
        fixtures.len(),
        failures.join("\n")
    );
}

fn documents() -> Vec<J> {
    fixtures("document")
        .into_iter()
        .chain(fixtures("concat"))
        .collect()
}

fn allowance() -> NativeAllowance {
    NativeAllowance::new(64 * 1024 * 1024).unwrap()
}

#[test]
fn corpus_is_present_and_small() {
    let all = manifest();
    assert!(all.len() >= 10, "corpus lost fixtures");
    let mut total = 0u64;
    for entry in std::fs::read_dir(dir()).unwrap() {
        total += entry.unwrap().metadata().unwrap().len();
    }
    assert!(total < 200 * 1024, "qos_08 corpus grew to {total} bytes");
    // Every intended feature is represented.
    let text = std::fs::read_to_string(dir().join("expected.json")).unwrap();
    for needle in [
        "\"table\"",
        "\"counter\"",
        "\"marks\"",
        "\"text\"",
        "\"list\"",
    ] {
        assert!(text.contains(needle), "corpus lacks {needle}");
    }
}

// ---- plain 0.12 API --------------------------------------------------------

fn document_files(f: &J) -> Vec<String> {
    let mut files = vec![s(&f["file"]).to_string()];
    if let Some(p) = f["nocompress_file"].as_str() {
        files.push(p.to_string());
    }
    files
}

#[test]
fn plain_load_of_08_documents_matches() {
    each(documents(), |f| {
        for file in document_files(f) {
            let mut doc = AutoCommit::load(&read(&file)).unwrap_or_else(|e| panic!("{file}: {e}"));
            check_state(&file, &mut doc, f, "changes");
        }
    });
}

#[test]
fn plain_apply_of_08_change_chunks_matches() {
    each(fixtures("changes"), |f| {
        let name = s(&f["name"]);
        // Concatenated chunks through load_incremental.
        let mut doc = AutoCommit::new();
        doc.load_incremental(&read(s(&f["all_file"]))).unwrap();
        check_state(&format!("{name} (concat)"), &mut doc, f, "applied_changes");
        // One chunk at a time through Change::from_bytes + apply_changes; the
        // change hash must survive the 0.12 parser (also for compressed chunks).
        let mut doc = AutoCommit::new();
        for file in f["files"].as_array().unwrap() {
            let change = Change::from_bytes(read(s(&file["file"]))).unwrap();
            assert_eq!(
                change.hash().to_string(),
                s(&file["hash"]),
                "{name}: change hash"
            );
            doc.apply_changes([change]).unwrap();
        }
        check_state(
            &format!("{name} (per chunk)"),
            &mut doc,
            f,
            "applied_changes",
        );
    });
}

/// Re-saving on 0.12 must preserve the change set and state, and the bytes:
/// - uncompressed saves are byte-identical to 0.8's;
/// - a compressed save in which 0.8 deflated nothing is byte-identical;
/// - where 0.8 deflated columns, the DEFLATE streams may differ (0.12 uses
///   flate2/zlib-rs), but decompressing both (an uncompressed re-save of the
///   0.8 file on 0.12) gives exactly 0.8's uncompressed save.
#[test]
fn resave_on_012_round_trips() {
    each(fixtures("document"), |f| {
        let orphans = f["retain_orphans"].as_bool().unwrap();
        let compressed_file = s(&f["file"]);
        let plain_file = s(&f["nocompress_file"]);
        let plain_08 = read(plain_file);
        for (file, deflate) in [(compressed_file, true), (plain_file, false)] {
            let original = read(file);
            let mut doc = AutoCommit::load(&original).unwrap();
            let saved = doc.save_with_options(SaveOptions {
                deflate,
                retain_orphans: orphans,
            });
            let equal = saved == original;
            println!(
                "{file}: bytes_equal={equal} ({} -> {})",
                original.len(),
                saved.len()
            );
            let mut back = AutoCommit::load(&saved).unwrap();
            check_state(&format!("{file} resaved"), &mut back, f, "changes");
            if !deflate || !f["compressed_differs"].as_bool().unwrap() {
                assert!(equal, "{file}: 0.12 re-save is not byte-identical to 0.8");
            }
            let normalized = doc.save_with_options(SaveOptions {
                deflate: false,
                retain_orphans: orphans,
            });
            assert!(
                normalized == plain_08,
                "{file}: 0.12 uncompressed re-save differs from 0.8's"
            );
        }
    });
}

// ---- checked path ----------------------------------------------------------

/// normalize_input -> reconstruct, then observe through the bounded API.
#[test]
fn checked_reconstruct_of_08_documents_matches() {
    each(documents(), |f| {
        for file in document_files(f) {
            let input = read(&file);
            let credit = allowance();
            let mut registry = NativeAllocationSet::new(credit.clone()).unwrap();
            let turn = registry.work_turn(credit.clone()).unwrap();
            let normalized = turn
                .normalize_input(&input, &NativeLimits::default())
                .unwrap();
            let owned = normalized
                .reconstruct(&mut registry, &turn)
                .unwrap_or_else(|e| panic!("{file}: {e}"));
            let heads = owned.base_heads_bounded(&turn).unwrap();
            assert_eq!(
                json!(hashes(heads.as_slice().to_vec())),
                f["heads"],
                "{file}: bounded heads"
            );
            assert_eq!(
                owned.has_missing_dependencies(),
                !f["missing_deps"].as_array().unwrap().is_empty(),
                "{file}: missing deps"
            );
            let orphans = f["retain_orphans"].as_bool().unwrap_or(false);
            let saved = owned
                .save_bounded(
                    &turn,
                    SaveOptions {
                        deflate: true,
                        retain_orphans: orphans,
                    },
                )
                .unwrap();
            let mut back = AutoCommit::load(saved.bytes()).unwrap();
            check_state(&format!("{file} checked"), &mut back, f, "changes");
            let history = owned.changes_since_bounded(&[], &turn).unwrap();
            let mut got: Vec<String> = (0..history.len())
                .map(|i| history.hash(i).unwrap().to_string())
                .collect();
            got.sort();
            let want: Vec<String> = f["changes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| s(&c["hash"]).to_string())
                .collect();
            assert_eq!(got, want, "{file}: bounded history");
        }
    });
}

/// apply_incoming_checked of 0.8 change chunks into an empty 0.12 document.
#[test]
fn checked_incoming_of_08_change_chunks_matches() {
    each(fixtures("changes"), |f| {
        let name = s(&f["name"]);
        let orphan = !f["missing_deps"].as_array().unwrap().is_empty();
        let all = read(s(&f["all_file"]));
        let limits = NativeLimits::default();

        let mut doc = AutoCommit::new();
        let before = doc.save();
        let outcome = doc
            .apply_incoming_checked(
                &all,
                &limits,
                &allowance(),
                NativeDependencyPolicy::RequireComplete,
            )
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        if orphan {
            assert_eq!(
                outcome,
                NativeIncomingOutcome::MissingDependencies,
                "{name}"
            );
            assert_eq!(
                doc.save(),
                before,
                "{name}: RequireComplete must not mutate"
            );
        } else {
            assert_eq!(
                outcome,
                NativeIncomingOutcome::Applied { changed: true },
                "{name}"
            );
            check_state(
                &format!("{name} RequireComplete"),
                &mut doc,
                f,
                "applied_changes",
            );
        }

        let mut doc = AutoCommit::new();
        let outcome = doc
            .apply_incoming_checked(
                &all,
                &limits,
                &allowance(),
                NativeDependencyPolicy::RetainOrphans,
            )
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            outcome,
            NativeIncomingOutcome::Applied { changed: !orphan },
            "{name} RetainOrphans"
        );
        check_state(
            &format!("{name} RetainOrphans"),
            &mut doc,
            f,
            "applied_changes",
        );

        // Chunk by chunk, in stored order.
        let mut doc = AutoCommit::new();
        for file in f["files"].as_array().unwrap() {
            doc.apply_incoming_checked(
                &read(s(&file["file"])),
                &limits,
                &allowance(),
                NativeDependencyPolicy::RetainOrphans,
            )
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        }
        check_state(&format!("{name} per chunk"), &mut doc, f, "applied_changes");
    });
}

/// apply_incoming_checked of whole 0.8 documents into an empty 0.12 document.
#[test]
fn checked_incoming_of_08_documents_matches() {
    each(documents(), |f| {
        let file = s(&f["file"]);
        let mut doc = AutoCommit::new();
        let outcome = doc
            .apply_incoming_checked(
                &read(file),
                &NativeLimits::default(),
                &allowance(),
                NativeDependencyPolicy::RetainOrphans,
            )
            .unwrap_or_else(|e| panic!("{file}: {e}"));
        assert!(
            matches!(outcome, NativeIncomingOutcome::Applied { .. }),
            "{file}"
        );
        check_state(&format!("{file} incoming"), &mut doc, f, "changes");
    });
}
