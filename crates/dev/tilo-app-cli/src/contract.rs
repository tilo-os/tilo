//! The app contract `tilo-app check` holds an app to beyond compiling: what
//! its sources promise the framework and the people reading it.
//!
//! - Verbs (`#[action]` methods of a `#[resource]` impl) run again on every
//!   replica, so their bodies may not read a clock, randomness, files, the
//!   network, the environment or other processes. What they need from
//!   outside arrives as a parameter or through `ActionCx`.
//! - Every label key a view names literally is a message of
//!   `locales/en-US.ftl`, and every shipped translation has every message of
//!   the source catalog.
//!
//! The checks read the same shipped module trees as the portability lint.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use proc_macro2::TokenStream;
use syn::spanned::Spanned as _;
use syn::visit_mut::VisitMut;

/// The check's name, as printed in diagnostics.
pub const CHECK: &str = "app contract";

/// How much a finding matters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// Fails the check.
    Error,
    /// Reported; the check still passes.
    Warning,
}

/// One broken promise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContractFinding {
    /// Path relative to the app directory.
    pub path: PathBuf,
    /// One-based line, `0` for a whole file.
    pub line: usize,
    pub level: Level,
    /// The rule, for example `verb-determinism`.
    pub rule: &'static str,
    /// What was found and how to fix it.
    pub message: String,
}

/// The findings for one app.
#[derive(Debug, Default)]
pub struct ContractReport {
    pub findings: Vec<ContractFinding>,
}

impl ContractReport {
    /// Whether no finding fails the check.
    #[must_use]
    pub fn passed(&self) -> bool {
        !self.findings.iter().any(|f| f.level == Level::Error)
    }

    /// One line per finding, or a pass line.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        for finding in &self.findings {
            let level = match finding.level {
                Level::Error => "error",
                Level::Warning => "warning",
            };
            let _ = writeln!(
                out,
                "{level}[{}]: {}:{}: {}",
                finding.rule,
                finding.path.display(),
                finding.line,
                finding.message
            );
        }
        if self.findings.is_empty() {
            out.push_str("app contract holds\n");
        }
        out
    }

    /// The findings as JSON.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::Value::Array(
            self.findings
                .iter()
                .map(|finding| {
                    serde_json::json!({
                        "path": finding.path,
                        "line": finding.line,
                        "level": match finding.level {
                            Level::Error => "error",
                            Level::Warning => "warning",
                        },
                        "rule": finding.rule,
                        "message": finding.message,
                    })
                })
                .collect(),
        )
    }
}

/// Check the source contract of the app containing `start`.
///
/// # Errors
///
/// Returns an error when `start` is not inside an app, or the app's sources
/// or catalogs cannot be read or parsed.
pub fn check_app(start: &Path) -> Result<ContractReport> {
    let app_dir = &crate::resolve_app_dir(start)?;
    let mut report = ContractReport::default();
    let sources = crate::portability::shipped_sources(app_dir)?;
    let catalog = source_catalog(app_dir)?;
    for (path, tokens) in &sources {
        let relative = path.strip_prefix(app_dir).unwrap_or(path).to_path_buf();
        report
            .findings
            .extend(check_source(&relative, tokens.clone(), catalog.as_ref())?);
    }
    if let Some(source) = &catalog {
        report.findings.extend(check_translations(app_dir, source)?);
    }
    Ok(report)
}

/// The message ids of `locales/en-US.ftl`, when the app has one.
pub(crate) fn source_catalog(app_dir: &Path) -> Result<Option<BTreeSet<String>>> {
    let path = app_dir.join("locales/en-US.ftl");
    if !path.is_file() {
        return Ok(None);
    }
    let source =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    Ok(Some(
        message_ids(&source).with_context(|| format!("parse {}", path.display()))?,
    ))
}

fn message_ids(source: &str) -> Result<BTreeSet<String>> {
    use fluent_syntax::ast::Entry;
    let resource = fluent_syntax::parser::parse(source)
        .map_err(|(_, errors)| anyhow::anyhow!("{errors:?}"))?;
    Ok(resource
        .body
        .iter()
        .filter_map(|entry| match entry {
            Entry::Message(message) => Some(message.id.name.to_owned()),
            _ => None,
        })
        .collect())
}

/// Every shipped translation carries every message of the source catalog.
fn check_translations(app_dir: &Path, source: &BTreeSet<String>) -> Result<Vec<ContractFinding>> {
    let mut findings = Vec::new();
    let Ok(entries) = std::fs::read_dir(app_dir.join("locales")) else {
        return Ok(findings);
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "ftl"))
        .filter(|path| path.file_stem().is_some_and(|stem| stem != "en-US"))
        .collect();
    files.sort();
    for path in files {
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let ids = message_ids(&text).with_context(|| format!("parse {}", path.display()))?;
        let missing: Vec<&str> = source
            .iter()
            .filter(|id| !ids.contains(*id))
            .map(String::as_str)
            .collect();
        if !missing.is_empty() {
            findings.push(ContractFinding {
                path: path.strip_prefix(app_dir).unwrap_or(&path).to_path_buf(),
                line: 0,
                level: Level::Error,
                rule: "locale-complete",
                message: format!(
                    "missing {}; `tilo-app locale sync` copies them from en-US for translation",
                    missing.join(", ")
                ),
            });
        }
    }
    Ok(findings)
}

/// The findings of one shipped source file.
///
/// # Errors
///
/// Returns an error when `tokens` is not a Rust file.
pub fn check_source(
    path: &Path,
    tokens: TokenStream,
    catalog: Option<&BTreeSet<String>>,
) -> Result<Vec<ContractFinding>> {
    let mut file: syn::File =
        syn::parse2(tokens).with_context(|| format!("parse {}", path.display()))?;
    let mut visitor = Visitor {
        path,
        catalog,
        findings: Vec::new(),
    };
    visitor.visit_file_mut(&mut file);
    Ok(visitor.findings)
}

struct Visitor<'a> {
    path: &'a Path,
    catalog: Option<&'a BTreeSet<String>>,
    findings: Vec<ContractFinding>,
}

fn has_attribute(attrs: &[syn::Attribute], name: &str) -> bool {
    attrs.iter().any(|attr| {
        attr.path()
            .segments
            .last()
            .is_some_and(|segment| segment.ident == name)
    })
}

impl VisitMut for Visitor<'_> {
    fn visit_item_impl_mut(&mut self, item: &mut syn::ItemImpl) {
        if has_attribute(&item.attrs, "resource") {
            for member in &mut item.items {
                let syn::ImplItem::Fn(method) = member else {
                    continue;
                };
                if !has_attribute(&method.attrs, "action") {
                    continue;
                }
                let mut verb = VerbBody {
                    verb: method.sig.ident.to_string(),
                    found: Vec::new(),
                };
                verb.visit_block_mut(&mut method.block);
                for (line, what, remedy) in verb.found {
                    self.findings.push(ContractFinding {
                        path: self.path.to_path_buf(),
                        line,
                        level: Level::Error,
                        rule: "verb-determinism",
                        message: format!(
                            "verb `{}` uses {what}: verbs run again on every replica, so {remedy}",
                            verb.verb
                        ),
                    });
                }
            }
        }
        syn::visit_mut::visit_item_impl_mut(self, item);
    }

    fn visit_expr_method_call_mut(&mut self, call: &mut syn::ExprMethodCall) {
        let method = call.method.to_string();
        let position = match method.as_str() {
            "button" | "checkbox" | "effect_button" | "with_placeholder" => Some(0),
            _ => None,
        };
        if let (Some(catalog), Some(position)) = (self.catalog, position)
            && let Some(syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(literal),
                ..
            })) = call.args.iter().nth(position)
        {
            let key = literal.value();
            if looks_like_key(&key) && !catalog.contains(&key) {
                self.findings.push(ContractFinding {
                    path: self.path.to_path_buf(),
                    line: literal.span().start().line,
                    level: Level::Error,
                    rule: "label-key",
                    message: format!(
                        "`{method}` names the message {key:?}, which locales/en-US.ftl does not have; add it there or use the generated `locales::keys` constant"
                    ),
                });
            }
        }
        syn::visit_mut::visit_expr_method_call_mut(self, call);
    }
}

/// Whether a label is written as a catalog message id: lowercase words
/// joined by hyphens, as every app's messages are named.
fn looks_like_key(label: &str) -> bool {
    label.contains('-')
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// What a verb body may not reach, as path suffixes, with the remedy.
const FORBIDDEN: &[(&[&str], &str, &str)] = &[
    (
        &["SystemTime", "now"],
        "the system clock",
        "read the time from `cx.now_ms()`",
    ),
    (
        &["Instant", "now"],
        "the monotonic clock",
        "read the time from `cx.now_ms()`",
    ),
    (
        &["Utc", "now"],
        "the clock",
        "read the time from `cx.now_ms()`",
    ),
    (
        &["Local", "now"],
        "the clock",
        "read the time from `cx.now_ms()`",
    ),
    (
        &["OffsetDateTime", "now_utc"],
        "the clock",
        "read the time from `cx.now_ms()`",
    ),
    (
        &["tilo_clock"],
        "the clock",
        "read the time from `cx.now_ms()`",
    ),
    (&["rand"], "randomness", "mint ids with `cx.id()`"),
    (&["thread_rng"], "randomness", "mint ids with `cx.id()`"),
    (&["fastrand"], "randomness", "mint ids with `cx.id()`"),
    (&["getrandom"], "randomness", "mint ids with `cx.id()`"),
    (&["Uuid", "new_v4"], "randomness", "mint ids with `cx.id()`"),
    (&["Id", "random"], "randomness", "mint ids with `cx.id()`"),
    (
        &["fs"],
        "the file system",
        "pass file contents in as a parameter or queue the write in an `Outbox`",
    ),
    (
        &["File", "open"],
        "the file system",
        "pass file contents in as a parameter or queue the write in an `Outbox`",
    ),
    (
        &["File", "create"],
        "the file system",
        "pass file contents in as a parameter or queue the write in an `Outbox`",
    ),
    (
        &["OpenOptions"],
        "the file system",
        "pass file contents in as a parameter or queue the write in an `Outbox`",
    ),
    (
        &["tilo_files"],
        "the file system",
        "pass file contents in as a parameter or queue the write in an `Outbox`",
    ),
    (
        &["net"],
        "the network",
        "queue the request in an `Outbox` and record its answer with an action",
    ),
    (
        &["TcpStream"],
        "the network",
        "queue the request in an `Outbox` and record its answer with an action",
    ),
    (
        &["UdpSocket"],
        "the network",
        "queue the request in an `Outbox` and record its answer with an action",
    ),
    (
        &["reqwest"],
        "the network",
        "queue the request in an `Outbox` and record its answer with an action",
    ),
    (
        &["ureq"],
        "the network",
        "queue the request in an `Outbox` and record its answer with an action",
    ),
    (
        &["process"],
        "another process",
        "queue the work in an `Outbox` and record its outcome with an action",
    ),
    (
        &["Command", "new"],
        "another process",
        "queue the work in an `Outbox` and record its outcome with an action",
    ),
    (
        &["env", "var"],
        "the environment",
        "pass the value in as a parameter",
    ),
    (
        &["env", "var_os"],
        "the environment",
        "pass the value in as a parameter",
    ),
    (
        &["env", "vars"],
        "the environment",
        "pass the value in as a parameter",
    ),
];

struct VerbBody {
    verb: String,
    found: Vec<(usize, &'static str, &'static str)>,
}

impl VerbBody {
    fn check_path(&mut self, path: &syn::Path) {
        let segments: Vec<String> = path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect();
        for (pattern, what, remedy) in FORBIDDEN {
            let hit = segments
                .windows(pattern.len())
                .any(|window| window.iter().zip(pattern.iter()).all(|(a, b)| a == b));
            // A lone `fs`, `net`, `process` or `rand` segment only counts as
            // a module on the way to something, not as a local's name.
            let module_only = matches!(*pattern, ["fs" | "net" | "process" | "rand"]);
            if hit && (!module_only || segments.len() > 1) {
                self.found.push((path.span().start().line, what, remedy));
                return;
            }
        }
    }
}

impl VisitMut for VerbBody {
    fn visit_path_mut(&mut self, path: &mut syn::Path) {
        self.check_path(path);
        syn::visit_mut::visit_path_mut(self, path);
    }

    fn visit_use_tree_mut(&mut self, tree: &mut syn::UseTree) {
        let mut segments = Vec::new();
        collect_use(tree, &mut segments, &mut |path, line| {
            let path: syn::Path = syn::parse_str(&path.join("::")).unwrap_or_else(|_| syn::Path {
                leading_colon: None,
                segments: syn::punctuated::Punctuated::new(),
            });
            let before = self.found.len();
            self.check_path(&path);
            if let Some(entry) = self.found.get_mut(before) {
                entry.0 = line;
            }
        });
    }

    fn visit_macro_mut(&mut self, mac: &mut syn::Macro) {
        self.check_path(&mac.path);
        // Arguments of `format!`-like macros are expressions too.
        if let Ok(args) = mac.parse_body_with(
            syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated,
        ) {
            for mut arg in args {
                self.visit_expr_mut(&mut arg);
            }
        }
    }
}

fn collect_use(
    tree: &syn::UseTree,
    prefix: &mut Vec<String>,
    visit: &mut dyn FnMut(&[String], usize),
) {
    match tree {
        syn::UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            collect_use(&path.tree, prefix, visit);
            prefix.pop();
        }
        syn::UseTree::Name(name) => {
            prefix.push(name.ident.to_string());
            visit(prefix, name.ident.span().start().line);
            prefix.pop();
        }
        syn::UseTree::Rename(rename) => {
            prefix.push(rename.ident.to_string());
            visit(prefix, rename.ident.span().start().line);
            prefix.pop();
        }
        syn::UseTree::Glob(glob) => {
            prefix.push("*".to_owned());
            visit(prefix, glob.star_token.span.start().line);
            prefix.pop();
        }
        syn::UseTree::Group(group) => {
            for item in &group.items {
                collect_use(item, prefix, visit);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn findings(source: &str, catalog: &[&str]) -> Vec<ContractFinding> {
        let catalog: BTreeSet<String> = catalog.iter().map(|id| (*id).to_owned()).collect();
        check_source(
            Path::new("src/lib.rs"),
            source.parse().expect("tokens"),
            Some(&catalog),
        )
        .expect("checks")
    }

    #[test]
    fn verbs_may_not_reach_outside_the_document() {
        let found = findings(
            r#"
            #[resource(kind = "demo")]
            impl Demo {
                #[action]
                pub fn stamp(&mut self) {
                    self.at = std::time::SystemTime::now();
                }
                #[action]
                pub fn roll(&mut self) {
                    self.value = rand::random();
                }
                #[action]
                pub fn load(&mut self) {
                    self.text = std::fs::read_to_string("x").unwrap();
                }
                #[action]
                pub fn fetch(&mut self) {
                    let _ = std::net::TcpStream::connect("example.com:80");
                }
                #[action]
                pub fn spawn(&mut self) {
                    use std::process::Command;
                    let _ = Command::new("ls").status();
                }
                #[action]
                pub fn home(&mut self) {
                    self.text = format!("{:?}", std::env::var("HOME"));
                }
                #[action]
                pub fn fine(&mut self, cx: &mut ActionCx, fs: u32) {
                    self.at = cx.now_ms();
                    self.id = cx.id();
                    self.count = fs;
                }
                pub fn helper(&mut self) {
                    self.at = std::time::SystemTime::now();
                }
            }
            "#,
            &[],
        );
        let verbs: BTreeSet<String> = found
            .iter()
            .filter(|f| f.rule == "verb-determinism")
            .map(|f| f.message.split('`').nth(1).unwrap_or_default().to_owned())
            .collect();
        assert_eq!(
            verbs,
            ["fetch", "home", "load", "roll", "spawn", "stamp"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        assert!(found.iter().all(|f| f.level == Level::Error));
    }

    #[test]
    fn literal_label_keys_must_be_catalog_messages() {
        let found = findings(
            r#"
            fn view(cx: &mut ViewCx<Notes>) -> Element {
                let add = cx.button("notes-add", Notes::ADD.invoke());
                let typo = cx.button("notes-ad", Notes::ADD.invoke());
                let plain = cx.button("Add", Notes::ADD.invoke());
                let hint = cx.input(p.title).with_placeholder("notes-title-hint");
                Box::new(ui::column![add, typo, plain, hint])
            }
            "#,
            &["notes-add"],
        );
        let keys: Vec<&str> = found
            .iter()
            .filter(|f| f.rule == "label-key")
            .map(|f| f.message.split('"').nth(1).unwrap_or_default())
            .collect();
        assert_eq!(keys, ["notes-ad", "notes-title-hint"]);
    }

    #[test]
    fn shipped_translations_carry_every_source_message() {
        let temp = tempfile::tempdir().unwrap();
        let locales = temp.path().join("locales");
        std::fs::create_dir_all(&locales).unwrap();
        std::fs::write(locales.join("en-US.ftl"), "a-b = A\nc-d = C\n").unwrap();
        std::fs::write(locales.join("de-DE.ftl"), "a-b = A\n").unwrap();
        std::fs::write(locales.join("fr-FR.ftl"), "a-b = A\nc-d = C\n").unwrap();
        let source = source_catalog(temp.path()).unwrap().unwrap();
        let found = check_translations(temp.path(), &source).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, Path::new("locales/de-DE.ftl"));
        assert!(found[0].message.starts_with("missing c-d"));
    }
}
