//! What agents' programs read (agents.md, "Querying" and "Reading and describing"): `grep`
//! over the text of resources, `find` by the palette's words, `read` and `describe`.
//!
//! Each is answered where the records are. A runtime answers a [`ReadRequest`] over the apps it
//! holds ([`Runtime::answer_read`]); a program asks the same of an app its host does not hold,
//! and the platform carries the request to that app's host and the answer back
//! ([`ProgramInputs::answers`]). A program that needed an answer it was not given ends with
//! [`crate::program::ProgramOutcome::NeedsReads`], having changed nothing, and runs again with
//! the answers: what it read is part of the step, as a model's answer is.
//!
//! The text of a resource is its text fields, each cut into paragraphs with anchors as people
//! cite them (`¶4`, `¶2.1` for the first item of the list that is the second paragraph): a
//! rich text by its blocks, a plain text by its lines, a line read as markdown writes it (`> `
//! a quote, `# ` a heading, `- ` a list item, a fenced block code). A one-line field that is no
//! body is cited by its name (`title`).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::core::{Core, HResult, trap};
use crate::ids::Principal;
use crate::meta::FieldKind;
use crate::registry::{FieldInfo, TypeInfo};
use crate::runtime::Runtime;
use crate::value::{RefValue, ResourceRef, Value};

/// What a program asks of an app: answered by the host that holds the app's records.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "read", rename_all = "snake_case")]
pub enum ReadRequest {
    /// The matches of `pattern` (a regular expression) in the text of the app's resources, or
    /// of those `within` (ids) and below them.
    Grep {
        app: String,
        pattern: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        within: Vec<String>,
        /// Kinds of block left out (`quote`).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        not: Vec<String>,
        /// A time as a filter writes it: only resources last changed then.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        changed: Option<String>,
        /// A person, by participant id: only resources whose text mentions them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mentions: Option<String>,
    },
    /// The resources whose text holds every one of `words`.
    Find { app: String, words: String },
    /// One resource's text and fields, by its type's name and id.
    Read { app: String, ty: String, id: String },
    /// The resources of the type `ty` (its id) anywhere in the app that `pred` holds of, in
    /// the order `order`: a typed query that looks inside no one resource, by their ids.
    Select {
        app: String,
        ty: String,
        pred: crate::store::Pred,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        order: Option<crate::query::Order>,
    },
    /// The app, or one of its declarations (`name`: a type, an action, a root), or a field of
    /// a type.
    Describe {
        app: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        field: Option<String>,
    },
}

impl ReadRequest {
    /// The app it asks.
    #[must_use]
    pub fn app(&self) -> &str {
        match self {
            Self::Grep { app, .. }
            | Self::Find { app, .. }
            | Self::Read { app, .. }
            | Self::Select { app, .. }
            | Self::Describe { app, .. } => app,
        }
    }

    /// Its canonical text: what an answer is filed under.
    #[must_use]
    pub fn key(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// A resource as an answer names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shown {
    /// How a program writes it: `@notes.Note:root-1/notes:n2`.
    pub literal: String,
    pub app: String,
    /// Its type's name.
    pub ty: String,
    /// Its id ([`ResourceRef::id_string`]).
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// One match of a grep.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchRow {
    pub doc: Shown,
    pub anchor: String,
    pub text: String,
    pub line: String,
    pub groups: Vec<String>,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub written_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed: Option<String>,
}

/// What a grep of one app found, and what it could not cover.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrepAnswer {
    pub matches: Vec<MatchRow>,
    /// The resources whose text was searched.
    pub searched: usize,
    /// What it could not cover, in words.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncovered: Vec<String>,
}

/// A paragraph or a field of a resource's text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    pub anchor: String,
    pub kind: String,
    pub text: String,
}

/// A resource as `read` gives it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadAnswer {
    pub shown: Shown,
    pub parts: Vec<Part>,
    /// Its other fields, each as a program prints it.
    pub fields: Vec<(String, String)>,
}

/// What a program is given beside its source, for what it reads.
#[derive(Clone, Debug, Default)]
pub struct ProgramInputs {
    /// The apps it may read that its host does not hold, by the name programs call each and
    /// its id.
    pub sources: Vec<(String, String)>,
    /// The apps whose records this runtime holds; `None` for every app it has loaded.
    pub local: Option<Vec<String>>,
    /// Answers the platform brought, by [`ReadRequest::key`].
    pub answers: BTreeMap<String, Json>,
    /// The person the program's task acts for: `me`.
    pub me: Option<Principal>,
}

/// Paragraphs a page of `read` shows.
pub const READ_PAGE: usize = 40;

/// The kind of a line of plain text, as markdown writes it, and the text without its marker.
fn line_kind(line: &str, in_code: bool) -> (&'static str, &str) {
    if in_code {
        return ("code", line);
    }
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix('>') {
        return ("quote", rest.trim_start());
    }
    if trimmed.starts_with('#') {
        return ("heading", trimmed.trim_start_matches('#').trim_start());
    }
    for marker in ["- ", "* ", "+ "] {
        if trimmed.starts_with(marker) {
            return ("list", line);
        }
    }
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    if digits > 0
        && trimmed
            .get(digits..)
            .is_some_and(|rest| rest.starts_with(". "))
    {
        return ("list", line);
    }
    ("paragraph", line)
}

/// Plain text cut into paragraphs with anchors: each line one, consecutive list items numbered
/// under their list (`¶2.1`), a fenced block one paragraph of code per line.
fn plain_parts(text: &str, prefix: &str) -> Vec<Part> {
    let mut out = Vec::new();
    let (mut top, mut item, mut in_list, mut in_code) = (0usize, 0usize, false, false);
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if line.trim().is_empty() {
            in_list = false;
            continue;
        }
        let (kind, _) = line_kind(line, in_code);
        let anchor = if kind == "list" {
            if !in_list {
                top = top.saturating_add(1);
                item = 0;
                in_list = true;
            }
            item = item.saturating_add(1);
            format!("{prefix}¶{top}.{item}")
        } else {
            in_list = false;
            top = top.saturating_add(1);
            format!("{prefix}¶{top}")
        };
        out.push(Part {
            anchor,
            kind: kind.to_owned(),
            text: line.to_owned(),
        });
    }
    out
}

/// The kind of a rich text block, by the kinds the document model exports.
fn block_kind(block: &tilo_doc::snapshot::BlockView) -> &'static str {
    use tilo_doc::export::{BlockKind, ExportBlock};
    match ExportBlock::new(block).kind() {
        BlockKind::Heading => "heading",
        BlockKind::Quote => "quote",
        BlockKind::ListItem => "list",
        BlockKind::Code => "code",
        BlockKind::Paragraph | BlockKind::Table | BlockKind::Image => "paragraph",
    }
}

/// `pattern` compiled, or why it is no regular expression.
fn regex(pattern: &str) -> Result<regex::Regex, String> {
    regex::Regex::new(pattern).map_err(|e| format!("`/{pattern}/` is no regular expression: {e}"))
}

/// A text field of a type, with how its anchors are written.
struct TextField {
    info: FieldInfo,
    /// Cited by paragraph alone (`¶3`): the type's body, or its one long text.
    body: bool,
}

/// What [`Core::text_of`] reads of a resource: its paragraphs, and the people its rich text
/// mentions.
struct TextOf {
    parts: Vec<Part>,
    mentions: Vec<String>,
    /// Authored runs of rich text: the part's anchor, its span and who wrote it.
    authored: Vec<(String, usize, usize, String)>,
}

impl Core {
    /// The text fields of the type `info` (of the package `meta`), secrets left out.
    fn text_fields(meta: &crate::meta::Meta, info: &TypeInfo) -> Vec<TextField> {
        let declared = meta.types.iter().find(|t| t.id == info.id);
        let role = |f: &FieldInfo| {
            declared
                .and_then(|t| t.fields.iter().find(|m| m.id == f.id))
                .and_then(|m| m.role.clone())
        };
        let texts: Vec<&FieldInfo> = info
            .fields
            .iter()
            // Prose: a text that merges by splices. A register of text is a code or a label.
            .filter(|f| matches!(f.kind, FieldKind::Text | FieldKind::RichText))
            .filter(|f| role(f).as_deref() != Some("secret"))
            .collect();
        let body = texts
            .iter()
            .find(|f| role(f).as_deref() == Some("body"))
            .map(|f| f.id.clone());
        texts
            .into_iter()
            .map(|f| TextField {
                body: body.as_deref() == Some(f.id.as_str()),
                info: f.clone(),
            })
            .collect()
    }

    /// The paragraphs of a resource's text fields.
    fn text_of(&self, target: &ResourceRef, fields: &[TextField]) -> TextOf {
        let mut out = TextOf {
            parts: Vec::new(),
            mentions: Vec::new(),
            authored: Vec::new(),
        };
        for field in fields {
            let prefix = if field.body {
                String::new()
            } else {
                format!("{} ", field.info.name)
            };
            match self.read_value(target, &field.info, None) {
                Ok(Value::Text(text)) => {
                    if !field.body && !text.contains('\n') {
                        if !text.trim().is_empty() {
                            out.parts.push(Part {
                                anchor: field.info.name.clone(),
                                kind: "field".to_owned(),
                                text,
                            });
                        }
                    } else {
                        out.parts.extend(plain_parts(&text, &prefix));
                    }
                }
                Ok(Value::Rich(rich)) => {
                    for reference in rich.references() {
                        if let Some(person) = reference.as_person() {
                            out.mentions.push(person.to_owned());
                        }
                    }
                    for (anchor, block) in rich.anchors() {
                        let anchor = format!("{prefix}{anchor}");
                        for (start, end, who) in tilo_doc::document::author_runs(&block.marks) {
                            out.authored.push((anchor.clone(), start, end, who));
                        }
                        if block.text.trim().is_empty() {
                            continue;
                        }
                        out.parts.push(Part {
                            anchor,
                            kind: block_kind(block).to_owned(),
                            text: block.text.clone(),
                        });
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// How an answer names the resource `target`.
    pub(crate) fn shown(&self, target: &ResourceRef) -> Shown {
        let (app, ty) = self
            .registry
            .borrow()
            .type_info(&target.ty)
            .map(|(pkg, info)| (pkg.meta.app.clone(), info.name.clone()))
            .unwrap_or_default();
        Shown {
            literal: self.ref_literal(target),
            app,
            ty,
            id: target.id_string(),
            title: self.program_title(target),
        }
    }

    /// Every resource of `app` that has text, with its type's text fields; and the types whose
    /// resources could not be listed, with why.
    #[allow(clippy::type_complexity, reason = "one pass's two results")]
    fn texts_of_app(&self, app: &str) -> (Vec<(ResourceRef, Rc<Vec<TextField>>)>, Vec<String>) {
        let packages: Vec<Rc<crate::registry::Package>> = self
            .registry
            .borrow()
            .packages()
            .into_iter()
            .filter(|p| p.meta.app == app)
            .collect();
        let Some(pkg) = packages.last() else {
            return (Vec::new(), vec![format!("{app} is not loaded here")]);
        };
        let mut out = Vec::new();
        let mut unlisted = Vec::new();
        for info in &pkg.types {
            let fields = Self::text_fields(&pkg.meta, info);
            if fields.is_empty() {
                continue;
            }
            let fields = Rc::new(fields);
            let found =
                self.store
                    .borrow()
                    .select(&info.id, &crate::store::Pred::Const(true), None);
            match found {
                Ok(found) => {
                    for target in found {
                        out.push((target, Rc::clone(&fields)));
                    }
                }
                Err(e) => unlisted.push(format!("{}s were not listed: {e}", info.name)),
            }
        }
        (out, unlisted)
    }

    /// Who wrote the text of `target`, when the log says one person did: everyone who changed
    /// it is that one person.
    fn sole_writer(&self, target: &ResourceRef) -> Option<String> {
        let versions = self.store.borrow().versions(target).ok()?;
        let mut who = versions.iter().map(|v| v.who.as_str());
        let first = who.next()?;
        who.all(|w| w == first).then(|| first.to_owned())
    }

    /// The lines of the text of `target` that `re` matches, each at its first match, leaving out
    /// the kinds of block `not`.
    fn matches_in(
        &self,
        target: &ResourceRef,
        text: &TextOf,
        re: &regex::Regex,
        not: &[String],
        changed: Option<&str>,
    ) -> Vec<MatchRow> {
        let mut out = Vec::new();
        let mut doc: Option<Shown> = None;
        let mut writer: Option<Option<String>> = None;
        for part in text.parts.iter().filter(|p| !not.contains(&p.kind)) {
            // A line is listed once, at its first match, as grep lists it: its other matches
            // would list the same line again.
            if let Some(caps) = re.captures(&part.text) {
                let Some(whole) = caps.get(0) else {
                    continue;
                };
                let authored = text.authored.iter().find(|(anchor, start, end, _)| {
                    *anchor == part.anchor && *start <= whole.start() && whole.end() <= *end
                });
                let written_by = match authored {
                    Some((.., who)) => Some(who.clone()),
                    None => writer
                        .get_or_insert_with(|| self.sole_writer(target))
                        .clone(),
                };
                out.push(MatchRow {
                    doc: doc.get_or_insert_with(|| self.shown(target)).clone(),
                    anchor: part.anchor.clone(),
                    text: whole.as_str().to_owned(),
                    line: part.text.clone(),
                    groups: caps
                        .iter()
                        .map(|g| g.map_or_else(String::new, |g| g.as_str().to_owned()))
                        .collect(),
                    kind: part.kind.clone(),
                    written_by,
                    changed: changed.map(str::to_owned),
                });
            }
        }
        out
    }

    /// The grep `req` over this runtime's records.
    pub(crate) fn grep_local(&self, req: &ReadRequest) -> Result<GrepAnswer, String> {
        let ReadRequest::Grep {
            app,
            pattern,
            within,
            not,
            changed,
            mentions,
        } = req
        else {
            return Err("not a grep".to_owned());
        };
        let re = regex(pattern)?;
        let clock = self.program_clock_value()?;
        let (resources, unlisted) = self.texts_of_app(app);
        let mut answer = GrepAnswer {
            uncovered: unlisted,
            ..GrepAnswer::default()
        };
        let mut unwritten = 0usize;
        let mut plain_for_mentions = 0usize;
        for (target, fields) in resources {
            let id = target.id_string();
            if !within.is_empty()
                && !within
                    .iter()
                    .any(|w| id == *w || id.starts_with(&format!("{w}/")))
            {
                continue;
            }
            let last = self
                .store
                .borrow()
                .version_end(&target, true)
                .ok()
                .flatten();
            if let Some(when) = changed {
                let Some(last) = &last else {
                    continue;
                };
                if !crate::query::time_holds(when, &last.at, &clock)? {
                    continue;
                }
            }
            let text = self.text_of(&target, &fields);
            if let Some(person) = mentions {
                if text.mentions.is_empty()
                    && fields.iter().all(|f| f.info.kind != FieldKind::RichText)
                {
                    plain_for_mentions = plain_for_mentions.saturating_add(1);
                }
                if !text.mentions.contains(person) {
                    continue;
                }
            }
            answer.searched = answer.searched.saturating_add(1);
            let changed = last.as_ref().map(|v| v.at.as_str());
            for row in self.matches_in(&target, &text, &re, not, changed) {
                if row.written_by.is_none() {
                    unwritten = unwritten.saturating_add(1);
                }
                answer.matches.push(row);
            }
        }
        if unwritten > 0 {
            answer.uncovered.push(format!(
                "{unwritten} {} in text several people changed, whose writer is not recorded",
                if unwritten == 1 {
                    "match is"
                } else {
                    "matches are"
                }
            ));
        }
        if plain_for_mentions > 0 {
            answer.uncovered.push(format!(
                "{plain_for_mentions} resources hold plain text, which records no mentions"
            ));
        }
        Ok(answer)
    }

    /// The resources of `app` whose text holds every one of `words`, those holding them in a
    /// title-like field first.
    pub(crate) fn find_local(&self, app: &str, words: &str) -> Vec<Shown> {
        let words: Vec<String> = words.split_whitespace().map(str::to_lowercase).collect();
        if words.is_empty() {
            return Vec::new();
        }
        let searched = self.text_search.borrow();
        if let Some(search) = searched.as_ref() {
            return search(&words.join(" "), crate::program::FIND_LIMIT)
                .into_iter()
                .filter(|r| {
                    self.registry
                        .borrow()
                        .type_info(&r.ty)
                        .is_some_and(|(pkg, _)| pkg.meta.app == app)
                })
                .map(|r| self.shown(&r))
                .collect();
        }
        drop(searched);
        let (resources, _) = self.texts_of_app(app);
        let mut ranked: Vec<(u8, Shown)> = Vec::new();
        for (target, fields) in resources {
            let text = self.text_of(&target, &fields);
            let all: String = text
                .parts
                .iter()
                .map(|p| p.text.to_lowercase())
                .collect::<Vec<_>>()
                .join("\n");
            if !words.iter().all(|w| all.contains(w.as_str())) {
                continue;
            }
            let titled = text.parts.iter().filter(|p| p.kind == "field").any(|p| {
                words
                    .iter()
                    .all(|w| p.text.to_lowercase().contains(w.as_str()))
            });
            ranked.push((u8::from(!titled), self.shown(&target)));
        }
        ranked.sort_by_key(|(rank, _)| *rank);
        ranked.into_iter().map(|(_, shown)| shown).collect()
    }

    /// `target` as `read` gives it.
    pub(crate) fn read_local(&self, target: &ResourceRef) -> Result<ReadAnswer, String> {
        let (pkg, info) = self
            .registry
            .borrow()
            .type_info(&target.ty)
            .ok_or_else(|| format!("no resource `{}` here", target.id_string()))?;
        let fields = Self::text_fields(&pkg.meta, &info);
        let parts = self.text_of(target, &fields).parts;
        let mut others = Vec::new();
        for f in &info.fields {
            if fields.iter().any(|t| t.info.id == f.id) {
                continue;
            }
            let shown = match self.read_value(target, f, None) {
                Ok(Value::List(items)) if f.kind == FieldKind::Collection => {
                    format!("{} items", items.len())
                }
                Ok(value) => self.show_inline(&value),
                Err(error) => format!("error: {error}"),
            };
            others.push((f.name.clone(), shown));
        }
        Ok(ReadAnswer {
            shown: self.shown(target),
            parts,
            fields: others,
        })
    }

    /// The answer to `req` over this runtime's records, as JSON.
    pub(crate) fn answer_local(&self, req: &ReadRequest) -> Result<Json, String> {
        let to_json = |v: &dyn erased::Ser| v.json();
        match req {
            ReadRequest::Grep { .. } => Ok(to_json(&self.grep_local(req)?)),
            ReadRequest::Find { app, words } => Ok(to_json(&self.find_local(app, words))),
            ReadRequest::Read { app, ty, id } => {
                let ty_id = self
                    .registry
                    .borrow()
                    .packages()
                    .into_iter()
                    .filter(|p| p.meta.app == *app)
                    .find_map(|p| p.type_by_name(ty).map(|t| t.id.clone()))
                    .ok_or_else(|| format!("{app} has no type {ty}"))?;
                let target = ResourceRef::parse_id(id, ty_id)
                    .ok_or_else(|| format!("`{id}` is no resource id"))?;
                Ok(to_json(&self.read_local(&target)?))
            }
            ReadRequest::Select {
                ty, pred, order, ..
            } => {
                let found = self
                    .store
                    .borrow()
                    .select(ty, pred, order.as_ref())
                    .map_err(|error| error.to_string())?;
                Ok(Json::Array(
                    found
                        .iter()
                        .map(|at| Json::String(at.id_string()))
                        .collect(),
                ))
            }
            ReadRequest::Describe { app, name, field } => Ok(Json::String(self.describe_local(
                app,
                name.as_deref(),
                field.as_deref(),
            )?)),
        }
    }
}

/// Serializing an answer, whatever its type.
mod erased {
    pub trait Ser {
        fn json(&self) -> serde_json::Value;
    }

    impl<T: serde::Serialize> Ser for T {
        fn json(&self) -> serde_json::Value {
            serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
        }
    }
}

// ---- describe -------------------------------------------------------------------------------

/// `ty` as a program writes it: a type of `app` by its name alone.
fn short_ty(ty: &str, app: &str) -> String {
    ty.replace("enum ", "").replace(&format!("{app}."), "")
}

impl Core {
    /// `describe` over the metadata of `app`: the app, a type, an action, a root or a field.
    pub(crate) fn describe_local(
        &self,
        app: &str,
        name: Option<&str>,
        field: Option<&str>,
    ) -> Result<String, String> {
        let pkg = self
            .registry
            .borrow()
            .packages()
            .into_iter()
            .rev()
            .find(|p| p.meta.app == app)
            .ok_or_else(|| format!("{app} is not loaded here"))?;
        let meta = &pkg.meta;
        let alias = app.rsplit('.').next().unwrap_or(app);
        let Some(name) = name else {
            return Ok(describe_app(meta, alias));
        };
        if let Some(t) = meta.types.iter().find(|t| t.name == name) {
            let Some(field) = field else {
                return Ok(describe_type(meta, t, alias));
            };
            let f = t.fields.iter().find(|f| f.name == field).ok_or_else(|| {
                let names: Vec<&str> = t.fields.iter().map(|f| f.name.as_str()).collect();
                format!(
                    "{} has no field `{field}`{}; fields: {}",
                    t.name,
                    tilolang::diag::suggestion(field, names.iter().copied()),
                    names.join(", ")
                )
            })?;
            let mut out = format!(
                "{alias}.{}.{}: {}{}",
                t.name,
                f.name,
                short_ty(&f.ty, app),
                f.role
                    .as_ref()
                    .map_or_else(String::new, |r| format!(" ({r})"))
            );
            if !f.doc.is_empty() {
                let _ = write!(out, "\n  {}", f.doc);
            }
            if t.search
                .filters
                .iter()
                .any(|x| x.field.as_deref() == Some(f.id.as_str()))
            {
                let _ = write!(out, "\n  a filter: find({}, {}: ...)", t.name, f.name);
            }
            return Ok(out);
        }
        if let Some(a) = meta.actions.iter().find(|a| a.name == name && !a.scenario) {
            return Ok(describe_action(a, app, alias));
        }
        if let Some(root) = meta.named_roots.iter().find(|r| r.name == name) {
            let ty = meta
                .types
                .iter()
                .find(|t| t.id == root.ty)
                .map_or_else(|| short_ty(&root.ty, app), |t| t.name.clone());
            return Ok(format!("root {alias}.{}: {ty}", root.name));
        }
        let mut names: Vec<&str> = meta.types.iter().map(|t| t.name.as_str()).collect();
        names.extend(
            meta.actions
                .iter()
                .filter(|a| !a.scenario)
                .map(|a| a.name.as_str()),
        );
        names.extend(meta.named_roots.iter().map(|r| r.name.as_str()));
        Err(format!(
            "{alias} has no `{name}`{}; it declares {}",
            tilolang::diag::suggestion(name, names.iter().copied()),
            names.join(", ")
        ))
    }
}

fn describe_app(meta: &crate::meta::Meta, alias: &str) -> String {
    let app = &meta.app;
    let mut out = format!("app {alias} \"{}\"  ({app})", meta.title);
    for root in &meta.named_roots {
        let ty = meta
            .types
            .iter()
            .find(|t| t.id == root.ty)
            .map_or_else(|| short_ty(&root.ty, app), |t| t.name.clone());
        let _ = write!(out, "\nroot {alias}.{}: {ty}", root.name);
    }
    for t in &meta.types {
        let fields: Vec<String> = t
            .fields
            .iter()
            .map(|f| format!("{}: {}", f.name, short_ty(&f.ty, app)))
            .collect();
        let _ = write!(out, "\ntype {} {{ {} }}", t.name, fields.join(", "));
    }
    for a in meta.actions.iter().filter(|a| !a.scenario) {
        let _ = write!(out, "\n{}", action_line(a, app, alias));
    }
    let scenarios: Vec<String> = meta
        .scenarios
        .iter()
        .map(|s| format!("{:?}", s.name))
        .collect();
    if !scenarios.is_empty() {
        let _ = write!(out, "\nscenarios: {}", scenarios.join(", "));
    }
    out.push_str("\ndescribe a type or an action for its detail: describe(");
    out.push_str(meta.types.first().map_or(alias, |t| t.name.as_str()));
    out.push(')');
    out
}

fn describe_type(meta: &crate::meta::Meta, t: &crate::meta::TypeMeta, alias: &str) -> String {
    let app = &meta.app;
    let mut out = String::new();
    if !t.doc.is_empty() {
        for line in t.doc.lines() {
            let _ = writeln!(out, "/// {line}");
        }
    }
    let _ = write!(out, "type {alias}.{} {{", t.name);
    for f in &t.fields {
        let role = f
            .role
            .as_ref()
            .map_or_else(String::new, |r| format!(" ({r})"));
        let _ = write!(out, "\n    {}: {}{role}", f.name, short_ty(&f.ty, app));
    }
    out.push_str("\n}");
    let filters: Vec<&str> = t.search.filters.iter().map(|f| f.key.as_str()).collect();
    if !filters.is_empty() {
        let _ = write!(
            out,
            "\nfilters: {}  (find({}, {}: ...))",
            filters.join(", "),
            t.name,
            filters.first().copied().unwrap_or_default()
        );
    }
    if let Some(sort) = &t.search.sort {
        let _ = write!(out, "\nsorted by: {}", sort.key);
    }
    let taking: Vec<&crate::meta::ActionMeta> = meta
        .actions
        .iter()
        .filter(|a| !a.scenario)
        .filter(|a| {
            a.params
                .iter()
                .any(|p| short_ty(&p.ty, app) == t.name || p.ty.ends_with(&format!(".{}", t.name)))
        })
        .collect();
    for a in taking {
        let _ = write!(out, "\n{}", action_line(a, app, alias));
    }
    out
}

fn action_line(a: &crate::meta::ActionMeta, app: &str, alias: &str) -> String {
    let params: Vec<String> = a
        .params
        .iter()
        .map(|p| {
            let default = p
                .default
                .as_ref()
                .map(|d| format!(" = {}", d.text))
                .unwrap_or_default();
            format!("{}: {}{default}", p.name, short_ty(&p.ty, app))
        })
        .collect();
    let result = a
        .result
        .as_ref()
        .filter(|r| r.as_str() != "()")
        .map_or_else(String::new, |r| format!(" -> {}", short_ty(r, app)));
    let role = a
        .role
        .as_ref()
        .map_or_else(String::new, |r| format!("{r} "));
    format!(
        "{role}action {alias}.{}({}){result}",
        a.name,
        params.join(", ")
    )
}

fn describe_action(a: &crate::meta::ActionMeta, app: &str, alias: &str) -> String {
    let mut out = String::new();
    if !a.doc.is_empty() {
        for line in a.doc.lines() {
            let _ = writeln!(out, "/// {line}");
        }
    }
    out.push_str(&action_line(a, app, alias));
    let _ = write!(
        out,
        "\nlabel: {:?}; {}",
        a.label,
        match a.class {
            crate::meta::EffectClass::Read => "reads only",
            crate::meta::EffectClass::Undoable => "an undoable change",
            crate::meta::EffectClass::Outside => "reaches outside: it asks the person first",
        }
    );
    let _ = write!(
        out,
        "\nthis program may apply it to {alias}'s resources when it runs in {alias}"
    );
    out
}

impl Runtime {
    /// The answer to `req`, read from this runtime's records: what an app's host gives a
    /// program that runs elsewhere.
    ///
    /// # Errors
    /// What it cannot read: a pattern that is no regular expression, a resource or a name it
    /// does not hold.
    pub fn answer_read(&self, req: &ReadRequest) -> Result<Json, String> {
        self.core.answer_local(req)
    }
}

/// Results a value carries that `show` prints for the resources of apps the runtime does not
/// hold: their literal and title.
#[derive(Debug, Default)]
pub struct Foreign {
    by_key: BTreeMap<String, Shown>,
}

impl Foreign {
    /// The key of a foreign resource: its type (`app#Type`) and id.
    fn key(target: &ResourceRef) -> String {
        format!("{}@{}", target.ty, target.id_string())
    }

    pub(crate) fn get(&self, target: &ResourceRef) -> Option<&Shown> {
        self.by_key.get(&Self::key(target))
    }
}

/// Whether `target` is a resource of an app this runtime does not hold.
#[must_use]
pub fn is_foreign(target: &ResourceRef) -> bool {
    target.ty.contains('#')
}

impl Core {
    /// The value standing for `shown` here: a resource of this runtime when it holds `shown`'s
    /// app, else a foreign one that prints as its literal and title.
    pub(crate) fn value_of_shown(&self, shown: &Shown, local: bool) -> HResult<Value> {
        let author = self.module_of_frame()?;
        if local {
            let ty = self
                .registry
                .borrow()
                .packages()
                .into_iter()
                .filter(|p| p.meta.app == shown.app)
                .find_map(|p| p.type_by_name(&shown.ty).map(|t| t.id.clone()));
            if let Some(target) = ty.and_then(|ty| ResourceRef::parse_id(&shown.id, ty)) {
                return Ok(Value::Ref(RefValue {
                    target: Rc::new(target),
                    author: Some(author),
                }));
            }
        }
        let Some(target) = ResourceRef::parse_id(&shown.id, format!("{}#{}", shown.app, shown.ty))
        else {
            return trap(format!("`{}` is no resource id", shown.id));
        };
        self.foreign
            .borrow_mut()
            .by_key
            .insert(Foreign::key(&target), shown.clone());
        Ok(Value::Ref(RefValue {
            target: Rc::new(target),
            author: Some(author),
        }))
    }

    /// The clock programs read, as a query reads it.
    pub(crate) fn program_clock_value(&self) -> Result<crate::query::Clock, String> {
        let now = self
            .clock
            .borrow()
            .as_ref()
            .map_or_else(|| crate::core::EPOCH.to_owned(), |clock| clock());
        crate::query::Clock::at(&now)
    }
}
