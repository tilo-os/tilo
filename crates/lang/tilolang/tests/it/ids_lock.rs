use crate::common::{compile, fixture};
use tilolang::{IdsLock, SeededEntropy};

fn lock_of(src: &str) -> IdsLock {
    tilolang::compile_package_with(src, &IdsLock::new(), &[], &mut SeededEntropy(1))
        .expect("compiles")
        .ids
}

#[test]
fn every_declaration_gets_an_id_and_the_lock_round_trips() {
    let c = compile(&fixture("counter.tl"));
    for name in [
        "Counter",
        "Counter.value",
        "increment",
        "counter",
        "counter#plus",
    ] {
        assert!(c.ids.get(name).is_some(), "{name} has no id");
    }
    let back = IdsLock::parse(&c.ids.to_text()).expect("parses");
    assert_eq!(back, c.ids);
}

#[test]
fn ids_are_eight_base32_characters() {
    let c = compile(&fixture("counter.tl"));
    for (_, id) in c.ids.entries() {
        assert_eq!(id.len(), 8);
        assert!(
            id.chars()
                .all(|ch| ch.is_ascii_lowercase() || ('2'..='7').contains(&ch))
        );
    }
}

#[test]
fn existing_ids_are_kept_on_recompile() {
    let first = lock_of(&fixture("counter.tl"));
    let again =
        tilolang::compile_package_with(&fixture("counter.tl"), &first, &[], &mut SeededEntropy(99))
            .expect("compiles");
    assert_eq!(again.ids, first);
}

#[test]
fn the_randomness_source_is_injectable() {
    let a = lock_of(&fixture("counter.tl"));
    let b = lock_of(&fixture("counter.tl"));
    assert_eq!(a, b);
    let c = tilolang::compile_package_with(
        &fixture("counter.tl"),
        &IdsLock::new(),
        &[],
        &mut SeededEntropy(2),
    )
    .expect("compiles");
    assert_ne!(a.get("increment"), c.ids.get("increment"));
}

#[test]
fn rename_keeps_the_id_and_everything_under_it() {
    let mut lock = lock_of(&fixture("counter.tl"));
    let type_id = lock.get("Counter").map(str::to_string).expect("id");
    let field_id = lock.get("Counter.value").map(str::to_string).expect("id");
    lock.rename("Counter", "Tally").expect("renames");
    assert_eq!(lock.get("Tally"), Some(type_id.as_str()));
    assert_eq!(lock.get("Tally.value"), Some(field_id.as_str()));
    assert_eq!(lock.get("Counter"), None);
    let src = fixture("counter.tl").replace("Counter", "Tally");
    let c =
        tilolang::compile_package_with(&src, &lock, &[], &mut SeededEntropy(5)).expect("compiles");
    assert_eq!(c.meta.types.first().map(|t| t.id.clone()), Some(type_id));
    assert_eq!(
        c.meta
            .types
            .first()
            .and_then(|t| t.fields.first())
            .map(|f| f.id.clone()),
        Some(field_id)
    );
}

#[test]
fn rename_refuses_unknown_and_taken_names() {
    let mut lock = lock_of(&fixture("counter.tl"));
    assert!(lock.rename("Nope", "X").is_err());
    assert!(lock.rename("increment", "counter").is_err());
}

#[test]
fn a_removed_declaration_is_retired_and_its_id_is_never_reused() {
    let first = lock_of(&fixture("counter.tl"));
    let old = first.get("increment").map(str::to_string).expect("id");
    let src = fixture("counter.tl")
        .replace("action increment(c: Counter) {\n    c.value += 1\n}\n", "")
        .replace("on: increment(c)", "on: increment2(c)");
    // Drop the action: the source no longer declares it.
    let src = src.replace(
        "button(\"+1\", on: increment2(c)) as plus",
        "text(\"none\")",
    );
    let after =
        tilolang::compile_package_with(&src, &first, &[], &mut SeededEntropy(3)).expect("compiles");
    assert!(after.ids.get("increment").is_none());
    assert!(
        after
            .ids
            .retired()
            .any(|(n, i)| n == "increment" && i == old)
    );
    // Declaring the name again assigns a new id, never the retired one.
    let back = tilolang::compile_package_with(
        &fixture("counter.tl"),
        &after.ids,
        &[],
        &mut SeededEntropy(3),
    )
    .expect("compiles");
    let fresh = back.ids.get("increment").expect("id");
    assert_ne!(fresh, old);
    assert!(back.ids.retired().any(|(_, i)| i == old));
    assert!(back.ids.to_text().contains("# retired"));
}

#[test]
fn malformed_lock_lines_are_errors() {
    assert!(IdsLock::parse("not a line").is_err());
    assert!(IdsLock::parse("a = x\na = y").is_err());
}
