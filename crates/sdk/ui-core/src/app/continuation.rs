/// Toolkit-owned continuation schema used when an app has no narrower
/// domain-specific projection. The CRDT document travels on its stable record
/// route; this payload restores the complementary local presentation state.
pub const GENERIC_VIEW_CONTINUATION_SCHEMA: u16 = u16::MAX;
/// Framework-owned continuation envelope. Every new checkpoint uses this
/// schema, so app semantics augment rather than replace presentation state.
pub const STRUCTURAL_CONTINUATION_SCHEMA: u16 = u16::MAX - 1;

/// Host-owned generic continuation. Deliberately excludes widget `ephemeral`
/// strings and `RecordId::for_file` values: both may contain local paths or
/// app secrets. File navigation is restored by the shell's typed
/// `FileRecordId` launch intent; this fallback carries presentation only.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct PortableContinuationView {
    scroll: Vec<f64>,
    focus_ordinal: Option<usize>,
    viewport: crate::view::ViewRect,
    view_mode: Option<String>,
}

impl PortableContinuationView {
    fn capture(view: crate::view::View) -> Self {
        let scroll = view
            .scroll
            .into_iter()
            .filter(|value| value.is_finite())
            .take(32)
            .collect();
        let view_mode = view.view_mode.filter(|mode| {
            !mode.is_empty()
                && mode.len() <= 64
                && mode
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        });
        Self {
            scroll,
            focus_ordinal: view.focus.map(|focus| focus.ordinal),
            viewport: view.viewport,
            view_mode,
        }
    }

    fn into_view(self) -> crate::view::View {
        crate::view::View {
            scroll: self.scroll,
            focus: self.focus_ordinal.map(|ordinal| crate::view::FocusView {
                ordinal,
                ephemeral: String::new(),
            }),
            // Stable file records are routed by the shell and never encoded
            // as the toolkit's path-bearing `RecordId::for_file`.
            open_resource: None,
            viewport: self.viewport,
            view_mode: self.view_mode,
        }
    }

    fn valid(&self) -> bool {
        self.scroll.len() <= 32
            && self.scroll.iter().all(|value| value.is_finite())
            && [
                self.viewport.x,
                self.viewport.y,
                self.viewport.width,
                self.viewport.height,
            ]
            .into_iter()
            .all(f64::is_finite)
            && self.view_mode.as_ref().is_none_or(|mode| {
                !mode.is_empty()
                    && mode.len() <= 64
                    && mode
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            })
    }
}

const CONTINUATION_FACET_PRESENTATION: u8 = 1 << 0;
const CONTINUATION_FACET_SEMANTIC: u8 = 1 << 1;
const CONTINUATION_KNOWN_FACETS: u8 = CONTINUATION_FACET_PRESENTATION | CONTINUATION_FACET_SEMANTIC;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct StructuralContinuationEnvelope {
    facets: u8,
    presentation: PortableContinuationView,
    semantic: Option<StructuralSemanticProjection>,
    /// The record the window shows, so a device continuing it can bring
    /// that record over before opening it. Never a file's path or identity:
    /// files are routed by the shell's own file intents.
    record: Option<String>,
}

/// Whether `record` may name a window's record in a continuation.
fn portable_record(record: &str) -> bool {
    crate::record::RecordId::parse(record).is_some_and(|id| {
        !id.is_package_primary() && id.file_path().is_none() && id.file_record().is_none()
    })
}

/// `metadata` (a structural checkpoint) naming the record its window shows.
/// Unchanged when `record` may not travel.
#[must_use]
pub fn with_continuation_record(
    metadata: crate::collaboration::ContinuationMetadata,
    record: &crate::record::RecordId,
) -> crate::collaboration::ContinuationMetadata {
    if metadata.schema_version != STRUCTURAL_CONTINUATION_SCHEMA
        || !portable_record(record.as_str())
    {
        return metadata;
    }
    let Ok(mut envelope) = postcard::from_bytes::<StructuralContinuationEnvelope>(&metadata.bytes)
    else {
        return metadata;
    };
    envelope.record = Some(record.as_str().to_owned());
    match postcard::to_stdvec(&envelope) {
        Ok(bytes) if bytes.len() <= 32 * 1024 => crate::collaboration::ContinuationMetadata {
            schema_version: STRUCTURAL_CONTINUATION_SCHEMA,
            bytes,
        },
        _ => metadata,
    }
}

#[cfg(test)]
mod continuation_record_tests {
    use super::*;

    #[test]
    fn a_checkpoint_names_its_record_but_never_a_file() {
        let base = compose_structural_continuation_metadata(crate::view::View::default(), None)
            .expect("a checkpoint");
        assert_eq!(structural_continuation_record(&base.bytes), None);
        let minted = crate::record::RecordId::minted();
        let named = with_continuation_record(base.clone(), &minted);
        assert_eq!(structural_continuation_record(&named.bytes), Some(minted));
        assert!(
            sanitize_structural_continuation_metadata(&named.bytes, |_, _| true).is_some(),
            "a named record passes the shell boundary"
        );
        let file = crate::record::RecordId::for_file("/home/ada/secret.txt");
        let unnamed = with_continuation_record(base.clone(), &file);
        assert_eq!(unnamed, base, "a file's path never travels in a checkpoint");
    }
}

/// The record a structural checkpoint says its window shows.
pub fn structural_continuation_record(bytes: &[u8]) -> Option<crate::record::RecordId> {
    let envelope = postcard::from_bytes::<StructuralContinuationEnvelope>(bytes).ok()?;
    if !valid_structural_envelope(&envelope) {
        return None;
    }
    envelope
        .record
        .as_deref()
        .and_then(crate::record::RecordId::parse)
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct StructuralSemanticProjection {
    schema_version: u16,
    #[serde(with = "serde_bytes")]
    bytes: Vec<u8>,
}

/// Facets present in a validated structural continuation envelope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StructuralContinuationFacets(u8);

impl StructuralContinuationFacets {
    pub fn presentation(self) -> bool {
        self.0 & CONTINUATION_FACET_PRESENTATION != 0
    }

    pub fn semantic(self) -> bool {
        self.0 & CONTINUATION_FACET_SEMANTIC != 0
    }
}

fn valid_structural_envelope(envelope: &StructuralContinuationEnvelope) -> bool {
    envelope.facets & !CONTINUATION_KNOWN_FACETS == 0
        && envelope.facets & CONTINUATION_FACET_PRESENTATION != 0
        && (envelope.facets & CONTINUATION_FACET_SEMANTIC != 0) == envelope.semantic.is_some()
        && envelope.presentation.valid()
        && envelope
            .record
            .as_deref()
            .is_none_or(|record| record.len() <= 1024 && portable_record(record))
}

/// Inspect the framework facets without exposing their private wire shape.
pub fn structural_continuation_facets(bytes: &[u8]) -> Option<StructuralContinuationFacets> {
    let envelope = postcard::from_bytes::<StructuralContinuationEnvelope>(bytes).ok()?;
    valid_structural_envelope(&envelope).then_some(StructuralContinuationFacets(envelope.facets))
}

/// Compose the toolkit-owned checkpoint used by [`App`]. Public shell/runtime
/// hosts may use this to produce the identical envelope in conformance tests.
pub fn compose_structural_continuation_metadata(
    view: crate::view::View,
    semantic: Option<crate::collaboration::ContinuationMetadata>,
) -> Option<crate::collaboration::ContinuationMetadata> {
    // Invalid widget projections must never suppress the toolkit checkpoint.
    // Shell policy performs app-specific validation; these common bounds
    // prevent malformed implementations from poisoning publication.
    let semantic = semantic
        .filter(|metadata| {
            metadata.schema_version != STRUCTURAL_CONTINUATION_SCHEMA
                && metadata.schema_version != GENERIC_VIEW_CONTINUATION_SCHEMA
                && !metadata.bytes.is_empty()
                && metadata.bytes.len() <= 32 * 1024
        })
        .map(|metadata| StructuralSemanticProjection {
            schema_version: metadata.schema_version,
            bytes: metadata.bytes,
        });
    let facets = CONTINUATION_FACET_PRESENTATION
        | if semantic.is_some() {
            CONTINUATION_FACET_SEMANTIC
        } else {
            0
        };
    let mut envelope = StructuralContinuationEnvelope {
        facets,
        presentation: PortableContinuationView::capture(view),
        semantic,
        record: None,
    };
    let mut bytes = postcard::to_stdvec(&envelope).ok()?;
    if bytes.len() > 32 * 1024 && envelope.semantic.is_some() {
        envelope.semantic = None;
        envelope.facets &= !CONTINUATION_FACET_SEMANTIC;
        bytes = postcard::to_stdvec(&envelope).ok()?;
    }
    (!bytes.is_empty() && bytes.len() <= 32 * 1024).then_some(
        crate::collaboration::ContinuationMetadata {
            schema_version: STRUCTURAL_CONTINUATION_SCHEMA,
            bytes,
        },
    )
}

/// Validate and canonicalize a framework envelope at the shell boundary.
/// An unrecognized app projection is removed rather than retained as opaque
/// bytes; its generic presentation checkpoint remains usable on every device.
pub fn sanitize_structural_continuation_metadata(
    bytes: &[u8],
    semantic_is_safe: impl FnOnce(u16, &[u8]) -> bool,
) -> Option<Vec<u8>> {
    if bytes.is_empty() || bytes.len() > 32 * 1024 {
        return None;
    }
    let mut envelope = postcard::from_bytes::<StructuralContinuationEnvelope>(bytes).ok()?;
    if !valid_structural_envelope(&envelope) {
        return None;
    }
    if envelope.semantic.as_ref().is_some_and(|semantic| {
        semantic.schema_version == STRUCTURAL_CONTINUATION_SCHEMA
            || semantic.schema_version == GENERIC_VIEW_CONTINUATION_SCHEMA
            || semantic.bytes.is_empty()
            || semantic.bytes.len() > 32 * 1024
            || !semantic_is_safe(semantic.schema_version, &semantic.bytes)
    }) {
        envelope.semantic = None;
        envelope.facets &= !CONTINUATION_FACET_SEMANTIC;
    }
    postcard::to_stdvec(&envelope)
        .ok()
        .filter(|canonical| !canonical.is_empty() && canonical.len() <= 32 * 1024)
}

/// Validate the toolkit-owned generic wire shape at the shell boundary.
/// App-provided opaque bytes must not be accepted merely because they use the
/// generic schema number.
pub fn validate_generic_continuation_metadata(bytes: &[u8]) -> bool {
    let Ok(view) = postcard::from_bytes::<PortableContinuationView>(bytes) else {
        return false;
    };
    view.valid()
}

fn continuation_publication(
    app_id: String,
    metadata: Option<&crate::collaboration::ContinuationMetadata>,
    had_previous: bool,
) -> Option<CollabRequest> {
    match metadata {
        Some(metadata) => Some(CollabRequest::ReportContinuation {
            app_id,
            schema_version: metadata.schema_version,
            metadata: metadata.bytes.clone(),
        }),
        None if had_previous => Some(CollabRequest::ClearContinuation { app_id }),
        None => None,
    }
}

impl App {
    /// Publish only a changed, app-declared resume projection. This remains
    /// deliberately separate from document collaboration: a continuation
    /// report is private to the local session and cannot make a remote app
    /// focus itself or start playing media.
    fn publish_continuation_metadata(&mut self) {
        self.apply_pending_continuation_restore();
        if !self.collaboration.manager.is_connected() || self.continuation.pending_view.is_some() {
            return;
        }
        let Some(route) = self.private_continuity_route() else {
            return;
        };
        let semantic = crate::widget::capability::capability::<dyn crate::widget::Continuable>(
            &*self.root_widget,
        )
        .and_then(|continuable| {
            continuable.continuation_metadata(&self.state.doc, &self.state.root)
        });
        let view = <Self as crate::view::FreezeThaw>::capture_view(self);
        let record = self
            .actions
            .bound_record_sync
            .as_ref()
            .map(|sync| sync.binding.checkout.id.clone());
        let metadata =
            compose_structural_continuation_metadata(view, semantic).map(
                |metadata| match &record {
                    Some(record) => with_continuation_record(metadata, record),
                    None => metadata,
                },
            );
        if metadata == self.continuation.last_published {
            return;
        }
        if let Some(request) = continuation_publication(
            route.app_id,
            metadata.as_ref(),
            self.continuation.last_published.is_some(),
        ) {
            self.collaboration.manager.send(request);
        }
        self.continuation.last_published = metadata;
    }

    fn apply_pending_continuation_restore(&mut self) {
        if let Some(metadata) = self.continuation.pending_restore.take() {
            if metadata.schema_version == STRUCTURAL_CONTINUATION_SCHEMA {
                if let Ok(envelope) =
                    postcard::from_bytes::<StructuralContinuationEnvelope>(&metadata.bytes)
                    && valid_structural_envelope(&envelope)
                {
                    self.continuation.pending_view = Some(envelope.presentation.into_view());
                    if let Some(semantic) = envelope.semantic {
                        let root = self.state.root.clone();
                        let metadata = crate::collaboration::ContinuationMetadata {
                            schema_version: semantic.schema_version,
                            bytes: semantic.bytes,
                        };
                        if let Some(continuable) = crate::widget::capability::capability_mut::<
                            dyn crate::widget::Continuable,
                        >(&mut *self.root_widget)
                        {
                            let _ = continuable.restore_continuation_metadata(
                                &metadata,
                                &mut self.state.doc,
                                &root,
                            );
                        }
                        self.invalidate_widget_resource_cache();
                    }
                }
                return;
            }
            if metadata.schema_version == GENERIC_VIEW_CONTINUATION_SCHEMA {
                if let Ok(view) = postcard::from_bytes::<PortableContinuationView>(&metadata.bytes)
                {
                    self.continuation.pending_view = Some(view.into_view());
                }
                return;
            }
            let root = self.state.root.clone();
            if let Some(continuable) = crate::widget::capability::capability_mut::<
                dyn crate::widget::Continuable,
            >(&mut *self.root_widget)
            {
                let _ = continuable.restore_continuation_metadata(
                    &metadata,
                    &mut self.state.doc,
                    &root,
                );
            }
            self.invalidate_widget_resource_cache();
        }
    }
}
