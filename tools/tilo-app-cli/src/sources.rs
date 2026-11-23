//! An app's shipped sources and its workspace: the code and manifests the
//! checks read.
//!
//! [`shipped_sources`] is the module tree an app runs: its library and binary
//! roots, every module they reach, and `#[cfg(test)]` items and test-only
//! modules removed. The capability check and the app contract both read it,
//! so both judge the code an app ships and never its tests.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use proc_macro2::{Delimiter, Spacing, TokenStream, TokenTree};
use quote::ToTokens as _;
use syn::punctuated::Punctuated;
use syn::visit_mut::VisitMut;

/// The app's shipped module trees (library and binaries), test items
/// removed, for the checks that read its code.
pub(crate) fn shipped_sources(app_dir: &Path) -> Result<Vec<(PathBuf, TokenStream)>> {
    shipped_modules(&roots(app_dir)?)
}

/// The roots an app ships from: its library, `src/main.rs`, and every binary
/// its manifest or `src/bin` declares.
fn roots(app_dir: &Path) -> Result<Vec<PathBuf>> {
    let path = app_dir.join("Cargo.toml");
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let manifest: toml::Table =
        toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    let mut roots = vec![
        app_dir
            .join(table_path(manifest.get("lib")).unwrap_or_else(|| PathBuf::from("src/lib.rs"))),
        app_dir.join("src/main.rs"),
    ];
    roots.extend(
        manifest
            .get("bin")
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|bin| table_path(Some(bin)))
            .map(|bin| app_dir.join(bin)),
    );
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
    Ok(roots)
}

/// Every Tilo app that is a member of the workspace containing `start`.
pub fn workspace_apps(start: &Path) -> Result<Vec<PathBuf>> {
    let (dir, workspace) = workspace_dir(start)?;
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
    Ok(apps
        .into_iter()
        .filter(|path| path.join("tilo-app.toml").is_file() && path.join("Cargo.toml").is_file())
        .collect())
}

/// The root of the Cargo workspace containing `start` and its `[workspace]`
/// table.
fn workspace_dir(start: &Path) -> Result<(PathBuf, toml::Table)> {
    let start =
        std::fs::canonicalize(start).with_context(|| format!("resolve {}", start.display()))?;
    for dir in start.ancestors() {
        let manifest = dir.join("Cargo.toml");
        let Ok(source) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        let table: toml::Table =
            toml::from_str(&source).with_context(|| format!("parse {}", manifest.display()))?;
        if let Some(workspace) = table.get("workspace").and_then(toml::Value::as_table) {
            return Ok((dir.to_path_buf(), workspace.clone()));
        }
    }
    anyhow::bail!(
        "no Cargo workspace above {}; `--all` checks the apps of a workspace",
        start.display()
    )
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
fn shipped_modules(roots: &[PathBuf]) -> Result<Vec<(PathBuf, TokenStream)>> {
    modules_without(roots, &[], &mut BTreeMap::new())
}

/// [`shipped_modules`], also without the items that only exist when one of
/// `absent` (features a build leaves off) is on.
///
/// `parsed` holds the files read so far: a second pass over the same package
/// prunes a copy of what was parsed rather than reading and parsing it again.
fn modules_without(
    roots: &[PathBuf],
    absent: &[&str],
    parsed: &mut BTreeMap<PathBuf, syn::File>,
) -> Result<Vec<(PathBuf, TokenStream)>> {
    let mut seen = BTreeSet::new();
    let mut modules = Vec::new();
    let mut queue: VecDeque<(PathBuf, bool)> =
        roots.iter().map(|root| (root.clone(), true)).collect();
    while let Some((file, is_root)) = queue.pop_front() {
        if !seen.insert(file.clone()) {
            continue;
        }
        let syntax = match parsed.entry(file.clone()) {
            std::collections::btree_map::Entry::Occupied(held) => held.into_mut(),
            std::collections::btree_map::Entry::Vacant(vacant) => {
                let source = std::fs::read_to_string(&file)
                    .with_context(|| format!("read {}", file.display()))?;
                let syntax = syn::parse_file(&source)
                    .with_context(|| format!("parse {}", file.display()))?;
                vacant.insert(syntax)
            }
        };
        // A file whose own `#![cfg(test)]` only holds in a test build is test
        // code, however it was declared.
        if test_only(&syntax.attrs, absent) {
            continue;
        }
        let mut syntax = syntax.clone();
        Pruner { absent }.visit_file_mut(&mut syntax);
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

/// Removes items, statements, fields and arms that only exist in test builds,
/// or only with one of `absent` (features the build leaves off) on.
struct Pruner<'a> {
    absent: &'a [&'a str],
}

/// Whether `attrs` keep their item out of a build that is no test and has
/// none of `absent` on.
fn test_only(attrs: &[syn::Attribute], absent: &[&str]) -> bool {
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
            .is_ok_and(|predicate| requires_test(&predicate, absent))
    })
}

/// Whether a `cfg` predicate can only hold in a test build, or with one of
/// `absent` on.
fn requires_test(predicate: &syn::Meta, absent: &[&str]) -> bool {
    match predicate {
        syn::Meta::Path(path) => path.is_ident("test"),
        syn::Meta::List(list) => {
            let Ok(nested) =
                list.parse_args_with(Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
            else {
                return false;
            };
            if list.path.is_ident("all") {
                nested.iter().any(|nested| requires_test(nested, absent))
            } else if list.path.is_ident("any") {
                !nested.is_empty() && nested.iter().all(|nested| requires_test(nested, absent))
            } else {
                false
            }
        }
        syn::Meta::NameValue(value) => {
            value.path.is_ident("feature")
                && matches!(&value.value, syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(name),
                    ..
                }) if absent.contains(&name.value().as_str()))
        }
    }
}

impl VisitMut for Pruner<'_> {
    fn visit_file_mut(&mut self, file: &mut syn::File) {
        file.items.retain(|item| !item_test_only(item, self.absent));
        syn::visit_mut::visit_file_mut(self, file);
    }

    fn visit_item_mod_mut(&mut self, module: &mut syn::ItemMod) {
        if let Some((_, items)) = &mut module.content {
            items.retain(|item| !item_test_only(item, self.absent));
        }
        syn::visit_mut::visit_item_mod_mut(self, module);
    }

    fn visit_item_impl_mut(&mut self, item: &mut syn::ItemImpl) {
        item.items.retain(|item| {
            !test_only(
                match item {
                    syn::ImplItem::Const(item) => &item.attrs,
                    syn::ImplItem::Fn(item) => &item.attrs,
                    syn::ImplItem::Type(item) => &item.attrs,
                    syn::ImplItem::Macro(item) => &item.attrs,
                    _ => &[],
                },
                self.absent,
            )
        });
        syn::visit_mut::visit_item_impl_mut(self, item);
    }

    fn visit_item_trait_mut(&mut self, item: &mut syn::ItemTrait) {
        item.items.retain(|item| {
            !test_only(
                match item {
                    syn::TraitItem::Const(item) => &item.attrs,
                    syn::TraitItem::Fn(item) => &item.attrs,
                    syn::TraitItem::Type(item) => &item.attrs,
                    syn::TraitItem::Macro(item) => &item.attrs,
                    _ => &[],
                },
                self.absent,
            )
        });
        syn::visit_mut::visit_item_trait_mut(self, item);
    }

    fn visit_block_mut(&mut self, block: &mut syn::Block) {
        block.stmts.retain(|stmt| match stmt {
            syn::Stmt::Item(item) => !item_test_only(item, self.absent),
            syn::Stmt::Local(local) => !test_only(&local.attrs, self.absent),
            syn::Stmt::Macro(mac) => !test_only(&mac.attrs, self.absent),
            syn::Stmt::Expr(expr, _) => !test_only(expr_attrs(expr), self.absent),
        });
        syn::visit_mut::visit_block_mut(self, block);
    }

    fn visit_fields_named_mut(&mut self, fields: &mut syn::FieldsNamed) {
        fields.named = std::mem::take(&mut fields.named)
            .into_pairs()
            .filter(|pair| !test_only(&pair.value().attrs, self.absent))
            .collect();
        syn::visit_mut::visit_fields_named_mut(self, fields);
    }

    fn visit_expr_struct_mut(&mut self, expr: &mut syn::ExprStruct) {
        expr.fields = std::mem::take(&mut expr.fields)
            .into_pairs()
            .filter(|pair| !test_only(&pair.value().attrs, self.absent))
            .collect();
        syn::visit_mut::visit_expr_struct_mut(self, expr);
    }

    fn visit_item_enum_mut(&mut self, item: &mut syn::ItemEnum) {
        item.variants = std::mem::take(&mut item.variants)
            .into_pairs()
            .filter(|pair| !test_only(&pair.value().attrs, self.absent))
            .collect();
        syn::visit_mut::visit_item_enum_mut(self, item);
    }

    fn visit_expr_match_mut(&mut self, expr: &mut syn::ExprMatch) {
        expr.arms.retain(|arm| !test_only(&arm.attrs, self.absent));
        syn::visit_mut::visit_expr_match_mut(self, expr);
    }
}

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

fn item_test_only(item: &syn::Item, absent: &[&str]) -> bool {
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
    test_only(attrs, absent)
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
            // `::name` at the start of a path is still a crate name; a lone
            // `:` is a field's type, which starts one.
            let double = matches!(
                before.checked_sub(1).map(|colon| tokens.get(colon)),
                Some(Some(TokenTree::Punct(colon))) if colon.as_char() == ':'
            );
            double
                && before.checked_sub(2).is_some_and(|start| {
                    matches!(
                        tokens.get(start),
                        Some(TokenTree::Ident(_) | TokenTree::Group(_))
                    )
                })
        }
        _ => false,
    }
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
