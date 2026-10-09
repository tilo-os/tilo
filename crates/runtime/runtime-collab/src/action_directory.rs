//! The action surface an application exports, and the paged directory the
//! broker reads it through.
//!
//! Every type here holds the typed `tilo_actions` vocabulary: an operation
//! kind is an [`OperationKind`], a contract an [`ActionContract`], a
//! presentation a [`ResourcePresentation`]. The Cap'n Proto schema still
//! carries contracts, parameters and presentations as JSON text
//! (`paramsJson`, `contractJson`, `resourcePresentationJson`,
//! `visibilityContractJson`), and that encoding is confined to this module's
//! codec: callers never see a JSON string, and a malformed payload is refused
//! at decode with the row it came from named, rather than dropped further
//! along by whoever happened to parse it.

use tilo_actions::{
    ActionContract, ActionDescriptor, ActionParam, Collects, InputSpec, OperationKind,
    ResourcePresentation, ResourceSemanticIdentity, ResourceViewDescriptor,
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
    pub description: Option<String>,
    pub operation_kind: OperationKind,
    /// The canonical typed parameter descriptors.
    pub params: Vec<ActionParam>,
    /// The complete safety/lifecycle/result contract.
    pub contract: ActionContract,
    pub view: Option<ResourceViewDescriptor>,
    pub input: ExportedInput,
}

/// Complete inert expected state for explicit fresh semantic resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreshActionBinding {
    pub descriptor: ExportedAction,
    /// Canonical JSON encoding of the action descriptor's execution locus.
    pub locus: String,
    /// Optional application operation type within the descriptor's broader
    /// document/effect/view class.
    pub operation_type: Option<String>,
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
            description: descriptor.description.clone(),
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
}

/// A hierarchy-preserving directory row. Resource rows have no action or
/// descriptor; action rows contain the canonical descriptor for that exact
/// resource/action address.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// Complete current owner binding for semantic resolution without a
    /// caller-held directory revision.
    pub fresh_binding: Option<FreshActionBinding>,
    pub has_children: bool,
    /// The permission-bearing action, and its contract, that discloses this
    /// row. An action row names itself; a container row names the action
    /// whose permission governs seeing it.
    pub visibility: Option<RowVisibility>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionDirectoryPage {
    pub revision: String,
    pub rows: Vec<ActionDirectoryRow>,
    pub next_cursor: Vec<u8>,
    pub exhausted: bool,
    pub stale_cursor: bool,
    pub budget_exhausted: bool,
}

/// Exact, current-authority response before an invocation is admitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionDirectoryResolution {
    Found(Box<ActionDirectoryRow>),
    NotFound,
    StaleRevision,
    Unavailable,
}

/// The two mutually exclusive ways to ask a live application to resolve one
/// exact directory action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionDirectoryResolutionRequest {
    RevisionBound {
        revision: String,
        resource: String,
        action: String,
    },
    FreshBinding {
        resource: String,
        action: String,
        expected: Box<FreshActionBinding>,
    },
}

/// An exact-resolution outcome and the live owner's authoritative revision.
/// Revision-bound replies keep `revision` empty; a successful fresh reply must
/// carry a non-empty revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionDirectoryResolutionReply {
    pub resolution: ActionDirectoryResolution,
    pub revision: String,
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

/// An upper bound on `row`'s Cap'n Proto size, which the broker checks a
/// page against: every text field costs its bytes, a NUL, padding to a word
/// and an 8-byte pointer; every struct a fixed allowance for its data words,
/// pointer and list tag.
#[must_use]
pub fn action_directory_row_wire_bytes(row: &ActionDirectoryRow) -> usize {
    const STRUCT: usize = 64;
    fn sum<const N: usize>(parts: [usize; N]) -> usize {
        parts.into_iter().fold(0, usize::saturating_add)
    }
    fn text_len(len: usize) -> usize {
        // The bytes and a NUL, padded to a word, and the pointer to them.
        (len / 8)
            .saturating_add(1)
            .saturating_mul(8)
            .saturating_add(8)
    }
    fn text(value: &str) -> usize {
        text_len(value.len())
    }
    fn optional(value: Option<&str>) -> usize {
        text(value.unwrap_or(""))
    }
    fn json<T: serde::Serialize + ?Sized>(value: &T) -> usize {
        text_len(wire_json_len(value))
    }
    let action = |descriptor: &ExportedAction| {
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
            optional(descriptor.description.as_deref()),
            text(descriptor.operation_kind.as_str()),
            json(&descriptor.params),
            json(&descriptor.contract),
            input,
            view,
        ])
    };
    let descriptor = row.descriptor.as_ref().map_or(0, &action);
    let fresh_binding = row.fresh_binding.as_ref().map_or(0, |binding| {
        sum([
            STRUCT,
            action(&binding.descriptor),
            text(&binding.locus),
            optional(binding.operation_type.as_deref()),
        ])
    });
    let visibility = row.visibility.as_ref().map_or_else(
        || text("").saturating_mul(2),
        |visibility| text(&visibility.action).saturating_add(json(&visibility.contract)),
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
        fresh_binding,
    ])
}

#[cfg(feature = "client")]
pub(crate) use codec::decode_action_directory_row;
#[cfg(feature = "client")]
pub use codec::{
    decode_action_directory_page, decode_action_directory_resolution,
    decode_action_directory_resolution_reply, decode_exported_action, decode_fresh_action_binding,
    encode_fresh_action_binding, write_action_directory_page, write_action_directory_resolution,
    write_action_directory_row, write_exported_action,
};

/// The Cap'n Proto codec: the only place an exported action or directory
/// row is turned into, or read back from, its wire text.
#[cfg(feature = "client")]
mod codec {
    use super::{
        ActionDirectoryPage, ActionDirectoryResolution, ActionDirectoryResolutionReply,
        ActionDirectoryRow, ExportedAction, ExportedInput, FreshActionBinding,
        MAX_DIRECTORY_CONTRACT_WIRE_BYTES, MAX_DIRECTORY_PARAMS_WIRE_BYTES,
        MAX_DIRECTORY_PRESENTATION_WIRE_BYTES, RowVisibility,
    };
    use crate::collab_capnp;
    use tilo_actions::{
        ActionContract, OperationKind, ResourcePresentation, ResourceSemanticIdentity,
        ResourceViewDescriptor, ResourceViewMountSpec, ResourceViewRangeSpec,
    };

    /// Which reader an exported action reaches the decoder through. The
    /// published surface has always accepted an older client's empty kind,
    /// parameters and contract; a directory row has always required all
    /// three, bounded, because its contract is what authorizes showing it.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Source {
        Published,
        Directory,
    }

    /// Names the address a decode error came from, so a refused payload
    /// points at the action its application published.
    struct Address<'a> {
        source: Source,
        resource: &'a str,
        action: &'a str,
    }

    impl Address<'_> {
        fn error(&self, what: impl std::fmt::Display) -> capnp::Error {
            let noun = match self.source {
                Source::Published => "exported action",
                Source::Directory => "action directory row",
            };
            capnp::Error::failed(format!(
                "{noun} `{}`/`{}`: {what}",
                self.resource, self.action
            ))
        }
    }

    fn capnp_text(value: capnp::text::Reader<'_>) -> Result<String, capnp::Error> {
        value.to_str().map(str::to_owned).map_err(|error| {
            capnp::Error::failed(format!("invalid UTF-8 in action directory: {error}"))
        })
    }

    fn encode_json<T: serde::Serialize + ?Sized>(
        value: &T,
        what: &str,
    ) -> Result<String, capnp::Error> {
        serde_json::to_string(value)
            .map_err(|error| capnp::Error::failed(format!("cannot encode {what}: {error}")))
    }

    /// Parse one JSON payload, refusing an oversized or malformed one with
    /// the row named.
    fn decode_json<T: serde::de::DeserializeOwned>(
        address: &Address<'_>,
        json: &str,
        what: &str,
        maximum: usize,
    ) -> Result<T, capnp::Error> {
        if json.is_empty() || json.len() > maximum {
            return Err(address.error(format_args!("{what} must be non-empty and bounded")));
        }
        serde_json::from_str(json)
            .map_err(|error| address.error(format_args!("invalid {what}: {error}")))
    }

    fn operation_kind(address: &Address<'_>, wire: &str) -> Result<OperationKind, capnp::Error> {
        match (wire, address.source) {
            ("document", _) | ("", Source::Published) => Ok(OperationKind::Document),
            ("effect", _) => Ok(OperationKind::Effect),
            ("view", _) => Ok(OperationKind::View),
            (other, _) => Err(address.error(format_args!("unknown operation kind `{other}`"))),
        }
    }

    /// Encode one exported action into its wire struct.
    ///
    /// # Errors
    ///
    /// Returns an error when the semantic identity is invalid or does not
    /// name the action's resource kind, or when a typed payload cannot be
    /// encoded.
    pub fn write_exported_action(
        mut out: collab_capnp::exported_action::Builder<'_>,
        action: &ExportedAction,
    ) -> Result<(), capnp::Error> {
        out.set_resource(&action.resource);
        if let Some(identity) = &action.semantic_identity {
            if !identity.is_valid() || identity.kind != action.resource_kind {
                return Err(capnp::Error::failed(
                    "invalid resource semantic identity".into(),
                ));
            }
            let mut out = out.reborrow().init_semantic_identity();
            out.set_kind(&identity.kind);
            out.set_key(&identity.key);
        }
        out.set_action(&action.action);
        out.set_resource_kind(&action.resource_kind);
        out.set_resource_label(&action.resource_label);
        out.set_action_label(&action.action_label);
        out.set_description(action.description.as_deref().unwrap_or(""));
        out.set_operation_kind(action.operation_kind.as_str());
        out.set_params_json(&encode_json(&action.params, "action parameters")?);
        out.set_contract_json(&encode_json(&action.contract, "action contract")?);
        if let Some(view) = &action.view {
            write_view(out.reborrow().init_view(), view);
        }
        let mut input = out.init_input();
        match &action.input {
            ExportedInput::None => input.set_none(()),
            ExportedInput::Text {
                prompt,
                placeholder,
                wants_content,
            } => {
                let mut text = input.init_text();
                text.set_prompt(prompt);
                text.set_placeholder(placeholder);
                text.set_wants_content(*wants_content);
            }
        }
        Ok(())
    }

    /// Encode a complete fresh semantic binding into its Cap'n Proto form.
    ///
    /// # Errors
    ///
    /// Returns an error when the binding's descriptor cannot be encoded.
    pub fn encode_fresh_action_binding(
        mut out: collab_capnp::fresh_action_binding::Builder<'_>,
        binding: &FreshActionBinding,
    ) -> Result<(), capnp::Error> {
        write_exported_action(out.reborrow().init_descriptor(), &binding.descriptor)?;
        out.set_locus(&binding.locus);
        out.set_operation_type(binding.operation_type.as_deref().unwrap_or(""));
        Ok(())
    }

    fn write_view(
        mut out: collab_capnp::resource_view_descriptor::Builder<'_>,
        view: &ResourceViewDescriptor,
    ) {
        out.set_id(&view.id);
        out.set_label(&view.label);
        out.set_description(view.description.as_deref().unwrap_or(""));
        out.set_open_action(&view.open_action);
        out.set_primary(view.primary);
        if let Some(range) = &view.range {
            let mut range_out = out.reborrow().init_range();
            range_out.set_action(&range.action);
            range_out.set_start_param(&range.start_param);
            range_out.set_end_param(&range.end_param);
            range_out.set_unit_param(&range.unit_param);
        }
        if let Some(mount) = &view.mount {
            let mut out = out.init_mount();
            out.set_action(&mount.action);
            out.set_capability_param(&mount.capability_param);
            out.set_view_param(&mount.view_param);
        }
    }

    /// Encode one directory row into its wire struct.
    ///
    /// # Errors
    ///
    /// Returns an error when the row's descriptor cannot be written (see
    /// [`write_exported_action`]) or a typed payload cannot be encoded.
    pub fn write_action_directory_row(
        mut out: collab_capnp::action_directory_row::Builder<'_>,
        row: &ActionDirectoryRow,
    ) -> Result<(), capnp::Error> {
        out.set_parent_resource(&row.parent_resource);
        out.set_resource(&row.resource);
        out.set_resource_kind(&row.resource_kind);
        out.set_resource_label(&row.resource_label);
        if let Some(presentation) = &row.presentation {
            out.set_resource_presentation_json(&encode_json(
                presentation,
                "resource presentation",
            )?);
        }
        out.set_action(row.action.as_deref().unwrap_or(""));
        out.set_has_children(row.has_children);
        if let Some(visibility) = &row.visibility {
            out.set_visibility_action(&visibility.action);
            out.set_visibility_contract_json(&encode_json(
                &visibility.contract,
                "visibility contract",
            )?);
        }
        if let Some(descriptor) = &row.descriptor {
            write_exported_action(out.reborrow().init_descriptor(), descriptor)?;
        }
        if let Some(binding) = &row.fresh_binding {
            encode_fresh_action_binding(out.reborrow().init_fresh_binding(), binding)?;
        }
        Ok(())
    }

    /// Encode one directory page into its wire struct.
    ///
    /// # Errors
    ///
    /// Returns an error when the page has more rows than a list can address,
    /// or any row cannot be written.
    pub fn write_action_directory_page(
        mut out: collab_capnp::action_directory_page::Builder<'_>,
        page: &ActionDirectoryPage,
    ) -> Result<(), capnp::Error> {
        out.set_revision(&page.revision);
        out.set_next_cursor(&page.next_cursor);
        out.set_exhausted(page.exhausted);
        out.set_stale_cursor(page.stale_cursor);
        out.set_budget_exhausted(page.budget_exhausted);
        let len = u32::try_from(page.rows.len())
            .map_err(|_| capnp::Error::failed("too many action directory rows".into()))?;
        let mut rows = out.reborrow().init_rows(len);
        for (index, row) in (0..len).zip(&page.rows) {
            write_action_directory_row(rows.reborrow().get(index), row)?;
        }
        Ok(())
    }

    /// Encode one exact resolution into its wire union.
    ///
    /// # Errors
    ///
    /// Returns an error when a found row cannot be written.
    pub fn write_action_directory_resolution(
        mut out: collab_capnp::action_directory_resolution::Builder<'_>,
        resolution: &ActionDirectoryResolution,
    ) -> Result<(), capnp::Error> {
        match resolution {
            ActionDirectoryResolution::Found(row) => {
                write_action_directory_row(out.init_found(), row)?;
            }
            ActionDirectoryResolution::NotFound => out.set_not_found(()),
            ActionDirectoryResolution::StaleRevision => out.set_stale_revision(()),
            ActionDirectoryResolution::Unavailable => out.set_unavailable(()),
        }
        Ok(())
    }

    fn decode_semantic_identity(
        address: &Address<'_>,
        row: collab_capnp::exported_action::Reader<'_>,
    ) -> Result<Option<ResourceSemanticIdentity>, capnp::Error> {
        if !row.has_semantic_identity() {
            return Ok(None);
        }
        let identity = row.get_semantic_identity()?;
        let kind = identity.get_kind()?;
        let key = identity.get_key()?;
        if kind.len() > ResourceSemanticIdentity::MAX_KIND_BYTES
            || key.len() > ResourceSemanticIdentity::MAX_KEY_BYTES
        {
            return Err(address.error("oversized resource semantic identity"));
        }
        let identity = ResourceSemanticIdentity {
            kind: capnp_text(kind)?,
            key: capnp_text(key)?,
        };
        if !identity.is_valid() || identity.kind != capnp_text(row.get_resource_kind()?)? {
            return Err(address.error("invalid resource semantic identity"));
        }
        Ok(Some(identity))
    }

    fn decode_view(
        view: collab_capnp::resource_view_descriptor::Reader<'_>,
    ) -> Result<ResourceViewDescriptor, capnp::Error> {
        let description = capnp_text(view.get_description()?)?;
        Ok(ResourceViewDescriptor {
            id: capnp_text(view.get_id()?)?,
            label: capnp_text(view.get_label()?)?,
            description: (!description.is_empty()).then_some(description),
            open_action: capnp_text(view.get_open_action()?)?,
            primary: view.get_primary(),
            range: view
                .has_range()
                .then(|| view.get_range())
                .transpose()?
                .map(|range| {
                    Ok::<_, capnp::Error>(ResourceViewRangeSpec {
                        action: capnp_text(range.get_action()?)?,
                        start_param: capnp_text(range.get_start_param()?)?,
                        end_param: capnp_text(range.get_end_param()?)?,
                        unit_param: capnp_text(range.get_unit_param()?)?,
                    })
                })
                .transpose()?,
            mount: view
                .has_mount()
                .then(|| view.get_mount())
                .transpose()?
                .map(|mount| {
                    Ok::<_, capnp::Error>(ResourceViewMountSpec {
                        action: capnp_text(mount.get_action()?)?,
                        capability_param: capnp_text(mount.get_capability_param()?)?,
                        view_param: capnp_text(mount.get_view_param()?)?,
                    })
                })
                .transpose()?,
        })
    }

    fn decode_action(
        row: collab_capnp::exported_action::Reader<'_>,
        source: Source,
    ) -> Result<ExportedAction, capnp::Error> {
        let resource = capnp_text(row.get_resource()?)?;
        let action = capnp_text(row.get_action()?)?;
        let address = Address {
            source,
            resource: &resource,
            action: &action,
        };
        let semantic_identity = decode_semantic_identity(&address, row)?;
        let operation_kind =
            operation_kind(&address, capnp_text(row.get_operation_kind()?)?.as_str())?;
        let params_json = capnp_text(row.get_params_json()?)?;
        let contract_json = capnp_text(row.get_contract_json()?)?;
        let (params, contract) = match source {
            Source::Published => (
                if params_json.is_empty() {
                    Vec::new()
                } else {
                    serde_json::from_str(&params_json).map_err(|error| {
                        address.error(format_args!("invalid exported action parameters: {error}"))
                    })?
                },
                if contract_json.is_empty() {
                    // An older client sent no contract: select the
                    // conservative defaults for its operation kind.
                    match operation_kind {
                        OperationKind::View => ActionContract::view(),
                        OperationKind::Document | OperationKind::Effect => {
                            ActionContract::default()
                        }
                    }
                } else {
                    serde_json::from_str(&contract_json).map_err(|error| {
                        address.error(format_args!("invalid exported action contract: {error}"))
                    })?
                },
            ),
            Source::Directory => (
                decode_json(
                    &address,
                    &params_json,
                    "parameters",
                    MAX_DIRECTORY_PARAMS_WIRE_BYTES,
                )?,
                decode_json(
                    &address,
                    &contract_json,
                    "contract",
                    MAX_DIRECTORY_CONTRACT_WIRE_BYTES,
                )?,
            ),
        };
        let input = match row.get_input()?.which()? {
            collab_capnp::action_input::Which::None(()) => ExportedInput::None,
            collab_capnp::action_input::Which::Text(text) => ExportedInput::Text {
                prompt: capnp_text(text.get_prompt()?)?,
                placeholder: capnp_text(text.get_placeholder()?)?,
                wants_content: text.get_wants_content(),
            },
        };
        let view = row
            .has_view()
            .then(|| row.get_view())
            .transpose()?
            .map(decode_view)
            .transpose()?;
        let description = capnp_text(row.get_description()?)?;
        Ok(ExportedAction {
            semantic_identity,
            resource_kind: capnp_text(row.get_resource_kind()?)?,
            resource_label: capnp_text(row.get_resource_label()?)?,
            action_label: capnp_text(row.get_action_label()?)?,
            description: (!description.is_empty()).then_some(description),
            operation_kind,
            params,
            contract,
            view,
            input,
            resource,
            action,
        })
    }

    /// Decode one action from a published surface (`publishActions`).
    ///
    /// An older client's empty operation kind reads as a document edit, its
    /// empty parameters as none, and its empty contract as the conservative
    /// defaults for its kind. Anything present must parse.
    ///
    /// # Errors
    ///
    /// Returns an error naming the action when a field is absent, an
    /// operation kind is unknown, the semantic identity is invalid, or the
    /// parameters or contract do not decode.
    pub fn decode_exported_action(
        row: collab_capnp::exported_action::Reader<'_>,
    ) -> Result<ExportedAction, capnp::Error> {
        decode_action(row, Source::Published)
    }

    /// Decode a complete fresh semantic binding from its Cap'n Proto form.
    ///
    /// Fresh bindings originate in an application directory, so their
    /// descriptor receives the same strict typed and bounded decoding as the
    /// directory row containing it.
    ///
    /// # Errors
    ///
    /// Returns an error when the descriptor or text is malformed.
    pub fn decode_fresh_action_binding(
        binding: collab_capnp::fresh_action_binding::Reader<'_>,
    ) -> Result<FreshActionBinding, capnp::Error> {
        let operation_type = capnp_text(binding.get_operation_type()?)?;
        Ok(FreshActionBinding {
            descriptor: decode_action(binding.get_descriptor()?, Source::Directory)?,
            locus: capnp_text(binding.get_locus()?)?,
            operation_type: (!operation_type.is_empty()).then_some(operation_type),
        })
    }

    pub(crate) fn decode_action_directory_row(
        row: collab_capnp::action_directory_row::Reader<'_>,
    ) -> Result<ActionDirectoryRow, capnp::Error> {
        let resource = capnp_text(row.get_resource()?)?;
        let action = capnp_text(row.get_action()?)?;
        let address = Address {
            source: Source::Directory,
            resource: &resource,
            action: &action,
        };
        let presentation_json = capnp_text(row.get_resource_presentation_json()?)?;
        let presentation = if presentation_json.is_empty() {
            None
        } else {
            let presentation: ResourcePresentation = decode_json(
                &address,
                &presentation_json,
                "resource presentation",
                MAX_DIRECTORY_PRESENTATION_WIRE_BYTES,
            )?;
            if !presentation.is_valid() {
                return Err(address.error("resource presentation is out of bounds"));
            }
            Some(presentation)
        };
        let visibility_action = capnp_text(row.get_visibility_action()?)?;
        let visibility_contract = capnp_text(row.get_visibility_contract_json()?)?;
        let visibility = match (visibility_action.is_empty(), visibility_contract.is_empty()) {
            (true, true) => None,
            (false, false) => Some(RowVisibility {
                contract: decode_json(
                    &address,
                    &visibility_contract,
                    "visibility contract",
                    MAX_DIRECTORY_CONTRACT_WIRE_BYTES,
                )?,
                action: visibility_action,
            }),
            _ => {
                return Err(address.error("visibility action and contract must be set together"));
            }
        };
        let descriptor = row
            .has_descriptor()
            .then(|| row.get_descriptor())
            .transpose()?
            .map(|descriptor| decode_action(descriptor, Source::Directory))
            .transpose()?;
        let fresh_binding = row
            .has_fresh_binding()
            .then(|| row.get_fresh_binding())
            .transpose()?
            .map(decode_fresh_action_binding)
            .transpose()?;
        Ok(ActionDirectoryRow {
            parent_resource: capnp_text(row.get_parent_resource()?)?,
            resource_kind: capnp_text(row.get_resource_kind()?)?,
            resource_label: capnp_text(row.get_resource_label()?)?,
            presentation,
            action: (!action.is_empty()).then_some(action),
            descriptor,
            fresh_binding,
            has_children: row.get_has_children(),
            visibility,
            resource,
        })
    }

    /// Decode one bounded action-directory page from its Cap'n Proto wire
    /// form, into the typed rows an in-process shell client or the broker
    /// filters.
    ///
    /// # Errors
    ///
    /// Returns a Cap'n Proto error naming the row when a field is absent,
    /// malformed, oversized, or cannot be decoded into the typed
    /// resource/action representation. Nothing malformed is dropped
    /// silently: a refused row refuses its page.
    pub fn decode_action_directory_page(
        page: collab_capnp::action_directory_page::Reader<'_>,
    ) -> Result<ActionDirectoryPage, capnp::Error> {
        let rows = page
            .get_rows()?
            .iter()
            .map(decode_action_directory_row)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ActionDirectoryPage {
            revision: capnp_text(page.get_revision()?)?,
            rows,
            next_cursor: page.get_next_cursor()?.to_vec(),
            exhausted: page.get_exhausted(),
            stale_cursor: page.get_stale_cursor(),
            budget_exhausted: page.get_budget_exhausted(),
        })
    }

    /// Decode an exact action-directory resolution from its Cap'n Proto wire
    /// form.
    ///
    /// # Errors
    ///
    /// Returns a Cap'n Proto error when the union or a contained action row is
    /// malformed.
    pub fn decode_action_directory_resolution(
        resolution: collab_capnp::action_directory_resolution::Reader<'_>,
    ) -> Result<ActionDirectoryResolution, capnp::Error> {
        match resolution.which()? {
            collab_capnp::action_directory_resolution::Which::Found(row) => Ok(
                ActionDirectoryResolution::Found(Box::new(decode_action_directory_row(row?)?)),
            ),
            collab_capnp::action_directory_resolution::Which::NotFound(()) => {
                Ok(ActionDirectoryResolution::NotFound)
            }
            collab_capnp::action_directory_resolution::Which::StaleRevision(()) => {
                Ok(ActionDirectoryResolution::StaleRevision)
            }
            collab_capnp::action_directory_resolution::Which::Unavailable(()) => {
                Ok(ActionDirectoryResolution::Unavailable)
            }
        }
    }

    /// Decode an exact directory result together with any authoritative
    /// current revision supplied by its live owner.
    ///
    /// # Errors
    ///
    /// Returns an error when either the resolution or revision is malformed.
    pub fn decode_action_directory_resolution_reply(
        resolution: collab_capnp::action_directory_resolution::Reader<'_>,
    ) -> Result<ActionDirectoryResolutionReply, capnp::Error> {
        Ok(ActionDirectoryResolutionReply {
            revision: capnp_text(resolution.get_revision()?)?,
            resolution: decode_action_directory_resolution(resolution)?,
        })
    }
}

#[cfg(all(test, feature = "client"))]
#[path = "action_directory_tests.rs"]
mod tests;
