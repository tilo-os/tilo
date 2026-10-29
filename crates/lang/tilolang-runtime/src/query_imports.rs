//! The imports of agents' query words (agents.md, "Querying" and "Reading and describing"):
//! `grep`, `find` across apps, `read` with what it reads, `describe`, `print` with fields, `me`,
//! and the chaining functions `count_by`, `group`, `take` and `single`.
//!
//! What an app this runtime holds answers is read here ([`crate::reads`]); what another app
//! answers comes with the program ([`ProgramInputs::answers`]), or is asked for: the request is
//! noted and the program reads on as if it found nothing, to be run again with the answers.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Deserialize;
use serde_json::Value as Json;

use crate::core::{Core, HResult, trap};
use crate::program::{ProgramInputs, ReadRequest};
use crate::reads::{GrepAnswer, MatchRow, ReadAnswer, Shown, is_foreign};
use crate::value::{ResourceRef, Value};

/// Candidates `single` lists before it says how many more there are.
const SINGLE_LISTS: usize = 20;

/// Characters of a matched paragraph a match prints around what matched.
const EXCERPT: usize = 120;

/// `grep`'s options as the checker writes them.
#[derive(Deserialize)]
struct GrepOptions {
    #[serde(default)]
    apps: Vec<String>,
    #[serde(default)]
    not: Vec<String>,
    #[serde(default)]
    changed: Option<String>,
    #[serde(default)]
    mentions: Option<String>,
}

/// `read`'s options as the checker writes them.
#[derive(Deserialize, Default)]
struct ReadOptions {
    #[serde(default)]
    around: Option<usize>,
    #[serde(default)]
    at: Option<String>,
    #[serde(default)]
    page: Option<usize>,
}

/// What an app answered, or why there is no answer.
enum Answered {
    Here(Json),
    /// The platform could not read it: why.
    Unavailable(String),
    /// Not answered yet: asked for.
    Asked,
}

/// `line` cut to about [`EXCERPT`] characters around `text`.
fn excerpt(line: &str, text: &str) -> String {
    let chars = line.chars().count();
    if chars <= EXCERPT {
        return line.to_owned();
    }
    let at = line.find(text).map_or(0, |byte| {
        line.get(..byte).map_or(0, |before| before.chars().count())
    });
    let start = at.saturating_sub(EXCERPT / 3);
    let cut: String = line.chars().skip(start).take(EXCERPT).collect();
    let before = if start > 0 { "…" } else { "" };
    let after = if start.saturating_add(EXCERPT) < chars {
        "…"
    } else {
        ""
    };
    format!("{before}{cut}{after}")
}

impl Core {
    /// The inputs of the program running now.
    fn inputs(&self) -> HResult<ProgramInputs> {
        match self.program.borrow().as_ref() {
            Some(io) => Ok(io.inputs.clone()),
            None => trap("only a program reads across apps"),
        }
    }

    /// Whether this runtime holds the records of `app`.
    fn holds(&self, app: &str, inputs: &ProgramInputs) -> bool {
        match &inputs.local {
            Some(local) => local.iter().any(|a| a == app),
            None => self
                .registry
                .borrow()
                .packages()
                .iter()
                .any(|p| p.meta.app == app),
        }
    }

    /// Every app a program reads by default: those this runtime holds, then the others it may
    /// read.
    fn every_app(&self, inputs: &ProgramInputs) -> Vec<String> {
        let mut apps: Vec<String> = if let Some(local) = &inputs.local {
            local.clone()
        } else {
            let mut held: Vec<String> = Vec::new();
            for p in self.registry.borrow().packages() {
                let app = &p.meta.app;
                if app == tilolang::HOST_APP || app.starts_with("agent.") || held.contains(app) {
                    continue;
                }
                held.push(app.clone());
            }
            held
        };
        for (_, app) in &inputs.sources {
            if !apps.contains(app) {
                apps.push(app.clone());
            }
        }
        apps
    }

    /// The answer to `req`: read here when this runtime holds its app, else what the platform
    /// brought, else asked for.
    fn answered(&self, req: &ReadRequest, inputs: &ProgramInputs) -> HResult<Answered> {
        // A task's host holds no records: each app answers while the program runs.
        if let Some(reader) = self.reader.borrow().as_ref() {
            return Ok(match reader(req) {
                Ok(answer) => Answered::Here(answer),
                Err(why) => Answered::Unavailable(why),
            });
        }
        if self.holds(req.app(), inputs) {
            return Ok(Answered::Here(self.answer_local(req)?));
        }
        match inputs.answers.get(&req.key()) {
            Some(Json::Object(o)) if o.contains_key("unavailable") => Ok(Answered::Unavailable(
                o.get("unavailable")
                    .and_then(Json::as_str)
                    .unwrap_or("it did not answer")
                    .to_owned(),
            )),
            Some(answer) => Ok(Answered::Here(answer.clone())),
            None => {
                if let Some(io) = self.program.borrow_mut().as_mut()
                    && !io.missing.contains(req)
                {
                    io.missing.push(req.clone());
                }
                Ok(Answered::Asked)
            }
        }
    }

    /// Whether the program is waiting on answers: what it prints now is not shown.
    fn waiting(&self) -> bool {
        self.program
            .borrow()
            .as_ref()
            .is_some_and(|io| !io.missing.is_empty())
    }

    /// A line of the program's output.
    fn say(&self, line: String) {
        if let Some(io) = self.program.borrow_mut().as_mut() {
            io.output.push(line);
        }
    }

    /// The app a resource value belongs to, and whether this runtime holds it.
    fn app_of(&self, target: &ResourceRef) -> Option<String> {
        if is_foreign(target) {
            return target.ty.split('#').next().map(str::to_owned);
        }
        self.registry
            .borrow()
            .type_info(&target.ty)
            .map(|(pkg, _)| pkg.meta.app.clone())
    }

    pub(crate) fn agent_query_import(
        &self,
        imp: crate::abi::Imp,
        a: &crate::core::Args<'_>,
    ) -> HResult<Option<crate::engine::Wasm>> {
        use crate::abi::Imp;
        match imp {
            Imp::Grep => {
                let pattern = self.text(a.h(0)?)?;
                let options = self.text(a.h(1)?)?;
                let scope = self.val(a.h(2)?)?;
                let found = self.journaled_read(|| self.grep(&pattern, &options, &scope))?;
                self.ret_h(found)
            }
            Imp::Describe => {
                let path: Json = serde_json::from_str(&self.text(a.h(0)?)?)
                    .map_err(|e| format!("describe: {e}"))?;
                let text = self.journaled_read(|| self.describe(&path))?;
                self.ret_h(text)
            }
            Imp::DescribeRef => {
                let Value::Ref(r) = self.val(a.h(0)?)? else {
                    return trap("describe takes a resource");
                };
                let text = self.journaled_read(|| self.describe_ref(&r.target))?;
                self.ret_h(text)
            }
            Imp::ReadWith => {
                let Value::Ref(r) = self.val(a.h(0)?)? else {
                    return trap("read takes a resource");
                };
                let matching = self.text(a.h(1)?)?;
                let options: ReadOptions =
                    serde_json::from_str(&self.text(a.h(2)?)?).unwrap_or_default();
                let text =
                    self.journaled_read(|| self.read_with(&r.target, &matching, &options))?;
                self.ret_h(text)
            }
            Imp::PrintRows => {
                let list = self.val(a.h(0)?)?;
                let fields: Vec<String> =
                    serde_json::from_str(&self.text(a.h(1)?)?).unwrap_or_default();
                let ty = self.text(a.h(2)?)?;
                let items = match list {
                    Value::List(items) => items,
                    Value::Coll(c) => self.coll_items(&c)?,
                    other => vec![other],
                };
                let lines = self.rows(&items, &fields, &ty);
                for line in lines {
                    self.say(line);
                }
                Ok(None)
            }
            Imp::CountBy | Imp::GroupBy => {
                let Value::List(keys) = self.val(a.h(0)?)? else {
                    return trap("count_by takes a list");
                };
                let items = self.items_of(self.val(a.h(1)?)?, "count_by")?;
                self.ret_h(self.grouped(keys, items, imp == Imp::CountBy))
            }
            Imp::Take => {
                let n = usize::try_from(a.long(1)?).unwrap_or(0);
                let items = self.items_of(self.val(a.h(0)?)?, "take")?;
                self.ret_h(Value::List(items.into_iter().take(n).collect()))
            }
            Imp::Single => {
                let items = self.items_of(self.val(a.h(0)?)?, "single")?;
                self.ret_h(self.single(items)?)
            }
            Imp::Me => match self.inputs()?.me {
                Some(me) => self.ret_h(Value::Principal(me)),
                None => trap("`me` is the person a task acts for; this program acts for no one"),
            },
            Imp::ForeignRef => {
                let app = self.text(a.h(0)?)?;
                let ty = self.text(a.h(1)?)?;
                let id = self.text(a.h(2)?)?;
                let inputs = self.inputs()?;
                let shown = Shown {
                    literal: format!("@{}.{ty}:{id}", app_name(&app, &inputs)),
                    app: app.clone(),
                    ty,
                    id,
                    title: None,
                };
                let local = self.holds(&app, &inputs);
                self.ret_h(self.value_of_shown(&shown, local)?)
            }
            Imp::MatchGroup => {
                let group = match_group(&self.val(a.h(0)?)?, a.long(1)?)?;
                self.ret_h(group)
            }
            _ => trap("not a query import"),
        }
    }

    /// `grep(pattern, ...)`: the matches, each a `host.Match`, its counts said first.
    fn grep(&self, pattern: &str, options: &str, scope: &Value) -> HResult<Value> {
        let options: GrepOptions =
            serde_json::from_str(options).map_err(|e| format!("grep: {e}"))?;
        let inputs = self.inputs()?;
        let mentions = match options.mentions.as_deref() {
            Some("me") => Some(
                inputs
                    .me
                    .as_ref()
                    .map(|p| p.0.clone())
                    .ok_or_else(|| "`mentions: me`: this program acts for no one".to_owned())?,
            ),
            other => other.map(str::to_owned),
        };
        let ask = |app: &str, within: Vec<String>| ReadRequest::Grep {
            app: app.to_owned(),
            pattern: pattern.to_owned(),
            within,
            not: options.not.clone(),
            changed: options.changed.clone(),
            mentions: mentions.clone(),
        };
        let mut requests: Vec<ReadRequest> = options
            .apps
            .iter()
            .map(|app| ask(app, Vec::new()))
            .collect();
        // Resources named in `in:`, by app.
        let mut within: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let scoped = match scope {
            Value::List(items) => items.clone(),
            _ => Vec::new(),
        };
        for item in &scoped {
            let Value::Ref(r) = item else {
                continue;
            };
            if let Some(app) = self.app_of(&r.target) {
                within.entry(app).or_default().push(r.target.id_string());
            }
        }
        for (app, ids) in within {
            requests.push(ask(&app, ids));
        }
        let mut rows: Vec<(MatchRow, bool)> = Vec::new();
        let mut searched: Vec<(String, usize)> = Vec::new();
        let mut uncovered: Vec<String> = Vec::new();
        let mut unsearched: Vec<String> = Vec::new();
        for req in &requests {
            let app = req.app().to_owned();
            let name = app_name(&app, &inputs);
            match self.answered(req, &inputs)? {
                Answered::Here(json) => {
                    let answer: GrepAnswer = serde_json::from_value(json)
                        .map_err(|e| format!("grep: {name} answered something else: {e}"))?;
                    searched.push((name.clone(), answer.searched));
                    uncovered.extend(answer.uncovered.iter().map(|u| format!("{name}: {u}")));
                    let local = self.holds(&app, &inputs);
                    rows.extend(answer.matches.into_iter().map(|m| (m, local)));
                }
                Answered::Unavailable(why) => unsearched.push(format!("{name} ({why})")),
                Answered::Asked => {}
            }
        }
        let resources = {
            let mut docs: Vec<&str> = rows.iter().map(|(m, _)| m.doc.literal.as_str()).collect();
            docs.sort_unstable();
            docs.dedup();
            docs.len()
        };
        if !self.waiting() {
            let mut said = format!(
                "grep /{pattern}/: {} {} in {resources} of {} resources searched ({})",
                rows.len(),
                if rows.len() == 1 { "match" } else { "matches" },
                searched.iter().map(|(_, n)| n).sum::<usize>(),
                searched
                    .iter()
                    .map(|(name, n)| format!("{name} {n}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            for u in &uncovered {
                let _ = write!(said, "; {u}");
            }
            if !unsearched.is_empty() {
                let _ = write!(said, "; not searched: {}", unsearched.join(", "));
            }
            self.say(said);
        }
        let mut out = Vec::with_capacity(rows.len());
        for (m, local) in rows {
            out.push(self.match_value(m, local)?);
        }
        Ok(Value::List(out))
    }

    /// A match as a `host.Match`: a tuple of its fields in their order.
    fn match_value(&self, m: MatchRow, local: bool) -> HResult<Value> {
        let doc = self.value_of_shown(&m.doc, local)?;
        Ok(Value::Tuple(vec![
            doc,
            Value::Text(m.anchor),
            Value::Text(m.text),
            Value::Text(m.line),
            Value::List(m.groups.into_iter().map(Value::Text).collect()),
            Value::Text(m.kind),
            Value::Opt(
                m.written_by
                    .map(|w| Box::new(Value::Principal(crate::ids::Principal::new(w)))),
            ),
            Value::Opt(m.changed.map(|c| Box::new(Value::Text(c)))),
        ]))
    }

    /// The items of a list (or a collection read whole) handed to `what`.
    fn items_of(&self, list: Value, what: &str) -> HResult<Vec<Value>> {
        match list {
            Value::List(items) => Ok(items),
            Value::Coll(c) => Ok(self.coll_items(&c)?),
            _ => trap(format!("{what} takes a list")),
        }
    }

    /// `single()`: the one item, or an error listing the candidates.
    fn single(&self, items: Vec<Value>) -> HResult<Value> {
        if items.len() == 1 || self.waiting() {
            // Waiting on answers, the run is only finding what to read.
            if items.is_empty() {
                return trap("waiting on what it reads");
            }
            return Ok(Value::List(items.into_iter().take(1).collect()));
        }
        if items.is_empty() {
            return trap("nothing matches; widen what it looks in, or check the words");
        }
        let mut why = format!("{} match, not one; say which:", items.len());
        for item in items.iter().take(SINGLE_LISTS) {
            let _ = write!(why, "\n{}", self.show(item).lines().next().unwrap_or(""));
        }
        if items.len() > SINGLE_LISTS {
            let _ = write!(why, "\n… {} more", items.len().saturating_sub(SINGLE_LISTS));
        }
        trap(why)
    }

    /// `find("words")` with no type: the resources of every app the program may read whose
    /// text holds the words, each app's best first.
    pub(crate) fn find_everywhere(&self, words: &str) -> HResult<Value> {
        let inputs = self.inputs()?;
        let mut out = Vec::new();
        let mut unsearched = Vec::new();
        for app in self.every_app(&inputs) {
            let req = ReadRequest::Find {
                app: app.clone(),
                words: words.to_owned(),
            };
            match self.answered(&req, &inputs)? {
                Answered::Here(json) => {
                    let found: Vec<Shown> = serde_json::from_value(json).unwrap_or_default();
                    let local = self.holds(&app, &inputs);
                    for shown in found {
                        out.push(self.value_of_shown(&shown, local)?);
                    }
                }
                Answered::Unavailable(why) => {
                    unsearched.push(format!("{} ({why})", app_name(&app, &inputs)));
                }
                Answered::Asked => {}
            }
        }
        if !unsearched.is_empty() && !self.waiting() {
            self.say(format!(
                "find {words:?}: not searched: {}",
                unsearched.join(", ")
            ));
        }
        Ok(Value::List(out))
    }

    /// What `read(target)` answers, as it was read.
    fn read_answer(&self, target: &ResourceRef) -> HResult<Option<ReadAnswer>> {
        if !is_foreign(target) {
            return Ok(Some(self.read_local(target)?));
        }
        let inputs = self.inputs()?;
        let (app, ty) = target.ty.split_once('#').unwrap_or((&target.ty, ""));
        let req = ReadRequest::Read {
            app: app.to_owned(),
            ty: ty.to_owned(),
            id: target.id_string(),
        };
        match self.answered(&req, &inputs)? {
            Answered::Here(json) => Ok(Some(
                serde_json::from_value(json).map_err(|e| format!("read: {e}"))?,
            )),
            Answered::Unavailable(why) => trap(format!("read: {app} could not be read: {why}")),
            Answered::Asked => Ok(None),
        }
    }

    /// `read(target, matching:, around:, page:)`: its size first, then a page of its
    /// paragraphs, each line starting with the resource and the anchor.
    fn read_with(
        &self,
        target: &ResourceRef,
        matching: &str,
        options: &ReadOptions,
    ) -> HResult<Value> {
        let Some(answer) = self.read_answer(target)? else {
            return Ok(Value::Text(String::new()));
        };
        let lit = &answer.shown.literal;
        let chars: usize = answer.parts.iter().map(|p| p.text.chars().count()).sum();
        let mut chosen: Vec<usize> = (0..answer.parts.len()).collect();
        let mut asked = String::new();
        if !matching.is_empty() {
            let re = regex::Regex::new(matching)
                .map_err(|e| format!("`/{matching}/` is no regular expression: {e}"))?;
            let around = options.around.unwrap_or(0);
            let hits: Vec<usize> = answer
                .parts
                .iter()
                .enumerate()
                .filter(|(_, p)| re.is_match(&p.text))
                .map(|(i, _)| i)
                .collect();
            chosen = spread(&hits, around, answer.parts.len());
            asked = format!(
                "; {} {} /{matching}/",
                hits.len(),
                if hits.len() == 1 {
                    "paragraph matches"
                } else {
                    "paragraphs match"
                }
            );
        } else if let Some(at) = &options.at {
            let wanted = at.trim().trim_start_matches('¶');
            let hit = answer
                .parts
                .iter()
                .position(|p| {
                    p.anchor.trim_start_matches('¶') == wanted
                        || p.anchor.ends_with(&format!("¶{wanted}"))
                })
                .ok_or_else(|| format!("read: {lit} has no paragraph {at}"))?;
            chosen = spread(&[hit], options.around.unwrap_or(2), answer.parts.len());
        }
        let pages = chosen.len().div_ceil(crate::reads::READ_PAGE).max(1);
        let page = options.page.unwrap_or(1).clamp(1, pages);
        let title = answer
            .shown
            .title
            .as_ref()
            .map_or_else(String::new, |t| format!(" {t:?}"));
        let mut out = format!(
            "{lit}{title}  {}: {} {}, {chars} characters{asked}",
            answer.shown.ty,
            answer.parts.len(),
            if answer.parts.len() == 1 {
                "paragraph"
            } else {
                "paragraphs"
            }
        );
        if pages > 1 {
            let _ = write!(out, "; page {page} of {pages}");
        }
        let shown: Vec<usize> = chosen
            .iter()
            .copied()
            .skip(
                page.saturating_sub(1)
                    .saturating_mul(crate::reads::READ_PAGE),
            )
            .take(crate::reads::READ_PAGE)
            .collect();
        let mut last: Option<usize> = None;
        for i in shown {
            if last.is_some_and(|l| l.saturating_add(1) < i) {
                let _ = write!(out, "\n{lit} …");
            }
            if let Some(p) = answer.parts.get(i) {
                let _ = write!(out, "\n{lit} {}  {}", p.anchor, p.text);
            }
            last = Some(i);
        }
        if page == 1 && matching.is_empty() && options.at.is_none() {
            for (name, value) in &answer.fields {
                let _ = write!(out, "\n{lit} .{name}  {value}");
            }
        }
        if page < pages {
            let _ = write!(
                out,
                "\n{lit} more: read({lit}, page: {})",
                page.saturating_add(1)
            );
        }
        Ok(Value::Text(out))
    }

    /// `describe(...)` of a declaration of `path`'s app.
    fn describe(&self, path: &Json) -> HResult<Value> {
        let app = path["app"].as_str().unwrap_or_default().to_owned();
        let name = path["name"].as_str().map(str::to_owned);
        let field = path["field"].as_str().map(str::to_owned);
        let loaded = self
            .registry
            .borrow()
            .packages()
            .iter()
            .any(|p| p.meta.app == app);
        if loaded {
            return Ok(Value::Text(self.describe_local(
                &app,
                name.as_deref(),
                field.as_deref(),
            )?));
        }
        let inputs = self.inputs()?;
        let req = ReadRequest::Describe { app, name, field };
        match self.answered(&req, &inputs)? {
            Answered::Here(Json::String(text)) => Ok(Value::Text(text)),
            Answered::Here(other) => trap(format!("describe answered {other}")),
            Answered::Unavailable(why) => trap(format!("describe: {why}")),
            Answered::Asked => Ok(Value::Text(String::new())),
        }
    }

    /// `describe(r)`: what the resource is, and its type.
    fn describe_ref(&self, target: &ResourceRef) -> HResult<Value> {
        let lit = self.ref_literal(target);
        let title = self
            .program_title(target)
            .map_or_else(String::new, |t| format!(" {t:?}"));
        let (app, ty) = if is_foreign(target) {
            let (app, ty) = target.ty.split_once('#').unwrap_or((&target.ty, ""));
            (app.to_owned(), ty.to_owned())
        } else {
            let Some((pkg, info)) = self.registry.borrow().type_info(&target.ty) else {
                return trap("describe: the resource's type is not loaded");
            };
            (pkg.meta.app.clone(), info.name.clone())
        };
        let path = serde_json::json!({"app": app, "name": ty});
        let Value::Text(declared) = self.describe(&path)? else {
            return trap("describe answered no text");
        };
        Ok(Value::Text(format!("{lit}{title}  a {ty}\n{declared}")))
    }

    /// `count_by` (counts) or `group` (the items) of `items` by `keys`, the largest group
    /// first; items with no key are a group of their own, `none`.
    fn grouped(&self, keys: Vec<Value>, items: Vec<Value>, count: bool) -> Value {
        let mut groups: Vec<(String, Value, Vec<Value>)> = Vec::new();
        for (key, item) in keys.into_iter().zip(items) {
            let shown = self.show_inline(&key);
            match groups.iter_mut().find(|(k, ..)| *k == shown) {
                Some((.., members)) => members.push(item),
                None => groups.push((shown, key, vec![item])),
            }
        }
        groups.sort_by_key(|group| std::cmp::Reverse(group.2.len()));
        Value::List(
            groups
                .into_iter()
                .map(|(_, key, members)| {
                    let second = if count {
                        Value::Int(i64::try_from(members.len()).unwrap_or(i64::MAX))
                    } else {
                        Value::List(members)
                    };
                    Value::Tuple(vec![key, second])
                })
                .collect(),
        )
    }

    /// One line per item: the item (a resource by its literal and title, a match by where it
    /// is), then each of `fields`.
    fn rows(&self, items: &[Value], fields: &[String], ty: &str) -> Vec<String> {
        let host_value = ty
            .strip_prefix(&format!("{}.", tilolang::HOST_APP))
            .filter(|name| tilolang::host::value_type(name).is_some());
        let mut lines = Vec::with_capacity(items.len());
        for item in items {
            let mut line = match (item, host_value) {
                (Value::Tuple(_), Some("Match")) => self.show_match(item),
                (Value::Ref(_), _) => self.show(item),
                (other, _) => self.show_inline(other),
            };
            for field in fields {
                let value = match (item, host_value) {
                    (Value::Tuple(cells), Some(name)) => tilolang::host::value_field(name, field)
                        .and_then(|i| cells.get(i))
                        .map_or_else(|| "?".to_owned(), |v| self.show_inline(v)),
                    (Value::Ref(r), _) => self.field_shown(&r.target, field),
                    _ => "?".to_owned(),
                };
                let _ = write!(line, "  {field}: {value}");
            }
            lines.push(line);
        }
        if lines.is_empty() {
            lines.push("(none)".to_owned());
        }
        lines
    }

    /// The field `name` of `target` as a row prints it.
    fn field_shown(&self, target: &ResourceRef, name: &str) -> String {
        let Some((_, info)) = self.registry.borrow().type_info(&target.ty) else {
            return "?".to_owned();
        };
        let Some(field) = info.field_by_id_or_name(name).cloned() else {
            return "?".to_owned();
        };
        match self.read_value(target, &field, None) {
            Ok(Value::List(items)) if field.kind == crate::meta::FieldKind::Collection => {
                format!("{} items", items.len())
            }
            Ok(value) => self.show_inline(&value),
            Err(e) => format!("error: {e}"),
        }
    }

    /// A `host.Match` as it prints: its resource, the anchor, what the paragraph says around
    /// the match, and who wrote it when the system knows.
    fn show_match(&self, m: &Value) -> String {
        let Value::Tuple(cells) = m else {
            return self.show_inline(m);
        };
        let get =
            |name: &str| tilolang::host::value_field("Match", name).and_then(|i| cells.get(i));
        let doc = get("doc").map_or_else(String::new, |d| match d {
            Value::Ref(r) => self.ref_literal(&r.target),
            other => self.show_inline(other),
        });
        let text = |v: Option<&Value>| match v {
            Some(Value::Text(t)) => t.clone(),
            _ => String::new(),
        };
        let mut line = format!(
            "{doc} {}: {}",
            text(get("anchor")),
            excerpt(&text(get("line")), &text(get("text")))
        );
        if let Some(Value::Opt(Some(who))) = get("written_by") {
            let _ = write!(line, "  (by {})", self.show_inline(who));
        }
        line
    }

    /// `value` as `print` shows a value of the type `ty` (as metadata writes it).
    pub(crate) fn show_typed(&self, value: &Value, ty: &str) -> String {
        let is_match = |t: &str| t == format!("{}.Match", tilolang::HOST_APP);
        if is_match(ty) {
            return self.show_match(value);
        }
        if let Some(inner) = ty.strip_prefix("List<").and_then(|t| t.strip_suffix('>'))
            && is_match(inner)
        {
            let items = match value {
                Value::List(items) => items.clone(),
                _ => Vec::new(),
            };
            if items.is_empty() {
                return "(none)".to_owned();
            }
            return items
                .iter()
                .map(|m| self.show_match(m))
                .collect::<Vec<_>>()
                .join("\n");
        }
        self.show(value)
    }
}

/// How programs call `app`.
fn app_name(app: &str, inputs: &ProgramInputs) -> String {
    inputs
        .sources
        .iter()
        .find(|(_, known)| known == app)
        .map_or_else(
            || app.rsplit('.').next().unwrap_or(app).to_owned(),
            |(alias, _)| alias.clone(),
        )
}

/// Group `n` of a `host.Match`, the whole match for 0.
fn match_group(m: &Value, n: i64) -> HResult<Value> {
    let Value::Tuple(fields) = m else {
        return trap("group takes a match");
    };
    let at = tilolang::host::value_field("Match", "groups").unwrap_or(4);
    let Some(Value::List(groups)) = fields.get(at) else {
        return trap("the match holds no groups");
    };
    let Some(group) = usize::try_from(n).ok().and_then(|n| groups.get(n)) else {
        return trap(format!(
            "group({n}): the pattern has {} groups; 0 is the whole match",
            groups.len().saturating_sub(1)
        ));
    };
    Ok(group.clone())
}

/// The indexes `hits` and `around` of each on either side, in order, within `0..len`.
fn spread(hits: &[usize], around: usize, len: usize) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    for &hit in hits {
        let lo = hit.saturating_sub(around);
        let hi = hit.saturating_add(around).min(len.saturating_sub(1));
        for i in lo..=hi {
            if !out.contains(&i) {
                out.push(i);
            }
        }
    }
    out.sort_unstable();
    out
}
