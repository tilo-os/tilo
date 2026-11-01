//! The action surface an application exports, and the paged directory the
//! broker reads it through.
//!
//! Every type here holds the typed `tilo_actions` vocabulary: an operation
//! kind is an [`OperationKind`], a contract an [`ActionContract`], a
//! presentation a [`ResourcePresentation`]. A directory answer travels as
//! JSON and is checked row by row when it is read, so a malformed row is
//! refused with the row it came from named, rather than dropped further
//! along by whoever happened to parse it.

use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ActionParam, Collects, ExecutionLocus,
    InputSpec, OperationKind, ResourcePresentation, ResourceSemanticIdentity,
    ResourceViewDescriptor,
};

pub use tilo_actions::remote::RowVisibility;

/// The descriptive source identity an exported action may carry.
pub type ExportedResourceSemanticIdentity = ResourceSemanticIdentity;
/// Explicit resource-view metadata attached to the action that opens it.
pub type ExportedView = ResourceViewDescriptor;
/// The range binding of an [`ExportedView`].
pub type ExportedViewRange = tilo_actions::ResourceViewRangeSpec;
/// The mount binding of an [`ExportedView`].
pub type ExportedViewMount = tilo_actions::ResourceViewMountSpec;

/// What one exported action's invocation must carry, as it crosses the
/// wire — the transport-side twin of `tilo_actions::InputSpec`, which says
/// what kind of text it collects rather than whether it names content.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ExportedInput {
    None,
    Text {
        prompt: String,
        placeholder: String,
        /// Whether that text names content already in this machine's
        /// store rather than words somebody says.
        wants_content: bool,
    },
}

impl From<&InputSpec> for ExportedInput {
    fn from(input: &InputSpec) -> Self {
        match input {
            InputSpec::None => Self::None,
            InputSpec::Text {
                prompt,
                placeholder,
                collects,
            } => Self::Text {
                prompt: prompt.clone(),
                placeholder: placeholder.clone(),
                wants_content: *collects == Collects::Content,
            },
        }
    }
}

impl From<&ExportedInput> for InputSpec {
    fn from(input: &ExportedInput) -> Self {
        match input {
            ExportedInput::None => Self::None,
            ExportedInput::Text {
                prompt,
                placeholder,
                wants_content,
            } => Self::Text {
                prompt: prompt.clone(),
                placeholder: placeholder.clone(),
                collects: if *wants_content {
                    Collects::Content
                } else {
                    Collects::Words
                },
            },
        }
    }
}

/// One inert operation an application exports, without execution authority.
///
/// This is the one Rust shape of an exported action: the application builds
/// it from its typed [`ActionDescriptor`], the broker holds it as its
/// published surface, and only this crate's codec turns it into wire text.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExportedAction {
    pub resource: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_identity: Option<ResourceSemanticIdentity>,
    pub action: String,
    pub resource_kind: String,
    pub resource_label: String,
    pub action_label: String,
    /// The other words people use for the action, published with its label.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// The action's own picture, by its Lucide name, published with its label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub description: Option<String>,
    /// What the action's row shows of the state it acts on.
    #[serde(default, skip_serializing_if = "tilo_actions::ActionState::is_empty")]
    pub state: tilo_actions::ActionState,
    pub operation_kind: OperationKind,
    /// The canonical typed parameter descriptors.
    pub params: Vec<ActionParam>,
    /// The complete safety/lifecycle/result contract.
    pub contract: ActionContract,
    pub view: Option<ResourceViewDescriptor>,
    pub input: ExportedInput,
}

/// One application connection's exported actions, as the broker's action
/// catalogue lists them to an application.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct BrokerActionSurface {
    pub peer_id: u64,
    /// Stable route for retained offers; not a connection or trusted publisher.
    #[serde(default)]
    pub provider: Option<String>,
    pub app_id: String,
    pub cluster_id: String,
    pub record_id: String,
    pub live: bool,
    pub actions: Vec<ExportedAction>,
}

impl ExportedAction {
    /// Whether this action, read from the app that published it, names a
    /// semantic identity only of its own kind and in bounds.
    ///
    /// # Errors
    /// What is out of bounds, naming the action.
    pub fn check(&self) -> Result<(), String> {
        if self.semantic_identity.as_ref().is_some_and(|identity| {
            identity.kind.len() > ResourceSemanticIdentity::MAX_KIND_BYTES
                || identity.key.len() > ResourceSemanticIdentity::MAX_KEY_BYTES
                || !identity.is_valid()
                || identity.kind != self.resource_kind
        }) {
            return Err(format!(
                "action `{}`/`{}`: invalid resource semantic identity",
                self.resource, self.action
            ));
        }
        Ok(())
    }

    /// The typed descriptor this action exports, run at `locus`: the wire
    /// carries no locus of its own.
    pub fn descriptor(&self, locus: ExecutionLocus) -> ActionDescriptor {
        ActionDescriptor {
            semantic_identity: self.semantic_identity.clone(),
            address: ActionAddress::new(&self.resource, &self.action),
            resource_kind: self.resource_kind.clone(),
            resource_label: self.resource_label.clone(),
            action_label: self.action_label.clone(),
            keywords: self.keywords.clone(),
            icon: self.icon.clone(),
            description: self.description.clone(),
            state: self.state.clone(),
            locus,
            kind: self.operation_kind,
            contract: self.contract.clone(),
            input: (&self.input).into(),
            params: self.params.clone(),
            view: self.view.clone(),
        }
    }
}

impl From<&ActionDescriptor> for ExportedAction {
    fn from(descriptor: &ActionDescriptor) -> Self {
        Self {
            resource: descriptor.address.resource.clone(),
            semantic_identity: descriptor.semantic_identity.clone(),
            action: descriptor.address.action.clone(),
            resource_kind: descriptor.resource_kind.clone(),
            resource_label: descriptor.resource_label.clone(),
            action_label: descriptor.action_label.clone(),
            keywords: descriptor.keywords.clone(),
            icon: descriptor.icon.clone(),
            description: descriptor.description.clone(),
            state: descriptor.state.clone(),
            operation_kind: descriptor.kind,
            params: descriptor.params.clone(),
            contract: descriptor.contract.clone(),
            view: descriptor.view.clone(),
            input: ExportedInput::from(&descriptor.input),
        }
    }
}

/// Bounded, revision-aware request to an application-owned action directory.
///
/// `cursor` is opaque to callers. A source must reject it as stale when its
/// revision no longer names the same ordering, rather than silently skipping
/// or duplicating rows. Every numeric field is an upper bound, never a hint.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ActionDirectoryQuery {
    /// Broker-issued live connection id. Zero requests the broker's root
    /// directory; non-zero addresses one application directory.
    pub peer_id: u64,
    pub revision: String,
    pub cursor: Vec<u8>,
    pub needle: String,
    pub parent_resource: String,
    pub max_results: u32,
    pub max_bytes: u32,
    pub max_work: u32,
    pub max_depth: u16,
    /// Non-empty: only the top-level resources whose kind is this, or whose
    /// label is this ignoring ASCII case, answered whole on one page.
    pub resource_name: String,
}

/// A hierarchy-preserving directory row. Resource rows have no action or
/// descriptor; action rows contain the canonical descriptor for that exact
/// resource/action address.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ActionDirectoryRow {
    pub parent_resource: String,
    pub resource: String,
    /// Stable kind and user-facing label for resource/container rows. Action
    /// rows repeat their descriptor values here so a shell can render either
    /// shape without parsing raw resource identifiers.
    pub resource_kind: String,
    pub resource_label: String,
    /// Declarative search presentation; decode refuses one that is not
    /// [`ResourcePresentation::is_valid`].
    pub presentation: Option<ResourcePresentation>,
    pub action: Option<String>,
    pub descriptor: Option<ExportedAction>,
    pub has_children: bool,
    /// The permission-bearing action, and its contract, that discloses this
    /// row. An action row names itself; a container row names the action
    /// whose permission governs seeing it.
    pub visibility: Option<RowVisibility>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ActionDirectoryPage {
    pub revision: String,
    pub rows: Vec<ActionDirectoryRow>,
    pub next_cursor: Vec<u8>,
    pub exhausted: bool,
    pub stale_cursor: bool,
    pub budget_exhausted: bool,
}

/// Exact, current-authority response before an invocation is admitted.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ActionDirectoryResolution {
    /// The row, and the revision the owner resolved it against: the named
    /// one, or the directory as it is now for an empty revision. An
    /// admission is bound to that revision, so its host refuses to run it
    /// once the directory moved on. `None` only from an owner built before
    /// the field existed.
    Found {
        row: Box<ActionDirectoryRow>,
        revision: Option<String>,
    },
    NotFound,
    StaleRevision,
    Unavailable,
}

/// The most JSON text a directory descriptor's contract may occupy on the
/// wire.
pub const MAX_DIRECTORY_CONTRACT_WIRE_BYTES: usize = 32 * 1024;
/// The most JSON text a directory descriptor's parameters may occupy.
pub const MAX_DIRECTORY_PARAMS_WIRE_BYTES: usize = 32 * 1024;
/// The most JSON text a directory row's presentation may occupy. A valid
/// presentation is well under 1 KiB of text even fully escaped.
pub const MAX_DIRECTORY_PRESENTATION_WIRE_BYTES: usize = 8 * 1024;

/// Bytes `value` occupies as the JSON text the wire carries for it, or
/// `usize::MAX` when it cannot be encoded at all.
#[must_use]
pub fn wire_json_len<T: serde::Serialize + ?Sized>(value: &T) -> usize {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    match serde_json::to_writer(&mut counter, value) {
        Ok(()) => counter.0,
        Err(_) => usize::MAX,
    }
}

impl ActionDirectoryRow {
    /// Whether this row, read from the app that published it, is in bounds:
    /// its presentation valid, its descriptor's semantic identity naming its
    /// kind, and its contract, parameters and visibility contract bounded.
    ///
    /// # Errors
    /// What is out of bounds, naming the row.
    pub fn check(&self) -> Result<(), String> {
        let refuse = |what: &str| {
            Err(format!(
                "action directory row `{}`/`{}`: {what}",
                self.resource,
                self.action.as_deref().unwrap_or_default()
            ))
        };
        if let Some(presentation) = &self.presentation
            && (!presentation.is_valid()
                || wire_json_len(presentation) > MAX_DIRECTORY_PRESENTATION_WIRE_BYTES)
        {
            return refuse("resource presentation is out of bounds");
        }
        if let Some(visibility) = &self.visibility
            && (visibility.action.is_empty()
                || wire_json_len(&visibility.contract) > MAX_DIRECTORY_CONTRACT_WIRE_BYTES)
        {
            return refuse("visibility contract is out of bounds");
        }
        if let Some(descriptor) = &self.descriptor {
            descriptor.check()?;
            if wire_json_len(&descriptor.params) > MAX_DIRECTORY_PARAMS_WIRE_BYTES {
                return refuse("parameters are out of bounds");
            }
            if wire_json_len(&descriptor.contract) > MAX_DIRECTORY_CONTRACT_WIRE_BYTES {
                return refuse("contract is out of bounds");
            }
        }
        Ok(())
    }
}

/// Read a directory answer the app gave as JSON: refused when its rows'
/// size exceeds `max_bytes` or a row is out of bounds.
///
/// # Errors
/// The answer is oversized, malformed or out of bounds.
pub fn read_directory_answer<T: serde::de::DeserializeOwned>(
    json: &str,
    max_bytes: u64,
    rows: impl Fn(&T) -> Vec<&ActionDirectoryRow>,
) -> Result<T, String> {
    // JSON is wordier than the rows' size bound; four times it and a little
    // more is what a well-formed answer needs at most.
    let limit = max_bytes.saturating_mul(4).saturating_add(64 * 1024);
    if u64::try_from(json.len()).unwrap_or(u64::MAX) > limit {
        return Err("application exceeded directory byte budget".into());
    }
    let answer: T =
        serde_json::from_str(json).map_err(|error| format!("invalid directory answer: {error}"))?;
    let rows = rows(&answer);
    let size = rows
        .iter()
        .map(|row| action_directory_row_wire_bytes(row))
        .fold(0_usize, usize::saturating_add);
    if u64::try_from(size).unwrap_or(u64::MAX) > max_bytes {
        return Err("application exceeded directory byte budget".into());
    }
    rows.into_iter().try_for_each(ActionDirectoryRow::check)?;
    Ok(answer)
}

/// The parts of [`action_directory_row_wire_bytes`]'s bound.
mod wire_size {
    /// A struct's data words, pointer and list tag.
    pub(super) const STRUCT: usize = 64;

    pub(super) fn sum<const N: usize>(parts: [usize; N]) -> usize {
        parts.into_iter().fold(0, usize::saturating_add)
    }

    fn text_len(len: usize) -> usize {
        // The bytes and a NUL, padded to a word, and the pointer to them.
        (len / 8)
            .saturating_add(1)
            .saturating_mul(8)
            .saturating_add(8)
    }

    pub(super) fn text(value: &str) -> usize {
        text_len(value.len())
    }

    pub(super) fn optional(value: Option<&str>) -> usize {
        text(value.unwrap_or(""))
    }

    pub(super) fn json<T: serde::Serialize + ?Sized>(value: &T) -> usize {
        text_len(super::wire_json_len(value))
    }
}

/// The size `row` counts against a page's byte budget: every text field
/// costs its bytes, a NUL, padding to a word and an 8-byte pointer; every
/// struct a fixed allowance for its data words, pointer and list tag.
#[must_use]
pub fn action_directory_row_wire_bytes(row: &ActionDirectoryRow) -> usize {
    use wire_size::{STRUCT, json, optional, sum, text};
    let descriptor = row.descriptor.as_ref().map_or(0, |descriptor| {
        let input = match &descriptor.input {
            ExportedInput::None => 0,
            ExportedInput::Text {
                prompt,
                placeholder,
                ..
            } => sum([STRUCT, text(prompt), text(placeholder)]),
        };
        let view = descriptor.view.as_ref().map_or(0, |view| {
            let range = view.range.as_ref().map_or(0, |range| {
                sum([
                    STRUCT,
                    text(&range.action),
                    text(&range.start_param),
                    text(&range.end_param),
                    text(&range.unit_param),
                ])
            });
            let mount = view.mount.as_ref().map_or(0, |mount| {
                sum([
                    STRUCT,
                    text(&mount.action),
                    text(&mount.capability_param),
                    text(&mount.view_param),
                ])
            });
            sum([
                STRUCT,
                text(&view.id),
                text(&view.label),
                optional(view.description.as_deref()),
                text(&view.open_action),
                range,
                mount,
            ])
        });
        let identity = descriptor.semantic_identity.as_ref().map_or(0, |identity| {
            sum([STRUCT, text(&identity.kind), text(&identity.key)])
        });
        sum([
            STRUCT,
            text(&descriptor.resource),
            identity,
            text(&descriptor.action),
            text(&descriptor.resource_kind),
            text(&descriptor.resource_label),
            text(&descriptor.action_label),
            // A list of texts: its own pointer and tag, then each text.
            descriptor
                .keywords
                .iter()
                .map(|word| text(word))
                .fold(STRUCT, usize::saturating_add),
            optional(descriptor.icon.as_deref()),
            optional(descriptor.description.as_deref()),
            text(descriptor.operation_kind.as_str()),
            json(&descriptor.params),
            json(&descriptor.contract),
            input,
            view,
        ])
    });
    let visibility = row.visibility.as_ref().map_or_else(
        || text("").saturating_mul(3),
        |visibility| {
            sum([
                text(&visibility.action),
                json(&visibility.contract),
                text(
                    visibility
                        .governing_kind
                        .map_or("", tilo_actions::OperationKind::as_str),
                ),
            ])
        },
    );
    sum([
        STRUCT,
        text(&row.parent_resource),
        text(&row.resource),
        text(&row.resource_kind),
        text(&row.resource_label),
        row.presentation.as_ref().map_or_else(|| text(""), json),
        optional(row.action.as_deref()),
        visibility,
        descriptor,
    ])
}

#[cfg(test)]
#[path = "action_directory_tests.rs"]
mod tests;
