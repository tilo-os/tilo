//! Which capabilities an app's code uses, compared with what its
//! `tilo-app.toml` declares.
//!
//! The evidence table is `tools/policy/app_capability_usage.toml`, which the
//! repository's policy gate (`tools/policy/check_app_capabilities.py`) reads
//! too: production dependencies, `package/feature` selections, source paths
//! (also inside `use a::{..}` groups) and method calls, each naming the
//! capability it implies. The code read is the portability lint's: the
//! shipped module trees without test items.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context as _, Result};
use proc_macro2::{Delimiter, TokenStream, TokenTree};
use serde::Deserialize;

use crate::portability::{
    follows_dot, group_names, ident_is, is_doc_attribute, is_path_sep, manifest_key_line, path_at,
    shipped_dependency_tables, shipped_sources, wildcard, workspace,
};

const TABLE: &str = include_str!("../../../../tools/policy/app_capability_usage.toml");

#[derive(Debug, Deserialize)]
struct Table {
    uses: BTreeMap<String, Evidence>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Evidence {
    crates: Vec<String>,
    features: Vec<String>,
    paths: Vec<String>,
    methods: Vec<String>,
}

fn table() -> Result<&'static Table> {
    static TABLE_CELL: OnceLock<std::result::Result<Table, String>> = OnceLock::new();
    TABLE_CELL
        .get_or_init(|| toml::from_str(TABLE).map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|error| anyhow::anyhow!("app_capability_usage.toml: {error}"))
}

/// One place that shows a capability in use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Use {
    /// Relative to the app directory.
    pub path: PathBuf,
    pub line: usize,
    pub what: String,
}

/// Every capability the app's shipped code uses, with the sites that show it.
///
/// # Errors
///
/// When a manifest or a reachable source file cannot be read or parsed.
pub fn used(app_dir: &Path) -> Result<BTreeMap<String, Vec<Use>>> {
    let table = table()?;
    let mut uses: BTreeMap<String, Vec<Use>> = BTreeMap::new();
    manifest_uses(app_dir, table, &mut uses)?;
    for (file, tokens) in shipped_sources(app_dir)? {
        let relative = file.strip_prefix(app_dir).unwrap_or(&file).to_path_buf();
        scan(table, tokens, &relative, &mut uses);
    }
    for sites in uses.values_mut() {
        sites.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
        sites.dedup();
    }
    Ok(uses)
}

/// The capabilities the code uses that `declared` (required and optional)
/// does not list.
///
/// # Errors
///
/// As [`used`].
pub fn undeclared(app_dir: &Path, declared: &[&str]) -> Result<Vec<(String, Vec<Use>)>> {
    Ok(used(app_dir)?
        .into_iter()
        .filter(|(capability, _)| !declared.contains(&capability.as_str()))
        .collect())
}

/// The diagnostic for one undeclared capability.
#[must_use]
pub fn render(capability: &str, sites: &[Use], manifest: &str) -> String {
    let site = sites.first().map_or_else(String::new, |site| {
        format!(
            "\n  --> {}:{}: {}",
            site.path.display(),
            site.line,
            site.what
        )
    });
    let more = match sites.len() {
        0 | 1 => String::new(),
        n => format!(" (and {} more)", n.saturating_sub(1)),
    };
    format!(
        "error[capability]: the code uses `{capability}` but {manifest} does not declare it{site}{more}\n  help: `tilo-app add capability {capability}` adds it to `[capabilities] optional`\n        (or list it in `required` if the app cannot work without it)\n"
    )
}

fn push(
    uses: &mut BTreeMap<String, Vec<Use>>,
    capability: &str,
    path: &Path,
    line: usize,
    what: String,
) {
    uses.entry(capability.to_owned()).or_default().push(Use {
        path: path.to_path_buf(),
        line,
        what,
    });
}

fn manifest_uses(
    app_dir: &Path,
    table: &Table,
    uses: &mut BTreeMap<String, Vec<Use>>,
) -> Result<()> {
    let path = app_dir.join("Cargo.toml");
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let manifest: toml::Table =
        toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    let (_, workspace_deps) = workspace(app_dir);
    let cargo = Path::new("Cargo.toml");
    let mut packages: BTreeMap<String, String> = BTreeMap::new();
    let mut selections: Vec<(String, usize)> = Vec::new();
    for (table_name, deps) in shipped_dependency_tables(&manifest) {
        for (alias, spec) in deps {
            let inherited = spec
                .get("workspace")
                .and_then(toml::Value::as_bool)
                .unwrap_or(false)
                .then(|| workspace_deps.get(alias))
                .flatten();
            let package = spec
                .get("package")
                .or_else(|| inherited.and_then(|dep| dep.get("package")))
                .and_then(toml::Value::as_str)
                .unwrap_or(alias)
                .to_owned();
            let line = manifest_key_line(&text, &table_name, alias);
            for (capability, evidence) in &table.uses {
                if evidence
                    .crates
                    .iter()
                    .any(|pattern| wildcard(pattern, &package))
                {
                    push(
                        uses,
                        capability,
                        cargo,
                        line,
                        format!("depends on `{package}`"),
                    );
                }
            }
            let features = [Some(spec), inherited]
                .into_iter()
                .flatten()
                .filter_map(|spec| spec.get("features")?.as_array())
                .flatten()
                .filter_map(toml::Value::as_str);
            for feature in features {
                selections.push((format!("{alias}/{feature}"), line));
                selections.push((format!("{package}/{feature}"), line));
            }
            packages.insert(alias.clone(), package);
        }
    }
    if let Some(features) = manifest.get("features").and_then(toml::Value::as_table) {
        for (name, selected) in features {
            let line = manifest_key_line(&text, "features", name);
            for selection in selected
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(toml::Value::as_str)
            {
                let Some((alias, feature)) = selection.split_once('/') else {
                    continue;
                };
                let alias = alias.trim_start_matches("dep:").trim_end_matches('?');
                selections.push((format!("{alias}/{feature}"), line));
                if let Some(package) = packages.get(alias) {
                    selections.push((format!("{package}/{feature}"), line));
                }
            }
        }
    }
    for (selection, line) in selections {
        for (capability, evidence) in &table.uses {
            if evidence.features.contains(&selection) {
                push(
                    uses,
                    capability,
                    cargo,
                    line,
                    format!("selects `{selection}`"),
                );
            }
        }
    }
    Ok(())
}

fn scan(table: &Table, stream: TokenStream, file: &Path, uses: &mut BTreeMap<String, Vec<Use>>) {
    let tokens: Vec<TokenTree> = stream.into_iter().collect();
    for (index, token) in tokens.iter().enumerate() {
        let ident = match token {
            TokenTree::Group(group) => {
                if !is_doc_attribute(&tokens, index) {
                    scan(table, group.stream(), file, uses);
                }
                continue;
            }
            TokenTree::Ident(ident) => ident,
            _ => continue,
        };
        let line = ident.span().start().line;
        let name = ident.to_string();
        let dotted = follows_dot(&tokens, index);
        for (capability, evidence) in &table.uses {
            if dotted {
                let called = matches!(
                    tokens.get(index.saturating_add(1)),
                    Some(TokenTree::Group(group)) if group.delimiter() == Delimiter::Parenthesis
                );
                if called && evidence.methods.contains(&name) {
                    push(uses, capability, file, line, format!("`.{name}(`"));
                }
                continue;
            }
            for pattern in &evidence.paths {
                let segments: Vec<&str> = pattern.split("::").collect();
                if segments.first() != Some(&name.as_str()) {
                    continue;
                }
                if path_at(&tokens, index, &segments).is_some()
                    || in_use_group(&tokens, index, &segments)
                {
                    push(uses, capability, file, line, format!("`{pattern}`"));
                }
            }
        }
    }
}

/// `a::{.., b, ..}` for the pattern `a::b`.
fn in_use_group(tokens: &[TokenTree], index: usize, segments: &[&str]) -> bool {
    let Some((last, head)) = segments.split_last() else {
        return false;
    };
    if head.is_empty() {
        return false;
    }
    let Some(after) = path_at(tokens, index, head) else {
        return false;
    };
    if !is_path_sep(tokens, after) {
        return false;
    }
    match tokens.get(after.saturating_add(2)) {
        Some(TokenTree::Group(group)) if group.delimiter() == Delimiter::Brace => {
            group_names(&group.stream(), last)
        }
        _ => ident_is(tokens, after.saturating_add(2), last),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_names_only_tilo_caps_capabilities() {
        for capability in table().unwrap().uses.keys() {
            assert!(
                tilo_caps::Capability::from_name(capability).is_some(),
                "`{capability}` in app_capability_usage.toml is not a tilo-caps capability"
            );
        }
    }

    #[test]
    fn paths_groups_methods_and_manifests_are_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path();
        std::fs::create_dir_all(app.join("src")).unwrap();
        std::fs::write(
            app.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[features]\nsession = [\"ui/mpris\"]\n\n[dependencies]\nui = { package = \"tilo-app\", path = \"x\" }\ntilo-secrets = { path = \"y\" }\n\n[dev-dependencies]\ntilo-capture = { path = \"z\" }\n",
        )
        .unwrap();
        std::fs::write(
            app.join("src/lib.rs"),
            "use ui::{location, time};\n\
             pub fn open(files: &dyn Picker) { let _ = files.open_contents(); }\n\
             pub fn save() { let _ = ui::portal::PickRequest::save(\"a\", \"b\", \"c\"); }\n\
             pub fn read() { let _ = std::fs::read(\"x\"); }\n\
             #[cfg(test)]\nmod tests { fn t() { std::thread::spawn(|| {}); } }\n",
        )
        .unwrap();
        let uses = used(app).unwrap();
        let names: Vec<&str> = uses.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            [
                "files.open",
                "files.paths",
                "files.save",
                "location",
                "media.session",
                "secrets"
            ]
        );
        assert_eq!(uses["location"][0].line, 1);
        assert_eq!(uses["secrets"][0].path, PathBuf::from("Cargo.toml"));

        let missing =
            undeclared(app, &["files.open", "files.paths", "files.save", "secrets"]).unwrap();
        let missing: Vec<&str> = missing.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(missing, ["location", "media.session"]);
        let message = render("location", &uses["location"], "tilo-app.toml");
        assert!(message.contains("error[capability]: the code uses `location`"));
        assert!(message.contains("--> src/lib.rs:1: `ui::location`"));
        assert!(message.contains("tilo-app add capability location"));
    }
}
