//! What an agent does on the API beyond the published actions of the packages: run a block,
//! read the catalogue.
//!
//! They are an [`Effect`] vocabulary published on the `tl:host` resource, so the desktop agent,
//! the runtime api, shortcuts and grants reach them as they reach every other operation. The
//! window answers them with [`Host::answer`]; what they change in the workspace they change through
//! the runtime's own commits, which the window hands to the API like a press's.

use serde_json::{Value as Json, json};
use tilo_actions::ActionOutput;
use tilo_resource::Effect;
use tilolang_runtime::program::ReadRequest;
use tilolang_runtime::value::{ResourceRef, to_json};
use tilolang_runtime::{AgentBlock, UiNode};

use crate::host::Host;
use crate::publish::Caller;

/// The operations the host offers an agent.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "tilolang.process")]
pub enum HostEffect {
    /// Run a block.
    ///
    /// Evaluate a tilolang expression against every app of the workspace: a view block renders, a
    /// value block is shown as data, and an action block runs or, when it needs a person's
    /// approval, waits as an asked invocation until they answer it where the system asks them.
    /// `suggest act(args)` always asks the person first, even where the action could run without:
    /// an agent cannot answer its own suggestion.
    ///
    /// The block itself changes only the workspace's document: each action it applies is decided as
    /// the caller's code, and what it commits carries that action's own need (an outside effect
    /// needs what reaching outside needs), so running it is an undoable change, not an outside
    /// one.
    #[effect(kind = "undoable", returns)]
    RunBlock {
        #[param("The block's source, such as `letters.send(@letters.Draft:id)` or a view body")]
        source: String,
    },
    /// Read for a program.
    ///
    /// Answer what a program running in another app's host asks to read of this app: the
    /// matches of a grep, what search finds, a resource's text, or a declaration (agents.md,
    /// "Querying"). It reads only, as the caller.
    #[effect(kind = "read", returns)]
    ReadForProgram {
        #[param(
            "The request, as JSON: {\"read\": \"grep\" | \"find\" | \"read\" | \"describe\", \"app\": ..., ...}"
        )]
        request: String,
    },
    /// List the catalogue.
    ///
    /// The signatures of every type, action, view and fn of the workspace's apps, doc comments
    /// included.
    #[effect(kind = "read", returns)]
    Catalogue,
}

/// Showing what a search hit names. Each published resource offers it, bound to itself; the window
/// answers it by opening the app at that resource.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "tilolang.open")]
pub enum OpenEffect {
    /// Open.
    ///
    /// Show this resource in the window: its app, with it selected. It is how a resource's own
    /// row opens it, never a row of its own beside it.
    #[effect(view, row, presence, tag = tilo_actions::OPENS_HIT_TAG)]
    Open {
        #[param("The resource's id")]
        anchor: String,
    },
    /// Open in a view.
    ///
    /// Show this resource in the window through one of the views that show it (`open(r, with:
    /// v)`). Each published view other than the type's default opens through its own binding
    /// of it, so a view and the action that opens it name each other.
    #[effect(view, row, presence)]
    OpenWith {
        #[param("The resource's id")]
        anchor: String,
        #[param("The view's id")]
        view: String,
    },
}

/// The operation type every [`OpenEffect`] travels under: what a host that answers openings
/// recognises an invocation by (a session's `tilolang_service::ResourceService`).
pub const OPEN_OPERATION: &str = "tilolang.open";

impl OpenEffect {
    /// The resource this opens.
    #[must_use]
    pub fn anchor(&self) -> &str {
        match self {
            Self::Open { anchor } | Self::OpenWith { anchor, .. } => anchor,
        }
    }

    /// The view it opens the resource through, when not its type's default one.
    #[must_use]
    pub fn view(&self) -> Option<&str> {
        match self {
            Self::Open { .. } => None,
            Self::OpenWith { view, .. } => Some(view),
        }
    }
}

/// What opening a resource asks of the window: the app to show, and the resource whose window to
/// push on it (`open(r)`), when it is not the app's own root.
#[derive(Clone, Debug, PartialEq)]
pub struct OpenPlan {
    pub app: String,
    pub target: Option<tilolang_runtime::ResourceRef>,
    /// The view to show the target through, when not its type's default one.
    pub view: Option<String>,
}

/// Whether the window of one of the package's roots answers a request to show an item of the
/// type `ty` (an id) in its own view (`on show(x: T): ...`).
fn shown_by_a_root(meta: &tilolang_runtime::meta::Meta, ty: &str) -> bool {
    meta.roots.iter().any(|root| {
        meta.default_view(root)
            .and_then(|view| meta.views.iter().find(|v| v.id == view))
            .is_some_and(|view| view.shows.iter().any(|show| show.ty == ty))
    })
}

/// The id of the record type the field type `ty` (`List<WindowsApp>`) is a list of, when the
/// package `meta` declares it.
fn listed_type(meta: &tilolang_runtime::meta::Meta, ty: &str) -> Option<String> {
    let listed = ty.trim().strip_prefix("List<")?.strip_suffix('>')?;
    let name = listed.rsplit('.').next()?;
    meta.types
        .iter()
        .find(|declared| declared.name == name)
        .map(|declared| declared.id.clone())
}

fn tree_json(node: &UiNode) -> Json {
    let props: serde_json::Map<String, Json> = node
        .props
        .iter()
        .filter_map(|(name, value)| Some((name.clone(), to_json(value).ok()?)))
        .collect();
    let mut out = serde_json::Map::new();
    out.insert("kind".to_owned(), json!(node.kind));
    out.insert("key".to_owned(), json!(node.key));
    if !props.is_empty() {
        out.insert("props".to_owned(), Json::Object(props));
    }
    if !node.children.is_empty() {
        out.insert(
            "children".to_owned(),
            node.children.iter().map(tree_json).collect(),
        );
    }
    Json::Object(out)
}

/// How many items of a list or collection a read shows.
const READ_ITEMS: usize = 200;

/// How much of one text a read shows; a longer one says it was cut.
const READ_TEXT_CHARS: usize = 100_000;

/// Name each resource a projection holds (`{"type", "id", ...}`) by its published id too, the
/// `item` a read or an action of the API takes.
fn published_ids(value: &mut Json, app: &str) {
    match value {
        Json::Object(fields) => {
            let id = fields
                .get("id")
                .and_then(Json::as_str)
                .filter(|_| fields.contains_key("type"))
                .map(|id| crate::naming::resource_id(app, id));
            if let Some(id) = id {
                fields.insert("item".into(), Json::String(id));
            }
            for nested in fields.values_mut() {
                published_ids(nested, app);
            }
        }
        Json::Array(items) => {
            for nested in items {
                published_ids(nested, app);
            }
        }
        _ => {}
    }
}

fn output(value: Json) -> ActionOutput {
    ActionOutput {
        value: Some(value),
        resources: Vec::new(),
    }
}

/// What a reader asks of a resource: the resource, and a page of one of its collections.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReadAsk {
    /// The resource, by its published id; the workspace's documents when `None`.
    pub item: Option<String>,
    /// The collection to page, by field name; the item's one collection when it has one.
    pub field: Option<String>,
    /// The values the items' fields hold, by field name, a reference by its published id; `from`
    /// the resource the items were made from.
    pub matching: serde_json::Map<String, Json>,
    /// The page starts after this item, by its published id: the `next` of the page before.
    pub after: Option<String>,
    /// How many items the page holds.
    pub limit: Option<usize>,
}

impl ReadAsk {
    /// The ask a `read`'s arguments make (`tilo_resource::read`).
    ///
    /// # Errors
    /// An argument of the wrong type.
    pub fn of_arguments(args: &Json) -> Result<Self, String> {
        let text = |name: &str| -> Result<Option<String>, String> {
            match args.get(name) {
                None | Some(Json::Null) => Ok(None),
                Some(Json::String(text)) => Ok(Some(text.clone())),
                Some(other) => Err(format!("`{name}` is text, not {other}")),
            }
        };
        let matching = match args.get("where") {
            None | Some(Json::Null) => serde_json::Map::new(),
            Some(Json::Object(matching)) => matching.clone(),
            Some(other) => {
                return Err(format!(
                    "`where` names fields and their values, not {other}"
                ));
            }
        };
        let limit = match args.get("limit") {
            None | Some(Json::Null) => None,
            Some(limit) => Some(
                limit
                    .as_u64()
                    .and_then(|limit| usize::try_from(limit).ok())
                    .ok_or_else(|| format!("`limit` is a count, not {limit}"))?,
            ),
        };
        Ok(Self {
            item: text("item")?,
            field: text("field")?,
            matching,
            after: text("after")?,
            limit,
        })
    }

    /// The ask of the resource `item` as a whole, or of the workspace's documents.
    #[must_use]
    pub fn of(item: Option<&str>) -> Self {
        Self {
            item: item.map(str::to_owned),
            ..Self::default()
        }
    }

    /// Whether it pages a collection.
    #[must_use]
    pub fn pages(&self) -> bool {
        self.field.is_some()
            || !self.matching.is_empty()
            || self.after.is_some()
            || self.limit.is_some()
    }
}

/// What a read of `item` says when the workspace holds no resource of that id the reader may
/// read: the same for one that is not there and one they may not read.
fn unknown(item: &str) -> String {
    format!("no resource `{item}`; read the app for the documents it holds")
}

/// Each resource a projection of `host`'s shows (`{"type", "id", ...}`) that the reader may not
/// read, cut to the reference itself: what holds it names it, and nothing it holds.
fn withheld(shown: &mut Json, host: &Host, reads: &dyn Fn(&ResourceRef) -> bool) {
    use tilolang_runtime::store::Store as _;
    match shown {
        Json::Object(fields) => {
            let hidden = fields
                .get("id")
                .and_then(Json::as_str)
                .filter(|_| fields.contains_key("type"))
                .and_then(|id| ResourceRef::parse_id(id, String::new()))
                .and_then(|at| {
                    Some(ResourceRef {
                        ty: host.store().type_of(&at).ok()?,
                        ..at
                    })
                })
                .is_some_and(|at| !reads(&at));
            if hidden {
                fields.retain(|key, _| key == "id" || key == "type");
                return;
            }
            for nested in fields.values_mut() {
                withheld(nested, host, reads);
            }
        }
        Json::Array(items) => {
            for nested in items {
                withheld(nested, host, reads);
            }
        }
        _ => {}
    }
}

impl Host {
    /// Answer `effect` for `caller`, the actor the API admitted (the person at the window
    /// when `None`). An agent's code is written by its own actor, so what it applies to
    /// another app or outside asks a person first.
    pub fn answer(
        &self,
        effect: HostEffect,
        caller: Option<&Caller>,
    ) -> Result<ActionOutput, String> {
        let agent = &caller.map_or_else(|| self.viewer().clone(), |caller| caller.actor.clone());
        if let Some(caller) = caller.filter(|caller| caller.agent) {
            self.inner.admit_agent(caller);
        }
        match effect {
            HostEffect::RunBlock { source } => {
                let block = self
                    .runtime()
                    .run_agent_block(agent, &source)
                    .map_err(|error| error.to_string())?;
                Ok(output(match block {
                    AgentBlock::Rejected(lines) => {
                        return Err(format!("the block does not compile:\n{}", lines.join("\n")));
                    }
                    AgentBlock::View(tree) => json!({"kind": "view", "tree": tree_json(&tree)}),
                    // A written view: a view, with what it is.
                    AgentBlock::Definition { info, tree } => json!({
                        "kind": "view",
                        "definition": {
                            "name": info.name,
                            "shows": info.shows,
                            "params": info.params.iter().map(|p| json!({"name": p.name, "type": p.ty})).collect::<Vec<_>>(),
                        },
                        "tree": tree.as_deref().map(tree_json),
                    }),
                    AgentBlock::Data(value) => json!({"kind": "data", "value": value}),
                    AgentBlock::Invocation { record, resource } => json!({
                        "kind": "invocation",
                        "state": record.state,
                        "action": record.name,
                        "app": record.app,
                        "result": record.result,
                        "invocation": resource.id_string(),
                    }),
                }))
            }
            HostEffect::ReadForProgram { request } => {
                let request: ReadRequest = serde_json::from_str(&request)
                    .map_err(|error| format!("the request is not one a program makes: {error}"))?;
                self.runtime().answer_read(&request).map(output)
            }
            HostEffect::Catalogue => Ok(output(Json::Array(
                self.runtime()
                    .catalogue()
                    .iter()
                    .filter(|signature| signature.app != tilolang::HOST_APP)
                    .map(|signature| {
                        json!({
                            "app": signature.app,
                            "name": signature.name,
                            "signature": signature.render(),
                        })
                    })
                    .collect(),
            ))),
        }
    }

    /// How to open the resource `anchor` (an id as [`publish_tree`](crate::publish::publish_tree)
    /// publishes it) in the window of `app`: its app, and the deepest resource on the way to it
    /// whose type has a default view, which is the window `open(r)` shows. A hit on something
    /// with no window of its own (a message of a conversation) opens the window of what holds it.
    ///
    /// An anchor of a record a device reading lists (`windows-apps/adobe-reader`: the reading's
    /// resource, then the record's own id) is no resource the workspace holds: it opens `app`,
    /// with the record the reading names selected, which its view answers for its type
    /// ([`ViewInstance::show`](tilolang_runtime::ViewInstance::show)). No view of a reading's
    /// record is published, so `view` opens none of them.
    ///
    /// Opened through a view (`view`, an [`OpenEffect::OpenWith`]), the target is the anchor
    /// itself, which that view shows.
    ///
    /// # Errors
    /// The anchor names nothing in this workspace or in a reading `app` reads.
    pub fn open_plan(
        &self,
        app: &str,
        anchor: &str,
        view: Option<&str>,
    ) -> Result<OpenPlan, String> {
        // The store answers by a resource's type: it is the one the store holds the anchor as.
        if let Some(ty) = self.store().type_of_id(anchor) {
            return self.held_plan(anchor, ty, view);
        }
        // Nothing the workspace holds is called that: a device reading's record may be, which
        // its view shows where the reading stands.
        if view.is_none()
            && let Some(plan) = self.reading_plan(app, anchor)
        {
            return Ok(plan);
        }
        Err(format!("`{anchor}` is not in this workspace"))
    }

    /// How the window of `app` shows the record `anchor` names in a device reading
    /// (`windows-apps/adobe-reader`): the record, as the reading gives it a reference (its own
    /// id), and the type the reading's list declares for it.
    fn reading_plan(&self, app: &str, anchor: &str) -> Option<OpenPlan> {
        let (doc, item) = tilolang_runtime::value::split_id(anchor)?;
        let item = item?;
        // The package that publishes the reading `doc`, by the id it publishes it under.
        for meta in self.runtime().metas() {
            let meta = &meta;
            let Some(root) = meta
                .named_roots
                .iter()
                .find(|root| root.published.as_deref() == Some(doc))
            else {
                continue;
            };
            let Some(reading) = meta.types.iter().find(|declared| declared.id == root.ty) else {
                continue;
            };
            let (json, _) = self
                .runtime()
                .reading(&ResourceRef::root(doc, root.ty.clone()))?;
            // The field of the reading that lists the record, by the key the reading holds it
            // under: one holding a list with the record's id in it.
            let key = json
                .as_object()?
                .iter()
                .find(|(_, list)| {
                    list.as_array().is_some_and(|items| {
                        items
                            .iter()
                            .any(|held| held.get("id").and_then(Json::as_str) == Some(item))
                    })
                })
                .map(|(key, _)| key.as_str())?;
            let field = reading
                .fields
                .iter()
                .find(|field| field.id.rsplit('.').next() == Some(key))?;
            let ty = listed_type(meta, &field.ty)?;
            // A reading names a record it lists by its own id (`foreign::Decoder`), so the
            // record is the resource that id names, of the type the list declares.
            return Some(OpenPlan {
                app: app.to_owned(),
                target: Some(ResourceRef::root(item, ty)),
                view: None,
            });
        }
        None
    }

    /// How the window of the anchor's own app shows the resource `anchor`, held as `ty`.
    fn held_plan(&self, anchor: &str, ty: String, view: Option<&str>) -> Result<OpenPlan, String> {
        use tilolang_runtime::store::Store as _;
        let probe = ResourceRef::parse_id(anchor, ty)
            .ok_or_else(|| format!("`{anchor}` is not a resource id"))?;
        let (package, _, _) = self
            .records()
            .into_iter()
            .find(|(_, _, id)| *id == probe.doc)
            .ok_or_else(|| format!("`{anchor}` is not in this workspace"))?;
        if let Some(view) = view {
            let ty = self
                .store()
                .type_of(&probe)
                .map_err(|error| error.to_string())?;
            return Ok(OpenPlan {
                app: package.meta.app.clone(),
                target: Some(ResourceRef { ty, ..probe }),
                view: Some(view.to_owned()),
            });
        }
        // The items from the document down to the anchor, as they are held now.
        let mut chain = Vec::new();
        let mut at = Some(probe);
        while let Some(item) = at.filter(|at| at.item.is_some()) {
            at = self
                .store()
                .container(&item)
                .map_err(|error| error.to_string())?
                .map(|(holder, _)| holder);
            chain.push(item);
        }
        let mut target = None;
        for item in chain.into_iter().rev() {
            let ty = self
                .store()
                .type_of(&item)
                .map_err(|error| error.to_string())?;
            // An item opens on its own page, or where the window's own view shows it when that
            // view answers for its kind (`on show(p: Project): ...`) though it has no page.
            if package.meta.default_view(&ty).is_some() || shown_by_a_root(&package.meta, &ty) {
                target = Some(ResourceRef { ty, ..item });
            }
        }
        Ok(OpenPlan {
            app: package.meta.app.clone(),
            target,
            view: None,
        })
    }

    /// What the workspace's resources hold, as an agent, a script or an app outside tilolang reads
    /// it through the API's `read`: with no `item`, every document of the workspace with its id,
    /// kind and title; with an `item` (a published id, `tl:<app>:<id>`), that resource with its
    /// fields, each collection as a page of the items it holds, each item named by its published
    /// id. Every resource it shows carries who made it, when, and what from (`created`).
    ///
    /// `ask` pages one collection of the item: the items whose fields hold the values its `where`
    /// names (`from`: made from that resource), after the item `after`, `limit` of them. The
    /// index answers it, so a page of a collection of ten thousand costs the page.
    ///
    /// What it shows is what `reads` says the reader may read ([`Self::reads_for`]): a resource
    /// they may not read reads as one that is not there, an item of a collection they may not read
    /// is left off the page, and a reference to one names it without what it holds.
    ///
    /// # Errors
    /// The item names nothing the reader may read in this workspace, or the ask does not fit it.
    pub fn read_resource(
        &self,
        ask: &ReadAsk,
        reads: &dyn Fn(&ResourceRef) -> bool,
    ) -> Result<Json, String> {
        use crate::meta::MetaExt as _;
        let Some(item) = ask.item.as_deref() else {
            if ask.pages() {
                return Err(
                    "a page of a collection is read of the resource that holds it: name its `item`"
                        .to_owned(),
                );
            }
            return Ok(self.documents(reads));
        };
        let (app, at) = self.readable(item, reads)?;
        let package = self.package(app).ok_or_else(|| unknown(item))?;
        let declared = package
            .meta
            .type_named(&at.ty)
            .ok_or_else(|| unknown(item))?;
        let specs = self
            .runtime()
            .field_specs(&at.ty)
            .ok_or_else(|| unknown(item))?;
        let collections: Vec<&str> = specs
            .iter()
            .filter(|spec| spec.kind == tilolang_runtime::meta::FieldKind::Collection)
            .filter_map(|spec| declared.fields.iter().find(|field| field.id == spec.id))
            .map(|field| field.name.as_str())
            .collect();
        let paged = match ask.field.as_deref() {
            Some(field) => Some(*collections.iter().find(|name| **name == field).ok_or_else(
                || {
                    format!(
                        "`{field}` is no collection of `{item}`; its collections are: {}",
                        collections.join(", ")
                    )
                },
            )?),
            None if ask.pages() => match collections.as_slice() {
                [only] => Some(*only),
                _ => {
                    return Err(format!(
                        "name the collection to page as `field`; those of `{item}` are: {}",
                        collections.join(", ")
                    ));
                }
            },
            None => None,
        };
        let runtime = self.runtime();
        let options = tilolang_runtime::project::ProjectOptions {
            max_items: READ_ITEMS,
            max_text_chars: READ_TEXT_CHARS,
            depth: 1,
        };
        let mut fields = serde_json::Map::new();
        for field in &declared.fields {
            let shown = if collections.contains(&field.name.as_str()) {
                // Each collection reads its first page; the one the ask names, the page it asks.
                let whole = ReadAsk::default();
                let asked = if paged == Some(field.name.as_str()) {
                    ask
                } else {
                    &whole
                };
                self.page(&at, &field.name, asked, reads)?
            } else {
                let Ok(value) = runtime.read(&at, &field.id) else {
                    continue;
                };
                let mut shown = runtime.project_with(&value, options);
                withheld(&mut shown, self, reads);
                shown
            };
            fields.insert(field.name.clone(), shown);
        }
        let mut read = json!({
            "type": at.ty,
            "id": at.id_string(),
            "fields": fields,
        });
        published_ids(&mut read, app);
        if let Json::Object(out) = &mut read {
            out.insert("label".into(), json!(self.label_at(&at)));
            out.insert("item".into(), json!(item));
            out.insert("kind".into(), json!(package.meta.kind_of(&declared.name)));
            out.insert("created".into(), self.created_json(&at, reads));
        }
        Ok(read)
    }

    /// `at` whole, as the system's Export saves a resource no app says how to export: what a
    /// read shows of it ([`Self::read_resource`]), each text and list in full and each
    /// collection holding its items, each read whole the same way, down to the last. What the
    /// reader may not read is left out, as a read leaves it out.
    ///
    /// # Errors
    /// `at` is no resource of a package this host runs, or the store failed to read it.
    pub fn export_reading(
        &self,
        at: &ResourceRef,
        reads: &dyn Fn(&ResourceRef) -> bool,
    ) -> Result<Json, String> {
        let item = at.id_string();
        let app = self.app_of_type(&at.ty).ok_or_else(|| unknown(&item))?;
        let mut read = self.whole(at, &app, reads)?;
        published_ids(&mut read, &app);
        Ok(read)
    }

    /// `at` and everything under it, as [`Self::export_reading`] saves it.
    fn whole(
        &self,
        at: &ResourceRef,
        app: &str,
        reads: &dyn Fn(&ResourceRef) -> bool,
    ) -> Result<Json, String> {
        use crate::meta::MetaExt as _;
        let item = at.id_string();
        let package = self.package(app).ok_or_else(|| unknown(&item))?;
        let declared = package
            .meta
            .type_named(&at.ty)
            .ok_or_else(|| unknown(&item))?;
        let specs = self
            .runtime()
            .field_specs(&at.ty)
            .ok_or_else(|| unknown(&item))?;
        let runtime = self.runtime();
        let whole = tilolang_runtime::project::ProjectOptions {
            max_items: usize::MAX,
            max_text_chars: usize::MAX,
            depth: 0,
        };
        let mut fields = serde_json::Map::new();
        for field in &declared.fields {
            let collection = specs.iter().any(|spec| {
                spec.id == field.id && spec.kind == tilolang_runtime::meta::FieldKind::Collection
            });
            let shown = if collection {
                let page = runtime
                    .page(
                        &tilolang_runtime::paging::PageAsk {
                            at,
                            field: &field.name,
                            matching: &[],
                            from: None,
                            after: None,
                            limit: usize::MAX,
                        },
                        reads,
                    )
                    .map_err(|error| error.to_string())?;
                page.items
                    .iter()
                    .map(|under| self.whole(under, app, reads))
                    .collect::<Result<Vec<_>, _>>()?
                    .into()
            } else {
                let Ok(value) = runtime.read(at, &field.id) else {
                    continue;
                };
                let mut shown = runtime.project_with(&value, whole);
                withheld(&mut shown, self, reads);
                shown
            };
            fields.insert(field.name.clone(), shown);
        }
        Ok(json!({
            "type": at.ty,
            "id": item,
            "kind": package.meta.kind_of(&declared.name),
            "label": self.label_at(at),
            "created": self.created_json(at, reads),
            "fields": fields,
        }))
    }

    /// Every document of the workspace the reader may read, with its id, kind and title.
    fn documents(&self, reads: &dyn Fn(&ResourceRef) -> bool) -> Json {
        use crate::meta::MetaExt as _;
        let records = self
            .records()
            .into_iter()
            .filter_map(|(package, type_name, doc)| {
                let ty = package.meta.type_named(&type_name)?.id.clone();
                let at = ResourceRef::root(doc.clone(), ty);
                reads(&at).then(|| {
                    json!({
                        "item": crate::naming::resource_id(&package.meta.app, &doc),
                        "kind": package.meta.kind_of(&type_name),
                        "title": self.label_at(&at),
                        "created": self.created_json(&at, reads),
                    })
                })
            })
            .collect::<Vec<_>>();
        let kind = self
            .packages()
            .first()
            .map(|package| package.meta.resource_type())
            .unwrap_or_default();
        json!({ "kind": kind, "records": records })
    }

    /// The app and the resource the published id `item` names, when the reader may read it.
    fn readable<'a>(
        &self,
        item: &'a str,
        reads: &dyn Fn(&ResourceRef) -> bool,
    ) -> Result<(&'a str, ResourceRef), String> {
        use tilolang_runtime::store::Store as _;
        let (app, path) = item
            .strip_prefix("tl:")
            .and_then(|rest| rest.split_once(':'))
            .ok_or_else(|| unknown(item))?;
        self.package(app).ok_or_else(|| unknown(item))?;
        let probe = ResourceRef::parse_id(path, String::new()).ok_or_else(|| unknown(item))?;
        let ty = self.store().type_of(&probe).map_err(|_| unknown(item))?;
        let at = ResourceRef { ty, ..probe };
        if !reads(&at) {
            return Err(unknown(item));
        }
        Ok((app, at))
    }

    /// A page of the collection `field` of `at` as `ask` asks it: how many items it holds, its
    /// items, each as a row names it, and where the next page starts.
    fn page(
        &self,
        at: &ResourceRef,
        field: &str,
        ask: &ReadAsk,
        reads: &dyn Fn(&ResourceRef) -> bool,
    ) -> Result<Json, String> {
        let mut matching = Vec::new();
        let mut from = None;
        for (name, value) in &ask.matching {
            if name == "from" {
                let Json::String(id) = value else {
                    return Err("`from` names a resource by its published id".to_owned());
                };
                from = Some(self.readable(id, reads)?.1);
                continue;
            }
            // A reference is matched by the resource it names, which the reader must read.
            let value = match value {
                Json::String(id) if id.starts_with("tl:") => {
                    Json::String(self.readable(id, reads)?.1.id_string())
                }
                other => other.clone(),
            };
            matching.push((name.clone(), value));
        }
        let after = match ask.after.as_deref() {
            Some(after) => Some(
                self.readable(after, reads)?
                    .1
                    .item
                    .ok_or_else(|| unknown(after))?,
            ),
            None => None,
        };
        let limit = ask
            .limit
            .unwrap_or(tilo_resource::read::PAGE_SIZE)
            .clamp(1, tilo_resource::read::MAX_PAGE_SIZE);
        let page = self
            .runtime()
            .page(
                &tilolang_runtime::paging::PageAsk {
                    at,
                    field,
                    matching: &matching,
                    from: from.as_ref(),
                    after: after.as_deref(),
                    limit,
                },
                reads,
            )
            .map_err(|error| error.to_string())?;
        let app = self.app_of_type(&at.ty).unwrap_or_default();
        let items: Vec<Json> = page
            .items
            .iter()
            .map(|item| self.row(item, &app, reads))
            .collect();
        let mut out = serde_json::Map::new();
        out.insert("count".into(), json!(items.len()));
        out.insert("items".into(), Json::Array(items));
        if let Some(next) = page.next {
            let next = at.item(&next, String::new());
            out.insert(
                "next".into(),
                json!(crate::naming::resource_id(&app, &next.id_string())),
            );
        }
        Ok(Json::Object(out))
    }

    /// A resource as a page of a collection lists it: its type, id, published id and label, and
    /// who made it.
    fn row(&self, at: &ResourceRef, app: &str, reads: &dyn Fn(&ResourceRef) -> bool) -> Json {
        let mut row = self.runtime().project_with(
            &tilolang_runtime::Value::resource(at),
            tilolang_runtime::project::ProjectOptions {
                max_items: READ_ITEMS,
                max_text_chars: READ_TEXT_CHARS,
                depth: 0,
            },
        );
        withheld(&mut row, self, reads);
        published_ids(&mut row, app);
        if let Json::Object(fields) = &mut row {
            // A collection may hold several kinds' items (a chat's channels, its categories): a
            // reader tells each by the kind it is.
            let kind = self.package(app).and_then(|package| {
                use crate::meta::MetaExt as _;
                package
                    .meta
                    .type_named(&at.ty)
                    .map(|declared| package.meta.kind_of(&declared.name))
            });
            fields.insert("kind".into(), kind.map_or(Json::Null, Json::String));
            fields.insert("label".into(), json!(self.label_at(at)));
            fields.insert("created".into(), self.created_json(at, reads));
        }
        row
    }

    /// Who made `at`, when, and what from, as a read shows it: what it was made from only when
    /// the reader may read that.
    fn created_json(&self, at: &ResourceRef, reads: &dyn Fn(&ResourceRef) -> bool) -> Json {
        let Ok(Some(created)) = self.runtime().created(at) else {
            return Json::Null;
        };
        let mut out = serde_json::Map::new();
        out.insert(
            "who".into(),
            self.runtime()
                .project(&tilolang_runtime::Value::Actor(created.who)),
        );
        out.insert(
            "owner".into(),
            self.runtime()
                .project(&tilolang_runtime::Value::Actor(created.owner)),
        );
        out.insert("at".into(), json!(created.at));
        if let Some(from) = created.from.filter(|from| reads(from)) {
            let app = self.app_of_type(&from.ty).unwrap_or_default();
            out.insert(
                "from".into(),
                json!(crate::naming::resource_id(&app, &from.id_string())),
            );
        }
        Json::Object(out)
    }

    /// The app whose package declares the type `ty`.
    pub(crate) fn app_of_type(&self, ty: &str) -> Option<String> {
        self.find_in_packages(|package| {
            package
                .meta
                .types
                .iter()
                .any(|declared| declared.id == ty)
                .then(|| package.meta.app.clone())
        })
    }

    /// Answer a `read` (`tilo_resource::read`) whose arguments are `args` for `reader`, of the
    /// resources as they are now ([`Self::read_resource`]): the workspace's documents a page at a
    /// time, as every app's read lists them.
    ///
    /// # Errors
    /// As [`ReadAsk::of_arguments`] and [`Self::read_resource`].
    pub fn answer_read(
        &self,
        args: &Json,
        reader: &tilolang_runtime::Actor,
    ) -> Result<Json, String> {
        let ask = ReadAsk::of_arguments(args)?;
        // Who reads what is the policy's now, whichever host answers the read.
        self.follow_members();
        let content = self.read_resource(&ask, &self.reads_for(reader))?;
        if ask.item.is_some() {
            return Ok(content);
        }
        tilo_resource::read::page(
            content,
            args.get("cursor").and_then(Json::as_str),
            args.get("pageSize").and_then(Json::as_i64),
        )
    }

    /// Whether the reader `reader` may read a resource: what the workspace's grants give them, as
    /// the runtime was handed them ([`tilolang_runtime::Runtime::set_member_policy`]). A host
    /// given no policy has none to decide by, and the API decided what the caller reaches.
    pub fn reads_for<'a>(
        &'a self,
        reader: &'a tilolang_runtime::Actor,
    ) -> impl Fn(&ResourceRef) -> bool + 'a {
        let known = self.runtime().knows_members();
        move |at| !known || self.runtime().may_read(reader, at)
    }

    /// The resource a recorded invocation named by `id` is, for a caller that holds its id.
    #[must_use]
    pub fn invocation_ref(&self, id: &str) -> Option<ResourceRef> {
        ResourceRef::parse_id(id, tilolang::INVOCATION_TYPE.to_owned())
    }
}
