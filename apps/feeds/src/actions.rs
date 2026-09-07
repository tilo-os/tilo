//! Feeds' exported **actions** and the **resource tree** it publishes to Ctrl+K.
//!
//! Every collaborative operation Feeds performs on its shared subscription /
//! read-state store is a [`FeedsAction`]: a small serializable value that
//! deterministically mutates the CRDT (see [`crate::doc`], where folders, feeds
//! and articles live as keyed CRDT maps). Because the store already lives in the
//! shared document, an action replicates and replays for free. The exact same
//! [`EncodedAction`] is what the on-screen affordances (the per-feed context
//! menu, the reader's star / read toggles, the "Mark all read" toolbar button,
//! the add-subscription dialog's commit) dispatch and what the Ctrl+K palette
//! dispatches — one source of truth, one replay path.
//!
//! The resource tree mirrors the data model: the reader owns its folders and
//! subscriptions, and subscriptions own their articles. Each exports the
//! [`ActionSpec`]s Ctrl+K, agents and accessibility can invoke.
//!
//! ## What is *not* an action
//!
//! * **External** — fetching / refreshing a feed over the network and parsing
//!   its articles. Those are direct writes of network-derived content into the
//!   store + document cache, not replayable mutations. Adding a subscription is
//!   split accordingly: [`FeedsAction::AddSubscription`] creates the subscription
//!   *record* (deterministic), then the app kicks a fetch to populate its real
//!   title and articles (external).
//! * **Presentation** — which article is selected, scroll offsets, the active
//!   filter, the responsive layout, and the add-subscription modal opening /
//!   the typing inside it. Only the *commit* of the typed URL is the action.
//!
//! ## Determinism contract
//!
//! Ids are minted at *authoring* time and carried in the payload; a feed's
//! site-url and placeholder title are derived from the url by a pure function.
//! [`Action::apply`] never touches the RNG or the wall-clock, and a malformed
//! payload (an unparseable id) is a **no-op** rather than an error, so a bad
//! replay can't wedge a peer.

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use uuid::Uuid;

use qos_actions::{
    Action, ActionAddress, ActionContract, ActionError, ActionRegistry, ActionSpec, Doc,
    EncodedAction, ExecutionLocus, ExecutionMode, OperationKind, Resource, ResourceProvider,
    RetryPolicy,
};

use crate::doc::FeedsDoc;
use crate::model::{Feed, FeedKind, Folder, Id, ReadSettings};

/// Registry `type_id` for every Feeds action.
pub const FEEDS_ACTION_TYPE: &str = "feeds.action";
pub const FEEDS_OPERATION_TYPE: &str = "feeds.operation";
/// Contextual Ctrl+K snapshots stay small even for an archival-scale reader.
/// Search retains the complete catalogue and omitted stable ids resolve via
/// [`resolve_resource`].
pub const MAX_CONTEXT_FOLDER_RESOURCES: usize = 32;
pub const MAX_CONTEXT_FEED_RESOURCES: usize = 64;
pub const MAX_CONTEXT_ARTICLE_RESOURCES: usize = 128;
pub const MAX_CONTEXT_TAG_RESOURCES: usize = 32;

pub fn feed_resource_id(id: Id) -> String {
    format!("feeds.subscription:{id}")
}

pub fn article_resource_id(id: Id) -> String {
    format!("feeds.article:{id}")
}

pub fn tag_resource_id(tag: &str) -> String {
    let mut encoded = String::with_capacity(tag.len().saturating_mul(2));
    for byte in tag.as_bytes() {
        let _ = write!(encoded, "{byte:02x}");
    }
    format!("feeds.tag:{encoded}")
}

/// Decode a canonical tag resource id without accepting aliases or an
/// allocation bomb. Normalized tags contain at most 48 Unicode scalar values,
/// hence at most 192 UTF-8 bytes / 384 hexadecimal digits.
pub fn tag_from_resource_id(id: &str) -> Option<String> {
    let encoded = id.strip_prefix("feeds.tag:")?;
    if encoded.is_empty() || encoded.len() > 384 || encoded.len() % 2 != 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(encoded.len() / 2);
    for pair in encoded.as_bytes().chunks_exact(2) {
        let &[hi, lo] = pair else { return None };
        let hi = hex_nibble(hi)?;
        let lo = hex_nibble(lo)?;
        bytes.push((hi << 4) | lo);
    }
    let decoded = String::from_utf8(bytes).ok()?;
    let normalized = normalize_tags([decoded.as_str()]).into_iter().next()?;
    (tag_resource_id(&normalized) == id).then_some(normalized)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

pub fn open_feed_address(id: Id) -> ActionAddress {
    ActionAddress::new(feed_resource_id(id), "open")
}

pub fn open_article_address(id: Id) -> ActionAddress {
    ActionAddress::new(article_resource_id(id), "open")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FeedsOperation {
    RefreshAll,
    RevealFeed { id: String },
    RevealArticle { id: String },
    RevealTag { tag: String },
}

/// Decode a Feeds-local view/effect operation, declining foreign type ids.
///
/// # Errors
///
/// Returns a message when a Feeds operation payload is malformed.
pub fn decode_operation(type_id: &str, payload: &[u8]) -> Result<Option<FeedsOperation>, String> {
    if type_id != FEEDS_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|e| e.to_string())
}

/// Guess a feed's kind from its URL scheme — the same rule the fetch worker
/// uses so the record the action creates matches what a refresh would produce.
pub fn guess_kind(url: &str) -> FeedKind {
    if url.trim().starts_with("gemini://") {
        FeedKind::Gemini
    } else {
        FeedKind::Rss
    }
}

/// Every collaborative operation Feeds exports on its subscription + read-state
/// store. Each variant is a whole-item create/delete or a single field write,
/// carrying enough payload to replay deterministically from a shared base state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FeedsAction {
    /// Persist the reader's shared typography and paper preference. The panel
    /// itself remains a local view, but this value is durable and replayable.
    SetReadSettings { settings: ReadSettings },
    /// Create a subscription *record*: a feed row with a pre-minted id, the
    /// typed url, a derived site-url and placeholder title, in `folder` at
    /// `order`. Fetching its real title + articles is external and follows.
    AddSubscription {
        id: String,
        url: String,
        /// Owning folder id (string form), or `""` for uncategorised.
        folder: String,
        order: u32,
        /// Feed wire-kind (`"rss"` / `"atom"` / `"json"` / `"gemini"`).
        kind: String,
    },
    /// Unsubscribe: delete a feed row and every article belonging to it.
    RemoveSubscription { id: String },
    /// Rename a feed (a single field write, so a concurrent rename merges LWW).
    RenameFeed { id: String, title: String },
    /// Move a feed into `folder` (`""` = uncategorised).
    MoveFeed { id: String, folder: String },
    /// Create a folder row with a pre-minted id.
    AddFolder {
        id: String,
        name: String,
        order: u32,
    },
    /// Rename a folder.
    RenameFolder { id: String, name: String },
    /// Delete a folder; its feeds survive, unparented to uncategorised.
    DeleteFolder { id: String },
    /// Persist whether a folder's feed rows are collapsed in the shared
    /// sidebar model.
    SetFolderCollapsed { id: String, collapsed: bool },
    /// Set one article's `read` flag.
    MarkRead { id: String, read: bool },
    /// Set one article's `starred` flag.
    SetStarred { id: String, starred: bool },
    /// Replace an article's normalized user-authored tag set.
    SetArticleTags { id: String, tags: Vec<String> },
    /// Mark a batch of articles read (the ids are resolved from the active
    /// filter / target feed at authoring time and carried here, so `apply`
    /// needs no lookup and two replicas land identically).
    MarkAllRead { ids: Vec<String> },
}

impl Action for FeedsAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            FeedsAction::SetReadSettings { settings } => {
                FeedsDoc::write_settings(doc, &settings.clamped());
            }
            FeedsAction::AddSubscription {
                id,
                url,
                folder,
                order,
                kind,
            } => {
                // Palette prompts and the in-app dialog both trim before
                // authoring; retain that invariant at the replay boundary so
                // a malformed/empty remote payload cannot create an unusable
                // durable subscription resource.
                if !url.trim().is_empty()
                    && let Ok(uuid) = id.parse::<Id>()
                {
                    let feed = Feed {
                        id: uuid,
                        // Placeholder until the fetch fills the real title in —
                        // exactly the fallback the worker uses.
                        title: url.clone(),
                        url: url.clone(),
                        site_url: crate::fetch::derive_site_url(url),
                        folder: parse_opt_id(folder),
                        order: *order,
                        kind: FeedKind::from_key(kind),
                    };
                    FeedsDoc::upsert_feed(doc, &feed);
                }
            }
            FeedsAction::RemoveSubscription { id } => {
                if let Ok(uuid) = id.parse::<Id>() {
                    FeedsDoc::remove_feed(doc, uuid);
                }
            }
            FeedsAction::RenameFeed { id, title } => {
                if let Ok(uuid) = id.parse::<Id>() {
                    FeedsDoc::set_feed_title(doc, uuid, title);
                }
            }
            FeedsAction::MoveFeed { id, folder } => {
                if let Ok(uuid) = id.parse::<Id>() {
                    FeedsDoc::set_feed_folder(doc, uuid, parse_opt_id(folder));
                }
            }
            FeedsAction::AddFolder { id, name, order } => {
                if let Ok(uuid) = id.parse::<Id>() {
                    let folder = Folder {
                        id: uuid,
                        name: name.clone(),
                        order: *order,
                        collapsed: false,
                    };
                    FeedsDoc::upsert_folder(doc, &folder);
                }
            }
            FeedsAction::RenameFolder { id, name } => {
                if let Ok(uuid) = id.parse::<Id>() {
                    FeedsDoc::set_folder_name(doc, uuid, name);
                }
            }
            FeedsAction::DeleteFolder { id } => {
                if let Ok(uuid) = id.parse::<Id>() {
                    FeedsDoc::remove_folder(doc, uuid);
                }
            }
            FeedsAction::SetFolderCollapsed { id, collapsed } => {
                if let Ok(uuid) = id.parse::<Id>() {
                    FeedsDoc::set_folder_collapsed(doc, uuid, *collapsed);
                }
            }
            FeedsAction::MarkRead { id, read } => {
                if let Ok(uuid) = id.parse::<Id>() {
                    FeedsDoc::set_read(doc, uuid, *read);
                }
            }
            FeedsAction::SetStarred { id, starred } => {
                if let Ok(uuid) = id.parse::<Id>() {
                    FeedsDoc::set_starred(doc, uuid, *starred);
                }
            }
            FeedsAction::SetArticleTags { id, tags } => {
                if let Ok(uuid) = id.parse::<Id>() {
                    FeedsDoc::set_tags(doc, uuid, &normalize_tags(tags.iter().map(String::as_str)));
                }
            }
            FeedsAction::MarkAllRead { ids } => {
                for id in ids {
                    if let Ok(uuid) = id.parse::<Id>() {
                        FeedsDoc::set_read(doc, uuid, true);
                    }
                }
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

fn parse_opt_id(s: &str) -> Option<Id> {
    if s.is_empty() {
        None
    } else {
        s.parse::<Id>().ok()
    }
}

/// Register Feeds' action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<FeedsAction>(FEEDS_ACTION_TYPE);
}

/// Encode any [`FeedsAction`] into the wire form the palette and the on-screen
/// affordances both dispatch. The single point that turns a Feeds action into a
/// replayable envelope.
///
/// # Panics
///
/// Never panics: a `FeedsAction` that fails to serialize (it never should —
/// every field is a plain string or bool) falls back to an empty-payload
/// envelope rather than crashing the caller.
pub fn encode(action: &FeedsAction) -> EncodedAction {
    EncodedAction::encode(FEEDS_ACTION_TYPE, action).unwrap_or_else(|_| EncodedAction {
        type_id: FEEDS_ACTION_TYPE.to_string(),
        payload: Vec::new(),
    })
}

// ===========================================================================
// Action builders — the single source of truth shared by the on-screen
// affordances (see `crate::app`) and the Ctrl+K resource tree below.
// ===========================================================================

/// Build the create-subscription action for a freshly typed url. `id` is minted
/// at the call site (once), `order` resolved from the target folder.
pub fn add_subscription_action(id: Id, url: &str, folder: Option<Id>, order: u32) -> FeedsAction {
    FeedsAction::AddSubscription {
        id: id.to_string(),
        url: url.trim().to_string(),
        folder: folder.map(|f| f.to_string()).unwrap_or_default(),
        order,
        kind: guess_kind(url).as_str().to_string(),
    }
}

pub fn remove_subscription_action(id: Id) -> FeedsAction {
    FeedsAction::RemoveSubscription { id: id.to_string() }
}

pub fn rename_feed_action(id: Id, title: &str) -> FeedsAction {
    FeedsAction::RenameFeed {
        id: id.to_string(),
        title: title.trim().to_string(),
    }
}

pub fn move_feed_action(id: Id, folder: Option<Id>) -> FeedsAction {
    FeedsAction::MoveFeed {
        id: id.to_string(),
        folder: folder.map(|f| f.to_string()).unwrap_or_default(),
    }
}

pub fn add_folder_action(id: Id, name: &str, order: u32) -> FeedsAction {
    FeedsAction::AddFolder {
        id: id.to_string(),
        name: name.trim().to_string(),
        order,
    }
}

pub fn rename_folder_action(id: Id, name: &str) -> FeedsAction {
    FeedsAction::RenameFolder {
        id: id.to_string(),
        name: name.trim().to_string(),
    }
}

pub fn delete_folder_action(id: Id) -> FeedsAction {
    FeedsAction::DeleteFolder { id: id.to_string() }
}

pub fn set_folder_collapsed_action(id: Id, collapsed: bool) -> FeedsAction {
    FeedsAction::SetFolderCollapsed {
        id: id.to_string(),
        collapsed,
    }
}

pub fn set_read_settings_action(settings: ReadSettings) -> FeedsAction {
    FeedsAction::SetReadSettings {
        settings: settings.clamped(),
    }
}

pub fn mark_read_action(id: Id, read: bool) -> FeedsAction {
    FeedsAction::MarkRead {
        id: id.to_string(),
        read,
    }
}

pub fn set_starred_action(id: Id, starred: bool) -> FeedsAction {
    FeedsAction::SetStarred {
        id: id.to_string(),
        starred,
    }
}

pub fn mark_all_read_action(ids: &[Id]) -> FeedsAction {
    FeedsAction::MarkAllRead {
        ids: ids.iter().map(ToString::to_string).collect(),
    }
}

pub fn normalize_tags<'a>(tags: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut tags = tags
        .into_iter()
        .map(|tag| tag.trim().trim_start_matches('#').to_lowercase())
        .filter(|tag| !tag.is_empty())
        .map(|tag| tag.chars().take(48).collect::<String>())
        .collect::<Vec<_>>();
    tags.sort();
    tags.dedup();
    tags.truncate(24);
    tags
}

pub fn set_article_tags_action(id: Id, tags: &[String]) -> FeedsAction {
    FeedsAction::SetArticleTags {
        id: id.to_string(),
        tags: normalize_tags(tags.iter().map(String::as_str)),
    }
}

// ===========================================================================
// Resource tree
// ===========================================================================

/// One subscription surfaced as a resource, with the ids the palette actions
/// need pre-resolved so a palette "Mark all as read" builds the byte-identical
/// action the context menu does.
#[derive(Debug, Clone)]
pub struct FeedRef {
    pub id: Id,
    pub title: String,
    pub folder: Option<Id>,
    /// Unread article ids in this feed, resolved at authoring time.
    pub unread: Vec<Id>,
    /// Searchable articles nested beneath this stable subscription resource.
    pub articles: Vec<ArticleRef>,
}

/// One article surfaced as a resource with its current document-backed flags.
#[derive(Debug, Clone)]
pub struct ArticleRef {
    pub id: Id,
    pub title: String,
    pub read: bool,
    pub starred: bool,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TagRef {
    pub name: String,
}

/// One folder surfaced as a resource.
#[derive(Debug, Clone)]
pub struct FolderRef {
    pub id: Id,
    pub name: String,
    pub collapsed: bool,
}

/// The presentation context Feeds hands [`provide_resources`]: its folders and
/// subscriptions, plus the order a palette-created folder should take. Derived
/// entirely from state the document already holds; nothing is fabricated.
#[derive(Debug, Clone, Default)]
pub struct FeedsResourceCtx {
    pub folders: Vec<FolderRef>,
    pub feeds: Vec<FeedRef>,
    pub tags: Vec<TagRef>,
    /// Unread articles across the whole reader, resolved before the global
    /// action is authored so replay never depends on a peer's local filter.
    pub unread: Vec<Id>,
    /// The order a palette-created, uncategorised subscription takes.
    pub next_feed_order: u32,
    /// The order a "New folder" created from the palette takes.
    pub next_folder_order: u32,
    pub settings: ReadSettings,
}

fn immediate_spec(id: &str, label: &str, action: &FeedsAction) -> ActionSpec {
    ActionSpec::immediate(id, label, FEEDS_ACTION_TYPE, action)
        // `ActionSpec`'s fields are private now that it carries typed
        // parameters; build the fallback through the constructor that
        // takes an already-encoded action.
        .unwrap_or_else(|_| {
            ActionSpec::immediate_encoded(id, label.to_string(), action.locus(), encode(action))
        })
}

fn view_spec(id: &str, label: &str, operation: &FeedsOperation) -> Option<ActionSpec> {
    ActionSpec::operation(
        id,
        label,
        OperationKind::View,
        FEEDS_OPERATION_TYPE,
        operation,
    )
    .ok()
    .map(|spec| spec.retry_policy(RetryPolicy::Idempotent))
}

/// A one-line label for a feed, defaulting to a placeholder when untitled.
fn feed_label(f: &FeedRef) -> String {
    let t = f.title.trim();
    if t.is_empty() {
        "Untitled feed".to_string()
    } else {
        t.to_string()
    }
}

/// Build the resource tree Feeds currently has open: the reader (with a "New
/// folder" action), each folder (rename + delete), and each subscription
/// (open + rename + move + mark-all-read + unsubscribe), and each article
/// (open + read + star). Every document-mutating operation is exported as the
/// same action the on-screen affordance dispatches; open is a local View.
#[allow(
    clippy::too_many_lines,
    reason = "the complete reader/folder/feed/article hierarchy stays together so its bounded canonical action inventory is auditable"
)]
pub fn provide_resources(ctx: &FeedsResourceCtx, _doc: &AutoCommit) -> Vec<Resource> {
    let mut root = Resource::new("feeds.reader", "feeds.reader", "Feeds");

    if let Ok(refresh) = ActionSpec::operation(
        "refresh-all",
        "Refresh all subscriptions",
        OperationKind::Effect,
        FEEDS_OPERATION_TYPE,
        &FeedsOperation::RefreshAll,
    ) {
        let refresh = refresh.with_contract(ActionContract {
            retry: RetryPolicy::Idempotent,
            execution: ExecutionMode::Background { cancellable: false },
            tags: vec!["network".into(), "refresh".into()],
            ..ActionContract::default()
        });
        root = root.with_action(if ctx.feeds.is_empty() {
            refresh.unavailable("no_subscriptions", "There are no subscriptions to refresh")
        } else {
            refresh
        });
    }

    // Reader-level durable actions. UUIDs are minted once when prompts commit
    // and then baked into the envelope, so replay stays deterministic.
    let feed_order = ctx.next_feed_order;
    root = root.with_action(ActionSpec::prompted(
        "add-subscription",
        "Add subscription",
        "Feed URL",
        "https://example.com/feed.xml",
        ExecutionLocus::ClientLocal,
        move |url: &str| {
            encode(&add_subscription_action(
                Uuid::new_v4(),
                url,
                None,
                feed_order,
            ))
        },
    ));
    root = root.with_action(immediate_spec(
        "mark-all-read",
        "Mark all articles as read",
        &mark_all_read_action(&ctx.unread),
    ));
    let order = ctx.next_folder_order;
    root = root.with_action(ActionSpec::prompted(
        "new-folder",
        "New folder",
        "Name",
        "folder name…",
        ExecutionLocus::ClientLocal,
        move |name: &str| encode(&add_folder_action(Uuid::new_v4(), name, order)),
    ));

    let settings = ctx.settings;
    let mut settings_resource = Resource::new(
        "feeds.reader-settings",
        "feeds.reader-settings",
        "Reading settings",
    );
    for (id, label, next) in [
        (
            "font-decrease",
            "Decrease reader text size",
            ReadSettings {
                font_scale: settings.font_scale - 0.1,
                ..settings
            }
            .clamped(),
        ),
        (
            "font-increase",
            "Increase reader text size",
            ReadSettings {
                font_scale: settings.font_scale + 0.1,
                ..settings
            }
            .clamped(),
        ),
    ] {
        settings_resource = settings_resource.with_action(immediate_spec(
            id,
            label,
            &set_read_settings_action(next),
        ));
    }
    for font in crate::model::ReadFont::ALL {
        settings_resource = settings_resource.with_action(immediate_spec(
            font.as_str(),
            &format!("Use {} reader font", font.label()),
            &set_read_settings_action(ReadSettings { font, ..settings }),
        ));
    }
    for theme in crate::model::ReadTheme::ALL {
        settings_resource = settings_resource.with_action(immediate_spec(
            theme.as_str(),
            &format!("Use {} reader paper", theme.label()),
            &set_read_settings_action(ReadSettings { theme, ..settings }),
        ));
    }
    root = root.with_child(settings_resource);

    for tag in ctx.tags.iter().take(MAX_CONTEXT_TAG_RESOURCES) {
        let operation = FeedsOperation::RevealTag {
            tag: tag.name.clone(),
        };
        let mut resource = Resource::new(
            tag_resource_id(&tag.name),
            "feeds.tag",
            format!("#{}", tag.name),
        );
        if let Some(open) = view_spec("open", "Open tagged articles", &operation) {
            resource = resource.with_action(open);
        }
        root = root.with_child(resource);
    }

    // Known folder names → id, so a "Move to folder…" prompt can resolve a typed
    // name. Blank explicitly uncategorises; an unknown name is a safe no-op
    // instead of unexpectedly moving the subscription.
    let folder_lookup: Vec<(String, Id)> = ctx
        .folders
        .iter()
        .map(|f| (f.name.trim().to_lowercase(), f.id))
        .collect();

    for folder in ctx.folders.iter().take(MAX_CONTEXT_FOLDER_RESOURCES) {
        let id = folder.id;
        let child = Resource::new(
            format!("feeds.folder:{id}"),
            "feeds.folder",
            folder.name.clone(),
        )
        .with_action(ActionSpec::prompted(
            "rename",
            "Rename folder",
            "Name",
            "folder name…",
            ExecutionLocus::ClientLocal,
            move |name: &str| encode(&rename_folder_action(id, name)),
        ))
        .with_action(
            immediate_spec("delete", "Delete folder", &delete_folder_action(id))
                .destructive()
                .reversible(),
        )
        .with_action(immediate_spec(
            "set-collapsed",
            if folder.collapsed {
                "Expand folder"
            } else {
                "Collapse folder"
            },
            &set_folder_collapsed_action(id, !folder.collapsed),
        ));
        root = root.with_child(child);
    }

    let mut remaining_articles = MAX_CONTEXT_ARTICLE_RESOURCES;
    for feed in ctx.feeds.iter().take(MAX_CONTEXT_FEED_RESOURCES) {
        let id = feed.id;
        let mut child = Resource::new(feed_resource_id(id), "feeds.subscription", feed_label(feed));
        if let Some(open) = view_spec(
            "open",
            "Open feed",
            &FeedsOperation::RevealFeed { id: id.to_string() },
        ) {
            child = child.with_action(open);
        }
        child = child.with_action(ActionSpec::prompted(
            "rename",
            "Rename feed",
            "Title",
            "feed title…",
            ExecutionLocus::ClientLocal,
            move |title: &str| encode(&rename_feed_action(id, title)),
        ));
        child = child.with_action(immediate_spec(
            "mark-read",
            "Mark all as read",
            &mark_all_read_action(&feed.unread),
        ));
        let lookup = folder_lookup.clone();
        let current_folder = feed.folder;
        child = child.with_action(ActionSpec::prompted(
            "move",
            "Move to folder…",
            "Folder",
            "folder name (blank = none)…",
            ExecutionLocus::ClientLocal,
            move |name: &str| {
                let key = name.trim().to_lowercase();
                let folder = if key.is_empty() {
                    None
                } else {
                    lookup
                        .iter()
                        .find(|(n, _)| *n == key)
                        .map_or(current_folder, |(_, fid)| Some(*fid))
                };
                encode(&move_feed_action(id, folder))
            },
        ));
        child = child.with_action(
            immediate_spec(
                "unsubscribe",
                "Unsubscribe",
                &remove_subscription_action(id),
            )
            .destructive()
            .reversible(),
        );
        for article in feed.articles.iter().take(remaining_articles) {
            let article_id = article.id;
            let title = if article.title.trim().is_empty() {
                "Untitled article".to_string()
            } else {
                article.title.clone()
            };
            let mut article_node =
                Resource::new(article_resource_id(article_id), "feeds.article", title);
            if let Some(open) = view_spec(
                "open",
                "Open article",
                &FeedsOperation::RevealArticle {
                    id: article_id.to_string(),
                },
            ) {
                article_node = article_node.with_action(open);
            }
            article_node = article_node
                .with_action(immediate_spec(
                    "read",
                    if article.read {
                        "Mark unread"
                    } else {
                        "Mark read"
                    },
                    &mark_read_action(article_id, !article.read),
                ))
                .with_action(immediate_spec(
                    "star",
                    if article.starred { "Unstar" } else { "Star" },
                    &set_starred_action(article_id, !article.starred),
                ))
                .with_action(ActionSpec::prompted(
                    "set-tags",
                    "Set article tags",
                    "Tags",
                    "design, research, later…",
                    ExecutionLocus::ClientLocal,
                    move |value: &str| {
                        let tags = if value.trim().is_empty() {
                            Vec::new()
                        } else {
                            normalize_tags(value.split(','))
                        };
                        encode(&set_article_tags_action(article_id, &tags))
                    },
                ));
            child = child.with_child(article_node);
        }
        remaining_articles = remaining_articles.saturating_sub(feed.articles.len());
        root = root.with_child(child);
    }

    vec![root]
}

/// Resolve one current stable id without expanding the contextual snapshot.
/// A stale id deliberately returns `None`, so a cached search row cannot
/// resurrect a removed subscription or article.
pub fn resolve_resource(ctx: &FeedsResourceCtx, id: &str) -> Option<Resource> {
    let mut scoped = FeedsResourceCtx {
        unread: ctx.unread.clone(),
        next_feed_order: ctx.next_feed_order,
        next_folder_order: ctx.next_folder_order,
        settings: ctx.settings,
        ..FeedsResourceCtx::default()
    };
    if id == "feeds.reader" || id == "feeds.reader-settings" {
        return provide_resources(&scoped, &AutoCommit::new())
            .into_iter()
            .find_map(|root| {
                if root.id.as_str() == id {
                    Some(root)
                } else {
                    root.children
                        .into_iter()
                        .find(|child| child.id.as_str() == id)
                }
            });
    }
    if let Some(raw) = id.strip_prefix("feeds.folder:") {
        let folder_id = raw.parse::<Id>().ok()?;
        scoped.folders.push(
            ctx.folders
                .iter()
                .find(|folder| folder.id == folder_id)?
                .clone(),
        );
    } else if let Some(raw) = id.strip_prefix("feeds.subscription:") {
        let feed_id = raw.parse::<Id>().ok()?;
        let mut feed = ctx.feeds.iter().find(|feed| feed.id == feed_id)?.clone();
        feed.articles.clear();
        scoped.feeds.push(feed);
    } else if id.starts_with("feeds.tag:") {
        scoped.tags.push(
            ctx.tags
                .iter()
                .find(|tag| tag_resource_id(&tag.name) == id)?
                .clone(),
        );
    } else {
        let raw = id.strip_prefix("feeds.article:")?;
        let article_id = raw.parse::<Id>().ok()?;
        let feed = ctx
            .feeds
            .iter()
            .find(|feed| feed.articles.iter().any(|article| article.id == article_id))?;
        let mut feed = feed.clone();
        feed.articles.retain(|article| article.id == article_id);
        scoped.feeds.push(feed);
    }
    let root = provide_resources(&scoped, &AutoCommit::new()).pop()?;
    root.children.into_iter().find_map(|child| {
        if child.id.as_str() == id {
            Some(child)
        } else {
            child
                .children
                .into_iter()
                .find(|article| article.id.as_str() == id)
        }
    })
}

impl ResourceProvider for crate::app::FeedsApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        provide_resources(&self.feeds_resource_ctx(), doc)
    }

    fn resource(&self, _doc: &AutoCommit, id: &str) -> Option<Resource> {
        resolve_resource(&self.feeds_resource_ctx_for(id)?, id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::FeedsApp;
    use crate::model::Filter;
    use qos_actions::{ActionInput, LocalTransport};
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            ActionInput::Prompted { .. } => panic!("expected an immediate action: {}", spec.id),
        }
    }

    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            ActionInput::Immediate(_) => panic!("expected a prompted action: {}", spec.id),
        }
    }

    #[test]
    fn contextual_resource_tree_is_bounded_and_late_articles_resolve_exactly() {
        let mut ctx = FeedsResourceCtx::default();
        for feed_index in 0..(MAX_CONTEXT_FEED_RESOURCES + 2) {
            let feed_id = Uuid::from_u128(feed_index as u128 + 1);
            let articles = (0..3)
                .map(|article_index| ArticleRef {
                    id: Uuid::from_u128(10_000 + (feed_index * 3 + article_index) as u128),
                    title: format!("Article {feed_index}-{article_index}"),
                    read: false,
                    starred: false,
                    tags: Vec::new(),
                })
                .collect::<Vec<_>>();
            ctx.feeds.push(FeedRef {
                id: feed_id,
                title: format!("Feed {feed_index:03}"),
                folder: None,
                unread: articles.iter().map(|article| article.id).collect(),
                articles,
            });
        }
        let late = ctx
            .feeds
            .last()
            .and_then(|feed| feed.articles.last())
            .expect("fixture has a late article")
            .id;

        let page = provide_resources(&ctx, &AutoCommit::new());
        let root = &page[0];
        assert!(
            root.children
                .iter()
                .filter(|resource| resource.kind == "feeds.subscription")
                .count()
                <= MAX_CONTEXT_FEED_RESOURCES
        );
        assert!(
            root.children
                .iter()
                .flat_map(|resource| &resource.children)
                .filter(|resource| resource.kind == "feeds.article")
                .count()
                <= MAX_CONTEXT_ARTICLE_RESOURCES
        );
        assert!(
            qos_actions::door::resolve(&page, &open_article_address(late)).is_none(),
            "the late article is intentionally outside the contextual page"
        );
        let exact = resolve_resource(&ctx, &article_resource_id(late))
            .expect("the omitted article resolves lazily");
        assert!(
            qos_actions::door::resolve(&[exact], &open_article_address(late)).is_some(),
            "lazy resolution exports the exact canonical open action"
        );
        assert!(
            resolve_resource(&ctx, &article_resource_id(Uuid::new_v4())).is_none(),
            "a stale article id is refused"
        );
    }

    #[test]
    fn tag_resource_ids_are_lossless_canonical_and_bounded() {
        let tag = "design/日本語";
        let id = tag_resource_id(tag);
        assert_eq!(tag_from_resource_id(&id).as_deref(), Some(tag));
        assert!(tag_from_resource_id("feeds.tag:44455349474e").is_none());
        assert!(tag_from_resource_id("feeds.tag:0").is_none());
        assert!(tag_from_resource_id(&format!("feeds.tag:{}", "aa".repeat(193))).is_none());
    }

    /// A demo reader synced into a fresh document, plus the app that owns it.
    fn seeded() -> (FeedsApp, AutoCommit) {
        let mut app = FeedsApp::new_demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        (app, doc)
    }

    /// Ctrl+K on Feeds yields the tree: the reader with a "New folder" action, a
    /// `feeds.folder` child per folder (rename + delete), and a
    /// `feeds.subscription` child per feed (rename + mark-read + move +
    /// unsubscribe). Drives the real resource hook.
    #[allow(
        clippy::too_many_lines,
        reason = "one contract test deliberately audits the complete nested reader resource tree"
    )]
    #[test]
    fn ctrl_k_publishes_the_reader_tree() {
        let (app, doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        assert_eq!(resources.len(), 1, "one reader root");
        let reader = &resources[0];
        assert_eq!(reader.kind, "feeds.reader");
        let reader_actions: Vec<&str> = reader.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(
            reader_actions.contains(&"add-subscription"),
            "{reader_actions:?}"
        );
        assert!(
            reader_actions.contains(&"mark-all-read"),
            "{reader_actions:?}"
        );
        assert!(reader_actions.contains(&"new-folder"), "{reader_actions:?}");
        assert!(
            reader_actions.contains(&"refresh-all"),
            "{reader_actions:?}"
        );
        let refresh = reader
            .actions
            .iter()
            .find(|a| a.id == "refresh-all")
            .unwrap();
        assert_eq!(
            refresh.contract.execution,
            qos_actions::ExecutionMode::Background { cancellable: false }
        );
        assert_eq!(refresh.contract.result, qos_actions::ResultSpec::None);
        assert_eq!(refresh.kind, OperationKind::Effect);

        let folder = reader
            .children
            .iter()
            .find(|r| r.kind == "feeds.folder")
            .expect("a folder resource");
        let f_actions: Vec<&str> = folder.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(f_actions.contains(&"rename"), "{f_actions:?}");
        assert!(f_actions.contains(&"delete"), "{f_actions:?}");

        let feed = reader
            .children
            .iter()
            .find(|r| r.kind == "feeds.subscription")
            .expect("a subscription resource");
        let s_actions: Vec<&str> = feed.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(s_actions.contains(&"rename"), "{s_actions:?}");
        assert!(s_actions.contains(&"mark-read"), "{s_actions:?}");
        assert!(s_actions.contains(&"move"), "{s_actions:?}");
        assert!(s_actions.contains(&"unsubscribe"), "{s_actions:?}");
        assert_eq!(
            feed.actions.iter().find(|a| a.id == "open").unwrap().kind,
            OperationKind::View
        );
        assert_eq!(
            feed.actions
                .iter()
                .find(|a| a.id == "mark-read")
                .unwrap()
                .kind,
            OperationKind::Document
        );
        assert_eq!(
            feed.actions.iter().find(|a| a.id == "rename").unwrap().kind,
            OperationKind::Document
        );
        assert_eq!(
            feed.actions
                .iter()
                .find(|a| a.id == "unsubscribe")
                .unwrap()
                .contract
                .risk,
            qos_actions::RiskLevel::Destructive
        );

        let article = feed
            .children
            .iter()
            .find(|resource| resource.kind == "feeds.article")
            .expect("articles are first-class nested resources");
        assert_eq!(
            article
                .actions
                .iter()
                .find(|a| a.id == "open")
                .unwrap()
                .kind,
            OperationKind::View
        );
        assert_eq!(
            article
                .actions
                .iter()
                .find(|a| a.id == "read")
                .unwrap()
                .kind,
            OperationKind::Document
        );
        assert_eq!(
            article
                .actions
                .iter()
                .find(|a| a.id == "star")
                .unwrap()
                .kind,
            OperationKind::Document
        );

        // The full flattened tree exports every action — the palette's Actions
        // group is display-capped, so reachability is asserted on the flatten.
        let flat: Vec<&str> = qos_actions::flatten(&resources)
            .iter()
            .map(|f| f.action.label.as_str())
            .collect();
        assert!(flat.contains(&"Add subscription"), "{flat:?}");
        assert!(flat.contains(&"Mark all articles as read"), "{flat:?}");
        assert!(flat.contains(&"New folder"), "{flat:?}");
        assert!(flat.contains(&"Unsubscribe"), "{flat:?}");
        assert!(flat.contains(&"Delete folder"), "{flat:?}");

        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("New folder")),
            "{labels:?}"
        );
        palette.set_query("Rename folder");
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("Rename folder")),
            "{labels:?}"
        );
    }

    #[test]
    fn reader_level_add_subscription_uses_the_replayable_action_contract() {
        let (app, mut doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        let add = resources[0]
            .actions
            .iter()
            .find(|action| action.id == "add-subscription")
            .expect("reader exports canonical add-subscription");
        let encoded = prompted_encode(add, "https://example.test/updates.xml");
        assert_eq!(encoded.type_id, FEEDS_ACTION_TYPE);

        let decoded = registry()
            .decode(&encoded)
            .expect("prompt result decodes through the Feeds registry");
        let mut transport = LocalTransport::new();
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut transport)
            .expect("canonical prompt result applies deterministically");
        assert!(
            FeedsDoc::load(&doc)
                .feeds
                .iter()
                .any(|feed| feed.url == "https://example.test/updates.xml")
        );
    }

    #[test]
    fn blank_canonical_subscription_prompt_is_a_safe_no_op() {
        let (app, mut doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        let add = resources[0]
            .actions
            .iter()
            .find(|action| action.id == "add-subscription")
            .expect("reader exports canonical add-subscription");
        let encoded = prompted_encode(add, "   ");
        let decoded = registry().decode(&encoded).expect("prompt still decodes");
        let before = FeedsDoc::load(&doc).feeds.len();
        let mut transport = LocalTransport::new();
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut transport)
            .expect("blank action is harmless");
        assert_eq!(FeedsDoc::load(&doc).feeds.len(), before);
    }

    /// The full undo covenant on one action: dispatched through the executor it
    /// mutates the document and records exactly one undo boundary, and Ctrl+Z
    /// (the host's `undo`) reverts precisely that action.
    #[test]
    fn action_records_one_undo_boundary_and_reverts() {
        let (app, seeded_doc) = seeded();
        let mut state = ui::State::new();
        state.doc = seeded_doc;

        let victim = app.store_feed_ids()[0];
        let before_heads = state.doc.get_heads();
        let before = FeedsDoc::load(&state.doc).feeds.len();

        let action = remove_subscription_action(victim);
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        state.record_committed_action(before_heads, None, None);

        assert_eq!(
            FeedsDoc::load(&state.doc).feeds.len(),
            before - 1,
            "the action removed a feed"
        );
        assert!(
            !FeedsDoc::load(&state.doc)
                .feeds
                .iter()
                .any(|f| f.id == victim),
            "the right feed is gone"
        );
        assert!(
            state.peek_last_undo().is_some(),
            "exactly one undo boundary recorded"
        );

        state.undo(None, None).expect("an undo entry to pop");
        assert_eq!(
            FeedsDoc::load(&state.doc).feeds.len(),
            before,
            "undo reverted the removal and nothing else"
        );
        assert!(
            FeedsDoc::load(&state.doc)
                .feeds
                .iter()
                .any(|f| f.id == victim),
            "the feed is back after undo"
        );
    }

    /// The collaboration guarantee: replaying the same encoded action on a
    /// second independent replica (from the same base state) lands on the
    /// byte-identical document — same head hashes, not merely the same values.
    #[test]
    fn action_replays_byte_identical_on_a_peer() {
        let (app, seeded_doc) = seeded();
        let mut a = seeded_doc;
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "precondition: same base state"
        );

        // Star an article: a single field write, deterministic against the base.
        let article = app.store_article_ids()[0];
        let action = set_starred_action(article, true);
        let encoded = encode(&action);

        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "replay reproduced the identical change"
        );
        assert!(
            FeedsDoc::load(&a).article(article).unwrap().starred,
            "the star landed on both replicas"
        );
        assert!(FeedsDoc::load(&b).article(article).unwrap().starred);

        let tags = vec!["design".to_string(), "later".to_string()];
        let action = set_article_tags_action(article, &tags);
        let envelope = qos_actions::apply_local(&mut a, &action, encode(&action)).unwrap();
        qos_actions::replay(&mut b, &envelope, &registry()).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "tag edit replayed identically"
        );
        assert_eq!(
            FeedsDoc::load(&b).article(article).unwrap().tags,
            ["design", "later"]
        );
    }

    #[test]
    fn reader_preferences_and_folder_collapse_replay_byte_identically() {
        let (app, seeded_doc) = seeded();
        let folder = app.store_folder_ids()[0];
        let mut a = seeded_doc;
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();

        let settings = ReadSettings {
            font_scale: 1.35,
            font: crate::model::ReadFont::Serif,
            theme: crate::model::ReadTheme::Sepia,
        };
        for action in [
            set_read_settings_action(settings),
            set_folder_collapsed_action(folder, true),
        ] {
            let envelope = qos_actions::apply_local(&mut a, &action, encode(&action)).unwrap();
            qos_actions::replay(&mut b, &envelope, &registry()).unwrap();
        }

        assert_eq!(a.get_heads(), b.get_heads(), "shared preferences converge");
        assert_eq!(FeedsDoc::read_settings(&a), settings);
        assert!(FeedsDoc::load(&a).folder(folder).unwrap().collapsed);
    }

    /// Adding a subscription replays byte-identically too — the create path, not
    /// just a field toggle.
    #[test]
    fn add_subscription_replays_byte_identical() {
        let (_app, seeded_doc) = seeded();
        let mut a = seeded_doc;
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();

        let id = Uuid::from_u128(0x0BAD_0000_0000_0000_0000_0000_0000_0001);
        let action = add_subscription_action(id, "https://blog.rust-lang.org/feed.xml", None, 7);
        let encoded = encode(&action);

        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(a.get_heads(), b.get_heads(), "create replayed identically");
        let feed = FeedsDoc::load(&b)
            .feed(id)
            .cloned()
            .expect("the feed exists on the peer");
        assert_eq!(feed.url, "https://blog.rust-lang.org/feed.xml");
        assert_eq!(feed.site_url, "https://blog.rust-lang.org");
    }

    /// Single source of truth: the palette's "Unsubscribe" spec and the builder
    /// the context menu calls encode byte-identically, and likewise for the
    /// rename and mark-all-read specs.
    #[test]
    fn palette_and_builder_encode_identically() {
        let (app, doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        let feed = resources[0]
            .children
            .iter()
            .find(|r| r.kind == "feeds.subscription")
            .unwrap();
        let id: Id = feed
            .id
            .0
            .trim_start_matches("feeds.subscription:")
            .parse()
            .unwrap();

        let unsub_spec = feed.actions.iter().find(|a| a.id == "unsubscribe").unwrap();
        let from_spec = immediate_encode(unsub_spec);
        let from_builder = encode(&remove_subscription_action(id));
        assert_eq!(from_spec.type_id, from_builder.type_id);
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "unsubscribe is one source of truth"
        );

        let rename_spec = feed.actions.iter().find(|a| a.id == "rename").unwrap();
        let from_spec = prompted_encode(rename_spec, "My Feed");
        let from_builder = encode(&rename_feed_action(id, "My Feed"));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "rename is one source of truth"
        );

        // The palette's "Mark all as read" resolves the same unread ids the
        // context menu's per-feed mark-all does.
        let mark_spec = feed.actions.iter().find(|a| a.id == "mark-read").unwrap();
        let from_spec = immediate_encode(mark_spec);
        let unread: Vec<Id> = app.store_unread_ids(Filter::Feed(id));
        let from_builder = encode(&mark_all_read_action(&unread));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "mark-all-read is one source of truth"
        );
    }

    /// Deleting a folder through its action drops the folder but keeps its feeds,
    /// unparented to uncategorised — a subscription is never lost with its folder.
    #[test]
    fn delete_folder_keeps_its_feeds() {
        let (app, mut doc) = seeded();
        let folder = app.store_folder_ids()[0];
        let feeds_in: Vec<Id> = FeedsDoc::load(&doc)
            .feeds
            .iter()
            .filter(|f| f.folder == Some(folder))
            .map(|f| f.id)
            .collect();
        assert!(!feeds_in.is_empty(), "the folder starts with feeds");

        let action = delete_folder_action(folder);
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

        let loaded = FeedsDoc::load(&doc);
        assert!(loaded.folder(folder).is_none(), "the folder is gone");
        for fid in &feeds_in {
            let feed = loaded
                .feed(*fid)
                .expect("the feed survives the folder deletion");
            assert_eq!(feed.folder, None, "the feed is now uncategorised");
        }
    }
}
