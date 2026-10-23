//! The `portability` lint: apps call portable APIs, and the platform code
//! behind those APIs lives in Tilo's platform crates.
//!
//! The rules come from `tools/policy/app_portability_rules.toml`, the table
//! the repository's policy gate (`tools/policy/check_app_portability.py`)
//! counts against its baseline. The lint reads an app's shipped Rust sources
//! (the module tree reachable from its library and binary roots, minus
//! `#[cfg(test)]` items) and its Cargo manifest, and reports every site with
//! the rule's remedy. Tests are exempt: they may pin platform behaviour that
//! the platform crates promise.
//!
//! An app whose purpose is platform-specific narrows `[package] platforms` in
//! `tilo-app.toml`, with a `platform-reason`, instead of excusing individual
//! lines. Rules that cannot matter for its targets are skipped: the web
//! run-time traps when web is not a target, platform paths and OS tools for
//! `tilo-os` apps.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context as _, Result};
use proc_macro2::{Delimiter, Spacing, TokenStream, TokenTree};
use quote::ToTokens as _;
use serde::Deserialize;
use syn::punctuated::Punctuated;
use syn::visit_mut::VisitMut;
use tilo_app_package::{Package, Platform};

/// The lint's name, as printed in diagnostics.
pub const LINT: &str = "portability";

/// The rules, shared with the repository's policy gate.
const RULES_TABLE: &str = include_str!("../../../../tools/policy/app_portability_rules.toml");

#[derive(Debug, Deserialize)]
pub(crate) struct RulesTable {
    pub(crate) rules: BTreeMap<String, RuleSpec>,
    #[serde(default)]
    strict: Strict,
}

/// Rules that are errors, whatever their severity, in an app that declares
/// every one of `platforms`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Strict {
    platforms: Vec<String>,
    rules: Vec<String>,
}

impl Strict {
    fn covers(&self, package: &Package, rule: &str) -> bool {
        self.rules.iter().any(|name| name == rule)
            && self.platforms.iter().all(|name| {
                package
                    .platforms()
                    .iter()
                    .any(|platform| platform.as_str() == name)
            })
    }
}

/// One rule of the table. Each rule uses the keys that describe it.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub(crate) struct RuleSpec {
    summary: String,
    help: String,
    applies: Applies,
    severity: Severity,
    predicates: Vec<String>,
    exempt_cfg_attr: Vec<String>,
    cargo_target_sections: bool,
    build_only_target_dependencies: Vec<String>,
    std_modules: Vec<String>,
    paths: Vec<String>,
    extern_blocks: bool,
    native_crates: Vec<String>,
    literal_prefixes: Vec<String>,
    literal_contains: Vec<String>,
    exempt_literal_prefixes: Vec<String>,
    env_functions: Vec<String>,
    env_names: Vec<String>,
    env_prefixes: Vec<String>,
    constructors: Vec<String>,
    crates: Vec<String>,
    allowed_packages: Vec<String>,
    allowed_paths: Vec<String>,
    sdk_packages: Vec<String>,
    features: Vec<String>,
    app_features: Vec<String>,
    replacements: BTreeMap<String, String>,
}

/// Which apps a rule applies to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Applies {
    #[default]
    Always,
    /// Not to apps that run under Tilo OS host services.
    NotTiloOs,
    /// Only to apps that run on the web.
    Web,
}

/// How the lint treats a finding in an app that runs everywhere.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    /// Fails the lint.
    #[default]
    Error,
    /// Reported only, until the replacement exists for every app.
    Warn,
}

impl RuleSpec {
    fn applies_to(&self, package: &Package) -> bool {
        match self.applies {
            Applies::Always => true,
            Applies::NotTiloOs => !package.supports(Platform::TiloOs),
            Applies::Web => package.supports(Platform::Web),
        }
    }
}

/// The parsed rules table.
pub(crate) fn rules() -> Result<&'static RulesTable> {
    static RULES: OnceLock<Result<RulesTable, String>> = OnceLock::new();
    RULES
        .get_or_init(|| toml::from_str(RULES_TABLE).map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|error| anyhow::anyhow!("tools/policy/app_portability_rules.toml: {error}"))
}

impl RulesTable {
    fn rule(&self, id: &str) -> Option<&RuleSpec> {
        self.rules.get(id)
    }

    fn list(&self, id: &str, pick: impl Fn(&RuleSpec) -> &Vec<String>) -> Vec<&str> {
        self.rule(id)
            .map(|rule| pick(rule).iter().map(String::as_str).collect())
            .unwrap_or_default()
    }

    fn help(&self, id: &str) -> String {
        self.rule(id)
            .map(|rule| rule.help.clone())
            .unwrap_or_default()
    }

    fn summary(&self, id: &str) -> &str {
        self.rule(id).map_or("", |rule| rule.summary.as_str())
    }
}

/// What kind of platform-specific code a finding is. Each kind belongs to one
/// rule of the table ([`FindingKind::rule`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FindingKind {
    /// `cfg`, `cfg_attr`, `cfg!` or a Cargo `[target]` section selecting a
    /// target.
    TargetCfg,
    /// A `std::os` module.
    OsStd,
    /// A path into a native binding crate such as `libc::`.
    NativeCrate,
    /// An `extern { ... }` block.
    Ffi,
    /// A platform path or environment variable.
    PlatformPath,
    /// A subprocess naming an OS tool.
    OsTool,
    /// A platform or SDK-internal crate in the manifest's dependencies.
    NativeDependency,
    /// A Tilo crate outside the app SDK and the platform crates.
    InternalDependency,
    /// An SDK feature that names one backend.
    NativeSdkFeature,
    /// An OS thread.
    Thread,
    Clock,
    /// The `std` filesystem or sockets.
    RawIo,
}

impl FindingKind {
    /// The id of the rule in `tools/policy/app_portability_rules.toml`.
    #[must_use]
    pub fn rule(self) -> &'static str {
        match self {
            Self::TargetCfg => "R1",
            Self::OsStd | Self::NativeCrate | Self::Ffi => "R2",
            Self::PlatformPath => "R3",
            Self::OsTool => "R4",
            Self::NativeDependency => "D1",
            Self::InternalDependency => "D2",
            Self::NativeSdkFeature => "F1",
            Self::Thread => "W1",
            Self::Clock => "W2",
            Self::RawIo => "W3",
        }
    }

    /// The kind reported for a rule's `paths` and `std-modules` patterns.
    fn for_pattern_rule(rule: &str) -> Option<Self> {
        match rule {
            "R2" => Some(Self::OsStd),
            "W1" => Some(Self::Thread),
            "W2" => Some(Self::Clock),
            "W3" => Some(Self::RawIo),
            _ => None,
        }
    }
}

/// One platform-specific site in an app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortabilityFinding {
    /// Path relative to the app directory.
    pub path: PathBuf,
    /// One-based line.
    pub line: usize,
    pub kind: FindingKind,
    pub severity: Severity,
    /// What was found, for example "`std::os::unix::fs`".
    pub what: String,
    /// How to fix it.
    pub help: String,
}

/// The result of linting one app.
#[derive(Debug)]
pub struct PortabilityReport {
    pub app_dir: PathBuf,
    pub platforms: Vec<Platform>,
    /// The `package.platform-reason`, when the app narrows its platforms.
    pub platform_reason: Option<String>,
    pub findings: Vec<PortabilityFinding>,
    /// Sites skipped because their rule does not apply to the app's platforms.
    pub exempt: usize,
    /// Sites of the web rules (threads, the std clock, std files and sockets),
    /// whether or not the app declares the web: each makes it native-only.
    pub web: Vec<PortabilityFinding>,
    /// Number of Rust source files inspected.
    pub files: usize,
}

impl PortabilityReport {
    /// Whether the app narrows its platforms with a reason, which declares it
    /// platform-specific.
    #[must_use]
    pub fn declared(&self) -> bool {
        self.platform_reason.is_some()
            && tilo_app_package::DEFAULT_PLATFORMS
                .iter()
                .any(|platform| !self.platforms.contains(platform))
    }

    /// Whether the app passes: an app for every default platform has no
    /// error-level findings; an app that narrows its platforms with a reason
    /// is declared platform-specific and passes.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.declared() || !self.findings.iter().any(|f| f.severity == Severity::Error)
    }

    /// Whether the app can run on the web target: it declares the web, has no
    /// error-level findings, and calls nothing the web rules name, warnings
    /// included.
    #[must_use]
    pub fn web_capable(&self) -> bool {
        self.platforms.contains(&Platform::Web)
            && self.web.is_empty()
            && !self.findings.iter().any(|f| f.severity == Severity::Error)
    }

    /// Why the app cannot run on the web target, one line per site; empty
    /// when it can.
    #[must_use]
    pub fn render_web(&self) -> String {
        let mut out = String::new();
        if !self.platforms.contains(&Platform::Web) {
            let platforms: Vec<&str> = self.platforms.iter().map(|p| p.as_str()).collect();
            let _ = writeln!(
                out,
                "tilo-app.toml's package.platforms leaves out web: [{}]{}",
                platforms.join(", "),
                self.platform_reason
                    .as_deref()
                    .map(|reason| format!(" (\"{reason}\")"))
                    .unwrap_or_default()
            );
        }
        let errors = self
            .findings
            .iter()
            .filter(|finding| finding.severity == Severity::Error);
        let mut seen = BTreeSet::new();
        for finding in errors.chain(&self.web) {
            if seen.insert((&finding.path, finding.line, finding.kind.rule())) {
                let _ = writeln!(
                    out,
                    "{}:{}: {} {}",
                    finding.path.display(),
                    finding.line,
                    finding.kind.rule(),
                    finding.what
                );
            }
        }
        out
    }

    /// A human-readable report: one diagnostic per finding with the lint name,
    /// rule, location, source line and remedy.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = self.render_findings();
        if self.passed() && !self.web.is_empty() && self.platforms.contains(&Platform::Web) {
            let _ = writeln!(
                out,
                "{LINT}: native-only: {} site(s) the web rules name; `tilo-app build --platform \
                 web` refuses the app until they go through the SDK",
                self.web.len()
            );
        }
        out
    }

    fn render_findings(&self) -> String {
        let mut out = String::new();
        let app = self.app_dir.display();
        let (errors, warnings): (Vec<_>, Vec<_>) = self
            .findings
            .iter()
            .partition(|finding| finding.severity == Severity::Error);
        if self.declared() {
            let platforms: Vec<&str> = self.platforms.iter().map(|p| p.as_str()).collect();
            let _ = writeln!(
                out,
                "{LINT}: platforms narrowed to {} in tilo-app.toml: \"{}\" ({} platform-specific site(s))",
                platforms.join(", "),
                self.platform_reason.as_deref().unwrap_or_default(),
                self.findings.len().saturating_add(self.exempt)
            );
            if self.findings.is_empty() && self.exempt == 0 {
                let _ = writeln!(
                    out,
                    "  = note: no platform-specific code found in {app}; widen `platforms` if the \
                     reason no longer holds"
                );
            }
            return out;
        }
        if errors.is_empty() {
            if warnings.is_empty() {
                let _ = writeln!(
                    out,
                    "{LINT}: ok, no platform-specific code in {} source file(s)",
                    self.files
                );
            } else {
                let _ = writeln!(
                    out,
                    "{LINT}: ok in {} source file(s), with {} warning(s) for the web target",
                    self.files,
                    warnings.len()
                );
            }
        }
        for finding in errors.iter().chain(&warnings) {
            self.diagnostic(&mut out, finding);
        }
        if !errors.is_empty() {
            let _ = writeln!(
                out,
                "\n{LINT}: {} platform-specific site(s) in {app}.\n  \
                 Apps call portable APIs; the platform code behind them lives in platform \
                 crates (crates/platform). Tests (tests/, #[cfg(test)]) are exempt.\n  \
                 If the app itself is platform-specific (an OS installer, a PTY terminal), \
                 narrow its platforms once in tilo-app.toml instead:\n\n    \
                 [package]\n    platforms = [\"linux\", \"macos\"]\n    \
                 platform-reason = \"<one-line reason>\"",
                errors.len()
            );
        }
        out
    }

    fn diagnostic(&self, out: &mut String, finding: &PortabilityFinding) {
        let level = match finding.severity {
            Severity::Error => "error",
            Severity::Warn => "warning",
        };
        let _ = writeln!(
            out,
            "{level}[{LINT}/{}]: {}:{}: {}",
            finding.kind.rule(),
            finding.path.display(),
            finding.line,
            finding.what
        );
        if let Some(source) = source_line(&self.app_dir.join(&finding.path), finding.line) {
            let _ = writeln!(out, "  {:>5} | {source}", finding.line);
        }
        if !finding.help.is_empty() {
            let _ = writeln!(out, "  = help: {}", finding.help);
        }
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
    let table = rules()?;
    let manifest_path = app_dir.join("tilo-app.toml");
    let manifest_source = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest = tilo_app_package::Manifest::from_toml_str(&manifest_source)
        .with_context(|| format!("parse {}", manifest_path.display()))?;

    let cargo = CargoFacts::read(app_dir, table)?;
    let mut findings = cargo.findings.clone();
    let scanner = Scanner::new(table, &cargo.native_idents);
    let roots = cargo.roots(app_dir);
    let mut files: usize = 0;
    for (file, tokens) in shipped_modules(&roots)? {
        files = files.saturating_add(1);
        let relative = file.strip_prefix(app_dir).unwrap_or(&file).to_path_buf();
        let mut hits = Vec::new();
        scanner.scan(tokens, &mut hits);
        findings.extend(
            hits.into_iter()
                .map(|(line, kind, what)| PortabilityFinding {
                    path: relative.clone(),
                    line,
                    kind,
                    severity: Severity::Error,
                    help: table.help(kind.rule()),
                    what,
                }),
        );
    }
    let package = &manifest.package;
    let mut web: Vec<PortabilityFinding> = findings
        .iter()
        .filter_map(|finding| {
            let rule = table.rule(finding.kind.rule())?;
            (rule.applies == Applies::Web).then(|| PortabilityFinding {
                severity: rule.severity,
                ..finding.clone()
            })
        })
        .collect();
    let mut exempt: usize = 0;
    findings.retain_mut(|finding| match table.rule(finding.kind.rule()) {
        Some(rule) if rule.applies_to(package) => {
            finding.severity = if table.strict.covers(package, finding.kind.rule()) {
                Severity::Error
            } else {
                rule.severity
            };
            true
        }
        _ => {
            exempt = exempt.saturating_add(1);
            false
        }
    });
    for list in [&mut findings, &mut web] {
        list.sort_by(|a, b| {
            (&a.path, a.line, a.kind.rule(), a.kind).cmp(&(&b.path, b.line, b.kind.rule(), b.kind))
        });
        list.dedup_by(|a, b| {
            a.path == b.path && a.line == b.line && a.kind.rule() == b.kind.rule()
        });
    }
    Ok(PortabilityReport {
        app_dir: app_dir.to_path_buf(),
        platforms: package.platforms().to_vec(),
        platform_reason: package.platform_reason.clone(),
        findings,
        exempt,
        web,
        files,
    })
}

/// The app's shipped module trees (library and binaries), test items
/// removed, for other checks that read the same code as the lint.
///
/// # Errors
///
/// When the Cargo manifest or a reachable source file cannot be read or parsed.
pub(crate) fn shipped_sources(app_dir: &Path) -> Result<Vec<(PathBuf, TokenStream)>> {
    let cargo = CargoFacts::read(app_dir, rules()?)?;
    shipped_modules(&cargo.roots(app_dir))
}

/// Every Tilo app that is a member of the workspace containing `start`.
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
    fn read(app_dir: &Path, table: &RulesTable) -> Result<Self> {
        let path = app_dir.join("Cargo.toml");
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let manifest: toml::Table =
            toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
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
        facts.target_sections(&manifest, &text, table);
        let sdk_aliases = facts.dependencies(app_dir, &manifest, &text, table);
        facts.features(&manifest, &text, table, &sdk_aliases);
        Ok(facts)
    }

    fn push(&mut self, line: usize, kind: FindingKind, what: String, help: String) {
        self.findings.push(PortabilityFinding {
            path: PathBuf::from("Cargo.toml"),
            line,
            kind,
            severity: Severity::Error,
            what,
            help,
        });
    }

    /// `[target.'cfg(...)']` and `[target.<triple>]` sections.
    fn target_sections(&mut self, manifest: &toml::Table, text: &str, table: &RulesTable) {
        if !table
            .rule("R1")
            .is_some_and(|rule| rule.cargo_target_sections)
        {
            return;
        }
        let predicates = table.list("R1", |rule| &rule.predicates);
        let build_only = table.list("R1", |rule| &rule.build_only_target_dependencies);
        let targets = manifest.get("target").and_then(toml::Value::as_table);
        for (target, tables) in targets.into_iter().flatten() {
            let selects = !target.starts_with("cfg(")
                || predicates
                    .iter()
                    .any(|predicate| contains_word(target, predicate));
            let named: Vec<&String> = ["dependencies", "build-dependencies"]
                .iter()
                .filter_map(|kind| tables.get(*kind).and_then(toml::Value::as_table))
                .flat_map(toml::Table::keys)
                .collect();
            // Target-specific dev-dependencies only reach tests, which are exempt.
            let tests_only = named.is_empty() && tables.get("dev-dependencies").is_some();
            let plumbing = tests_only
                || (!named.is_empty()
                    && named
                        .iter()
                        .all(|alias| build_only.contains(&alias.as_str())));
            if selects && !plumbing {
                self.push(
                    header_line(text, target),
                    FindingKind::TargetCfg,
                    format!("`[target.{target}]` selects dependencies by target"),
                    table.help("R1"),
                );
            }
        }
    }

    /// Dependency findings; returns the aliases of the app SDK dependency.
    fn dependencies(
        &mut self,
        app_dir: &Path,
        manifest: &toml::Table,
        text: &str,
        table: &RulesTable,
    ) -> BTreeSet<String> {
        let (workspace_dir, workspace_deps) = workspace(app_dir);
        let native_crates = table.list("R2", |rule| &rule.native_crates);
        let d1 = table.list("D1", |rule| &rule.crates);
        let sdk_packages = table.list("F1", |rule| &rule.sdk_packages);
        let mut sdk_aliases = BTreeSet::new();
        for (table_name, deps) in shipped_dependency_tables(manifest) {
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
                let line = || manifest_key_line(text, &table_name, alias);
                if native_crates.contains(&package) {
                    self.native_idents.insert(alias.replace('-', "_"));
                }
                if d1.iter().any(|pattern| wildcard(pattern, package)) {
                    self.push(
                        line(),
                        FindingKind::NativeDependency,
                        format!("depends on the platform crate `{package}`"),
                        table.help("D1"),
                    );
                }
                let path = spec
                    .get("path")
                    .and_then(toml::Value::as_str)
                    .map(|path| app_dir.join(path))
                    .or_else(|| {
                        let path = inherited?.get("path")?.as_str()?;
                        Some(workspace_dir.as_deref()?.join(path))
                    });
                if let Some(where_) = path.and_then(|path| {
                    internal_dependency(table, package, &path, workspace_dir.as_deref())
                }) {
                    self.push(
                        line(),
                        FindingKind::InternalDependency,
                        format!("depends on the Tilo crate `{package}` ({where_})"),
                        table.help("D2"),
                    );
                }
                if !sdk_packages.contains(&package) {
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
                    if let Some((what, help)) = sdk_feature(table.rule("F1"), alias, feature) {
                        self.push(line(), FindingKind::NativeSdkFeature, what, help);
                    }
                }
            }
        }
        sdk_aliases
    }

    /// App features that select SDK backends or are backend selections.
    fn features(
        &mut self,
        manifest: &toml::Table,
        text: &str,
        table: &RulesTable,
        sdk_aliases: &BTreeSet<String>,
    ) {
        let Some(features) = manifest.get("features").and_then(toml::Value::as_table) else {
            return;
        };
        let f1 = table.rule("F1");
        for (name, selections) in features {
            if f1.is_some_and(|rule| rule.app_features.contains(name)) {
                self.push(
                    manifest_key_line(text, "features", name),
                    FindingKind::NativeSdkFeature,
                    format!("declares the backend feature `{name}`"),
                    table.help("F1"),
                );
            }
            let selections = selections.as_array().into_iter().flatten();
            for selection in selections.filter_map(toml::Value::as_str) {
                let Some((alias, feature)) = selection.split_once('/') else {
                    continue;
                };
                if !sdk_aliases.contains(alias.trim_end_matches('?')) {
                    continue;
                }
                let line = text
                    .lines()
                    .position(|line| line.contains(&format!("\"{selection}\"")))
                    .map_or(1, |index| index.saturating_add(1));
                if let Some((what, help)) = sdk_feature(f1, alias, feature) {
                    self.push(line, FindingKind::NativeSdkFeature, what, help);
                }
            }
        }
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

/// The finding for SDK feature `feature` selected through `alias`, if it names
/// one backend.
fn sdk_feature(rule: Option<&RuleSpec>, alias: &str, feature: &str) -> Option<(String, String)> {
    let rule = rule?;
    if !rule.features.iter().any(|name| name == feature) {
        return None;
    }
    let help = match rule.replacements.get(feature) {
        Some(semantic) => format!(
            "enable the semantic SDK feature `{alias}/{semantic}` instead; the SDK selects the \
             backend for each target"
        ),
        None => rule.help.clone(),
    };
    Some((
        format!("selects the SDK backend feature `{alias}/{feature}`"),
        help,
    ))
}

/// Where the path dependency `package` in `dir` lives, relative to the
/// workspace, when rule D2 does not allow it.
fn internal_dependency(
    table: &RulesTable,
    package: &str,
    dir: &Path,
    workspace_dir: Option<&Path>,
) -> Option<String> {
    let rule = table.rule("D2")?;
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let relative = workspace_dir
        .and_then(|root| std::fs::canonicalize(root).ok())
        .and_then(|root| dir.strip_prefix(root).ok().map(Path::to_path_buf))
        .unwrap_or(dir);
    let relative = relative.to_string_lossy().replace('\\', "/");
    let allowed = rule.allowed_packages.iter().any(|name| name == package)
        || rule
            .allowed_paths
            .iter()
            .any(|glob| wildcard(glob, &relative));
    (!allowed).then_some(relative)
}

/// `pattern` matches `text`, where `*` matches any run of characters.
pub(crate) fn wildcard(pattern: &str, text: &str) -> bool {
    let mut parts = pattern.split('*');
    let Some(first) = parts.next() else {
        return text.is_empty();
    };
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };
    let parts: Vec<&str> = parts.collect();
    let Some((last, middle)) = parts.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(index) => rest = rest.get(index.saturating_add(part.len())..).unwrap_or(""),
            None => return false,
        }
    }
    rest.ends_with(last)
}

/// Whether `text` contains `word` delimited by non-identifier characters.
fn contains_word(text: &str, word: &str) -> bool {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|part| part == word)
}

/// The line of the first table header that names `target`.
fn header_line(text: &str, target: &str) -> usize {
    text.lines()
        .position(|line| line.trim_start().starts_with('[') && line.contains(target))
        .map_or(1, |index| index.saturating_add(1))
}

/// The workspace root above `app_dir` and its `[workspace.dependencies]`.
pub(crate) fn workspace(app_dir: &Path) -> (Option<PathBuf>, toml::Table) {
    for dir in app_dir.ancestors().skip(1) {
        let Ok(source) = std::fs::read_to_string(dir.join("Cargo.toml")) else {
            continue;
        };
        let Ok(table) = toml::from_str::<toml::Table>(&source) else {
            continue;
        };
        if let Some(workspace) = table.get("workspace") {
            let deps = workspace
                .get("dependencies")
                .and_then(toml::Value::as_table)
                .cloned()
                .unwrap_or_default();
            return (Some(dir.to_path_buf()), deps);
        }
    }
    (None, toml::Table::new())
}

fn table_path(value: Option<&toml::Value>) -> Option<PathBuf> {
    value?.get("path")?.as_str().map(PathBuf::from)
}

/// Dependency tables that reach shipped code: normal and build dependencies,
/// including target-specific ones. Dev-dependencies only reach tests.
pub(crate) fn shipped_dependency_tables(manifest: &toml::Table) -> Vec<(String, &toml::Table)> {
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

/// The line of `key` inside `[table]` (or of a `[table.key]` header).
pub(crate) fn manifest_key_line(text: &str, table: &str, key: &str) -> usize {
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
pub(crate) fn shipped_modules(roots: &[PathBuf]) -> Result<Vec<(PathBuf, TokenStream)>> {
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
        if let syn::Item::Macro(item) = item
            && item.mac.path.is_ident("include")
            && let Ok(path) = item.mac.parse_body::<syn::LitStr>()
        {
            // `include!` paths are relative to the including file.
            let file = file_dir.join(path.value());
            if file.is_file() {
                queue.push_back((file, false));
            }
            continue;
        }
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
            syn::Stmt::Expr(expr, _) => !test_only(expr_attrs(expr)),
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

/// The outer attributes of an expression used as a statement.
fn expr_attrs(expr: &syn::Expr) -> &[syn::Attribute] {
    match expr {
        syn::Expr::Assign(expr) => &expr.attrs,
        syn::Expr::Block(expr) => &expr.attrs,
        syn::Expr::Call(expr) => &expr.attrs,
        syn::Expr::ForLoop(expr) => &expr.attrs,
        syn::Expr::If(expr) => &expr.attrs,
        syn::Expr::Loop(expr) => &expr.attrs,
        syn::Expr::Macro(expr) => &expr.attrs,
        syn::Expr::Match(expr) => &expr.attrs,
        syn::Expr::MethodCall(expr) => &expr.attrs,
        syn::Expr::Unsafe(expr) => &expr.attrs,
        syn::Expr::While(expr) => &expr.attrs,
        _ => &[],
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

pub(crate) fn is_path_sep(tokens: &[TokenTree], index: usize) -> bool {
    matches!(
        (tokens.get(index), tokens.get(index.saturating_add(1))),
        (Some(TokenTree::Punct(first)), Some(TokenTree::Punct(second)))
            if first.as_char() == ':' && first.spacing() == Spacing::Joint && second.as_char() == ':'
    )
}

pub(crate) fn ident_is(tokens: &[TokenTree], index: usize, name: &str) -> bool {
    matches!(tokens.get(index), Some(TokenTree::Ident(ident)) if ident == name)
}

/// Whether the identifier at `index` continues a path or field access, so it
/// is not a crate name.
pub(crate) fn continues_path(tokens: &[TokenTree], index: usize) -> bool {
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

/// Whether a `use std::{...}` group names `module` at its top level.
pub(crate) fn group_names(stream: &TokenStream, module: &str) -> bool {
    let tokens: Vec<TokenTree> = stream.clone().into_iter().collect();
    (0..tokens.len())
        .any(|index| ident_is(&tokens, index, module) && !continues_path(&tokens, index))
}

/// Whether the token at `index` follows a `.` (a method or field, not a path).
pub(crate) fn follows_dot(tokens: &[TokenTree], index: usize) -> bool {
    index
        .checked_sub(1)
        .and_then(|before| tokens.get(before))
        .is_some_and(|token| matches!(token, TokenTree::Punct(punct) if punct.as_char() == '.'))
}

/// Whether the path `segments` starts at `index`; returns the index after it.
pub(crate) fn path_at(tokens: &[TokenTree], index: usize, segments: &[&str]) -> Option<usize> {
    let mut at = index;
    for (position, segment) in segments.iter().enumerate() {
        if position > 0 {
            if !is_path_sep(tokens, at) {
                return None;
            }
            at = at.saturating_add(2);
        }
        if !ident_is(tokens, at, segment) {
            return None;
        }
        at = at.saturating_add(1);
    }
    Some(at)
}

/// The value of a string literal token (not byte or C strings).
fn string_value(literal: &proc_macro2::Literal) -> Option<String> {
    let text = literal.to_string();
    if !(text.starts_with('"') || text.starts_with('r')) {
        return None;
    }
    syn::parse_str::<syn::LitStr>(&text)
        .ok()
        .map(|lit| lit.value())
}

/// The string literal that is the first argument of the call group at `index`.
fn first_string_argument(tokens: &[TokenTree], index: usize) -> Option<String> {
    let Some(TokenTree::Group(group)) = tokens.get(index) else {
        return None;
    };
    if group.delimiter() != Delimiter::Parenthesis {
        return None;
    }
    match group.stream().into_iter().next() {
        Some(TokenTree::Literal(literal)) => string_value(&literal),
        _ => None,
    }
}

/// Whether the bracket group at `index` is a `#[doc = ...]` attribute, which is
/// how doc comments reach the token stream.
pub(crate) fn is_doc_attribute(tokens: &[TokenTree], index: usize) -> bool {
    let Some(TokenTree::Group(group)) = tokens.get(index) else {
        return false;
    };
    let after_hash = index
        .checked_sub(1)
        .and_then(|before| tokens.get(before))
        .is_some_and(|token| {
            matches!(token, TokenTree::Punct(punct) if matches!(punct.as_char(), '#' | '!'))
        });
    after_hash
        && group.delimiter() == Delimiter::Bracket
        && matches!(group.stream().into_iter().next(), Some(TokenTree::Ident(ident)) if ident == "doc")
}

type Hit = (usize, FindingKind, String);

/// The source-level rules of the table, ready to match tokens.
struct Scanner<'a> {
    table: &'a RulesTable,
    native: &'a BTreeSet<String>,
    predicates: Vec<&'a str>,
    exempt_cfg_attr: Vec<&'a str>,
    extern_blocks: bool,
    /// `(module, kind, rule)` for each `std-modules` entry.
    std_modules: Vec<(&'a str, FindingKind, &'a str)>,
    /// `(segments, kind, rule)` for each `paths` entry.
    paths: Vec<(Vec<&'a str>, FindingKind, &'a str)>,
    literal_prefixes: Vec<&'a str>,
    literal_contains: Vec<&'a str>,
    exempt_literal_prefixes: Vec<&'a str>,
    env_functions: Vec<Vec<&'a str>>,
    env_names: Vec<&'a str>,
    env_prefixes: Vec<&'a str>,
    constructors: Vec<Vec<&'a str>>,
}

impl<'a> Scanner<'a> {
    fn new(table: &'a RulesTable, native: &'a BTreeSet<String>) -> Self {
        let mut std_modules = Vec::new();
        let mut paths = Vec::new();
        for (id, rule) in &table.rules {
            let Some(kind) = FindingKind::for_pattern_rule(id) else {
                continue;
            };
            for module in &rule.std_modules {
                std_modules.push((module.as_str(), kind, id.as_str()));
            }
            for path in &rule.paths {
                paths.push((path.split("::").collect(), kind, id.as_str()));
            }
        }
        let split = |id: &str, pick: fn(&RuleSpec) -> &Vec<String>| -> Vec<Vec<&'a str>> {
            table
                .list(id, pick)
                .into_iter()
                .map(|path| path.split("::").collect())
                .collect()
        };
        Self {
            table,
            native,
            predicates: table.list("R1", |rule| &rule.predicates),
            exempt_cfg_attr: table.list("R1", |rule| &rule.exempt_cfg_attr),
            extern_blocks: table.rule("R2").is_some_and(|rule| rule.extern_blocks),
            std_modules,
            paths,
            literal_prefixes: table.list("R3", |rule| &rule.literal_prefixes),
            literal_contains: table.list("R3", |rule| &rule.literal_contains),
            exempt_literal_prefixes: table.list("R3", |rule| &rule.exempt_literal_prefixes),
            env_functions: split("R3", |rule| &rule.env_functions),
            env_names: table.list("R3", |rule| &rule.env_names),
            env_prefixes: table.list("R3", |rule| &rule.env_prefixes),
            constructors: split("R4", |rule| &rule.constructors),
        }
    }

    fn scan(&self, stream: TokenStream, hits: &mut Vec<Hit>) {
        let tokens: Vec<TokenTree> = stream.into_iter().collect();
        for (index, token) in tokens.iter().enumerate() {
            let ident = match token {
                TokenTree::Group(group) => {
                    if !is_doc_attribute(&tokens, index) {
                        self.scan(group.stream(), hits);
                    }
                    continue;
                }
                TokenTree::Literal(literal) => {
                    self.scan_literal(literal, hits);
                    continue;
                }
                TokenTree::Ident(ident) => ident,
                TokenTree::Punct(_) => continue,
            };
            let line = ident.span().start().line;
            let name = ident.to_string();
            let path_start = !continues_path(&tokens, index);
            if name == "std" && path_start && is_path_sep(&tokens, index.saturating_add(1)) {
                self.scan_std(&tokens, index, line, hits);
            } else if self.native.contains(&name)
                && path_start
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
            } else if name == "extern" && self.extern_blocks {
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
            } else if (name == "cfg" || name == "cfg_attr") && path_start {
                self.scan_cfg(&tokens, index, &name, line, hits);
            }
            if follows_dot(&tokens, index) {
                continue;
            }
            for (segments, kind, rule) in &self.paths {
                if path_at(&tokens, index, segments).is_some() {
                    hits.push((
                        line,
                        *kind,
                        format!("`{}` {}", segments.join("::"), self.table.summary(rule)),
                    ));
                }
            }
            for function in &self.env_functions {
                let Some(call) = path_at(&tokens, index, function) else {
                    continue;
                };
                let Some(variable) = first_string_argument(&tokens, call) else {
                    continue;
                };
                if self.env_names.contains(&variable.as_str())
                    || self
                        .env_prefixes
                        .iter()
                        .any(|prefix| variable.starts_with(prefix))
                {
                    hits.push((
                        line,
                        FindingKind::PlatformPath,
                        format!(
                            "`{}(\"{variable}\")` reads a platform environment variable",
                            function.join("::")
                        ),
                    ));
                }
            }
            for constructor in &self.constructors {
                let Some(call) = path_at(&tokens, index, constructor) else {
                    continue;
                };
                if let Some(program) = first_string_argument(&tokens, call) {
                    hits.push((
                        line,
                        FindingKind::OsTool,
                        format!(
                            "`{}(\"{program}\")` runs an OS tool",
                            constructor.join("::")
                        ),
                    ));
                }
            }
        }
    }

    fn scan_std(&self, tokens: &[TokenTree], index: usize, line: usize, hits: &mut Vec<Hit>) {
        let describe = |module: &str, kind: FindingKind, rule: &str, full: String| {
            if kind == FindingKind::OsStd {
                format!("`{full}` is an OS-specific standard library module")
            } else {
                format!("`std::{module}` {}", self.table.summary(rule))
            }
        };
        match tokens.get(index.saturating_add(3)) {
            Some(TokenTree::Ident(next)) => {
                for (module, kind, rule) in &self.std_modules {
                    if next == module {
                        let full = path_text(tokens, index, 4);
                        hits.push((line, *kind, describe(module, *kind, rule, full)));
                    }
                }
            }
            Some(TokenTree::Group(group)) if group.delimiter() == Delimiter::Brace => {
                for (module, kind, rule) in &self.std_modules {
                    if group_names(&group.stream(), module) {
                        let full = format!("std::{module}");
                        hits.push((line, *kind, describe(module, *kind, rule, full)));
                    }
                }
            }
            _ => {}
        }
    }

    fn scan_cfg(
        &self,
        tokens: &[TokenTree],
        index: usize,
        name: &str,
        line: usize,
        hits: &mut Vec<Hit>,
    ) {
        let mut next = index.saturating_add(1);
        if matches!(tokens.get(next), Some(TokenTree::Punct(punct)) if punct.as_char() == '!') {
            next = next.saturating_add(1);
        }
        let Some(TokenTree::Group(group)) = tokens.get(next) else {
            return;
        };
        let predicate = group.stream();
        // Linker settings such as the Windows subsystem sit on the binary
        // crate root and select no implementation.
        if group.delimiter() == Delimiter::Parenthesis
            && contains_ident(&predicate, &self.predicates)
            && !(name == "cfg_attr" && contains_ident(&predicate, &self.exempt_cfg_attr))
        {
            hits.push((
                line,
                FindingKind::TargetCfg,
                format!("`{name}({})` selects code by target", compact(&predicate)),
            ));
        }
    }

    fn scan_literal(&self, literal: &proc_macro2::Literal, hits: &mut Vec<Hit>) {
        if self.literal_prefixes.is_empty() && self.literal_contains.is_empty() {
            return;
        }
        let Some(value) = string_value(literal) else {
            return;
        };
        if self
            .exempt_literal_prefixes
            .iter()
            .any(|prefix| value.starts_with(prefix))
        {
            return;
        }
        let platform = self
            .literal_prefixes
            .iter()
            .any(|prefix| value.starts_with(prefix))
            || self
                .literal_contains
                .iter()
                .any(|part| value.contains(part));
        if platform {
            let shown: String = value.chars().take(60).collect();
            hits.push((
                literal.span().start().line,
                FindingKind::PlatformPath,
                format!("\"{shown}\" names a platform path"),
            ));
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
