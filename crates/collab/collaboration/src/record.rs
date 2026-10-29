//! A record's identity: what it is ([`RecordKind`]), which one it is
//! ([`RecordId`]), what it references ([`RecordRef`]) and the checkout that
//! names all three ([`RecordCheckout`]). The id travels on the collaboration
//! wire and names a file on disk; the toolkit's record store
//! (`ui::record`) keeps the documents these name.

use std::path::{Path, PathBuf};

const MAX_RECORD_ID_BYTES: usize = 1024;
const MAX_RECORD_KIND_BYTES: usize = 64;
/// The longest relation or label a record reference may carry.
pub const MAX_RECORD_RELATION_BYTES: usize = 64;
/// Prefix of a file identity by filesystem record (see [`RecordId`]).
const FILE_RECORD_PREFIX: &str = "file-record:";
/// Former identities a checkout may name for migration.
pub const MAX_FORMER_IDS: usize = 8;

/// The stable identity of a record. Encoded by prefix:
/// `file-record:<uuid>` (a file, by the durable id its filesystem record
/// carries — see [`RecordId::identify_file`]), `file:<absolute path>` (a
/// file known only by where it is: a not-yet-written Save As target, or a
/// filesystem that can't hold the durable id), `uuid:<uuid>` (minted, for a
/// record with no natural name), `tab:<tab id>` (a window's scratch document
/// until a file names it, see [`RecordId::for_tab`]) and
/// `derived:<role>@<id>` (see [`RecordId::derived`]).
///
/// ## Files
///
/// A file's identity is its file record, never its path or its bytes:
///
/// - **Rename and move keep it.** The collaborative filesystem keys files by
///   a `FileRecordId` that survives rename and move and serves it as the
///   `user.tilo.record-id` attribute; elsewhere the same attribute is written
///   once on first open and travels with the inode.
/// - **A copy is a new file.** The filesystem mints a new record for a
///   created file, and a plain local copy does not carry the attribute, so a
///   copy starts with its own records. That is deliberate: copying is how a
///   person forks a document — to send a clean version, or to try different
///   edits — and mirroring every note into the copy would surprise them and
///   leak notes into what they hand on. Tools asked to preserve extended
///   attributes (`cp -a`, `rsync -X`) are an explicit request to keep the
///   identity, and do.
/// - **Identical bytes are not the same file.** Nothing here is derived from
///   content, so two files with the same bytes never share edits by accident.
///   Content that *should* be shared by bytes is a blob reference instead
///   ([`add_blob_reference`]).
///
/// The prefix is part of the id, not a tag beside it: the id travels on the
/// wire and names a file on disk, so it must be self-describing.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RecordId(String);

impl RecordId {
    /// Internal custody key for a package's existing room-primary document.
    /// It never travels as a record id on the collaboration wire.
    pub fn package_primary() -> Self {
        Self("@tilo:package-primary".into())
    }
    pub fn is_package_primary(&self) -> bool {
        self.0 == "@tilo:package-primary"
    }

    /// The record a collaboration route's record id names: the empty id is
    /// the [package primary](Self::package_primary), anything else must be a
    /// record id. The package primary's custody key is not a route, so a
    /// route spelling it names nothing: one record never has two routes.
    #[must_use]
    pub fn from_route(record: &str) -> Option<Self> {
        if record.is_empty() {
            Some(Self::package_primary())
        } else {
            Self::parse(record)
        }
    }

    /// The record id this record travels under on the collaboration wire,
    /// what [`Self::from_route`] reads back.
    #[must_use]
    pub fn route(&self) -> &str {
        if self.is_package_primary() {
            ""
        } else {
            &self.0
        }
    }

    /// The record a file *is*. Relative paths resolve against the cwd, so
    /// two spellings of one file are one record.
    ///
    /// The id embeds the absolute path, so two peers reach one record only
    /// if the file sits at the same path on both. Don't use this for
    /// content embedded in a document (a dropped image) — that needs a
    /// content-addressed record, not a path-addressed one.
    pub fn for_file(path: impl AsRef<Path>) -> Self {
        let p = path.as_ref();
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            std::env::current_dir().map_or_else(|_| p.to_path_buf(), |cwd| cwd.join(p))
        };
        // Not `canonicalize`: it fails for a file that doesn't exist yet
        // (Save As), and a record must be nameable before its bytes are.
        Self(format!("file:{}", normalise(&abs).display()))
    }

    /// The record of the file whose filesystem record id is `id` — the
    /// collaborative filesystem's `FileRecordId`, or the id
    /// [`Self::identify_file`] wrote for a file elsewhere.
    pub fn for_file_record(id: uuid::Uuid) -> Self {
        Self(format!("{FILE_RECORD_PREFIX}{}", id.hyphenated()))
    }

    /// The durable identity of the file at `path`, so reopening it after a
    /// rename or move reaches the same records.
    ///
    /// Reads the file's `user.tilo.record-id` attribute (16 raw bytes — the
    /// collaborative filesystem serves its `FileRecordId` there; on macOS,
    /// where every copy inherits the attribute, the id is followed by the
    /// inode it belongs to so a copy is told apart). A file
    /// without one, on a filesystem that accepts user attributes, gets a
    /// freshly minted id written with create-only semantics (a concurrent
    /// opener that wins the race is re-read, so both agree). Anything else —
    /// no attribute support, a read-only file, not a regular file — falls
    /// back to the path identity [`Self::for_file`]: the best identity the
    /// file can carry.
    ///
    /// Two system calls at most besides the metadata lookup; call it off the
    /// UI thread when the file might be on a network filesystem.
    pub fn identify_file(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        match file_identity::read_or_mint(path) {
            Some(id) => Self::for_file_record(id),
            None => Self::for_file(path),
        }
    }

    pub fn minted() -> Self {
        Self(format!("uuid:{}", uuid::Uuid::new_v4()))
    }

    /// Reuses the tab id rather than minting a new one: it's already unique
    /// per machine and persisted with the window tree, so a separate id
    /// beside it could only drift.
    pub fn for_tab(tab: u64) -> Self {
        Self(format!("tab:{tab:016x}"))
    }

    /// The record an app opens in a room when nothing names another one:
    /// the first window of the app on any device of the room, native or
    /// browser, so they all meet on one document. Constant, so every
    /// device derives the same id without asking; the room scopes it.
    pub fn room_default() -> Self {
        Self::window_default(0)
    }

    /// The record a fresh window (`instance`, the launcher's tab id) of an
    /// app that keeps a record per window opens when nothing names another
    /// one. Window `0` is [`Self::room_default`], what a launch outside the
    /// launcher opens. See `NativeProcessHost` for which apps get one.
    pub fn window_default(instance: u64) -> Self {
        Self::derived("default", &Self::for_tab(instance))
    }

    /// The window whose [default record](Self::window_default) is stored
    /// under `storage_key` ([`Self::storage_key`]), if that is what it is.
    /// Exact: the whole key, hash included, must be that record's.
    pub fn window_default_of_storage_key(storage_key: &str) -> Option<Self> {
        let tab = storage_key
            .strip_prefix("derived_default_tab_")?
            .get(..16)?;
        if !tab.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')) {
            return None;
        }
        let id = Self::window_default(u64::from_str_radix(tab, 16).ok()?);
        (id.storage_key() == storage_key).then_some(id)
    }

    /// `person`'s own record of `kind` in a workspace
    /// ([`collab_types::member_record`]): only that person's devices read
    /// or change it. `None` when `person` is not the hex of a user root key
    /// or `kind` is not a label.
    #[must_use]
    pub fn of_member(person: &str, kind: &str) -> Option<Self> {
        Self::parse(&collab_types::member_record_id(
            &person.to_ascii_lowercase(),
            kind,
        ))
    }

    /// Identity of the record that plays `role` for `of` — "the annotations
    /// of this document". Distinct from its target so the two can be
    /// shared separately: handing over the document doesn't hand the notes.
    pub fn derived(role: &str, of: &RecordId) -> Self {
        Self(format!("derived:{role}@{}", of.0))
    }

    /// Part `index` of the chained record `self`: a record that grows
    /// without end is kept as numbered parts, each a record of its own. A
    /// room's host replays a joining device only the chain's head, the
    /// highest part it holds; every earlier part is sent to a device when
    /// it opens it. Two writers that start part `n + 1` start the same
    /// record. The wire form is `neighbour_wire::record_part`'s.
    #[must_use]
    pub fn part(&self, index: u64) -> Self {
        Self::derived(&format!("part.{index}"), self)
    }

    /// The chain and number of a [`Self::part`], or `None` for any other
    /// record.
    pub fn chain_part(&self) -> Option<(Self, u64)> {
        let (role, base) = self.0.strip_prefix("derived:")?.split_once('@')?;
        let index = role.strip_prefix("part.")?;
        let canonical = !index.is_empty()
            && index.bytes().all(|byte| byte.is_ascii_digit())
            && (index == "0" || !index.starts_with('0'));
        canonical
            // optional-decode: a probe of the id's shape; digits only, so only an index past u64 fails, which is no part
            .then(|| index.parse().ok())
            .flatten()
            .map(|index| (Self(base.to_owned()), index))
    }

    /// The number of the part of the chain `self` whose document is kept
    /// under `storage_key` ([`Self::storage_key`]), or `None` for any other
    /// record's: what a store lists of a chain it holds parts of without
    /// having opened them.
    #[must_use]
    pub fn part_of_storage_key(&self, storage_key: &str) -> Option<u64> {
        let index = storage_key.strip_prefix("derived_part.")?;
        let digits = index.bytes().take_while(u8::is_ascii_digit).count();
        // optional-decode: a probe of a storage key's shape; digits only, so only an index past u64 fails, which names no part
        let index: u64 = index.get(..digits)?.parse().ok()?;
        (self.part(index).storage_key() == storage_key).then_some(index)
    }

    /// The child record `id` of `self`: a resource kept as a record of its
    /// own inside its parent (a chat's channel), its id minted once and
    /// never changed, wherever its parent lists it
    /// (`tilo_resource::children`). `None` for an id that cannot name a
    /// record (empty, too long, or with characters other than letters,
    /// digits, `.`, `_` and `-`).
    #[must_use]
    pub fn child(&self, id: &str) -> Option<Self> {
        let role = format!("child.{id}");
        (!id.is_empty() && valid_label(&role, MAX_RECORD_RELATION_BYTES))
            .then(|| Self::derived(&role, self))
            .filter(|child| valid_record_id(&child.0, 0))
    }

    /// The parent and id of a [`Self::child`], or `None` for any other
    /// record.
    #[must_use]
    pub fn child_of(&self) -> Option<(Self, String)> {
        let (role, base) = self.0.strip_prefix("derived:")?.split_once('@')?;
        let id = role.strip_prefix("child.")?;
        (!id.is_empty()).then(|| (Self(base.to_owned()), id.to_owned()))
    }

    /// The record whose tree `self` belongs to: the chain of a
    /// [`Self::part`], the parent of a [`Self::child`]; `None` for a record
    /// of its own. Such a record lives and syncs with its tree: whatever
    /// holds the tree holds it, whether or not a view shows it.
    #[must_use]
    pub fn tree(&self) -> Option<Self> {
        self.chain_part()
            .map(|(chain, _)| chain)
            .or_else(|| self.child_of().map(|(parent, _)| parent))
    }

    /// `self` declared on-demand: a room's host does not replay it to a
    /// joining device, and sends it to a device when it opens it. The wire
    /// form is `neighbour_wire::on_demand_record`'s.
    #[must_use]
    pub fn on_demand(&self) -> Self {
        Self::derived("on-demand", self)
    }

    /// Adopt an id read back from a document, a room index, or the wire.
    /// Invalid, oversized, or non-canonical names fail closed: IDs name
    /// collaboration routes and storage directories, so accepting arbitrary
    /// strings here would turn a room index into an allocation oracle.
    pub fn parse(s: &str) -> Option<Self> {
        valid_record_id(s, 0).then(|| Self(s.to_string()))
    }

    /// The URI asking an app to open this record
    /// ([`collab_types::RECORD_URI_SCHEME`]).
    #[must_use]
    pub fn uri(&self) -> String {
        format!("{}:{}", collab_types::RECORD_URI_SCHEME, self.0)
    }

    /// The record a [`Self::uri`] names.
    #[must_use]
    pub fn from_uri(uri: &str) -> Option<Self> {
        uri.strip_prefix(collab_types::RECORD_URI_SCHEME)?
            .strip_prefix(':')
            .and_then(Self::parse)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The path a file record names, if it is one.
    pub fn file_path(&self) -> Option<PathBuf> {
        self.0.strip_prefix("file:").map(PathBuf::from)
    }

    /// The filesystem record id of a `file-record:` identity.
    pub fn file_record(&self) -> Option<uuid::Uuid> {
        self.0
            .strip_prefix(FILE_RECORD_PREFIX)
            // optional-decode: a probe of the identity's kind; a malformed id is no filesystem record
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
    }

    /// A filesystem-safe directory name for this record's document. Must be
    /// stable (same record → same directory next launch, or data is
    /// silently orphaned) and collision-free (two records sharing a
    /// directory merge their documents) — hence a readable sanitised prefix
    /// plus a hash of the whole id, which is what actually distinguishes
    /// them.
    pub fn storage_key(&self) -> String {
        let mut prefix = String::with_capacity(48);
        for c in self.0.chars() {
            if prefix.len() >= 40 {
                break;
            }
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                prefix.push(c);
            } else {
                prefix.push('_');
            }
        }
        format!("{prefix}-{:016x}", fnv1a64(self.0.as_bytes()))
    }
}

fn valid_record_id(s: &str, depth: usize) -> bool {
    if s.is_empty() || s.len() > MAX_RECORD_ID_BYTES || s.contains('\0') || depth > 4 {
        return false;
    }
    if let Some(path) = s.strip_prefix("file:") {
        let path = Path::new(path);
        return path.is_absolute() && normalise(path) == path;
    }
    if let Some(id) = s.strip_prefix(FILE_RECORD_PREFIX) {
        // Canonical hyphenated lowercase only, so one file has one id.
        return uuid::Uuid::parse_str(id)
            .is_ok_and(|parsed| !parsed.is_nil() && parsed.hyphenated().to_string() == id);
    }
    if let Some(uuid) = s.strip_prefix("uuid:") {
        return uuid::Uuid::parse_str(uuid).is_ok();
    }
    if s.starts_with("member:") {
        return collab_types::member_record(s).is_some();
    }
    if let Some(tab) = s.strip_prefix("tab:") {
        return tab.len() == 16 && tab.bytes().all(|c| c.is_ascii_hexdigit());
    }
    let Some(derived) = s.strip_prefix("derived:") else {
        return false;
    };
    let Some((role, target)) = derived.split_once('@') else {
        return false;
    };
    valid_label(role, MAX_RECORD_RELATION_BYTES)
        && depth
            .checked_add(1)
            .is_some_and(|next| valid_record_id(target, next))
}

/// Whether `value` is a label that can travel in a room index or record
/// stamp: at most `max` bytes of ASCII letters, digits, `.`, `_` and `-`,
/// beginning with a letter.
#[must_use]
pub fn valid_label(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value.bytes().enumerate().all(|(i, byte)| {
            (byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
                && (i != 0 || byte.is_ascii_alphabetic())
        })
}

impl std::fmt::Display for RecordId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Collapse `.` and `..` without touching the filesystem (`canonicalize`
/// needs the file to exist; a record must be nameable before its bytes
/// are). A surviving `..` would let two spellings of one path be two
/// records, and let a record key address a directory outside app state.
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

pub mod fields;
mod file_identity;
pub mod retirement;
pub mod save;
pub use save::RecordSaveStatus;

/// FNV-1a, 64-bit: a fixed algorithm, unlike a library's default hasher, so
/// the directory names it produces stay stable.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hasher = fnv::FnvHasher::default();
    std::hash::Hasher::write(&mut hasher, bytes);
    std::hash::Hasher::finish(&hasher)
}

/// What kind of thing a record is — `document`, `annotation-set`, `book`,
/// `playlist`. App-owned rather than system-registered, so an app can add
/// a kind without coordinating; the cost is no cross-app library view yet
/// (needs a system-owned type registry, not built here).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RecordKind(String);

/// Old Writer kind stamps still name the current rich-text resource.
/// Reopening a record must not rewrite its attributed CRDT history.
const FORMER_KINDS: [(&str, &str); 2] = [
    ("document", "tilo.rich-text"),
    ("qdoc.rich-text", "tilo.rich-text"),
];

impl RecordKind {
    pub fn new(kind: impl Into<String>) -> Self {
        let kind = kind.into();
        match FORMER_KINDS.iter().find(|(former, _)| *former == kind) {
            Some((_, current)) => Self((*current).to_owned()),
            None => Self(kind),
        }
    }

    /// Whether a stored current or former name identifies this kind.
    pub fn names(&self, stored: &str) -> bool {
        Self::new(stored) == *self
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this can safely travel in a room index or record stamp.
    pub fn is_valid(&self) -> bool {
        valid_label(&self.0, MAX_RECORD_KIND_BYTES)
    }
}

impl std::fmt::Display for RecordKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One record's reference to another: annotations → the document they mark up,
/// a duplicate → the record it was copied from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordRef {
    pub target: RecordId,
    /// What the reference means to the app — `marks-up`, `copied-from`,
    /// `attached-to`. Free-form for the same reason [`RecordKind`] is.
    pub relation: String,
}

impl RecordRef {
    pub fn new(target: RecordId, relation: impl Into<String>) -> Self {
        Self {
            target,
            relation: relation.into(),
        }
    }
}

/// A record named together with its type — what a widget declares it is
/// working on, and what a room index holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordCheckout {
    pub id: RecordId,
    pub kind: RecordKind,
    /// Records this one references, and what each means. Declared here
    /// (not written by the app) because a reference has two halves only
    /// the host can write: it goes into this record's document, and the
    /// target is brought into the room index so it doesn't dangle. One
    /// level deep.
    pub references: Vec<(RecordCheckout, String)>,
    /// Identities this record was known by before — a file record that used
    /// to be named by its path. When nothing is stored under [`Self::id`],
    /// the host adopts the stored document of the first former identity that
    /// no other successor has claimed (see [`RecordCheckout::formerly`]).
    pub formerly: Vec<RecordId>,
}

impl RecordCheckout {
    /// The record a desktop tab of `app_id` shows: the room's package-primary
    /// record for Chat, the tab's own record for every other app. Keeps the
    /// tab identity, so a tab dragged elsewhere keeps its document.
    pub fn for_component_tab(app_id: &str, tab: u64, kind: RecordKind) -> Self {
        let id = if app_id == "so.tilo.chat" {
            RecordId::package_primary()
        } else {
            RecordId::for_tab(tab)
        };
        Self::new(id, kind)
    }

    pub fn package_primary(kind: RecordKind) -> Self {
        Self::new(RecordId::package_primary(), kind)
    }

    pub fn new(id: RecordId, kind: RecordKind) -> Self {
        Self {
            id,
            kind,
            references: Vec::new(),
            formerly: Vec::new(),
        }
    }

    /// Declare that this record used to be `former`. Opening it where
    /// nothing is stored under the new id adopts the former record's
    /// document — its content, history and actors — restamped with the new
    /// identity. Custody of the former document is claimed durably by the
    /// first successor, so a different record naming the same former id
    /// (another file that later sits at the same path) never adopts it too.
    /// The former document itself stays on disk untouched.
    #[must_use]
    pub fn formerly(mut self, former: RecordId) -> Self {
        if former != self.id && !self.formerly.contains(&former) {
            self.formerly.push(former);
        }
        self
    }

    #[must_use]
    pub fn referencing(mut self, target: RecordCheckout, relation: impl Into<String>) -> Self {
        self.references.push((target, relation.into()));
        self
    }

    /// Room indexes contain names supplied by collaborators.  Keep the
    /// transport boundary typed and bounded before a host considers one.
    pub fn is_valid(&self) -> bool {
        RecordId::parse(self.id.as_str()).is_some()
            && self.kind.is_valid()
            && self.valid_reference_shape()
    }

    /// Whether this checkout may name a locally stored record: like
    /// [`Self::is_valid`], but the package primary's custody key is allowed.
    #[must_use]
    pub fn is_valid_local_storage_request(&self) -> bool {
        (self.id.is_package_primary() || RecordId::parse(self.id.as_str()).is_some())
            && self.kind.is_valid()
            && self.valid_reference_shape()
    }

    fn formerly_valid(checkout: &RecordCheckout) -> bool {
        checkout.formerly.len() <= MAX_FORMER_IDS
            && checkout
                .formerly
                .iter()
                .all(|former| RecordId::parse(former.as_str()).is_some() && *former != checkout.id)
    }

    fn valid_reference_shape(&self) -> bool {
        Self::formerly_valid(self)
            && self.references.iter().all(|(target, relation)| {
                target.references.is_empty()
                    && RecordId::parse(target.id.as_str()).is_some()
                    && target.kind.is_valid()
                    && Self::formerly_valid(target)
                    && valid_label(relation, MAX_RECORD_RELATION_BYTES)
            })
    }
}

impl RecordId {
    /// Whether a room's hosts send this record to a device only once it
    /// asks for it: a [`Self::part`] of a chain (the head too, which a
    /// device that joined before it started asks for) or an
    /// [`Self::on_demand`] record. Opening one reads what this device holds
    /// or asks for it, and never waits for it to arrive
    /// (`ui::record::RecordOpenError::Arriving`).
    #[must_use]
    pub fn arrives_on_demand(&self) -> bool {
        self.chain_part().is_some()
            || self
                .0
                .strip_prefix("derived:on-demand@")
                .is_some_and(|base| !base.is_empty())
    }
}

impl PartialOrd for RecordCheckout {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RecordCheckout {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.id
            .cmp(&other.id)
            .then_with(|| self.kind.cmp(&other.kind))
    }
}
