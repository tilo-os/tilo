//! Typed builders for the toolkit's described components, generated from the catalogue.
//!
//! Each component of `crates/dev/ui-graph/components.json` becomes a builder: a function that
//! takes its required props that no one may leave out as arguments, then a method per optional
//! prop, then the node ([`crate::ui::build`]). Each method takes the Rust type the prop's
//! declared kind maps to, so a view cannot name a prop the component does not declare, nor give
//! one a value of another type (one-app-model.md §2: a Rust view composes the same described
//! components as a tilolang one).
//!
//! The catalogue stays the one source: these builders are a projection of it, so a widget that
//! lands in ui-core is callable from a Rust view as soon as the catalogue is written again
//! (`cargo run -p tilo-ui --example component-catalogue > crates/dev/ui-graph/components.json`).

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const CATALOGUE: &str = "../../dev/ui-graph/components.json";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo::rerun-if-changed={CATALOGUE}");
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(CATALOGUE);
    let raw = std::fs::read_to_string(path)?;
    let catalogue: serde_json::Value = serde_json::from_str(&raw)?;
    let mut out = String::new();
    emit(&mut out, &catalogue)?;
    let output = std::env::var_os("OUT_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?
        .join("ui_builders.rs");
    std::fs::write(output, out)?;
    Ok(())
}

struct Field {
    name: String,
    ty: String,
    into: String,
    required: bool,
}

fn emit(out: &mut String, catalogue: &serde_json::Value) -> Result<(), Box<dyn std::error::Error>> {
    let mut choices: BTreeSet<(String, Vec<String>)> = BTreeSet::new();
    let mut components = Vec::new();
    let Some(list) = catalogue.get("components").and_then(|c| c.as_array()) else {
        return Err("the component catalogue holds no components".into());
    };
    for component in list {
        let name = component["name"].as_str().unwrap_or_default().to_owned();
        let mut fields = Vec::new();
        for prop in component
            .get("props")
            .and_then(|p| p.as_array())
            .into_iter()
            .flatten()
        {
            let prop_name = prop["name"].as_str().unwrap_or_default();
            if reserved_method(prop_name) {
                continue;
            }
            let ty = &prop["type"];
            let mapped = map_type(ty, &mut choices);
            let into = into_expr(ty, "value");
            fields.push(Field {
                name: prop_name.to_owned(),
                ty: mapped,
                into,
                required: prop["required"].as_bool().unwrap_or(false),
            });
        }
        components.push((name, fields));
    }
    for (name, cases) in &choices {
        let _ = writeln!(out, "/// One of the catalogue's closed sets (`{name}`).");
        let _ = writeln!(out, "#[derive(Clone, Copy, Debug, PartialEq, Eq)]");
        let _ = writeln!(out, "pub enum {name} {{");
        for case in cases {
            let _ = writeln!(out, "    {},", variant(case));
        }
        let _ = writeln!(out, "}}");
        let _ = writeln!(out, "impl {name} {{");
        let _ = writeln!(out, "    #[must_use]");
        let _ = writeln!(out, "    pub const fn as_str(self) -> &'static str {{");
        let _ = writeln!(out, "        match self {{");
        for case in cases {
            let _ = writeln!(out, "            Self::{} => \"{case}\",", variant(case));
        }
        let _ = writeln!(out, "        }}");
        let _ = writeln!(out, "    }}");
        let _ = writeln!(out, "}}");
        let _ = writeln!(
            out,
            "impl From<{name}> for crate::Value {{ fn from(value: {name}) -> Self {{ Self::Enum(value.as_str().to_owned()) }} }}"
        );
        let _ = writeln!(out);
    }
    for (name, fields) in &components {
        emit_builder(out, name, fields);
    }
    Ok(())
}

fn emit_builder(out: &mut String, name: &str, fields: &[Field]) {
    let builder = format!("{name}Builder");
    let _ = writeln!(
        out,
        "/// `ui.{name}`'s builder, generated from the catalogue."
    );
    let _ = writeln!(out, "pub struct {builder} {{");
    let _ = writeln!(out, "    node: crate::node::Node,");
    let _ = writeln!(out, "}}");
    let _ = writeln!(out, "impl {builder} {{");
    let required: Vec<&Field> = fields.iter().filter(|f| f.required).collect();
    let mut args = String::new();
    for (index, field) in required.iter().enumerate() {
        let _ = write!(args, ", arg{index}: {}", field.ty);
    }
    let _ = writeln!(
        out,
        "    /// The component of `key`, with its required props."
    );
    let _ = writeln!(out, "    #[must_use]");
    let _ = writeln!(out, "    pub fn new(key: &str{args}) -> Self {{");
    let binding = if required.is_empty() {
        "let node"
    } else {
        "let mut node"
    };
    let _ = writeln!(
        out,
        "        {binding} = crate::ui::build(\"{name}\", key);"
    );
    for (index, field) in required.iter().enumerate() {
        let into = field.into.replace("value", &format!("arg{index}"));
        let _ = writeln!(
            out,
            "        node = crate::ui::put(node, \"{}\", {into});",
            field.name
        );
    }
    let _ = writeln!(out, "        Self {{ node }}");
    let _ = writeln!(out, "    }}");
    for field in fields.iter().filter(|f| !f.required) {
        let _ = writeln!(out, "    /// `{}`.", field.name);
        let _ = writeln!(out, "    #[must_use]");
        let _ = writeln!(
            out,
            "    pub fn {}(mut self, value: {}) -> Self {{",
            field.name, field.ty
        );
        let _ = writeln!(
            out,
            "        self.node = crate::ui::put(self.node, \"{}\", {});",
            field.name, field.into
        );
        let _ = writeln!(out, "        self");
        let _ = writeln!(out, "    }}");
    }
    let _ = writeln!(out, "}}");
    let _ = writeln!(
        out,
        "impl From<{builder}> for crate::node::Node {{ fn from(builder: {builder}) -> Self {{ builder.node }} }}"
    );
    let _ = writeln!(out);
}

fn choice_of(ty: &serde_json::Value) -> Option<(String, Vec<String>)> {
    if let Some(choice) = ty.get("choice") {
        let name = choice["name"].as_str()?.to_owned();
        let cases = choice["cases"]
            .as_array()?
            .iter()
            .filter_map(|c| c.as_str().map(str::to_owned))
            .collect();
        return Some((name, cases));
    }
    for key in ["option", "list", "set"] {
        if let Some(inner) = ty.get(key) {
            return choice_of(inner);
        }
    }
    None
}

fn map_type(ty: &serde_json::Value, choices: &mut BTreeSet<(String, Vec<String>)>) -> String {
    if let Some(inner) = ty.get("option") {
        return map_type(inner, choices);
    }
    if let Some(inner) = ty.get("list") {
        let mapped = map_type(inner, choices);
        return match mapped.as_str() {
            "impl Into<String>" => "Vec<String>".to_owned(),
            other => format!("Vec<{other}>"),
        };
    }
    if let Some(inner) = ty.get("set") {
        let mapped = map_type(inner, choices);
        return match mapped.as_str() {
            "impl Into<String>" => "Vec<String>".to_owned(),
            other => format!("Vec<{other}>"),
        };
    }
    if let Some(map) = ty.get("map") {
        return format!(
            "Vec<({}, {})>",
            map_type(&map["key"], choices),
            map_type(&map["value"], choices)
        );
    }
    if let Some((name, cases)) = choice_of(ty) {
        choices.insert((name.clone(), cases));
        return name;
    }
    match ty.as_str() {
        Some("text" | "time" | "icon") => "impl Into<String>".to_owned(),
        Some("int") => "i64".to_owned(),
        Some("float") => "f64".to_owned(),
        Some("bool") => "bool".to_owned(),
        Some("resource") => "crate::ResourceRef".to_owned(),
        Some("principal") => "crate::Principal".to_owned(),
        Some("picture") => "crate::Picture".to_owned(),
        Some("action") => "crate::Offered".to_owned(),
        Some("rich_text") => "crate::patch::rich::RichText".to_owned(),
        _ => "crate::Value".to_owned(),
    }
}

/// How the builder's argument becomes the wire's value. `var` is the argument's name (`value`
/// for a method's, `arg0` for a constructor's).
fn into_expr(ty: &serde_json::Value, var: &str) -> String {
    if let Some(inner) = ty.get("option") {
        return into_expr(inner, var);
    }
    if let Some(inner) = ty.get("list") {
        let inner_expr = into_expr(inner, "v");
        return format!("crate::ui::list({var}.into_iter().map(|v| {inner_expr}).collect())");
    }
    if let Some(inner) = ty.get("set") {
        let inner_expr = into_expr(inner, "v");
        return format!("crate::ui::list({var}.into_iter().map(|v| {inner_expr}).collect())");
    }
    if let Some(map) = ty.get("map") {
        let key = into_expr(&map["key"], "k");
        let value_expr = into_expr(&map["value"], "v");
        return format!(
            "crate::ui::map({var}.into_iter().map(|(k, v)| ({key}, {value_expr})).collect())"
        );
    }
    if ty.get("choice").is_some() {
        return format!("crate::Value::from({var})");
    }
    let template = match ty.as_str() {
        Some("text" | "time" | "icon") => "crate::ui::text(__VAR__)",
        Some("int") => "crate::Value::Int(__VAR__)",
        Some("float") => "crate::Value::Float(__VAR__)",
        Some("bool") => "crate::Value::Bool(__VAR__)",
        Some("resource") => "crate::Value::Ref(__VAR__)",
        Some("principal") => "crate::Value::Principal(__VAR__)",
        Some("picture") => "crate::Value::Picture(__VAR__)",
        Some("action") => "crate::Value::Action(__VAR__)",
        Some("rich_text") => "crate::Value::Rich(__VAR__)",
        _ => "__VAR__",
    };
    template.replace("__VAR__", var)
}

fn variant(case: &str) -> String {
    let mut out = String::new();
    let mut upper = true;
    for ch in case.chars() {
        if ch == '_' || ch == '-' {
            upper = true;
        } else if upper {
            out.extend(ch.to_uppercase());
            upper = false;
        } else {
            out.push(ch);
        }
    }
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert_str(0, "Case");
    }
    out
}

fn reserved_method(name: &str) -> bool {
    const RESERVED: &[&str] = &[
        "as", "box", "break", "const", "continue", "crate", "do", "else", "enum", "extern",
        "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
        "pub", "ref", "return", "self", "static", "struct", "super", "trait", "true", "type",
        "unsafe", "use", "where", "while", "async", "await", "dyn", "abstract", "become", "final",
        "macro", "override", "priv", "typeof", "unsized", "virtual", "yield", "try",
    ];
    RESERVED.contains(&name)
}
