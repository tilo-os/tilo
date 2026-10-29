//! What code reads of the platform (`actions(r)`, `lenses(r)`, `meta(r)`, `phrase(...)`) and
//! the value an event carries before it happens.
//!
//! Each answer is a value of the language: a host value type (`host.Action`, `host.Lens`,
//! `host.Meta`, [`tilolang::host::VALUE_TYPES`]) held as a tuple of its fields in their order.
//! What an answer depends on is read through the view's reads, so the view runs again when it
//! moves: an action's label that follows its subject, a title, who is at a resource.

use std::rc::Rc;

use crate::core::{Args, Core, HResult, trap};
use crate::engine::Wasm;
use crate::handles::FrameKind;
use crate::ids::Principal;
use crate::meta::{ActionMeta, Meta, ParamMeta, TypeMeta};
use crate::render::Offer;
use crate::value::{ActionRef, RefValue, ResourceRef, Value};
use crate::view_state::StateKey;

/// The window the lenses on each resource are kept under, as view state of no window: set by
/// the host as presence moves, read by `lenses(r)`.
pub const LENS_WINDOW: &str = "$lenses";
/// Where the runtime keeps how each resource's outside effects stand (`delivery(r)`).
pub const DELIVERY_WINDOW: &str = "$delivery";

/// The platform's phrases in the person's language, with their arguments.
pub type Phrases = Box<dyn Fn(&str, &[(String, String)]) -> String>;

/// A name in sentence case: `new_note` and `NewNote` read "New note".
#[must_use]
pub fn sentence(name: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    for ch in name.chars() {
        if ch == '_' || ch == ' ' {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else if ch.is_uppercase() && !word.is_empty() {
            words.push(std::mem::take(&mut word));
            word.extend(ch.to_lowercase());
        } else {
            word.extend(ch.to_lowercase());
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    let joined = words.join(" ");
    let mut chars = joined.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// What several of a noun are called, as English writes most: `notes`, `entries`, `boxes`.
#[must_use]
pub fn plural(noun: &str) -> String {
    let consonant_y = noun.strip_suffix('y').is_some_and(|stem| {
        stem.chars()
            .last()
            .is_some_and(|c| !matches!(c, 'a' | 'e' | 'i' | 'o' | 'u'))
    });
    if consonant_y {
        format!("{}ies", noun.strip_suffix('y').unwrap_or(noun))
    } else if ["s", "x", "z", "ch", "sh"]
        .iter()
        .any(|end| noun.ends_with(end))
    {
        format!("{noun}es")
    } else {
        format!("{noun}s")
    }
}

/// The label an action is shown with: as the compiler derived it, else its name in sentence
/// case.
#[must_use]
pub fn label_of(action: &ActionMeta) -> String {
    if action.label.is_empty() {
        sentence(&action.name)
    } else {
        action.label.clone()
    }
}

/// The type a parameter of `meta`'s action is of, when it is one of `meta`'s record types.
fn type_named<'m>(meta: &'m Meta, name: &str) -> Option<&'m TypeMeta> {
    if let Some(found) = meta.types.iter().find(|ty| ty.id == name) {
        return Some(found);
    }
    let (app, last) = name
        .rsplit_once('.')
        .map_or((None, name), |(app, last)| (Some(app), last));
    if app.is_some_and(|app| app != meta.app) {
        return None;
    }
    meta.types.iter().find(|ty| ty.name == last)
}

/// Whether an invocation can be offered with only its first parameters given: each of the rest
/// has a default, or is a value its form asks the person for.
fn all_asked(meta: &Meta, params: &[ParamMeta]) -> bool {
    params
        .iter()
        .all(|param| param.default.is_some() || form_asks(meta, &param.ty))
}

/// Whether an action on a holder asks for one of the items it holds (`remove(tasks, t: Task)`):
/// such an action is offered on each item, which gives it, rather than on the holder.
fn asks_for_own_item(meta: &Meta, holder: &str, rest: &[ParamMeta]) -> bool {
    let Some(holder) = meta.types.iter().find(|ty| ty.id == holder) else {
        return false;
    };
    rest.iter()
        .filter(|param| param.default.is_none())
        .any(|param| {
            let item = param.ty.trim();
            holder.fields.iter().any(|field| {
                field
                    .ty
                    .strip_prefix("Collection<")
                    .and_then(|held| held.strip_suffix('>'))
                    .is_some_and(|held| held.trim() == item)
            })
        })
}

/// Whether an action's form can ask the person for a value of the type `ty` (as metadata writes
/// it): text, a number, a flag, a day or a moment, a person, a resource, or a case of the
/// package's own plain enum, or none of one of those. What a form shows for each is the
/// type's control (forms-and-diagrams.md §1).
#[must_use]
pub fn form_asks(meta: &Meta, ty: &str) -> bool {
    use crate::meta::Ty;
    fn plain(meta: &Meta, ty: &Ty) -> bool {
        match ty {
            Ty::Enum(key) => {
                key.app == meta.app
                    && meta.enums.iter().any(|declared| {
                        declared.name == key.name
                            && declared.variants.iter().all(|case| case.fields.is_empty())
                    })
            }
            Ty::Option(inner) => plain(meta, inner),
            _ => true,
        }
    }
    crate::meta::parse_ty(ty.trim(), &meta.app)
        .is_ok_and(|parsed| parsed.asked_by_form() && plain(meta, &parsed))
}

/// The state key `delivery(r)` reads for the resource `id`.
#[must_use]
pub fn delivery_key(id: &str) -> StateKey {
    StateKey {
        window: DELIVERY_WINDOW.to_owned(),
        principal: None,
        state: id.to_owned(),
    }
}

/// The key of the lenses on the resource `id`.
#[must_use]
pub fn lens_key(id: &str) -> StateKey {
    StateKey {
        window: LENS_WINDOW.to_owned(),
        principal: None,
        state: id.to_owned(),
    }
}

/// One action offered on a subject, before it is presented.
struct Offered<'m> {
    action: &'m ActionMeta,
    /// What it is applied to: the subject, or the resource holding the subject and the subject.
    args: Vec<Value>,
    /// Its parameters after those.
    rest: &'m [ParamMeta],
    /// Whether it is presented for the subject: its own actions are, its holder's not.
    present: bool,
}

impl Core {
    /// `delivery(r)` of the document `doc`: how its last outside effect stands, as the host
    /// last said; a view that read it runs again when that moves.
    fn delivery_of(&self, doc: &str) -> HResult<Value> {
        let key = delivery_key(doc);
        let (value, generation) = {
            let states = self.view_states.borrow();
            (
                states
                    .get(&key)
                    .cloned()
                    .map_or_else(Value::none, Value::some),
                states.generation(&key),
            )
        };
        self.note_state_read(key, generation, &value)?;
        Ok(value)
    }

    /// `dated(text, at)`: when `text` says something is due, read at the time `at` in the
    /// person's zone, as a `host.Dated` tuple of the rest of the text, the day and the moment.
    fn dated(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let text = self.text(a.h(0)?)?;
        let at = self.text(a.h(1)?)?;
        let Some(now) = tilolang::calendar::instant(&at) else {
            return trap(format!("dated: `{at}` is not a time"));
        };
        let offset = self.zone.borrow().as_ref().map_or(0, |zone| zone());
        let read = tilolang::natural::dated(&text, now, offset);
        let opt = |v: Option<String>| Value::Opt(v.map(|t| Box::new(Value::Text(t))));
        self.ret_h(Value::Tuple(vec![
            Value::Text(read.text),
            opt(read.day),
            opt(read.at),
        ]))
    }

    pub(crate) fn platform_import(
        &self,
        imp: crate::abi::Imp,
        a: &Args<'_>,
    ) -> HResult<Option<Wasm>> {
        use crate::abi::Imp;
        match imp {
            Imp::EventValue => self.ret_h(Value::Event),
            Imp::EventPart => self.ret_h(Value::EventPart(usize::try_from(a.h(0)?).unwrap_or(0))),
            Imp::Actions => {
                let target = self.ref_arg(a.h(0)?)?;
                let offered = self.actions_on(&target)?;
                self.ret_h(Value::List(offered))
            }
            Imp::Delivery => {
                let target = self.ref_arg(a.h(0)?)?;
                let value = self.delivery_of(&target.target.doc)?;
                self.ret_h(value)
            }
            Imp::Lenses => {
                let target = self.ref_arg(a.h(0)?)?;
                let key = lens_key(&target.target.id_string());
                let (value, generation) = {
                    let states = self.view_states.borrow();
                    (
                        states.get(&key).cloned().unwrap_or(Value::List(Vec::new())),
                        states.generation(&key),
                    )
                };
                self.note_state_read(key, generation, &value)?;
                self.ret_h(value)
            }
            Imp::Versions => {
                let target = self.ref_arg(a.h(0)?)?;
                let versions = self
                    .store
                    .borrow()
                    .versions(&target.target)
                    .map_err(|e| crate::engine::HostFail::Trap(e.to_string()))?;
                self.read_versions_of(&target.target)?;
                let list = versions
                    .into_iter()
                    .map(|v| self.version_value(v))
                    .collect();
                self.ret_h(Value::List(list))
            }
            Imp::VersionEnd => {
                let target = self.ref_arg(a.h(0)?)?;
                let last = a.h(1)? != 0;
                let end = self
                    .store
                    .borrow()
                    .version_end(&target.target, last)
                    .map_err(|e| crate::engine::HostFail::Trap(e.to_string()))?;
                self.read_versions_of(&target.target)?;
                let value = end.map_or_else(Value::none, |v| Value::some(self.version_value(v)));
                self.ret_h(value)
            }
            Imp::MetaOf => {
                let subject = self.val(a.h(0)?)?;
                let meta = match subject {
                    Value::Text(ty) => self.type_meta(&ty, None)?,
                    Value::Ref(r) => self.type_meta(&r.target.ty, Some(&r))?,
                    other => {
                        return trap(format!(
                            "meta: expected a resource or a type, got {}",
                            other.kind_name()
                        ));
                    }
                };
                self.ret_h(meta)
            }
            Imp::Dated => self.dated(a),
            Imp::Phrase => {
                let id = self.text(a.h(0)?)?;
                let args = match self.val(a.h(1)?)? {
                    Value::List(items) => items
                        .into_iter()
                        .filter_map(|item| match item {
                            Value::Tuple(pair) => match pair.as_slice() {
                                [Value::Text(name), Value::Text(value)] => {
                                    Some((name.clone(), value.clone()))
                                }
                                _ => None,
                            },
                            _ => None,
                        })
                        .collect::<Vec<_>>(),
                    _ => Vec::new(),
                };
                let text = match self.phrases.borrow().as_ref() {
                    Some(phrases) => phrases(&id, &args),
                    None => id,
                };
                self.ret_h(Value::Text(text))
            }
            _ => trap("not a platform import"),
        }
    }

    /// A version as the language holds it: who, when, by which action.
    fn version_value(&self, v: crate::store::Version) -> Value {
        Value::Tuple(vec![
            Value::Principal(Principal(v.who)),
            Value::Text(v.at),
            Value::Text(self.action_label(&v.action)),
        ])
    }

    /// Record, in a view's frame, that it read the versions of `target`: it runs again when any
    /// field of the resource changes, which is a new version.
    pub(crate) fn read_versions_of(&self, target: &crate::value::ResourceRef) -> HResult<()> {
        let id = target.id_string();
        let fields: Vec<String> = self
            .type_of_ref(&target.ty)
            .map(|(_, info)| info.fields.iter().map(|f| f.id.clone()).collect())
            .unwrap_or_default();
        if let FrameKind::View(vs) = &mut self.frames.borrow_mut().top_mut()?.kind {
            for field in fields {
                vs.reads.push((id.clone(), field));
            }
        }
        Ok(())
    }

    /// What an action is called where history names it: its label, or what the host did.
    fn action_label(&self, id: &str) -> String {
        if let Some(found) = self
            .registry
            .borrow()
            .action_package(id)
            .and_then(|pkg| pkg.action(id).map(label_of))
        {
            return found;
        }
        sentence(id.trim_start_matches('$'))
    }

    /// Take a view state slot read into the running view's reads, when a view runs.
    pub(crate) fn note_state_read(
        &self,
        key: StateKey,
        generation: u64,
        seen: &Value,
    ) -> HResult<()> {
        let mut frames = self.frames.borrow_mut();
        if let FrameKind::View(vs) = &mut frames.top_mut()?.kind {
            vs.state_reads.push((key.clone(), generation));
            vs.state_seen.push((key, seen.clone()));
        }
        Ok(())
    }

    /// Take `(resource, field)` reads into the running view's reads, when a view runs.
    pub(crate) fn note_reads(
        &self,
        reads: impl IntoIterator<Item = (String, String)>,
    ) -> HResult<()> {
        let mut frames = self.frames.borrow_mut();
        if let FrameKind::View(vs) = &mut frames.top_mut()?.kind {
            vs.reads.extend(reads);
        }
        Ok(())
    }

    /// What the platform shows of the type `ty`, and of the resource `r` of it when there is
    /// one: a `host.Meta`.
    fn type_meta(&self, ty: &str, r: Option<&RefValue>) -> HResult<Value> {
        let (pkg, _) =
            self.registry.borrow().type_info(ty).ok_or_else(|| {
                format!("meta: type `{ty}` is not declared by any loaded package")
            })?;
        let meta = &pkg.meta;
        let Some(declared) = meta.types.iter().find(|t| t.id == ty) else {
            return trap(format!("meta: type `{ty}` is not declared"));
        };
        let type_name = sentence(&declared.name);
        let title = match r {
            Some(r) => self.title_of(&r.target, meta, declared)?,
            None => type_name.clone(),
        };
        Ok(Value::Tuple(vec![
            Value::Text(title),
            Value::Text(type_name.clone()),
            Value::Text(type_name.to_lowercase()),
            Value::Text(plural(&type_name.to_lowercase())),
            meta.icon
                .clone()
                .map_or_else(Value::none, |icon| Value::some(Value::Text(icon))),
            Value::Bool(meta.default_view(ty).is_some()),
        ]))
    }

    /// The title of `target`: its field marked `title`, when it holds more than spaces; else,
    /// for a type without one, the first line of its field marked `body` (a message is called
    /// by what it says, rich text as its reader reads it); else, for a root of its package, the
    /// package's title; else its type's name.
    pub(crate) fn title_of(
        &self,
        target: &ResourceRef,
        meta: &Meta,
        ty: &TypeMeta,
    ) -> HResult<String> {
        let role = |name: &str| {
            ty.fields
                .iter()
                .find(|field| field.role.as_deref() == Some(name))
        };
        let titled = role("title").map(|field| (field, false));
        if let Some((field, body)) = titled.or_else(|| role("body").map(|field| (field, true))) {
            let (_, info) = self.lookup_field(target, &field.id)?;
            self.note_reads([(target.id_string(), field.id.clone())])?;
            let value = self.read_value(target, &info, None)?;
            let text = match value {
                Value::Text(text) if body => text
                    .lines()
                    .find(|line| !line.trim().is_empty())
                    .unwrap_or_default()
                    .to_owned(),
                Value::Text(text) => text,
                Value::Rich(rich) => self.read_rich(&rich, true)?,
                _ => String::new(),
            };
            if !text.trim().is_empty() {
                return Ok(text);
            }
        }
        if target.item.is_none() && meta.roots.contains(&ty.id) {
            return Ok(meta.title.clone());
        }
        Ok(sentence(&ty.name))
    }

    /// The actions offered on `subject`: its type's own that need nothing else (what else they
    /// take has a default, which the invocation gets), the primary one first, and then the
    /// actions of the resource holding it that take it as their second argument, each a
    /// `host.Action`.
    fn actions_on(&self, subject: &RefValue) -> HResult<Vec<Value>> {
        let target = &subject.target;
        let packages = self.registry.borrow().packages();
        let mut out = Vec::new();
        // The subject's own.
        let found = self.registry.borrow().type_info(&target.ty);
        if let Some((pkg, info)) = found {
            let mut own: Vec<&ActionMeta> = pkg
                .meta
                .offered_actions()
                .filter(|action| {
                    action.params.split_first().is_some_and(|(first, rest)| {
                        all_asked(&pkg.meta, rest)
                            && !asks_for_own_item(&pkg.meta, info.id.as_str(), rest)
                            && type_named(&pkg.meta, &first.ty).is_some_and(|t| t.id == info.id)
                    })
                })
                .collect();
            own.sort_by_key(|action| action.role.as_deref() != Some("primary"));
            for action in own {
                let shown = Offered {
                    action,
                    args: vec![Value::Ref(subject.clone())],
                    rest: action.params.get(1..).unwrap_or_default(),
                    present: true,
                };
                out.extend(self.action_value(&shown, subject)?);
            }
        }
        // The holder's, on it.
        if let Some(holder) = self.holder_of(target) {
            let holder_ty = holder.ty.clone();
            for pkg in &packages {
                let Some(holder_info) = pkg.type_by_id(&holder_ty) else {
                    continue;
                };
                for action in pkg.meta.offered_actions() {
                    let [first, second, rest @ ..] = action.params.as_slice() else {
                        continue;
                    };
                    if !all_asked(&pkg.meta, rest) {
                        continue;
                    }
                    let on_holder =
                        type_named(&pkg.meta, &first.ty).is_some_and(|t| t.id == holder_info.id);
                    let takes_subject =
                        type_named(&pkg.meta, &second.ty).is_some_and(|t| t.id == target.ty);
                    if !(on_holder && takes_subject) {
                        continue;
                    }
                    let shown = Offered {
                        action,
                        args: vec![
                            Value::Ref(RefValue {
                                target: Rc::new(holder.clone()),
                                author: subject.author.clone(),
                            }),
                            Value::Ref(subject.clone()),
                        ],
                        rest,
                        present: false,
                    };
                    out.extend(self.action_value(&shown, subject)?);
                }
            }
        }
        Ok(out)
    }

    /// The resource holding the item `target`, with its type: where the store has it now.
    fn holder_of(&self, target: &ResourceRef) -> Option<ResourceRef> {
        let (holder, _) = self.store.borrow().container(target).ok()??;
        Some(holder)
    }

    /// An offered action as a `host.Action`: presented for the subject (its label, icon and
    /// toggled state as its presentation view gives them now), the view taking what that read.
    /// `None` while a requirement of it on the subject does not hold: it is not offered.
    fn action_value(&self, shown: &Offered<'_>, subject: &RefValue) -> HResult<Option<Value>> {
        let action = shown.action;
        let mut label = label_of(action);
        let mut icon = action.icon.clone();
        let mut on: Option<bool> = None;
        if shown.present
            && let Some(view) = &action.present
            && let Some(node) = self.present(view, subject)?
        {
            if let Some(Value::Text(text)) = node.props.get("label") {
                label.clone_from(text);
            }
            if let Some(Value::Text(name)) = node.props.get("icon") {
                icon = Some(name.clone());
            }
            on = match node.props.get("on") {
                Some(Value::Bool(flag)) => Some(*flag),
                Some(Value::Int(flag)) => Some(*flag != 0),
                _ => None,
            };
            if matches!(node.props.get("unavailable"), Some(Value::Text(why)) if !why.is_empty()) {
                return Ok(None);
            }
        }
        // An action that still asks for something opens its form: its label says so, as a
        // menu item that opens a dialog does.
        if shown.rest.iter().any(|param| param.default.is_none()) && !label.ends_with('…') {
            label.push('…');
        }
        let role = action.role.clone().unwrap_or_default();
        let params = shown
            .rest
            .iter()
            .map(|param| {
                Value::Tuple(vec![
                    Value::Text(param.name.clone()),
                    Value::Text(param.ty.clone()),
                    param.default.as_ref().map_or_else(Value::none, |default| {
                        Value::some(Value::Text(default.text.clone()))
                    }),
                ])
            })
            .collect();
        let author = Some(self.module_of_frame()?);
        Ok(Some(Value::Tuple(vec![
            Value::Text(label),
            icon.map_or_else(Value::none, |icon| Value::some(Value::Text(icon))),
            Value::Text(role.clone()),
            Value::Bool(role == "primary" && shown.present),
            Value::Bool(role == "destructive"),
            on.map_or_else(Value::none, |on| Value::some(Value::Bool(on))),
            Value::List(params),
            Value::Action(ActionRef {
                action: action.id.clone(),
                args: shown.args.clone(),
                author,
            }),
        ])))
    }

    /// The `present` node the presentation view `view` builds for `subject`, rendered as part
    /// of the running view: what it read is the running view's too.
    fn present(&self, view: &str, subject: &RefValue) -> HResult<Option<crate::view::UiNode>> {
        let (space, window, viewer, depth) = {
            let frames = self.frames.borrow();
            match &frames.top()?.kind {
                FrameKind::View(vs) => (vs.space, vs.window.clone(), vs.viewer.clone(), 1),
                _ => (
                    crate::space::Space::default(),
                    String::new(),
                    Principal(String::new()),
                    1,
                ),
            }
        };
        let target = self.view_target(view).map_err(|e| e.to_string())?;
        let offer = Offer {
            space,
            viewer: &viewer,
            window: &window,
            depth,
        };
        let rendered = self
            .render_at(&target, &[Value::Ref(subject.clone())], offer, None)
            .map_err(|e| e.to_string())?;
        self.note_reads(rendered.reads.iter().cloned())?;
        {
            let mut frames = self.frames.borrow_mut();
            if let FrameKind::View(vs) = &mut frames.top_mut()?.kind {
                vs.state_reads.extend(rendered.state_reads.iter().cloned());
                vs.state_seen.extend(rendered.state_seen.iter().cloned());
                vs.space_reads.extend(rendered.space_reads.iter().cloned());
            }
        }
        Ok(Some(rendered.root))
    }
}
