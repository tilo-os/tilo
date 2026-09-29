//! The `portability` lint: apps call portable APIs, and the platform code
//! behind those APIs lives in Tilo's platform crates.
//!
//! The lint reads an app's shipped Rust sources (the module tree reachable from
//! its library and binary roots, minus `#[cfg(test)]` items) and its Cargo
//! manifest. It flags OS-specific standard-library modules, direct use of
//! native binding crates, FFI blocks, target-OS conditional compilation, native
//! dependencies, and SDK backend features. Tests are exempt: they may pin
//! platform behaviour that the platform crates promise.
//!
//! An app whose purpose is platform-specific declares that once in
//! `tilo-app.toml` (`[platform] native = "<reason>"`) instead of excusing
//! individual lines.
//!
//! The same scan records, separately, the standard-library APIs the web
//! target (wasm32-unknown-unknown) has no implementation of: threads, the
//! file system, processes, sockets and the system clock. They are portable
//! across native platforms, so they do not fail the lint, but an app that uses
//! one is not web-capable and `tilo-app build --platform web` refuses it.

use std::collections::{BTreeSet, VecDeque};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use proc_macro2::{Delimiter, Spacing, TokenStream, TokenTree};
use quote::ToTokens as _;
use syn::punctuated::Punctuated;
use syn::visit_mut::VisitMut;

/// The lint's name, as printed in diagnostics.
pub const LINT: &str = "portability";

/// Crates that expose operating-system mechanisms rather than portable
/// operations.
const NATIVE_CRATES: &[&str] = &[
    "libc",
    "nix",
    "rustix",
    "windows",
    "windows-sys",
    "ashpd",
    "zbus",
    "xattr",
];

/// SDK features that name one backend, with the semantic feature an app
/// selects instead (when there is one).
const NATIVE_SDK_FEATURES: &[(&str, Option<&str>)] = &[
    ("webview-wpe", Some("webview")),
    ("mpris", Some("media-session")),
    ("dmabuf-import", None),
];

/// Packages that make up the app SDK.
const SDK_PACKAGES: &[&str] = &["tilo-app", "tilo-ui", "ui-core"];

/// `cfg` predicates that select an operating system or OS family.
const PLATFORM_PREDICATES: &[&str] = &[
    "target_os",
    "target_family",
    "target_vendor",
    "target_env",
    "unix",
    "windows",
];

/// `std` modules the web target compiles but cannot run: every call panics or
/// fails there.
const NATIVE_ONLY_STD_MODULES: &[&str] = &["thread", "fs", "process", "net"];

/// `std::time` items that read the system clock, which the web target lacks.
const NATIVE_ONLY_STD_TIME: &[&str] = &["Instant", "SystemTime"];

/// Platform crates an app reaches OS mechanisms through.
const PLATFORM_CRATES_HINT: &str = "private-fs (owner-only files), atomic-file (durable \
     writes), xdg-paths (per-user directories), bounded-command (subprocesses), or `ui` host \
     services";

/// What kind of platform-specific code a finding is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FindingKind {
    /// A `std::os` module.
    OsStd,
    /// A path into a native binding crate such as `libc::`.
    NativeCrate,
    /// An `extern { ... }` block.
    Ffi,
    /// `cfg`, `cfg_attr` or `cfg!` selecting a target OS or family.
    TargetCfg,
    /// A native binding crate in the manifest's dependencies.
    NativeDependency,
    /// An SDK feature that names one backend.
    NativeSdkFeature,
    /// A `std` API with no implementation on the web target.
    NativeOnlyStd,
}

/// One platform-specific site in an app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortabilityFinding {
    /// Path relative to the app directory.
    pub path: PathBuf,
    /// One-based line.
    pub line: usize,
    pub kind: FindingKind,
    /// What was found, for example "`std::os::unix::fs`".
    pub what: String,
    /// How to fix it.
    pub help: String,
}

/// The result of linting one app.
#[derive(Debug)]
pub struct PortabilityReport {
    pub app_dir: PathBuf,
    /// The `[platform] native` reason, when the app declares one.
    pub native: Option<String>,
    pub findings: Vec<PortabilityFinding>,
    /// Uses of `std` APIs the web target cannot run. They do not fail the
    /// lint; they make the app native-only.
    pub web: Vec<PortabilityFinding>,
    /// Number of Rust source files inspected.
    pub files: usize,
}

impl PortabilityReport {
    /// Whether the app passes: portable apps have no findings, and declared
    /// native apps have platform code that justifies the declaration.
    #[must_use]
    pub fn passed(&self) -> bool {
        match self.native {
            Some(_) => !self.findings.is_empty(),
            None => self.findings.is_empty(),
        }
    }

    /// Whether the app can run on the web target: it is portable and calls no
    /// `std` API the web target lacks.
    #[must_use]
    pub fn web_capable(&self) -> bool {
        self.native.is_none() && self.findings.is_empty() && self.web.is_empty()
    }

    /// Why the app cannot run on the web target, one line per site; empty
    /// when it can.
    #[must_use]
    pub fn render_web(&self) -> String {
        let mut out = String::new();
        if let Some(reason) = &self.native {
            let _ = writeln!(out, "tilo-app.toml declares the app native: \"{reason}\"");
        }
        for finding in self.findings.iter().chain(&self.web) {
            let _ = writeln!(
                out,
                "{}:{}: {}",
                finding.path.display(),
                finding.line,
                finding.what
            );
        }
        out
    }

    /// A human-readable report: one diagnostic per finding with the lint name,
    /// location, source line and remedy.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = self.render_findings();
        if self.native.is_none() && self.findings.is_empty() && !self.web.is_empty() {
            let _ = writeln!(
                out,
                "{LINT}: native-only: {} site(s) use std APIs the web target cannot run:",
                self.web.len()
            );
            for finding in &self.web {
                let _ = writeln!(
                    out,
                    "  {}:{}: {}",
                    finding.path.display(),
                    finding.line,
                    finding.what
                );
            }
            let _ = writeln!(out, "  = help: {}", source_help(FindingKind::NativeOnlyStd));
        }
        out
    }

    fn render_findings(&self) -> String {
        let mut out = String::new();
        let app = self.app_dir.display();
        match (&self.native, self.findings.is_empty()) {
            (None, true) => {
                let _ = writeln!(
                    out,
                    "{LINT}: ok, no platform-specific code in {} source file(s)",
                    self.files
                );
            }
            (Some(reason), false) => {
                let _ = writeln!(
                    out,
                    "{LINT}: declared native in tilo-app.toml: \"{reason}\" ({} platform-specific site(s))",
                    self.findings.len()
                );
            }
            (Some(_), true) => {
                let _ = writeln!(
                    out,
                    "error[{LINT}]: tilo-app.toml: [platform] native is declared, but {app} has no platform-specific code\n  = help: remove the [platform] table; the app is portable"
                );
            }
            (None, false) => {
                for finding in &self.findings {
                    let _ = writeln!(
                        out,
                        "error[{LINT}]: {}:{}: {}",
                        finding.path.display(),
                        finding.line,
                        finding.what
                    );
                    if let Some(source) =
                        source_line(&self.app_dir.join(&finding.path), finding.line)
                    {
                        let _ = writeln!(out, "  {:>5} | {source}", finding.line);
                    }
                    let _ = writeln!(out, "  = help: {}", finding.help);
                }
                let _ = writeln!(
                    out,
                    "\n{LINT}: {} platform-specific site(s) in {app}.\n  \
                     Apps call portable APIs; the platform code behind them lives in platform \
                     crates (crates/system, crates/sdk). Tests (tests/, #[cfg(test)]) are exempt.\n  \
                     If the app itself is platform-specific (an OS installer, a PTY terminal), \
                     declare that once in tilo-app.toml instead:\n\n    \
                     [platform]\n    native = \"<one-line reason>\"",
                    self.findings.len()
                );
            }
        }
        out
    }
}

fn source_line(path: &Path, line: usize) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let source = text.lines().nth(line.checked_sub(1)?)?.trim();
    Some(source.chars().take(120).collect())
}

/// Lint one app directory.
///
/// # Errors
///
/// Returns an error when the app's manifests or a reachable source file cannot
/// be read or parsed.
pub fn lint_app(app_dir: &Path) -> Result<PortabilityReport> {
    let manifest_path = app_dir.join("tilo-app.toml");
    let manifest_source = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest = tilo_app_package::Manifest::from_toml_str(&manifest_source)
        .with_context(|| format!("parse {}", manifest_path.display()))?;

    let cargo = CargoFacts::read(app_dir)?;
    let mut findings = cargo.findings.clone();
    let mut web = Vec::new();
    let roots = cargo.roots(app_dir);
    let mut files: usize = 0;
    for (file, tokens) in shipped_modules(&roots)? {
        files = files.saturating_add(1);
        let relative = file.strip_prefix(app_dir).unwrap_or(&file).to_path_buf();
        let mut hits = Vec::new();
        scan_tokens(tokens, &cargo.native_idents, &mut hits);
        for (line, kind, what) in hits {
            let finding = PortabilityFinding {
                path: relative.clone(),
                line,
                kind,
                help: source_help(kind),
                what,
            };
            if kind == FindingKind::NativeOnlyStd {
                web.push(finding);
            } else {
                findings.push(finding);
            }
        }
    }
    for list in [&mut findings, &mut web] {
        list.sort_by(|a, b| (&a.path, a.line, a.kind).cmp(&(&b.path, b.line, b.kind)));
        list.dedup_by(|a, b| a.path == b.path && a.line == b.line && a.kind == b.kind);
    }
    Ok(PortabilityReport {
        app_dir: app_dir.to_path_buf(),
        native: manifest.platform.native,
        findings,
        web,
        files,
    })
}

fn source_help(kind: FindingKind) -> String {
    match kind {
        FindingKind::OsStd | FindingKind::NativeCrate => format!(
            "call a platform crate instead, such as {PLATFORM_CRATES_HINT}; if none offers the \
             operation, add it there with an implementation for each target"
        ),
        FindingKind::Ffi => {
            "put the binding and its safety contract in a platform crate and call its safe API"
                .to_owned()
        }
        FindingKind::TargetCfg => "choose per-target implementations inside a platform crate; \
             app code compiles unchanged for every target"
            .to_owned(),
        FindingKind::NativeOnlyStd => "run background work through the SDK's tasks \
             (`LatestTask`, `LatestRequest`), time through `ui::time`, and files and the network \
             through `ui` host services; or keep the app native-only"
            .to_owned(),
        FindingKind::NativeDependency | FindingKind::NativeSdkFeature => String::new(),
    }
}

/// Every Tilo app that is a member of the workspace containing `start`.
///
/// # Errors
///
/// Returns an error when no workspace manifest is found above `start`.
pub fn workspace_apps(start: &Path) -> Result<Vec<PathBuf>> {
    let start =
        std::fs::canonicalize(start).with_context(|| format!("resolve {}", start.display()))?;
    for dir in start.ancestors() {
        let manifest = dir.join("Cargo.toml");
        let Ok(source) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        let table: toml::Table =
            toml::from_str(&source).with_context(|| format!("parse {}", manifest.display()))?;
        let Some(workspace) = table.get("workspace").and_then(toml::Value::as_table) else {
            continue;
        };
        let mut apps = BTreeSet::new();
        let members = workspace.get("members").and_then(toml::Value::as_array);
        for member in members
            .into_iter()
            .flatten()
            .filter_map(toml::Value::as_str)
        {
            if let Some(parent) = member.strip_suffix("/*") {
                let Ok(entries) = std::fs::read_dir(dir.join(parent)) else {
                    continue;
                };
                for entry in entries.flatten() {
                    apps.insert(entry.path());
                }
            } else {
                apps.insert(dir.join(member));
            }
        }
        return Ok(apps
            .into_iter()
            .filter(|path| {
                path.join("tilo-app.toml").is_file() && path.join("Cargo.toml").is_file()
            })
            .collect());
    }
    anyhow::bail!(
        "no Cargo workspace above {}; `--all` lints the Tilo apps of a workspace",
        start.display()
    )
}

/// What the app's Cargo manifest says about platform code.
struct CargoFacts {
    /// Findings from the manifest itself.
    findings: Vec<PortabilityFinding>,
    /// Rust identifiers of native crates the shipped code can name.
    native_idents: BTreeSet<String>,
    lib_path: Option<PathBuf>,
    bin_paths: Vec<PathBuf>,
}

impl CargoFacts {
    fn read(app_dir: &Path) -> Result<Self> {
        let path = app_dir.join("Cargo.toml");
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let manifest: toml::Table =
            toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        let workspace_deps = workspace_dependencies(app_dir);
        let mut facts = Self {
            findings: Vec::new(),
            native_idents: BTreeSet::new(),
            lib_path: table_path(manifest.get("lib")),
            bin_paths: manifest
                .get("bin")
                .and_then(toml::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|bin| table_path(Some(bin)))
                .collect(),
        };
        let mut sdk_aliases = BTreeSet::new();
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
                    .unwrap_or(alias);
                let line = || manifest_key_line(&text, &table_name, alias);
                if NATIVE_CRATES.contains(&package) {
                    facts.native_idents.insert(alias.replace('-', "_"));
                    facts.findings.push(PortabilityFinding {
                        path: PathBuf::from("Cargo.toml"),
                        line: line(),
                        kind: FindingKind::NativeDependency,
                        what: format!("depends on the native binding crate `{package}`"),
                        help: format!(
                            "depend on the platform crate that wraps `{package}` for you, such as \
                             {PLATFORM_CRATES_HINT}"
                        ),
                    });
                }
                let path_name = spec
                    .get("path")
                    .and_then(toml::Value::as_str)
                    .and_then(|path| Path::new(path).file_name()?.to_str());
                let is_sdk = SDK_PACKAGES.contains(&package)
                    || path_name.is_some_and(|name| SDK_PACKAGES.contains(&name));
                if !is_sdk {
                    continue;
                }
                sdk_aliases.insert(alias.to_owned());
                let features = [Some(spec), inherited]
                    .into_iter()
                    .flatten()
                    .filter_map(|spec| spec.get("features")?.as_array())
                    .flatten()
                    .filter_map(toml::Value::as_str);
                for feature in features {
                    if let Some(finding) = sdk_feature_finding(alias, feature, line()) {
                        facts.findings.push(finding);
                    }
                }
            }
        }
        if let Some(features) = manifest.get("features").and_then(toml::Value::as_table) {
            for selections in features.values().filter_map(toml::Value::as_array) {
                for selection in selections.iter().filter_map(toml::Value::as_str) {
                    let Some((alias, feature)) = selection.split_once('/') else {
                        continue;
                    };
                    let alias = alias.trim_end_matches('?');
                    if !sdk_aliases.contains(alias) {
                        continue;
                    }
                    let line = text
                        .lines()
                        .position(|line| line.contains(&format!("\"{selection}\"")))
                        .map_or(1, |index| index.saturating_add(1));
                    if let Some(finding) = sdk_feature_finding(alias, feature, line) {
                        facts.findings.push(finding);
                    }
                }
            }
        }
        Ok(facts)
    }

    /// Crate roots of the shipped targets.
    fn roots(&self, app_dir: &Path) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        roots.push(
            app_dir.join(
                self.lib_path
                    .as_deref()
                    .unwrap_or_else(|| Path::new("src/lib.rs")),
            ),
        );
        roots.push(app_dir.join("src/main.rs"));
        roots.extend(self.bin_paths.iter().map(|path| app_dir.join(path)));
        if let Ok(entries) = std::fs::read_dir(app_dir.join("src/bin")) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "rs") {
                    roots.push(path);
                } else if path.join("main.rs").is_file() {
                    roots.push(path.join("main.rs"));
                }
            }
        }
        roots.retain(|path| path.is_file());
        roots.sort();
        roots.dedup();
        roots
    }
}

fn sdk_feature_finding(alias: &str, feature: &str, line: usize) -> Option<PortabilityFinding> {
    let (_, semantic) = NATIVE_SDK_FEATURES
        .iter()
        .find(|(name, _)| *name == feature)?;
    let help = match semantic {
        Some(semantic) => format!(
            "enable the semantic SDK feature `{alias}/{semantic}` instead; the SDK selects the \
             backend for each target"
        ),
        None => {
            "let the SDK select this backend through a semantic feature or capability".to_owned()
        }
    };
    Some(PortabilityFinding {
        path: PathBuf::from("Cargo.toml"),
        line,
        kind: FindingKind::NativeSdkFeature,
        what: format!("selects the SDK backend feature `{alias}/{feature}`"),
        help,
    })
}

fn table_path(value: Option<&toml::Value>) -> Option<PathBuf> {
    value?.get("path")?.as_str().map(PathBuf::from)
}

/// Dependency tables that reach shipped code: normal and build dependencies,
/// including target-specific ones. Dev-dependencies only reach tests.
fn shipped_dependency_tables(manifest: &toml::Table) -> Vec<(String, &toml::Table)> {
    let mut tables = Vec::new();
    for name in ["dependencies", "build-dependencies"] {
        if let Some(table) = manifest.get(name).and_then(toml::Value::as_table) {
            tables.push((name.to_owned(), table));
        }
    }
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        for (target, value) in targets {
            for name in ["dependencies", "build-dependencies"] {
                if let Some(table) = value.get(name).and_then(toml::Value::as_table) {
                    tables.push((format!("target.{target}.{name}"), table));
                }
            }
        }
    }
    tables
}

fn workspace_dependencies(app_dir: &Path) -> toml::Table {
    for dir in app_dir.ancestors().skip(1) {
        let Ok(source) = std::fs::read_to_string(dir.join("Cargo.toml")) else {
            continue;
        };
        let Ok(table) = toml::from_str::<toml::Table>(&source) else {
            continue;
        };
        if let Some(workspace) = table.get("workspace") {
            return workspace
                .get("dependencies")
                .and_then(toml::Value::as_table)
                .cloned()
                .unwrap_or_default();
        }
    }
    toml::Table::new()
}

/// The line of `key` inside `[table]` (or of a `[table.key]` header).
fn manifest_key_line(text: &str, table: &str, key: &str) -> usize {
    let normalize = |header: &str| -> String {
        header
            .chars()
            .filter(|c| !matches!(c, '"' | '\'' | ' ' | '\t'))
            .collect()
    };
    let wanted = normalize(table);
    let own_header = format!("{wanted}.{key}");
    let mut current = String::new();
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if let Some(header) = trimmed
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            current = normalize(header);
            if current == own_header {
                return index.saturating_add(1);
            }
            continue;
        }
        if current == wanted
            && let Some(rest) = trimmed
                .strip_prefix(key)
                .or_else(|| trimmed.strip_prefix(&format!("\"{key}\"")))
            && matches!(rest.trim_start().chars().next(), Some('=' | '.'))
        {
            return index.saturating_add(1);
        }
    }
    1
}

/// Every file of the module trees rooted at `roots` with its test-only items
/// removed, skipping modules that only exist under `cfg(test)`.
fn shipped_modules(roots: &[PathBuf]) -> Result<Vec<(PathBuf, TokenStream)>> {
    let mut seen = BTreeSet::new();
    let mut modules = Vec::new();
    let mut queue: VecDeque<(PathBuf, bool)> =
        roots.iter().map(|root| (root.clone(), true)).collect();
    while let Some((file, is_root)) = queue.pop_front() {
        if !seen.insert(file.clone()) {
            continue;
        }
        let source =
            std::fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?;
        let mut syntax =
            syn::parse_file(&source).with_context(|| format!("parse {}", file.display()))?;
        TestPruner.visit_file_mut(&mut syntax);
        let parent = file.parent().unwrap_or_else(|| Path::new("")).to_path_buf();
        let mod_rs = is_root || file.file_name().is_some_and(|name| name == "mod.rs");
        let children = if mod_rs {
            parent.clone()
        } else {
            parent.join(file.file_stem().unwrap_or_default())
        };
        collect_modules(&syntax.items, &parent, &children, false, &mut queue);
        modules.push((file, syntax.into_token_stream()));
    }
    modules.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(modules)
}

fn collect_modules(
    items: &[syn::Item],
    file_dir: &Path,
    children: &Path,
    inline: bool,
    queue: &mut VecDeque<(PathBuf, bool)>,
) {
    for item in items {
        let syn::Item::Mod(module) = item else {
            continue;
        };
        let name = module.ident.to_string();
        let path_attr = module.attrs.iter().find_map(|attr| {
            let syn::Meta::NameValue(value) = &attr.meta else {
                return None;
            };
            if !value.path.is_ident("path") {
                return None;
            }
            let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(path),
                ..
            }) = &value.value
            else {
                return None;
            };
            Some(path.value())
        });
        if let Some((_, nested)) = &module.content {
            let dir = match &path_attr {
                Some(path) => children.join(path),
                None => children.join(&name),
            };
            collect_modules(nested, file_dir, &dir, true, queue);
        } else {
            let candidates = match &path_attr {
                Some(path) => vec![if inline { children } else { file_dir }.join(path)],
                None => vec![
                    children.join(format!("{name}.rs")),
                    children.join(&name).join("mod.rs"),
                ],
            };
            if let Some(file) = candidates.into_iter().find(|path| path.is_file()) {
                queue.push_back((file, false));
            }
        }
    }
}

/// Removes items, statements, fields and arms that only exist in test builds.
struct TestPruner;

fn test_only(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        let path = attr.path();
        if path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "test")
        {
            return true;
        }
        if !path.is_ident("cfg") {
            return false;
        }
        attr.parse_args::<syn::Meta>()
            .is_ok_and(|predicate| requires_test(&predicate))
    })
}

/// Whether a `cfg` predicate can only hold in a test build.
fn requires_test(predicate: &syn::Meta) -> bool {
    match predicate {
        syn::Meta::Path(path) => path.is_ident("test"),
        syn::Meta::List(list) => {
            let Ok(nested) =
                list.parse_args_with(Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
            else {
                return false;
            };
            if list.path.is_ident("all") {
                nested.iter().any(requires_test)
            } else if list.path.is_ident("any") {
                !nested.is_empty() && nested.iter().all(requires_test)
            } else {
                false
            }
        }
        syn::Meta::NameValue(_) => false,
    }
}

impl VisitMut for TestPruner {
    fn visit_file_mut(&mut self, file: &mut syn::File) {
        file.items.retain(|item| !item_test_only(item));
        syn::visit_mut::visit_file_mut(self, file);
    }

    fn visit_item_mod_mut(&mut self, module: &mut syn::ItemMod) {
        if let Some((_, items)) = &mut module.content {
            items.retain(|item| !item_test_only(item));
        }
        syn::visit_mut::visit_item_mod_mut(self, module);
    }

    fn visit_item_impl_mut(&mut self, item: &mut syn::ItemImpl) {
        item.items.retain(|item| {
            !test_only(match item {
                syn::ImplItem::Const(item) => &item.attrs,
                syn::ImplItem::Fn(item) => &item.attrs,
                syn::ImplItem::Type(item) => &item.attrs,
                syn::ImplItem::Macro(item) => &item.attrs,
                _ => &[],
            })
        });
        syn::visit_mut::visit_item_impl_mut(self, item);
    }

    fn visit_item_trait_mut(&mut self, item: &mut syn::ItemTrait) {
        item.items.retain(|item| {
            !test_only(match item {
                syn::TraitItem::Const(item) => &item.attrs,
                syn::TraitItem::Fn(item) => &item.attrs,
                syn::TraitItem::Type(item) => &item.attrs,
                syn::TraitItem::Macro(item) => &item.attrs,
                _ => &[],
            })
        });
        syn::visit_mut::visit_item_trait_mut(self, item);
    }

    fn visit_block_mut(&mut self, block: &mut syn::Block) {
        block.stmts.retain(|stmt| match stmt {
            syn::Stmt::Item(item) => !item_test_only(item),
            syn::Stmt::Local(local) => !test_only(&local.attrs),
            syn::Stmt::Macro(mac) => !test_only(&mac.attrs),
            syn::Stmt::Expr(..) => true,
        });
        syn::visit_mut::visit_block_mut(self, block);
    }

    fn visit_fields_named_mut(&mut self, fields: &mut syn::FieldsNamed) {
        fields.named = std::mem::take(&mut fields.named)
            .into_pairs()
            .filter(|pair| !test_only(&pair.value().attrs))
            .collect();
        syn::visit_mut::visit_fields_named_mut(self, fields);
    }

    fn visit_expr_struct_mut(&mut self, expr: &mut syn::ExprStruct) {
        expr.fields = std::mem::take(&mut expr.fields)
            .into_pairs()
            .filter(|pair| !test_only(&pair.value().attrs))
            .collect();
        syn::visit_mut::visit_expr_struct_mut(self, expr);
    }

    fn visit_item_enum_mut(&mut self, item: &mut syn::ItemEnum) {
        item.variants = std::mem::take(&mut item.variants)
            .into_pairs()
            .filter(|pair| !test_only(&pair.value().attrs))
            .collect();
        syn::visit_mut::visit_item_enum_mut(self, item);
    }

    fn visit_expr_match_mut(&mut self, expr: &mut syn::ExprMatch) {
        expr.arms.retain(|arm| !test_only(&arm.attrs));
        syn::visit_mut::visit_expr_match_mut(self, expr);
    }
}

fn item_test_only(item: &syn::Item) -> bool {
    let attrs: &[syn::Attribute] = match item {
        syn::Item::Const(item) => &item.attrs,
        syn::Item::Enum(item) => &item.attrs,
        syn::Item::ExternCrate(item) => &item.attrs,
        syn::Item::Fn(item) => &item.attrs,
        syn::Item::ForeignMod(item) => &item.attrs,
        syn::Item::Impl(item) => &item.attrs,
        syn::Item::Macro(item) => &item.attrs,
        syn::Item::Mod(item) => &item.attrs,
        syn::Item::Static(item) => &item.attrs,
        syn::Item::Struct(item) => &item.attrs,
        syn::Item::Trait(item) => &item.attrs,
        syn::Item::TraitAlias(item) => &item.attrs,
        syn::Item::Type(item) => &item.attrs,
        syn::Item::Union(item) => &item.attrs,
        syn::Item::Use(item) => &item.attrs,
        _ => &[],
    };
    test_only(attrs)
}

fn is_path_sep(tokens: &[TokenTree], index: usize) -> bool {
    matches!(
        (tokens.get(index), tokens.get(index.saturating_add(1))),
        (Some(TokenTree::Punct(first)), Some(TokenTree::Punct(second)))
            if first.as_char() == ':' && first.spacing() == Spacing::Joint && second.as_char() == ':'
    )
}

fn ident_is(tokens: &[TokenTree], index: usize, name: &str) -> bool {
    matches!(tokens.get(index), Some(TokenTree::Ident(ident)) if ident == name)
}

/// Whether the identifier at `index` continues a path or field access, so it
/// is not a crate name.
fn continues_path(tokens: &[TokenTree], index: usize) -> bool {
    let Some(before) = index.checked_sub(1) else {
        return false;
    };
    match tokens.get(before) {
        Some(TokenTree::Punct(punct)) if punct.as_char() == '.' => true,
        Some(TokenTree::Punct(punct)) if punct.as_char() == ':' => {
            // `::name` at the start of a path is still a crate name.
            before.checked_sub(2).is_some_and(|start| {
                matches!(
                    tokens.get(start),
                    Some(TokenTree::Ident(_) | TokenTree::Group(_))
                )
            })
        }
        _ => false,
    }
}

/// The `a::b::c` path text starting at `index`, at most `segments` long.
fn path_text(tokens: &[TokenTree], mut index: usize, segments: usize) -> String {
    let mut parts = Vec::new();
    while let Some(TokenTree::Ident(ident)) = tokens.get(index) {
        parts.push(ident.to_string());
        if parts.len() == segments || !is_path_sep(tokens, index.saturating_add(1)) {
            break;
        }
        index = index.saturating_add(3);
    }
    parts.join("::")
}

fn contains_ident(stream: &TokenStream, names: &[&str]) -> bool {
    stream.clone().into_iter().any(|token| match token {
        TokenTree::Ident(ident) => names.iter().any(|name| ident == name),
        TokenTree::Group(group) => contains_ident(&group.stream(), names),
        _ => false,
    })
}

/// Whether a `use std::{...}` group names the `os` module at its top level.
fn group_names_os(stream: &TokenStream) -> bool {
    let tokens: Vec<TokenTree> = stream.clone().into_iter().collect();
    (0..tokens.len()).any(|index| ident_is(&tokens, index, "os") && !continues_path(&tokens, index))
}

/// The native-only `std` API the path continuing at `index` (after `std::`)
/// names, if any: a module such as `thread`, a clock such as `time::Instant`,
/// or either inside a `use std::{...}` group.
fn native_only_std(tokens: &[TokenTree], index: usize) -> Option<String> {
    match tokens.get(index)? {
        TokenTree::Ident(module) if NATIVE_ONLY_STD_MODULES.iter().any(|name| module == name) => {
            Some(format!(
                "`std::{module}` has no implementation on the web target"
            ))
        }
        TokenTree::Ident(module)
            if module == "time" && is_path_sep(tokens, index.saturating_add(1)) =>
        {
            match tokens.get(index.saturating_add(3))? {
                TokenTree::Ident(item) if NATIVE_ONLY_STD_TIME.iter().any(|name| item == name) => {
                    Some(format!(
                        "`std::time::{item}` reads a clock the web target lacks"
                    ))
                }
                TokenTree::Group(group) if group.delimiter() == Delimiter::Brace => {
                    let items: Vec<TokenTree> = group.stream().into_iter().collect();
                    (0..items.len())
                        .find_map(|at| {
                            NATIVE_ONLY_STD_TIME.iter().find(|name| {
                                ident_is(&items, at, name) && !continues_path(&items, at)
                            })
                        })
                        .map(|item| {
                            format!("`std::time::{item}` reads a clock the web target lacks")
                        })
                }
                _ => None,
            }
        }
        TokenTree::Group(group) if group.delimiter() == Delimiter::Brace => {
            let items: Vec<TokenTree> = group.stream().into_iter().collect();
            (0..items.len()).find_map(|at| {
                if continues_path(&items, at) {
                    return None;
                }
                native_only_std(&items, at)
            })
        }
        _ => None,
    }
}

fn scan_tokens(
    stream: TokenStream,
    native: &BTreeSet<String>,
    hits: &mut Vec<(usize, FindingKind, String)>,
) {
    let tokens: Vec<TokenTree> = stream.into_iter().collect();
    for (index, token) in tokens.iter().enumerate() {
        let ident = match token {
            TokenTree::Group(group) => {
                scan_tokens(group.stream(), native, hits);
                continue;
            }
            TokenTree::Ident(ident) => ident,
            _ => continue,
        };
        let line = ident.span().start().line;
        let name = ident.to_string();
        if name == "std"
            && !continues_path(&tokens, index)
            && is_path_sep(&tokens, index.saturating_add(1))
        {
            if let Some(what) = native_only_std(&tokens, index.saturating_add(3)) {
                hits.push((line, FindingKind::NativeOnlyStd, what));
            }
            match tokens.get(index.saturating_add(3)) {
                Some(TokenTree::Ident(next)) if next == "os" => hits.push((
                    line,
                    FindingKind::OsStd,
                    format!(
                        "`{}` is an OS-specific standard library module",
                        path_text(&tokens, index, 4)
                    ),
                )),
                Some(TokenTree::Group(group))
                    if group.delimiter() == Delimiter::Brace && group_names_os(&group.stream()) =>
                {
                    hits.push((
                        line,
                        FindingKind::OsStd,
                        "`std::os` is an OS-specific standard library module".to_owned(),
                    ));
                }
                _ => {}
            }
        } else if native.contains(&name)
            && !continues_path(&tokens, index)
            && is_path_sep(&tokens, index.saturating_add(1))
        {
            hits.push((
                line,
                FindingKind::NativeCrate,
                format!(
                    "`{}` calls a native binding crate directly",
                    path_text(&tokens, index, 2)
                ),
            ));
        } else if name == "extern" {
            let mut next = index.saturating_add(1);
            if matches!(tokens.get(next), Some(TokenTree::Literal(_))) {
                next = next.saturating_add(1);
            }
            if matches!(tokens.get(next), Some(TokenTree::Group(group)) if group.delimiter() == Delimiter::Brace)
            {
                hits.push((
                    line,
                    FindingKind::Ffi,
                    "declares a foreign function interface block".to_owned(),
                ));
            }
        } else if (name == "cfg" || name == "cfg_attr") && !continues_path(&tokens, index) {
            let mut next = index.saturating_add(1);
            if matches!(tokens.get(next), Some(TokenTree::Punct(punct)) if punct.as_char() == '!') {
                next = next.saturating_add(1);
            }
            let Some(TokenTree::Group(group)) = tokens.get(next) else {
                continue;
            };
            let predicate = group.stream();
            // The Windows subsystem is a linker setting that must sit on the
            // binary crate root; it selects no implementation.
            if group.delimiter() == Delimiter::Parenthesis
                && contains_ident(&predicate, PLATFORM_PREDICATES)
                && !(name == "cfg_attr" && contains_ident(&predicate, &["windows_subsystem"]))
            {
                hits.push((
                    line,
                    FindingKind::TargetCfg,
                    format!(
                        "`{name}({})` selects code by target OS",
                        compact(&predicate)
                    ),
                ));
            }
        }
    }
}

fn compact(stream: &TokenStream) -> String {
    let text = stream.to_string();
    let text = text
        .replace(" (", "(")
        .replace("( ", "(")
        .replace(" )", ")")
        .replace(" ,", ",");
    if text.chars().count() > 60 {
        format!("{}…", text.chars().take(60).collect::<String>())
    } else {
        text
    }
}
