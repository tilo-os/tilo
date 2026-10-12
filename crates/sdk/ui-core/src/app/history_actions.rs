//! Document History as ordinary published actions.
//!
//! One `tilo.history` resource puts History on the app's public surface, so the
//! palette, `tilo-control`, agents and assistive technology all use the same
//! door. Listing needs Read. Previewing a version, comparing, closing a preview
//! and restoring need Write; each action declares its requirement, so the
//! broker leaves it out of the catalogue of any caller without that grant and
//! refuses the invocation, and a local actor without it never resolves it.
//! Every operation reads through the History model behind the pane, and
//! restore takes the pane's review, staleness check and write.

use std::collections::VecDeque;
use std::hash::{Hash, Hasher};

use automerge::{AutoCommit, ChangeHash};
use serde::{Deserialize, Serialize};
use tilo_actions::{
    ActionContract, ActionOutput, ActionParam, ActionSpec, DoorError, EncodedAction, OperationKind,
    ParamType, Resource, ResultSpec, RetryPolicy, RiskLevel,
};

use super::App;
use crate::history_source::{AuthorDirectory, DEFAULT_MAX_ENTRIES, committed_heads};
use crate::i18n::I18nManager;
use crate::permissions::{ActionKind, PermissionScope, UserId};
use crate::standalone_history::{
    HistoryError, HistoryReadContext, HistoryTarget, StandaloneHistory,
};
use crate::standalone_history_host::{decode_entry, encode_entry};
use crate::widget::{HistoryEntry, HistoryEntryId};

pub(crate) const OPERATION: &str = "tilo.history.operation";
pub(crate) const RESOURCE: &str = "tilo.history";
pub(crate) const OPEN: &str = "history.open";
pub(crate) const LIST: &str = "history.list";
pub(crate) const VIEW_AT: &str = "history.view_at";
pub(crate) const COMPARE: &str = "history.compare";
pub(crate) const CLOSE: &str = "history.close";
pub(crate) const RESTORE: &str = "history.restore";

const PAGE: usize = 50;
const MAX_PREVIEWS: usize = 8;
const MAX_HEADS: usize = 64;
/// A preview larger than this reports its heads without the document body.
const MAX_SNAPSHOT_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    Open,
    List {
        resource: Option<String>,
        cursor: Option<String>,
    },
    ViewAt {
        resource: Option<String>,
        entry: String,
    },
    Compare {
        resource: Option<String>,
        entries: Vec<String>,
    },
    Close {
        session: String,
    },
    Restore {
        resource: Option<String>,
        entry: String,
        reviewed_live_heads: Vec<String>,
    },
}

/// Previews an actor opened with `history.view_at`, bounded and pinned to the
/// document they were taken from.
#[derive(Default)]
pub(super) struct HistoryPreviews {
    open: VecDeque<Preview>,
}

struct Preview {
    session: uuid::Uuid,
    actor: String,
    document_identity: uuid::Uuid,
}

impl HistoryPreviews {
    fn open(&mut self, actor: &str, document_identity: uuid::Uuid) -> uuid::Uuid {
        while self.open.len() >= MAX_PREVIEWS {
            self.open.pop_front();
        }
        let session = uuid::Uuid::new_v4();
        self.open.push_back(Preview {
            session,
            actor: actor.to_owned(),
            document_identity,
        });
        session
    }

    fn close(&mut self, actor: &str, session: uuid::Uuid) -> bool {
        let before = self.open.len();
        self.open
            .retain(|preview| !(preview.session == session && preview.actor == actor));
        self.open.len() != before
    }

    /// A replaced document ends every preview taken from the old one.
    fn retain_document(&mut self, document_identity: uuid::Uuid) {
        self.open
            .retain(|preview| preview.document_identity == document_identity);
    }
}

fn lookup(key: &str) -> String {
    I18nManager::lookup_plain(key)
}

fn json_contract(kind: ActionKind, retry: RetryPolicy) -> ActionContract {
    ActionContract {
        kind,
        risk: RiskLevel::None,
        retry,
        result: ResultSpec::Json {
            schema: serde_json::json!({"type": "object"}),
        },
        ..ActionContract::default()
    }
}

fn resource_param() -> ActionParam {
    ActionParam::optional_owned(
        "resource",
        ParamType::Text,
        lookup("history-action-param-resource"),
    )
}

fn entry_param() -> ActionParam {
    ActionParam::required_owned(
        "entry",
        ParamType::Text,
        lookup("history-action-param-entry"),
    )
    .with_schema(serde_json::json!({"pattern": "^[0-9a-f]{32}$"}))
}

fn text(args: &tilo_actions::ActionArgs, name: &str) -> Option<String> {
    args.text(name).map(str::to_owned)
}

fn strings(args: &tilo_actions::ActionArgs, name: &str) -> Vec<String> {
    args.array(name)
        .unwrap_or_default()
        .iter()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_owned)
        .collect()
}

#[expect(
    clippy::expect_used,
    reason = "History commands hold only strings and serialize infallibly"
)]
fn encode(command: &Command) -> EncodedAction {
    EncodedAction::encode_operation(OPERATION, command).expect("History command serializes")
}

fn typed(
    id: &str,
    label: &str,
    params: Vec<ActionParam>,
    contract: ActionContract,
    build: impl Fn(&tilo_actions::ActionArgs) -> Command + Send + Sync + 'static,
) -> ActionSpec {
    ActionSpec::typed_operation(
        id,
        lookup(label),
        lookup("history-action-param-entry"),
        OperationKind::Effect,
        OPERATION,
        params,
        move |args| encode(&build(args)),
    )
    .with_contract(contract)
    .programmatic()
}

/// The published History resource. Only `history.open` is offered to a person
/// picking from a list; the rest take entry ids and heads, which a caller reads
/// from `history.list` and `history.view_at`.
#[expect(
    clippy::expect_used,
    reason = "the nullary Open command serializes infallibly"
)]
pub(crate) fn resource() -> Resource {
    let open = ActionSpec::operation(
        OPEN,
        lookup("history-action-open"),
        OperationKind::View,
        OPERATION,
        &Command::Open,
    )
    .expect("History open serializes");
    let list = typed(
        LIST,
        "history-action-list",
        vec![
            resource_param(),
            ActionParam::optional_owned(
                "cursor",
                ParamType::Text,
                lookup("history-action-param-cursor"),
            ),
        ],
        json_contract(ActionKind::Read, RetryPolicy::Idempotent),
        |args| Command::List {
            resource: text(args, "resource"),
            cursor: text(args, "cursor"),
        },
    );
    let view_at = typed(
        VIEW_AT,
        "history-action-view-at",
        vec![resource_param(), entry_param()],
        json_contract(ActionKind::Undoable, RetryPolicy::AtMostOnce),
        |args| Command::ViewAt {
            resource: text(args, "resource"),
            entry: text(args, "entry").unwrap_or_default(),
        },
    );
    let compare = typed(
        COMPARE,
        "history-action-compare",
        vec![
            resource_param(),
            ActionParam::required_owned(
                "entries",
                ParamType::Array,
                lookup("history-action-param-entries"),
            )
            .with_schema(serde_json::json!({
                "minItems": 1,
                "maxItems": DEFAULT_MAX_ENTRIES,
                "items": {"type": "string", "pattern": "^[0-9a-f]{32}$"},
            })),
        ],
        json_contract(ActionKind::Undoable, RetryPolicy::Idempotent),
        |args| Command::Compare {
            resource: text(args, "resource"),
            entries: strings(args, "entries"),
        },
    );
    let close = typed(
        CLOSE,
        "history-action-close",
        vec![
            ActionParam::required_owned(
                "session",
                ParamType::Text,
                lookup("history-action-param-session"),
            )
            .with_schema(serde_json::json!({"format": "uuid"})),
        ],
        json_contract(ActionKind::Undoable, RetryPolicy::Idempotent),
        |args| Command::Close {
            session: text(args, "session").unwrap_or_default(),
        },
    );
    let restore = typed(
        RESTORE,
        "history-action-restore",
        vec![
            resource_param(),
            entry_param(),
            ActionParam::required_owned(
                "reviewed_live_heads",
                ParamType::Array,
                lookup("history-action-param-heads"),
            )
            .with_schema(serde_json::json!({
                "minItems": 1,
                "maxItems": MAX_HEADS,
                "items": {"type": "string", "pattern": "^[0-9a-f]{64}$"},
            })),
        ],
        ActionContract {
            risk: RiskLevel::Low,
            reversible: true,
            ..json_contract(ActionKind::Undoable, RetryPolicy::AtMostOnce)
        },
        |args| Command::Restore {
            resource: text(args, "resource"),
            entry: text(args, "entry").unwrap_or_default(),
            reviewed_live_heads: strings(args, "reviewed_live_heads"),
        },
    );
    Resource::new(RESOURCE, RESOURCE, lookup("history-action-resource"))
        .with_action(open)
        .with_action(list)
        .with_action(view_at)
        .with_action(compare)
        .with_action(close)
        .with_action(restore)
}

/// The in-app palette already lists the History chrome's own launcher.
pub(super) fn without_duplicate_launcher(resources: &mut [Resource]) {
    let chrome = resources
        .iter()
        .any(|resource| resource.id.as_str() == crate::standalone_history_host::HISTORY_RESOURCE);
    if !chrome {
        return;
    }
    for resource in resources
        .iter_mut()
        .filter(|resource| resource.id.as_str() == RESOURCE)
    {
        resource.actions.retain(|action| action.id != OPEN);
    }
}

fn door(error: HistoryError) -> DoorError {
    let (code, key) = match error {
        HistoryError::Denied => ("permission_denied", "history-action-denied"),
        HistoryError::StaleReview => ("history_stale_review", "standalone-history-restore-stale"),
        HistoryError::PendingEdits => (
            "history_pending_edits",
            "standalone-history-restore-pending",
        ),
        HistoryError::InvalidSelection => {
            return DoorError::InvalidArguments(lookup("history-action-unknown-entry"));
        }
        HistoryError::Stale => ("history_stale", "history-action-stale-document"),
        HistoryError::Unavailable | HistoryError::Comparison(_) => (
            "history_unavailable",
            "standalone-history-restore-unavailable",
        ),
    };
    DoorError::Unavailable {
        code: code.into(),
        reason: lookup(key),
    }
}

fn parse_entry(value: &str) -> Result<HistoryEntryId, DoorError> {
    decode_entry(value)
        .map_err(|_| DoorError::InvalidArguments(lookup("history-action-unknown-entry")))
}

fn hex_heads(heads: &[ChangeHash]) -> Vec<String> {
    let mut heads: Vec<String> = heads.iter().map(ToString::to_string).collect();
    heads.sort_unstable();
    heads
}

/// Names the committed frontier a `history.list` cursor was issued against.
fn frontier_key(heads: &[ChangeHash]) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hex_heads(heads).hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn entry_json(entry: &HistoryEntry) -> serde_json::Value {
    let time_ms = entry
        .timestamp
        .duration_since(crate::time::SystemTime::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        });
    serde_json::json!({
        "entry": encode_entry(entry.id),
        "summary": entry.summary,
        "author": entry.author.name,
        "time_ms": time_ms,
    })
}

/// The document as JSON, or `None` when it exceeds [`MAX_SNAPSHOT_BYTES`].
fn snapshot_json(doc: &AutoCommit) -> Option<serde_json::Value> {
    let value = serde_json::to_value(automerge::AutoSerde::from(doc)).ok()?;
    let size = serde_json::to_vec(&value).map_or(usize::MAX, |bytes| bytes.len());
    (size <= MAX_SNAPSHOT_BYTES).then_some(value)
}

fn output(value: serde_json::Value) -> ActionOutput {
    ActionOutput {
        value: Some(value),
        resources: Vec::new(),
    }
}

impl App {
    /// The published History resource, present only where History is.
    pub(super) fn history_action_resources(&self) -> Vec<Resource> {
        if self.history_chrome_enabled {
            vec![resource()]
        } else {
            Vec::new()
        }
    }

    fn history_document_resource(&self) -> String {
        format!("document:{}", self.state.document_identity())
    }

    /// Run `visit` on a fresh History model over this app's document, with the
    /// same Read admission the pane applies when it opens.
    fn with_history_model<R>(
        &self,
        visit: impl FnOnce(&mut StandaloneHistory, &HistoryReadContext<'_>) -> Result<R, HistoryError>,
    ) -> Result<R, HistoryError> {
        let active_record = self
            .persistence
            .records
            .as_ref()
            .and_then(|records| records.active());
        let context = HistoryReadContext {
            document: &self.state.doc,
            active_record,
            document_identity: self.state.document_identity(),
            permissions: &self.permission_ctx,
        };
        let target = HistoryTarget::Legacy {
            app_id: crate::collaboration::local_app_id(),
            record: active_record.cloned(),
            document_identity: context.document_identity,
        };
        let mut model = StandaloneHistory::open(target, &context, AuthorDirectory::new())?;
        visit(&mut model, &context)
    }

    fn require_history_write(&self) -> Result<(), DoorError> {
        if self.permission_ctx.check(&PermissionScope::app(
            crate::collaboration::local_app_id(),
            ActionKind::Undoable,
        )) {
            Ok(())
        } else {
            Err(door(HistoryError::Denied))
        }
    }

    fn require_history_document(&self, resource: Option<&str>) -> Result<(), DoorError> {
        match resource {
            Some(resource) if resource != self.history_document_resource() => {
                Err(door(HistoryError::Stale))
            }
            _ => Ok(()),
        }
    }

    /// Execute one admitted `tilo.history` operation for `actor`.
    pub(super) fn invoke_history_action(
        &mut self,
        encoded: &EncodedAction,
        actor: &str,
    ) -> Result<ActionOutput, DoorError> {
        let command: Command = serde_json::from_slice(&encoded.payload)
            .map_err(|_| DoorError::InvalidArguments(lookup("history-action-invalid")))?;
        if !self.history_chrome_enabled {
            return Err(door(HistoryError::Unavailable));
        }
        self.actions
            .history_previews
            .retain_document(self.state.document_identity());
        match command {
            Command::Open => {
                self.open_document_history()
                    .map_err(|_| door(HistoryError::Unavailable))?;
                Ok(ActionOutput::default())
            }
            Command::List { resource, cursor } => {
                self.require_history_document(resource.as_deref())?;
                self.list_history(cursor.as_deref())
            }
            Command::ViewAt { resource, entry } => {
                self.require_history_document(resource.as_deref())?;
                self.view_history_at(actor, &entry)
            }
            Command::Compare { resource, entries } => {
                self.require_history_document(resource.as_deref())?;
                self.compare_history(&entries)
            }
            Command::Close { session } => {
                self.require_history_write()?;
                let session = uuid::Uuid::parse_str(&session).map_err(|_| {
                    DoorError::InvalidArguments(lookup("history-action-unknown-session"))
                })?;
                if self.actions.history_previews.close(actor, session) {
                    Ok(output(serde_json::json!({"closed": true})))
                } else {
                    Err(DoorError::InvalidArguments(lookup(
                        "history-action-unknown-session",
                    )))
                }
            }
            Command::Restore {
                resource,
                entry,
                reviewed_live_heads,
            } => {
                self.require_history_document(resource.as_deref())?;
                self.restore_history(actor, &entry, &reviewed_live_heads)
            }
        }
    }

    fn list_history(&self, cursor: Option<&str>) -> Result<ActionOutput, DoorError> {
        let (rows, heads) = self
            .with_history_model(|model, context| {
                Ok((model.rows().to_vec(), committed_heads(context.document)))
            })
            .map_err(door)?;
        let frontier = frontier_key(&heads);
        let offset = match cursor {
            None => 0,
            Some(cursor) => cursor
                .split_once(':')
                .filter(|(_, issued)| *issued == frontier)
                .and_then(|(offset, _)| offset.parse::<usize>().ok())
                .ok_or_else(|| DoorError::Unavailable {
                    code: "history_stale_cursor".into(),
                    reason: lookup("history-action-stale-cursor"),
                })?,
        };
        let entries: Vec<serde_json::Value> = rows
            .iter()
            .rev()
            .skip(offset)
            .take(PAGE)
            .map(entry_json)
            .collect();
        let end = offset.saturating_add(entries.len());
        let next_cursor = (end < rows.len()).then(|| format!("{end}:{frontier}"));
        Ok(output(serde_json::json!({
            "resource": self.history_document_resource(),
            "live_heads": hex_heads(&heads),
            "entries": entries,
            "next_cursor": next_cursor,
        })))
    }

    fn view_history_at(&mut self, actor: &str, entry: &str) -> Result<ActionOutput, DoorError> {
        self.require_history_write()?;
        let id = parse_entry(entry)?;
        let (review, version) = self
            .with_history_model(|model, context| {
                let review = model.review_restore(context, id)?;
                let version = crate::history_mode::frozen_root::document_at(
                    context.document,
                    review.target_heads(),
                );
                Ok((review, version))
            })
            .map_err(door)?;
        let document = snapshot_json(&version);
        let session = self
            .actions
            .history_previews
            .open(actor, self.state.document_identity());
        Ok(output(serde_json::json!({
            "session": session.to_string(),
            "resource": self.history_document_resource(),
            "entry": encode_entry(id),
            "summary": review.label,
            "heads": hex_heads(review.target_heads()),
            "reviewed_live_heads": hex_heads(review.reviewed_live_heads()),
            "document_truncated": document.is_none(),
            "document": document,
        })))
    }

    fn compare_history(&self, entries: &[String]) -> Result<ActionOutput, DoorError> {
        self.require_history_write()?;
        if entries.is_empty() || entries.len() > DEFAULT_MAX_ENTRIES {
            return Err(DoorError::InvalidArguments(lookup(
                "history-action-unknown-entry",
            )));
        }
        let ids = entries
            .iter()
            .map(|entry| parse_entry(entry))
            .collect::<Result<Vec<_>, _>>()?;
        let comparison = self
            .with_history_model(|model, context| {
                model.select(context, &ids)?;
                model.compare_selection(context).cloned()
            })
            .map_err(door)?;
        let side = |bytes: &[u8]| {
            AutoCommit::load(bytes)
                .ok()
                .and_then(|doc| snapshot_json(&doc))
        };
        let before = side(&comparison.before_document);
        let after = side(&comparison.after_document);
        Ok(output(serde_json::json!({
            "resource": self.history_document_resource(),
            "entries": comparison.entries.iter().copied().map(encode_entry).collect::<Vec<_>>(),
            "before_heads": hex_heads(&comparison.before_heads),
            "after_heads": hex_heads(&comparison.after_heads),
            "changes": comparison.patches.len(),
            "documents_truncated": before.is_none() || after.is_none(),
            "before": before,
            "after": after,
        })))
    }

    fn restore_history(
        &mut self,
        actor: &str,
        entry: &str,
        reviewed_live_heads: &[String],
    ) -> Result<ActionOutput, DoorError> {
        let id = parse_entry(entry)?;
        let heads = reviewed_live_heads
            .iter()
            .map(|head| head.parse::<ChangeHash>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| DoorError::InvalidArguments(lookup("history-action-param-heads")))?;
        if heads.is_empty() || heads.len() > MAX_HEADS {
            return Err(DoorError::InvalidArguments(lookup(
                "history-action-param-heads",
            )));
        }
        let confirmed = self
            .with_history_model(|model, context| {
                let review = model.review_restore_against(context, id, heads)?;
                model.validate_restore(context, &review)?;
                Ok(crate::standalone_history::ConfirmedHistoryRestore {
                    review,
                    target: model.target().clone(),
                })
            })
            .map_err(door)?;
        let author = if actor == self.collaboration.local_author.0 {
            self.local_author().clone()
        } else {
            UserId(actor.to_owned())
        };
        let restored = self
            .apply_history_restore(&confirmed, author)
            .map_err(door)?;
        Ok(output(serde_json::json!({
            "restored": restored,
            "resource": self.history_document_resource(),
            "live_heads": hex_heads(&committed_heads(&self.state.doc)),
        })))
    }
}
