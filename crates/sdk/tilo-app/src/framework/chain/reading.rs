//! What a chain publishes to read it and what one read asks for: the read
//! offered on the chain and on each item, the arguments it carries, where a
//! page ended, and one item read whole from the part keeping it, at a
//! version of that part when asked.

use std::any::Any;
use std::rc::Rc;

use tilo_actions::OperationKind;

use super::{CHAIN_READ_TYPE, ChainState, PartType, READ_PAGE, READ_PAGE_MAX, listing};

/// Offer the chain's read on every resource under an item that its part's
/// table reads (an item's rich-text body), bound to that resource.
pub(super) fn offer_nested_reads(
    children: &mut [tilo_actions::ResourceSpec],
    table: &'static tilo_resource::table::ModelTable,
    part: &PartType,
    chain: &str,
) {
    for child in children {
        let id = child.id.as_str().to_owned();
        if tilo_resource::table::reads_item(table, part.kind, &id)
            && !child.actions.iter().any(|spec| spec.id == "read")
        {
            child.actions.push(read_spec(part.label, chain, Some(&id)));
        }
        offer_nested_reads(&mut child.children, table, part, chain);
    }
}

/// The read a chain of parts labelled `label` publishes: on the chain, what it holds, newest part
/// first and paged; on an item of a part, that item's content. `item` is the
/// item's resource id, bound here rather than passed, as the record's own
/// read binds the item it is offered on.
pub(super) fn read_spec(label: &str, chain: &str, item: Option<&str>) -> tilo_actions::ActionSpec {
    let (chain, item) = (chain.to_owned(), item.map(str::to_owned));
    let label = format!("Read the {}", label.to_lowercase());
    let mut spec = tilo_actions::ActionSpec::typed_operation(
        "read",
        label.clone(),
        label,
        OperationKind::View,
        CHAIN_READ_TYPE,
        vec![
            tilo_actions::ActionParam::optional(
                "cursor",
                tilo_actions::ParamType::Text,
                "Where the page read before ended, as its `nextCursor` said (default: the newest part)",
            )
            .nullable(),
            tilo_actions::ActionParam::optional(
                "pageSize",
                tilo_actions::ParamType::Integer,
                "How many items to answer with at most",
            )
            .with_schema(serde_json::json!({
                "type": "integer",
                "minimum": 1,
                "maximum": READ_PAGE_MAX,
            })),
            tilo_actions::ActionParam::optional(
                "fields",
                tilo_actions::ParamType::Array,
                "The fields each row carries of its item, by name (default: what these list of themselves)",
            )
            .with_schema(serde_json::json!({
                "type": "array",
                "items": { "type": "string" },
            })),
            tilo_actions::ActionParam::optional(
                "where",
                tilo_actions::ParamType::Object,
                "Answer only the items whose fields equal these: each a field's name (dotted to reach into a record) with its value, or a list of values for any one of them",
            )
            .with_schema(serde_json::json!({ "type": "object" })),
            tilo_actions::ActionParam::optional(
                "version",
                tilo_actions::ParamType::Text,
                "On an item: a change id or exact causal frontier of the part keeping it (default: current)",
            )
            .nullable(),
            // The chain's own read names no item, but a kind that derives
            // something over every part (a calendar's span of events) answers
            // it by name here, and a read of any item it holds takes its
            // address: both are the item's content rather than a page.
            tilo_actions::ActionParam::optional(
                "item",
                tilo_actions::ParamType::Text,
                "The item to read whole, by its id: one of the chain's, or one the kind derives over every part (default: the listing)",
            )
            .nullable(),
        ]
        .into_iter()
        // An item is read whole, at a version; the chain's listing is paged.
        // The chain's own read can name the item, so it takes both.
        .filter(|param| param.name.as_ref() != "item" || item.is_none())
        .collect(),
        move |args| {
            let given = args.to_json();
            let asked = given
                .get("item")
                .and_then(serde_json::Value::as_str)
                .filter(|item| !item.is_empty())
                .map(str::to_owned);
            let payload = serde_json::json!({
                "chain": chain,
                "item": item.clone().or(asked),
                "cursor": args.text("cursor"),
                "pageSize": args.integer("pageSize"),
                "fields": given.get("fields"),
                "where": given.get("where"),
                "version": args.text("version"),
            });
            tilo_actions::EncodedAction::encode_operation(CHAIN_READ_TYPE, &payload)
                .unwrap_or_else(|_| tilo_actions::EncodedAction {
                    type_id: CHAIN_READ_TYPE.into(),
                    payload: Vec::new(),
                })
        },
    )
    .of_kind(tilo_actions::ActionKind::Read)
    .with_description(
        "What this holds as typed data: the items of its parts, the newest part first, a page at \
         a time with `cursor` passing on where a page ended, each as its collection holds it \
         (`{id, value}`, the value kept to the fields asked for in `fields`), only the items \
         `where` holds of; an item's own content when the read is of that item, by its id.",
    )
    // Typed data for a program to reason about; a person reads what the
    // app shows.
    .for_audience(tilo_actions::Audience::Agents);
    spec.type_mut().contract.result = tilo_actions::ResultSpec::Json {
        schema: serde_json::json!({"type": "object"}),
    };
    spec.reads_document()
}

/// What one read of a chain asks for, as its published action carries it.
pub(super) struct ChainRead {
    pub(super) item: Option<String>,
    pub(super) cursor: Option<String>,
    pub(super) page_size: usize,
    /// The fields each row carries, when the caller names them.
    pub(super) fields: Option<Vec<String>>,
    /// What an item's fields equal for its row to be answered.
    pub(super) filter: listing::Filter,
    /// On an item, the version of the part keeping it to read it at.
    pub(super) version: Option<String>,
}

impl ChainRead {
    pub(super) fn of(payload: &[u8]) -> Result<Self, String> {
        let args: serde_json::Value = if payload.is_empty() {
            serde_json::Value::Null
        } else {
            tilo_actions::decode_json(payload).map_err(|error| error.to_string())?
        };
        let text = |key: &str| {
            args.get(key)
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        let page_size = args
            .get("pageSize")
            .and_then(serde_json::Value::as_u64)
            .and_then(|size| usize::try_from(size).ok())
            .unwrap_or(READ_PAGE)
            .clamp(1, READ_PAGE_MAX);
        let (fields, filter) = listing::asked(&args)?;
        Ok(Self {
            item: text("item"),
            cursor: text("cursor"),
            page_size,
            fields,
            filter,
            version: text("version"),
        })
    }
}

/// Where a page of a chain's read ended: the part it was reading and how many
/// of that part's items, newest first, it has answered with.
pub(super) fn read_cursor(part: u64, taken: usize) -> String {
    format!("{part}:{taken}")
}

pub(super) fn parse_read_cursor(cursor: &str) -> Option<(u64, usize)> {
    let (part, taken) = cursor.split_once(':')?;
    Some((part.parse().ok()?, taken.parse().ok()?))
}

impl ChainState {
    /// One item of a part this device holds, as the read of it answers:
    /// what the part's resource reads of it ([`Resource::read_content`]: a
    /// note as its Markdown and blocks), else its fields; at `version`, a
    /// version of the part keeping it. An item the chain does not show is
    /// not there.
    pub(super) fn read_item(
        &self,
        table: &'static tilo_resource::table::ModelTable,
        parts: &[(u64, Rc<dyn Any>)],
        hidden: &std::collections::HashSet<String>,
        item: &str,
        version: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let part = self.part();
        if hidden.iter().any(|gone| {
            item == gone
                || item
                    .strip_prefix(gone.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        }) {
            return Err(format!("{item} is not there"));
        }
        for (index, state) in parts.iter().rev() {
            if tilo_resource::table::item_json(table, &**state, part.kind, item).is_err() {
                continue;
            }
            return self
                .core
                .workspace
                .store
                .read(&self.core.chain.part(*index), |doc| match version {
                    // A version of the item is one of the part keeping it.
                    Some(version) => {
                        let asked = serde_json::json!({ "item": item, "version": version });
                        let payload =
                            serde_json::to_vec(&asked).map_err(|error| error.to_string())?;
                        super::super::root::answer_read(
                            doc,
                            &payload,
                            &|_| Err("a version is read".to_owned()),
                            &part.content_at,
                        )
                    }
                    None => (part.content)(&**state, doc, Some(item)),
                })
                .ok_or_else(|| "the part is not open here".to_owned())?;
        }
        Err(format!(
            "{item} is in a part of these {} this device does not hold",
            part.label.to_lowercase()
        ))
    }
}

impl ChainState {
    /// [`ChainCore::read`] of the parts held, without the items the chain
    /// does not show; an item read from the part keeping it
    /// ([`Self::read_item`]).
    pub(crate) fn read(&self, payload: &[u8]) -> Result<serde_json::Value, String> {
        let part = self.part();
        let read = ChainRead::of(payload)?;
        let parts = self.parts();
        let typed = (part.typed)(&parts);
        let hidden = (part.hidden)(&*typed);
        if let Some(item) = read.item.as_deref() {
            if let Some(answer) = (part.read_over)(&*(self.base)(), &*typed, item) {
                return answer;
            }
            let table = part
                .table
                .ok_or_else(|| format!("{} answer no reads", part.label))?;
            let address = self.address(table, &parts, item);
            let item = address.as_deref().unwrap_or(item);
            return self.read_item(table, &parts, &hidden, item, read.version.as_deref());
        }
        let base = (self.base)();
        let prepare = |filter: &mut listing::Filter| (part.prepare_read)(filter, &*base, &*typed);
        let held: Vec<(u64, &dyn Any)> = parts
            .iter()
            .map(|(index, state)| (*index, &**state))
            .collect();
        self.core.read(part.table, &held, &read, &prepare, &hidden)
    }
}

impl super::Chains {
    /// `read` of every part the chains hold, each chain's oldest first.
    pub(crate) fn read_parts<T>(
        &self,
        read: &dyn Fn(&ui_core::automerge::AutoCommit) -> T,
    ) -> Vec<(ui_core::history_mode::HistoryPart, T)> {
        let held: Vec<Rc<super::ChainState>> = self.held.borrow().values().cloned().collect();
        let mut read_parts = Vec::new();
        for chain in &held {
            let core = &chain.core;
            for index in core.parts.borrow().keys() {
                let part = ui_core::history_mode::HistoryPart {
                    chain: core.chain.as_str().to_owned(),
                    index: *index,
                };
                read_parts.extend(
                    core.workspace
                        .store
                        .read(&core.chain.part(*index), read)
                        .map(|read| (part, read)),
                );
            }
        }
        read_parts
    }

    /// The chain the part belongs to, while it is held.
    fn holding(&self, part: &ui_core::history_mode::HistoryPart) -> Option<Rc<super::ChainState>> {
        self.held
            .borrow()
            .values()
            .find(|chain| chain.core.chain.as_str() == part.chain)
            .cloned()
    }

    /// The chain's parts as they are now, but `version.part` as it was at `version.heads`, in a store
    /// of their own; with who reads them and the item the selected changes changed, by its address.
    ///
    /// # Errors
    /// The part's chain is not held, or the part did not read at that version.
    pub(crate) fn past_part(
        &self,
        version: &ui_core::history_mode::PartVersion<'_>,
    ) -> Result<PastPart, String> {
        let chain = self
            .holding(version.part)
            .ok_or("the part's chain is not open")?;
        let core = &chain.core;
        let store = ui_core::resource::ResourceStore::in_memory();
        let mut changed = None;
        let indices: Vec<u64> = core.parts.borrow().keys().copied().collect();
        for index in indices {
            let Some(mut doc) = core.workspace.store.read(
                &core.chain.part(index),
                ui_core::automerge::AutoCommit::clone,
            ) else {
                continue;
            };
            if index == version.part.index {
                changed = changed_item(&mut doc, version, core.part.kind);
                doc = doc
                    .fork_at(version.heads)
                    .map_err(|error| error.to_string())?;
            }
            let checkout = core.checkout(index);
            store.open_resource(&checkout);
            store
                .with(&checkout.id, |held| *held = doc)
                .ok_or("the part's version could not be held")?;
        }
        Ok(PastPart {
            store,
            me: core.workspace.me.clone(),
            changed,
        })
    }

    /// Write `part` back as it was at `heads`, by `author`, through the part's history.
    ///
    /// # Errors
    /// The part's chain is not held, or the part refused.
    pub(crate) fn restore_part(
        &self,
        part: &ui_core::history_mode::HistoryPart,
        heads: &[ui_core::automerge::ChangeHash],
        author: &tilo_resource::MemberId,
    ) -> Result<(), String> {
        let chain = self.holding(part).ok_or("the part's chain is not open")?;
        let edit = super::PartEdit {
            part: part.index,
            edit: super::super::history::HistoryEdit::Restore {
                version: super::super::history::version_name(heads),
                ids: None,
            },
        };
        let action = edit.encoded()?;
        super::super::resources::ResourceApi::apply_encoded(
            &*chain,
            &action,
            &tilo_actions::ActionInvoker::member(author.clone()),
        )?;
        let _ = chain.refresh();
        Ok(())
    }
}

/// A part's version beside the chain's other parts as they are now ([`super::Chains::past_part`]).
pub(crate) struct PastPart {
    pub(crate) store: ui_core::resource::ResourceStore,
    pub(crate) me: tilo_resource::MemberId,
    /// The address of the item the selected changes changed.
    pub(crate) changed: Option<String>,
}

/// The address of the first item of `kind` the changes between `version.before` and
/// `version.after` reach: a field of the part's root and the item's id under it.
fn changed_item(
    doc: &mut ui_core::automerge::AutoCommit,
    version: &ui_core::history_mode::PartVersion<'_>,
    kind: &str,
) -> Option<String> {
    use ui_core::automerge::{PatchAction, Prop};
    doc.diff(version.before, version.after)
        .into_iter()
        .find_map(|patch| {
            let mut keys = patch
                .path
                .iter()
                .map(|(_, prop)| prop.clone())
                .collect::<Vec<_>>();
            match patch.action {
                PatchAction::PutMap { key, .. } | PatchAction::DeleteMap { key } => {
                    keys.push(Prop::Map(key));
                }
                _ => {}
            }
            match keys.as_slice() {
                [Prop::Map(field), Prop::Map(item), ..] => {
                    Some(tilo_resource::items::item_resource_id_in(
                        kind,
                        field,
                        &tilo_resource::Id::from_raw(item.clone()),
                    ))
                }
                _ => None,
            }
        })
}

impl ChainState {
    /// The address of the item a listing row names by its own `id`
    /// (`<kind>/<field>/<id>`), found in the part keeping it; `None` for an
    /// address already, or an id no part held here keeps.
    fn address(
        &self,
        table: &'static tilo_resource::table::ModelTable,
        parts: &[(u64, Rc<dyn Any>)],
        item: &str,
    ) -> Option<String> {
        if item.contains('/') {
            return None;
        }
        let id = tilo_resource::Id::from(item);
        let index = self.indexed(parts).holding(&id)?;
        let (_, state) = parts.iter().find(|(at, _)| *at == index)?;
        let mut field = None;
        tilo_resource::stored_items::each_entry(table, &**state, &mut |name, found, _, _| {
            if field.is_none() && found == id {
                field = Some(name);
            }
        });
        Some(tilo_resource::items::item_resource_id_in(
            self.part().kind,
            field?,
            &id,
        ))
    }
}
