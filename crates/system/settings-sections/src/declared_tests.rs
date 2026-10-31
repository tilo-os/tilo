//! Every setting the session keeps is a declared value: an agent reading
//! the published field edit learns what it is, what it may hold and what it
//! holds until someone changes it, without trying a change to find out.

use ui::tilo_resource::{FieldShape, Shape};

/// Texts whose values are open: what they name comes from elsewhere (a
/// theme, a language, a picture), checked by the section's own invariant or
/// by whoever reads it.
const FREE_TEXT: &[(&str, &str)] = &[
    // A preset id or `custom:<id>` of a theme Theme Studio wrote.
    ("settings.appearance", "dark_theme"),
    ("settings.appearance", "light_theme"),
    // `HH:MM`, checked by the section's invariant.
    ("settings.appearance", "sunrise"),
    ("settings.appearance", "sunset"),
    // A BCP 47 tag among the languages installed.
    ("settings.appearance", "language"),
    // Any family a theme's type role may name.
    ("settings.appearance", "ui_face"),
    // A name the presence palette gives a hue, checked by the section's
    // invariant.
    ("settings.appearance", "accent"),
    ("settings.text-assistance", "spelling_language"),
    // A shipped wallpaper's id, checked against the ones installed.
    ("settings.appearance", "wallpaper"),
    // A sound theme installed on the device.
    ("settings.audio", "system_sounds.theme"),
    // What search keeps of the person's choices, in its own encoding.
    ("settings.search_history", "chosen"),
    ("settings.search_history", "chosen_for_text"),
];

fn number_kind(ty: &str) -> Option<bool> {
    let ty = ty
        .strip_prefix("Option<")
        .and_then(|inner| inner.strip_suffix('>'))
        .unwrap_or(ty);
    match ty {
        "f32" | "f64" => Some(true),
        "u8" | "u16" | "u32" | "u64" | "usize" | "i8" | "i16" | "i32" | "i64" => Some(false),
        _ => None,
    }
}

fn is_text(ty: &str) -> bool {
    matches!(ty, "String" | "Option<String>")
}

/// The schema of the value itself, past an optional field's `null`.
fn value_of(schema: &serde_json::Value) -> serde_json::Value {
    schema
        .get("oneOf")
        .and_then(serde_json::Value::as_array)
        .and_then(|alternatives| {
            alternatives
                .iter()
                .find(|alternative| alternative.get("type") != Some(&serde_json::json!("null")))
        })
        .cloned()
        .unwrap_or_else(|| schema.clone())
}

/// The largest value a type holds by itself, which a declared range narrows.
fn type_maximum(ty: &str) -> Option<f64> {
    let ty = ty
        .strip_prefix("Option<")
        .and_then(|inner| inner.strip_suffix('>'))
        .unwrap_or(ty);
    Some(match ty {
        "u8" => f64::from(u8::MAX),
        "u16" => f64::from(u16::MAX),
        "u32" => f64::from(u32::MAX),
        "i8" => f64::from(i8::MAX),
        "i16" => f64::from(i16::MAX),
        "i32" => f64::from(i32::MAX),
        _ => return None,
    })
}

fn undeclared(kind: &str, prefix: &str, field: &FieldShape, top: bool, out: &mut Vec<String>) {
    if field.readonly {
        return;
    }
    let path = format!("{prefix}{}", field.key);
    let at = format!("{kind} `{path}`");
    let schema = (field.schema)();
    if field.title.to_lowercase() == field.name.replace('_', " ").to_lowercase() {
        out.push(format!("{at}: no description (a doc comment)"));
    }
    if top && schema.get("default").is_none() {
        out.push(format!("{at}: no published default (`#[model(defaults)]`)"));
    }
    let value = value_of(&schema);
    match number_kind(field.ty) {
        Some(float) => {
            let maximum = value.get("maximum").and_then(serde_json::Value::as_f64);
            let narrowed = maximum
                .is_some_and(|maximum| type_maximum(field.ty).is_none_or(|own| maximum < own));
            if value.get("minimum").is_none() || !narrowed {
                out.push(format!(
                    "{at}: a {} without a declared range (`#[model(range = low..=high)]`)",
                    if float { "fraction" } else { "number" }
                ));
            }
        }
        None if is_text(field.ty)
            && value.get("enum").is_none()
            && !FREE_TEXT.contains(&(kind, path.as_str())) =>
        {
            out.push(format!(
                "{at}: a text without declared choices (`#[model(choices = ..)]`)"
            ));
        }
        None => {}
    }
    if let Shape::Record(fields) = field.shape {
        for inner in fields {
            undeclared(kind, &format!("{path}."), inner, false, out);
        }
    }
}

#[test]
fn every_kept_setting_is_declared() {
    let mut out = Vec::new();
    for (kind, shape) in crate::kept() {
        let Shape::Record(fields) = shape else {
            out.push(format!("{kind}: not a record"));
            continue;
        };
        for field in fields {
            undeclared(kind, "", field, true, &mut out);
        }
    }
    assert!(out.is_empty(), "undeclared settings:\n{}", out.join("\n"));
}

/// A declared range is held through the section's field edit, as any door
/// sends it.
#[test]
fn a_kept_setting_holds_only_what_it_declares() {
    use ui::tilo_resource::Replica;
    let paths = crate::prefs::AppearancePrefs::paths();
    let mut appearance = Replica::<crate::prefs::AppearancePrefs>::new();
    appearance.apply(paths.density.set(1.25)).unwrap();
    let refused = format!(
        "{:?}",
        appearance.apply(paths.density.set(9.0)).unwrap_err()
    );
    assert!(refused.contains("between"), "{refused}");
    assert!(
        appearance
            .apply(paths.schedule.set("dusk".to_owned()))
            .is_err()
    );
    assert!(
        appearance
            .apply(paths.mode.set("automatic".to_owned()))
            .is_ok()
    );
}
