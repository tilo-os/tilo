//! Authenticated, in-session resume metadata reported by native apps.
//!
//! This is deliberately a small cache, not another document store. Apps keep
//! their authoritative state in their own CRDT documents and report only a
//! bounded, path-free continuation projection over the collaboration socket.
//! The network layer can read this registry while capturing a shell layout.

mod owner_views;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::shell::personal_continuity::{
    AppInstanceId, ContinuityError, LaunchIntent, ResumeMetadata, ResumeSensitivity,
};
use crate::shell::state::RoomId;
use launcher::manifest::{ContinuityContract, ContinuityFacet, ContinuitySemanticSchema};

/// Whether a payload may carry a schema the receiving app has not been seen
/// to declare. A checkpoint from another device usually names an app this
/// device has not installed yet ("apps are not installed"); its tagged schema
/// is checked on its own, and the installed manifest decides at launch.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Uninstalled {
    DeferToLaunch,
    Refuse,
}

/// Shape and invariants of `schema`, with the toolkit deciding CRDT anchors.
fn schema_payload_valid(schema: ContinuitySemanticSchema, bytes: &[u8]) -> bool {
    schema.validate(bytes, |bytes| {
        serde_json::from_slice::<ui::text_position::TextPosition>(bytes)
            .is_ok_and(|position| position.valid())
    })
}

pub(crate) fn validate_app_metadata(app_id: &str, schema: u16, bytes: &[u8]) -> bool {
    validate_app_metadata_for(app_id, schema, bytes, Uninstalled::DeferToLaunch)
}

fn validate_app_metadata_for(
    app_id: &str,
    schema: u16,
    bytes: &[u8],
    uninstalled: Uninstalled,
) -> bool {
    if schema == ui::app::STRUCTURAL_CONTINUATION_SCHEMA {
        return ui::app::sanitize_structural_continuation_metadata(bytes, |schema, bytes| {
            semantic_metadata_valid(app_id, schema, bytes, uninstalled)
        })
        .is_some();
    }
    if schema == ui::app::GENERIC_VIEW_CONTINUATION_SCHEMA {
        return ui::app::validate_generic_continuation_metadata(bytes);
    }
    semantic_metadata_valid(app_id, schema, bytes, uninstalled)
}

/// A tagged payload says which schema it follows, so it needs no knowledge
/// of the app beyond agreement with an installed manifest. An untagged one
/// is read as the schema this device knows the app by.
fn semantic_metadata_valid(
    app_id: &str,
    schema: u16,
    bytes: &[u8],
    uninstalled: Uninstalled,
) -> bool {
    if let Some(declared) = ContinuitySemanticSchema::from_wire_tag(schema) {
        return schema_payload_valid(declared, bytes)
            && installed_schema_agrees(app_id, declared, uninstalled);
    }
    schema == ContinuitySemanticSchema::LEGACY_UNTAGGED_TAG
        && semantic_schema_for_app(app_id)
            .is_some_and(|portable_schema| schema_payload_valid(portable_schema, bytes))
}

fn installed_schema_agrees(
    app_id: &str,
    declared: ContinuitySemanticSchema,
    uninstalled: Uninstalled,
) -> bool {
    match crate::shell::launcher::installed_continuity_contract(app_id) {
        crate::shell::launcher::InstalledContinuityRequirements::Required(contract) => {
            contract.semantic_schema == Some(declared)
        }
        crate::shell::launcher::InstalledContinuityRequirements::NotInstalled => {
            uninstalled == Uninstalled::DeferToLaunch
        }
        crate::shell::launcher::InstalledContinuityRequirements::Invalid => false,
    }
}

/// The resume an installed app may receive at launch. A checkpoint accepted
/// while the app was not installed here carried only the sending device's
/// word for its semantic schema; now the signed manifest decides. A
/// disagreeing semantic projection is dropped and the framework's
/// presentation kept, so the tab still opens where it was — unless the
/// manifest requires the semantic facet, in which case a presentation-only
/// resume breaks its contract and the launch starts fresh instead.
pub(crate) fn resume_for_installed_launch(
    app_id: &str,
    schema: u16,
    bytes: Vec<u8>,
) -> Option<(u16, Vec<u8>)> {
    let contract = continuity_contract_for_app(app_id).ok()?;
    let bytes = if schema == ui::app::STRUCTURAL_CONTINUATION_SCHEMA {
        ui::app::sanitize_structural_continuation_metadata(&bytes, |inner, payload| {
            semantic_metadata_valid(app_id, inner, payload, Uninstalled::Refuse)
        })?
    } else {
        validate_app_metadata_for(app_id, schema, &bytes, Uninstalled::Refuse).then_some(bytes)?
    };
    let resume = ResumeMetadata::new(schema, ResumeSensitivity::Syncable, bytes).ok()?;
    resume_has_required_facets(&resume, &contract.required)
        .then(|| (schema, resume.bytes().to_vec()))
}

/// The schema this device knows `app_id` by: its signed manifest, or for an
/// app not installed here, the transition table below.
fn semantic_schema_for_app(app_id: &str) -> Option<ContinuitySemanticSchema> {
    match crate::shell::launcher::installed_continuity_contract(app_id) {
        crate::shell::launcher::InstalledContinuityRequirements::Required(contract) => {
            contract.semantic_schema
        }
        crate::shell::launcher::InstalledContinuityRequirements::NotInstalled => {
            legacy_untagged_builtin_schema(app_id)
        }
        crate::shell::launcher::InstalledContinuityRequirements::Invalid => None,
    }
}

/// TRANSITION ONLY: senders still write semantic payloads untagged
/// (`ContinuitySemanticSchema::LEGACY_UNTAGGED_TAG`), and an untagged payload
/// from an app this device has not installed can only be read by package id.
/// Delete this table once senders stamp `ContinuitySemanticSchema::wire_tag`
/// and every paired device runs a shell that accepts tags; tagged payloads
/// never consult it.
fn legacy_untagged_builtin_schema(app_id: &str) -> Option<ContinuitySemanticSchema> {
    match app_id {
        "so.tilo.browser" => Some(ContinuitySemanticSchema::WebLocationV1),
        "so.tilo.ebook" => Some(ContinuitySemanticSchema::PublicationPositionV1),
        "so.tilo.music" => Some(ContinuitySemanticSchema::MediaPositionV1),
        "so.tilo.chat" => Some(ContinuitySemanticSchema::RealtimeSessionV1),
        "so.tilo.code-editor" => Some(ContinuitySemanticSchema::TextPositionV1),
        _ => None,
    }
}

/// A local app's report against its own signed contract: untagged, or
/// tagged with exactly the schema the contract declares.
fn reported_semantic_valid(
    contract_schema: Option<ContinuitySemanticSchema>,
    schema: u16,
    bytes: &[u8],
) -> bool {
    contract_schema.is_some_and(|declared| {
        (schema == ContinuitySemanticSchema::LEGACY_UNTAGGED_TAG
            || ContinuitySemanticSchema::from_wire_tag(schema) == Some(declared))
            && schema_payload_valid(declared, bytes)
    })
}

fn continuity_contract_for_app(app_id: &str) -> Result<ContinuityContract, ContinuityError> {
    match crate::shell::launcher::installed_continuity_contract(app_id) {
        crate::shell::launcher::InstalledContinuityRequirements::NotInstalled => {
            Ok(ContinuityContract::default())
        }
        crate::shell::launcher::InstalledContinuityRequirements::Required(contract) => Ok(contract),
        crate::shell::launcher::InstalledContinuityRequirements::Invalid => {
            Err(ContinuityError::InvalidResumePayload)
        }
    }
}

/// Latest safe continuation projection for one native application in a room.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppContinuationReport {
    pub room: RoomId,
    pub app_id: String,
    pub instance_id: AppInstanceId,
    pub resume: ResumeMetadata,
}

/// A resume projection together with the CRDT record that owns it.  The
/// record is a launcher-issued numeric instance id, not an app supplied
/// pathname or handle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedContinuation {
    pub resume: ResumeMetadata,
    pub record_instance: AppInstanceId,
}

/// Level-triggered reports keyed by `(room, app id, stable instance id)`.
/// A connection can replace only its own application route; the Cap'n Proto
/// broker verifies that property before calling [`Self::report`].
#[derive(Clone, Debug, Default)]
pub struct ContinuationRegistry {
    owner_views: owner_views::OwnerViews,
    reports: BTreeMap<(RoomId, String, AppInstanceId), AppContinuationReport>,
    tab_instances: BTreeMap<AppInstanceId, AppInstanceId>,
    /// Cold-start offers from another device.  Keeping these separate from
    /// `tab_instances` is essential: an incoming update must not replace the
    /// record identity of a local process already bound into that tab.
    remote_resumes: BTreeMap<(RoomId, String, AppInstanceId), ResolvedContinuation>,
    remote_records: BTreeMap<(RoomId, String, AppInstanceId), AppInstanceId>,
}

impl ContinuationRegistry {
    pub(crate) fn owner_view_key(
        &self,
        room: RoomId,
        app: &crate::shell::personal_continuity::AppInstanceState,
    ) -> Option<(RoomId, String, AppInstanceId)> {
        let LaunchIntent::App { app_id } = &app.launch else {
            return None;
        };
        if semantic_schema_for_app(app_id) != Some(ContinuitySemanticSchema::TextPositionV1) {
            return None;
        }
        let instance = app.record_instance.unwrap_or(app.id);
        let key = (room, app_id.clone(), instance);
        if self.tab_instances.get(&app.id) != Some(&instance) || !self.reports.contains_key(&key) {
            return None;
        }
        let resume = app.resume.as_ref()?;
        validate_app_metadata(app_id, resume.schema_version(), resume.bytes()).then_some(key)
    }

    pub(crate) fn offer_owner_view(
        &mut self,
        room: RoomId,
        app: &crate::shell::personal_continuity::AppInstanceState,
        route: Option<crate::shell::network::AuthenticatedPersonalRoute>,
    ) {
        let Some(key) = self.owner_view_key(room, app) else {
            return;
        };
        if let Some(resume) = app.resume.as_ref() {
            self.owner_views
                .offer(key, app.stamp.clone(), resume, route);
        }
    }

    pub(crate) fn refresh_owner_view(&mut self, room: RoomId, app: &str, instance: AppInstanceId) {
        self.owner_views.refresh(room, app, instance);
    }

    pub(crate) fn retain_owner_view_candidates(
        &mut self,
        keys: &std::collections::BTreeSet<(RoomId, String, AppInstanceId)>,
    ) {
        self.owner_views.retain_candidates(keys);
    }

    pub(crate) fn stop_owner_views_unless(
        &mut self,
        allowed: impl Fn(&crate::shell::network::AuthenticatedPersonalRoute) -> bool,
    ) {
        self.owner_views.stop_unless(allowed);
    }

    pub(crate) fn drain_owner_views(
        &mut self,
    ) -> Vec<(
        ui::collaboration::owner_view::OwnerView,
        Option<crate::shell::network::AuthenticatedPersonalRoute>,
    )> {
        self.owner_views.drain()
    }

    pub(crate) fn acknowledge_owner_claims(
        &mut self,
        revision: u64,
        snapshot: &crate::shell::personal_continuity::DeviceContinuationSnapshot,
    ) {
        self.owner_views.acknowledge(revision, snapshot);
    }

    pub(crate) fn owner_claim_revision(&self) -> u64 {
        self.owner_views.revision
    }

    pub(crate) fn owner_claimed(
        &self,
        room: RoomId,
        app: &crate::shell::personal_continuity::AppInstanceState,
    ) -> bool {
        let LaunchIntent::App { app_id } = &app.launch else {
            return false;
        };
        self.owner_views.claims.contains_key(&(
            room,
            app_id.clone(),
            app.record_instance.unwrap_or(app.id),
        ))
    }

    pub(crate) fn claim_owner_view(
        &mut self,
        room: RoomId,
        app: String,
        instance: AppInstanceId,
        token: u64,
        metadata: ui::collaboration::ContinuationMetadata,
    ) -> Result<(), String> {
        if semantic_schema_for_app(&app) != Some(ContinuitySemanticSchema::TextPositionV1) {
            return Err("this app has no supported live owner view".into());
        }
        if !self.tab_instances.values().any(|bound| *bound == instance) {
            return Err("owner takeover has no live local binding".into());
        }
        let key = (room, app.clone(), instance);
        if !self.reports.contains_key(&key) {
            return Err("owner takeover has no acknowledged app report".into());
        }
        if !self.owner_views.can_claim(&key, token)? {
            return Ok(());
        }
        self.report(
            room,
            app.clone(),
            instance,
            metadata.schema_version,
            metadata.bytes,
        )
        .map_err(|error| error.to_string())?;
        self.owner_views.claim((room, app, instance), token);
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error when the app identifier or resume metadata is not
    /// valid for a synchronizable continuation record.
    pub fn report(
        &mut self,
        room: RoomId,
        app_id: impl Into<String>,
        instance_id: AppInstanceId,
        schema_version: u16,
        bytes: Vec<u8>,
    ) -> Result<bool, ContinuityError> {
        let app_id = app_id.into();
        let contract = continuity_contract_for_app(&app_id)?;
        self.report_with_contract(room, app_id, instance_id, schema_version, bytes, &contract)
    }

    fn report_with_contract(
        &mut self,
        room: RoomId,
        app_id: String,
        instance_id: AppInstanceId,
        schema_version: u16,
        bytes: Vec<u8>,
        contract: &ContinuityContract,
    ) -> Result<bool, ContinuityError> {
        self.report_with_validator(
            room,
            app_id,
            instance_id,
            schema_version,
            bytes,
            &contract.required,
            contract.semantic_schema,
        )
    }

    #[cfg(test)]
    fn report_with_required_facets(
        &mut self,
        room: RoomId,
        app_id: String,
        instance_id: AppInstanceId,
        schema_version: u16,
        bytes: Vec<u8>,
        required: &[ContinuityFacet],
    ) -> Result<bool, ContinuityError> {
        let semantic_schema = legacy_untagged_builtin_schema(&app_id);
        self.report_with_validator(
            room,
            app_id,
            instance_id,
            schema_version,
            bytes,
            required,
            semantic_schema,
        )
    }

    fn report_with_validator(
        &mut self,
        room: RoomId,
        app_id: String,
        instance_id: AppInstanceId,
        schema_version: u16,
        bytes: Vec<u8>,
        required: &[ContinuityFacet],
        semantic_schema: Option<ContinuitySemanticSchema>,
    ) -> Result<bool, ContinuityError> {
        // Reuse the launch target validator: a process cannot smuggle a local
        // pathname into the registry key either.
        LaunchIntent::app(app_id.clone())?;
        let resume = if schema_version == ui::app::STRUCTURAL_CONTINUATION_SCHEMA {
            let bytes =
                ui::app::sanitize_structural_continuation_metadata(&bytes, |schema, bytes| {
                    reported_semantic_valid(semantic_schema, schema, bytes)
                })
                .ok_or(ContinuityError::InvalidResumePayload)?;
            ResumeMetadata::new(schema_version, ResumeSensitivity::Syncable, bytes)?
        } else {
            let resume = ResumeMetadata::new(schema_version, ResumeSensitivity::Syncable, bytes)?;
            if schema_version != ui::app::GENERIC_VIEW_CONTINUATION_SCHEMA
                && !reported_semantic_valid(semantic_schema, schema_version, resume.bytes())
            {
                return Err(ContinuityError::InvalidResumePayload);
            }
            resume
        };
        if !resume_has_required_facets(&resume, required) {
            return Err(ContinuityError::InvalidResumePayload);
        }
        let report = AppContinuationReport {
            room,
            app_id: app_id.clone(),
            instance_id,
            resume,
        };
        let key = (room, app_id, instance_id);
        if self.reports.get(&key) == Some(&report) {
            return Ok(false);
        }
        self.reports.insert(key, report);
        Ok(true)
    }

    pub fn resume_for(
        &self,
        room: RoomId,
        app_id: &str,
        instance_id: AppInstanceId,
    ) -> Option<ResumeMetadata> {
        self.reports
            .get(&(room, app_id.to_string(), instance_id))
            .map(|report| report.resume.clone())
    }

    /// Resolve metadata for one exact shell app instance. File references and
    /// foreign/non-app launch targets never receive app-process metadata.
    pub fn resume_for_launch(
        &self,
        room: RoomId,
        instance_id: AppInstanceId,
        launch: &LaunchIntent,
    ) -> Option<ResumeMetadata> {
        let LaunchIntent::App { app_id } = launch else {
            return None;
        };
        self.resume_for(room, app_id, instance_id)
    }

    /// Bind the shell's durable tab identity to the launch instance that
    /// claimed it. This is established only by the pending-launch/toplevel
    /// handshake; apps cannot choose a tab by reporting metadata.
    pub fn bind_tab(&mut self, tab: AppInstanceId, instance: AppInstanceId) {
        self.tab_instances.insert(tab, instance);
        self.remove_remote_tab(tab);
    }

    /// Drop a local tab association after the tab's launch identity changes.
    /// Keep exact remote offers: they may now be the tab's intended source.
    pub(crate) fn forget_local_binding(&mut self, tab: AppInstanceId) {
        self.tab_instances.remove(&tab);
    }

    /// Remove every process-local association owned by a closed tab.
    pub fn forget_local_tab(&mut self, tab: AppInstanceId) {
        if let Some(instance) = self.tab_instances.remove(&tab) {
            let owned: Vec<_> = self
                .reports
                .keys()
                .filter(|(_, _, id)| *id == instance)
                .cloned()
                .collect();
            for (room, app, instance) in owned {
                self.owner_views.forget(room, &app, instance);
            }
            self.reports
                .retain(|(_, _, candidate), _| *candidate != instance);
        }
        self.remove_remote_tab(tab);
    }

    /// Level-triggered clear for an app which no longer has safe resume
    /// metadata. The route supplies the exact room/app/instance tuple.
    pub fn clear(&mut self, room: RoomId, app_id: &str, instance: AppInstanceId) -> bool {
        self.reports
            .remove(&(room, app_id.to_string(), instance))
            .is_some()
    }

    /// Retain a validated remote checkpoint until the synchronized tab opens
    /// locally. It remains metadata only; this does not launch or focus
    /// anything by itself.
    pub fn store_remote_resume(
        &mut self,
        room: RoomId,
        tab: AppInstanceId,
        app_id: String,
        resume: ResumeMetadata,
        record_instance: AppInstanceId,
    ) {
        let Ok(contract) = continuity_contract_for_app(&app_id) else {
            return;
        };
        if !resume_has_required_facets(&resume, &contract.required) {
            return;
        }
        self.remote_resumes.insert(
            (room, app_id.clone(), tab),
            ResolvedContinuation {
                resume,
                record_instance,
            },
        );
        self.remote_records
            .insert((room, app_id, tab), record_instance);
    }

    pub fn store_remote_record(
        &mut self,
        room: RoomId,
        tab: AppInstanceId,
        app_id: String,
        record_instance: AppInstanceId,
    ) {
        self.remote_records
            .insert((room, app_id, tab), record_instance);
    }

    /// Forget a remote offer once its continuation-owned shell tab was
    /// removed.  Keeping it would let a subsequently recreated tab recover
    /// a checkpoint that its source device has already closed.
    pub fn remove_remote_tab(&mut self, tab: AppInstanceId) {
        self.remote_resumes
            .retain(|(_, _, candidate), _| *candidate != tab);
        self.remote_records
            .retain(|(_, _, candidate), _| *candidate != tab);
    }

    /// Purge every unclaimed offer learned from another device while keeping
    /// process-local app reports intact. This is the revocation boundary for
    /// the device-local App continuations privacy switch.
    pub fn clear_remote(&mut self) {
        self.remote_resumes.clear();
        self.remote_records.clear();
        self.owner_views.stop_unless(|_| false);
    }

    pub fn record_instance_for_tab(
        &self,
        room: RoomId,
        tab: AppInstanceId,
        launch: &LaunchIntent,
    ) -> Option<AppInstanceId> {
        let LaunchIntent::App { app_id } = launch else {
            return None;
        };
        self.tab_instances.get(&tab).copied().or_else(|| {
            self.remote_records
                .get(&(room, app_id.clone(), tab))
                .copied()
        })
    }

    /// Resolve a capture by its exact durable tab id, never merely app id.
    pub fn resume_for_tab(
        &self,
        room: RoomId,
        tab: AppInstanceId,
        launch: &LaunchIntent,
    ) -> Option<ResumeMetadata> {
        self.continuation_for_tab(room, tab, launch)
            .map(|continuation| continuation.resume)
    }

    pub fn continuation_for_tab(
        &self,
        room: RoomId,
        tab: AppInstanceId,
        launch: &LaunchIntent,
    ) -> Option<ResolvedContinuation> {
        let LaunchIntent::App { app_id } = launch else {
            return None;
        };
        if let Some(instance) = self.tab_instances.get(&tab).copied()
            && let Some(resume) = self.resume_for_launch(room, instance, launch)
        {
            return Some(ResolvedContinuation {
                resume,
                record_instance: instance,
            });
        }
        self.remote_resumes
            .get(&(room, app_id.clone(), tab))
            .cloned()
    }

    pub fn len(&self) -> usize {
        self.reports.len()
    }

    pub fn is_empty(&self) -> bool {
        self.reports.is_empty()
    }
}

fn resume_has_required_facets(resume: &ResumeMetadata, required: &[ContinuityFacet]) -> bool {
    let (presentation, semantic) = match resume.schema_version() {
        ui::app::STRUCTURAL_CONTINUATION_SCHEMA => {
            let Some(facets) = ui::app::structural_continuation_facets(resume.bytes()) else {
                return false;
            };
            (facets.presentation(), facets.semantic())
        }
        ui::app::GENERIC_VIEW_CONTINUATION_SCHEMA => (true, false),
        _ => (false, true),
    };
    // Presentation is a framework invariant, not an app-controlled opt-in.
    // Even a malformed/legacy signed contract with an empty requirement list
    // cannot downgrade the structural checkpoint back to opaque app bytes.
    presentation
        && required.iter().all(|facet| match facet {
            ContinuityFacet::Presentation => presentation,
            ContinuityFacet::Semantic => semantic,
        })
}

#[cfg(test)]
thread_local! {
    static TEST_REGISTRY: std::cell::RefCell<Option<Arc<Mutex<ContinuationRegistry>>>> = const { std::cell::RefCell::new(None) };
}

/// Tests may model a new process without erasing another test's live reports.
#[cfg(test)]
pub(crate) fn replace_test_registry(
    registry: Option<Arc<Mutex<ContinuationRegistry>>>,
) -> Option<Arc<Mutex<ContinuationRegistry>>> {
    TEST_REGISTRY.with(|slot| slot.replace(registry))
}

/// The shell broker and network share this process-local registry. It is not
/// persisted: authoritative app CRDT state repopulates it after launch.
pub fn global_registry() -> Arc<Mutex<ContinuationRegistry>> {
    static REGISTRY: OnceLock<Arc<Mutex<ContinuationRegistry>>> = OnceLock::new();
    #[cfg(test)]
    if let Some(registry) = TEST_REGISTRY.with(|slot| slot.borrow().clone()) {
        return registry;
    }
    Arc::clone(REGISTRY.get_or_init(|| Arc::new(Mutex::new(ContinuationRegistry::default()))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_takeover_reserves_bounded_acknowledgement_capacity() {
        use crate::shell::personal_continuity::{
            AppInstanceState, ContinuityStamp, DeviceContinuationSnapshot, DeviceId, TabPlacement,
            WorkspaceContinuation,
        };
        let room = RoomId::new();
        let device = DeviceId::new("owner-capacity").unwrap();
        let metadata =
            ui::app::compose_structural_continuation_metadata(ui::view::View::default(), None)
                .unwrap();
        let mut registry = ContinuationRegistry::default();
        let mut apps = Vec::new();
        for id in 1..=130 {
            let instance = AppInstanceId(id);
            registry.bind_tab(instance, instance);
            registry
                .report(
                    room,
                    "so.tilo.code-editor",
                    instance,
                    metadata.schema_version,
                    metadata.bytes.clone(),
                )
                .unwrap();
            apps.push(AppInstanceState::new(
                instance,
                TabPlacement {
                    strip: 0,
                    group: u32::try_from(id).unwrap(),
                    tab: 0,
                },
                LaunchIntent::App {
                    app_id: "so.tilo.code-editor".into(),
                },
                registry.resume_for(room, "so.tilo.code-editor", instance),
                ContinuityStamp {
                    counter: 1,
                    device: device.clone(),
                },
            ));
        }
        for app in &apps[..128] {
            registry.offer_owner_view(room, app, None);
        }
        assert_eq!(registry.drain_owner_views().len(), 128);
        let original = registry.resume_for(room, "so.tilo.code-editor", AppInstanceId(129));
        assert!(
            registry
                .claim_owner_view(
                    room,
                    "so.tilo.code-editor".into(),
                    AppInstanceId(129),
                    2,
                    metadata.clone()
                )
                .is_err()
        );
        assert_eq!(registry.owner_claim_revision(), 0);
        assert_eq!(
            registry.resume_for(room, "so.tilo.code-editor", AppInstanceId(129)),
            original
        );
        registry.forget_local_tab(AppInstanceId(1));
        registry
            .claim_owner_view(
                room,
                "so.tilo.code-editor".into(),
                AppInstanceId(129),
                2,
                metadata,
            )
            .unwrap();
        // A different offered app cannot steal the pending claim's last slot.
        registry.offer_owner_view(room, &apps[129], None);
        assert!(registry.drain_owner_views().is_empty());
        let snapshot = DeviceContinuationSnapshot::new(
            device,
            1,
            None,
            vec![WorkspaceContinuation {
                room,
                apps: vec![apps[128].clone()],
            }],
        )
        .unwrap();
        registry.acknowledge_owner_claims(registry.owner_claim_revision(), &snapshot);
        let views = registry.drain_owner_views();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].0.instance, 129);
        assert!(views[0].0.local_claim);
        assert!(!registry.owner_claimed(room, &apps[128]));
    }

    #[test]
    fn native_text_metadata_is_bounded_path_free_and_structurally_checked() {
        use automerge::{ObjType, ROOT, transaction::Transactable};
        use ui::text_position::{ResolvedTextPosition, TextPosition};
        let mut doc = automerge::AutoCommit::new();
        let text = doc.put_object(ROOT, "body", ObjType::Text).unwrap();
        doc.splice_text(&text, 0, 0, "private buffer content")
            .unwrap();
        let mut position = TextPosition::capture(
            &doc,
            &text,
            "11111111-2222-4333-8444-555555555555",
            &ResolvedTextPosition {
                carets: vec![(2, Some(1))],
                top_character: 0,
                vertical_fraction: 0.25,
                horizontal_columns: 2.0,
            },
        )
        .unwrap();
        let safe = serde_json::to_vec(&position).unwrap();
        assert!(validate_app_metadata("so.tilo.code-editor", 1, &safe));
        assert!(!validate_app_metadata("so.tilo.code-editor", 2, &safe));
        assert!(!String::from_utf8(safe).unwrap().contains("private buffer"));
        position.carets[0].cursor = vec![255];
        assert!(!validate_app_metadata(
            "so.tilo.code-editor",
            1,
            &serde_json::to_vec(&position).unwrap()
        ));
        assert!(!validate_app_metadata(
            "so.tilo.code-editor",
            1,
            br#"{"resource_id":"/home/alice/private","carets":[],"password":"secret"}"#
        ));
        let empty =
            ui::app::compose_structural_continuation_metadata(ui::view::View::default(), None)
                .unwrap();
        let contract = ContinuityContract {
            required: vec![ContinuityFacet::Presentation],
            semantic_schema: Some(ContinuitySemanticSchema::TextPositionV1),
        };
        assert!(
            ContinuationRegistry::default()
                .report_with_contract(
                    RoomId::new(),
                    "so.tilo.code-editor".into(),
                    AppInstanceId(1),
                    empty.schema_version,
                    empty.bytes,
                    &contract,
                )
                .unwrap()
        );
    }

    fn browser_checkpoint(url: &[u8]) -> ui::collaboration::ContinuationMetadata {
        ui::app::compose_structural_continuation_metadata(
            ui::view::View::default(),
            Some(ui::collaboration::ContinuationMetadata {
                schema_version: 1,
                bytes: url.to_vec(),
            }),
        )
        .unwrap()
    }

    fn report_browser(
        reports: &mut ContinuationRegistry,
        room: RoomId,
        instance: AppInstanceId,
        url: &[u8],
    ) -> Result<bool, ContinuityError> {
        let metadata = browser_checkpoint(url);
        reports.report_with_contract(
            room,
            "so.tilo.browser".into(),
            instance,
            metadata.schema_version,
            metadata.bytes,
            &ContinuityContract {
                required: vec![ContinuityFacet::Presentation, ContinuityFacet::Semantic],
                semantic_schema: Some(ContinuitySemanticSchema::WebLocationV1),
            },
        )
    }

    #[test]
    fn registry_accepts_only_changed_safe_structurally_valid_reports() {
        fn replaces_only_changed_safe_reports() {
            let room = RoomId::new();
            let mut reports = ContinuationRegistry::default();
            assert!(
                report_browser(
                    &mut reports,
                    room,
                    AppInstanceId(11),
                    br#"{"url":"https://tilo.so"}"#,
                )
                .unwrap()
            );
            assert!(
                !report_browser(
                    &mut reports,
                    room,
                    AppInstanceId(11),
                    br#"{"url":"https://tilo.so"}"#,
                )
                .unwrap()
            );
            assert_eq!(reports.len(), 1);
            assert!(
                reports
                    .resume_for(room, "so.tilo.browser", AppInstanceId(11))
                    .is_some()
            );
        }

        fn rejects_pathful_reports_before_they_enter_the_registry() {
            let error = ContinuationRegistry::default()
                .report(
                    RoomId::new(),
                    "so.tilo.browser",
                    AppInstanceId(11),
                    1,
                    b"file:///home/alice/private".to_vec(),
                )
                .unwrap_err();
            assert_eq!(error, ContinuityError::LocalPathInResumeState);
        }

        fn rejects_unknown_opaque_and_structurally_invalid_app_metadata() {
            let room = RoomId::new();
            let mut reports = ContinuationRegistry::default();
            assert_eq!(
                reports.report(
                    room,
                    "so.tilo.mail",
                    AppInstanceId(1),
                    1,
                    br#"{"password":"secret"}"#.to_vec(),
                ),
                Err(ContinuityError::InvalidResumePayload)
            );
            assert_eq!(
                reports.report(
                    room,
                    "so.tilo.browser",
                    AppInstanceId(2),
                    1,
                    br#"{"url":"https://alice:secret@example.test"}"#.to_vec(),
                ),
                Err(ContinuityError::InvalidResumePayload)
            );
            assert_eq!(reports.len(), 0);
        }

        fn chat_continuation_accepts_only_bounded_semantic_call_intent() {
            let safe = br#"{
                "workspace_id":"ws_acme",
                "channel_id":"c_huddle",
                "media":{"microphone":true,"camera":true,"screen_share":false,"share_audio":false},
                "controls":{"microphone_muted":true,"camera_muted":false,"screen_share_paused":false,"share_audio_muted":true}
            }"#;
            assert!(validate_app_metadata("so.tilo.chat", 1, safe));
            assert!(!validate_app_metadata(
                "so.tilo.chat",
                1,
                br#"{
                    "workspace_id":"ws_acme","channel_id":"https://join.invalid/secret",
                    "media":{"microphone":true,"camera":false,"screen_share":false,"share_audio":false},
                    "controls":{"microphone_muted":false,"camera_muted":true,"screen_share_paused":false,"share_audio_muted":true}
                }"#,
            ));
            assert!(!validate_app_metadata(
                "so.tilo.chat",
                1,
                br#"{
                    "workspace_id":"ws_acme","channel_id":"c_huddle","members":["alice"],
                    "media":{"microphone":true,"camera":false,"screen_share":false,"share_audio":false},
                    "controls":{"microphone_muted":false,"camera_muted":true,"screen_share_paused":false,"share_audio_muted":true}
                }"#,
            ));
        }

        replaces_only_changed_safe_reports();
        rejects_pathful_reports_before_they_enter_the_registry();
        rejects_unknown_opaque_and_structurally_invalid_app_metadata();
        chat_continuation_accepts_only_bounded_semantic_call_intent();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn manifest_semantics_cannot_opt_out_of_framework_presentation() {
        fn unknown_app_semantics_cannot_suppress_framework_presentation() {
            let metadata = ui::app::compose_structural_continuation_metadata(
                ui::view::View {
                    scroll: vec![24.0],
                    focus: None,
                    open_resource: None,
                    viewport: ui::view::ViewRect::default(),
                    view_mode: Some("grid".into()),
                },
                Some(ui::collaboration::ContinuationMetadata {
                    schema_version: 1,
                    bytes: br#"{"password":"must-not-survive"}"#.to_vec(),
                }),
            )
            .unwrap();
            let room = RoomId::new();
            let instance = AppInstanceId(41);
            let mut reports = ContinuationRegistry::default();
            assert!(
                reports
                    .report(
                        room,
                        "so.tilo.custom",
                        instance,
                        metadata.schema_version,
                        metadata.bytes,
                    )
                    .unwrap()
            );
            let retained = reports
                .resume_for(room, "so.tilo.custom", instance)
                .expect("generic presentation is retained");
            let facets = ui::app::structural_continuation_facets(retained.bytes()).unwrap();
            assert!(facets.presentation());
            assert!(!facets.semantic());
            assert!(
                !retained
                    .bytes()
                    .windows(8)
                    .any(|window| window == b"password")
            );
        }

        fn semantic_manifest_contract_rejects_presentation_only_regression() {
            let room = RoomId::new();
            let instance = AppInstanceId(72);
            let presentation_only =
                ui::app::compose_structural_continuation_metadata(ui::view::View::default(), None)
                    .unwrap();
            let required = [ContinuityFacet::Presentation, ContinuityFacet::Semantic];
            let mut reports = ContinuationRegistry::default();
            assert_eq!(
                reports.report_with_required_facets(
                    room,
                    "so.tilo.browser".into(),
                    instance,
                    presentation_only.schema_version,
                    presentation_only.bytes,
                    &required,
                ),
                Err(ContinuityError::InvalidResumePayload)
            );

            let complete = browser_checkpoint(br#"{"url":"https://tilo.so"}"#);
            assert!(
                reports
                    .report_with_required_facets(
                        room,
                        "so.tilo.browser".into(),
                        instance,
                        complete.schema_version,
                        complete.bytes,
                        &required,
                    )
                    .unwrap()
            );
        }

        fn third_party_app_uses_signed_portable_schema_not_package_allowlist() {
            let room = RoomId::new();
            let instance = AppInstanceId(74);
            let contract = ContinuityContract {
                required: vec![ContinuityFacet::Presentation, ContinuityFacet::Semantic],
                semantic_schema: Some(ContinuitySemanticSchema::WebLocationV1),
            };
            let safe = ui::app::compose_structural_continuation_metadata(
                ui::view::View::default(),
                Some(ui::collaboration::ContinuationMetadata {
                    schema_version: 1,
                    bytes: br#"{"url":"https://docs.example/guide"}"#.to_vec(),
                }),
            )
            .unwrap();
            let mut reports = ContinuationRegistry::default();
            assert!(
                reports
                    .report_with_contract(
                        room,
                        "org.example.knowledge".into(),
                        instance,
                        safe.schema_version,
                        safe.bytes,
                        &contract,
                    )
                    .unwrap()
            );

            let unsafe_payload = ui::app::compose_structural_continuation_metadata(
                ui::view::View::default(),
                Some(ui::collaboration::ContinuationMetadata {
                    schema_version: 1,
                    bytes: br#"{"url":"file:///home/alice/private"}"#.to_vec(),
                }),
            )
            .unwrap();
            assert_eq!(
                reports.report_with_contract(
                    room,
                    "org.example.knowledge".into(),
                    AppInstanceId(75),
                    unsafe_payload.schema_version,
                    unsafe_payload.bytes,
                    &contract,
                ),
                Err(ContinuityError::InvalidResumePayload)
            );
        }

        fn empty_manifest_contract_cannot_opt_out_of_framework_presentation() {
            let mut reports = ContinuationRegistry::default();
            assert_eq!(
                reports.report_with_required_facets(
                    RoomId::new(),
                    "so.tilo.browser".into(),
                    AppInstanceId(73),
                    1,
                    br#"{"url":"https://tilo.so"}"#.to_vec(),
                    &[],
                ),
                Err(ContinuityError::InvalidResumePayload)
            );
        }

        unknown_app_semantics_cannot_suppress_framework_presentation();
        semantic_manifest_contract_rejects_presentation_only_regression();
        third_party_app_uses_signed_portable_schema_not_package_allowlist();
        empty_manifest_contract_cannot_opt_out_of_framework_presentation();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn registry_instances_and_remote_tabs_keep_exact_projections() {
        fn two_browser_instances_keep_distinct_urls() {
            let room = RoomId::new();
            let mut reports = ContinuationRegistry::default();
            report_browser(
                &mut reports,
                room,
                AppInstanceId(11),
                br#"{"url":"https://a.test"}"#,
            )
            .unwrap();
            report_browser(
                &mut reports,
                room,
                AppInstanceId(22),
                br#"{"url":"https://b.test"}"#,
            )
            .unwrap();
            reports.bind_tab(AppInstanceId(101), AppInstanceId(11));
            reports.bind_tab(AppInstanceId(202), AppInstanceId(22));
            let launch = LaunchIntent::app("so.tilo.browser").unwrap();
            assert_eq!(reports.len(), 2);
            assert_ne!(
                reports.resume_for_tab(room, AppInstanceId(101), &launch),
                reports.resume_for_tab(room, AppInstanceId(202), &launch),
            );
        }

        fn clear_removes_only_the_certified_instance_projection() {
            let room = RoomId::new();
            let mut reports = ContinuationRegistry::default();
            for (instance, url) in [
                (AppInstanceId(11), br#"{"url":"https://a.test"}"#.as_slice()),
                (AppInstanceId(22), br#"{"url":"https://b.test"}"#.as_slice()),
            ] {
                report_browser(&mut reports, room, instance, url).unwrap();
            }

            assert!(reports.clear(room, "so.tilo.browser", AppInstanceId(11)));
            assert!(
                reports
                    .resume_for(room, "so.tilo.browser", AppInstanceId(11))
                    .is_none()
            );
            assert!(
                reports
                    .resume_for(room, "so.tilo.browser", AppInstanceId(22))
                    .is_some()
            );
            assert!(!reports.clear(room, "so.tilo.browser", AppInstanceId(11)));
        }

        fn remote_refresh_does_not_replace_a_live_tab_record() {
            let room = RoomId::new();
            let launch = LaunchIntent::app("so.tilo.browser").unwrap();
            let mut reports = ContinuationRegistry::default();
            reports.bind_tab(AppInstanceId(10), AppInstanceId(101));
            report_browser(
                &mut reports,
                room,
                AppInstanceId(101),
                br#"{"url":"https://local.test"}"#,
            )
            .unwrap();
            reports.store_remote_resume(
                room,
                AppInstanceId(10),
                "so.tilo.browser".into(),
                ResumeMetadata::new(
                    1,
                    ResumeSensitivity::Syncable,
                    br#"{"url":"https://remote.test"}"#.to_vec(),
                )
                .unwrap(),
                AppInstanceId(202),
            );
            let resolved = reports
                .continuation_for_tab(room, AppInstanceId(10), &launch)
                .unwrap();
            assert_eq!(resolved.record_instance, AppInstanceId(101));
            assert_eq!(
                resolved.resume.schema_version(),
                ui::app::STRUCTURAL_CONTINUATION_SCHEMA
            );
            let facets = ui::app::structural_continuation_facets(resolved.resume.bytes()).unwrap();
            assert!(facets.presentation() && facets.semantic());
        }

        fn removing_a_closed_remote_tab_drops_its_staged_checkpoint() {
            let room = RoomId::new();
            let tab = AppInstanceId(10);
            let launch = LaunchIntent::app("so.tilo.browser").unwrap();
            let mut reports = ContinuationRegistry::default();
            let metadata = browser_checkpoint(br#"{"url":"https://closed.test"}"#);
            reports.store_remote_resume(
                room,
                tab,
                "so.tilo.browser".into(),
                ResumeMetadata::new(
                    metadata.schema_version,
                    ResumeSensitivity::Syncable,
                    metadata.bytes,
                )
                .unwrap(),
                AppInstanceId(101),
            );
            reports.remove_remote_tab(tab);
            assert!(reports.continuation_for_tab(room, tab, &launch).is_none());
            assert!(
                reports
                    .record_instance_for_tab(room, tab, &launch)
                    .is_none()
            );
        }

        two_browser_instances_keep_distinct_urls();
        clear_removes_only_the_certified_instance_projection();
        remote_refresh_does_not_replace_a_live_tab_record();
        removing_a_closed_remote_tab_drops_its_staged_checkpoint();
    }

    const READER: &str = "com.example.reader";
    const PAGE_42: &[u8] = br#"{"book_id":"urn:isbn:978-0","chapter":3,"page":42}"#;

    /// An app store holding at most the third-party reader, declaring
    /// `schema` when given. Callers hold `process_env`.
    fn reader_store(schema: Option<&str>) -> (tempfile::TempDir, tilo_test_env::ScopedEnv) {
        reader_store_requiring(schema, r#"["presentation", "semantic"]"#)
    }

    fn reader_store_requiring(
        schema: Option<&str>,
        required: &str,
    ) -> (tempfile::TempDir, tilo_test_env::ScopedEnv) {
        let dir = tempfile::tempdir().unwrap();
        if let Some(schema) = schema {
            let payload = dir.path().join("payloads").join(READER);
            std::fs::create_dir_all(payload.join("bin")).unwrap();
            let manifest = payload.join("manifest.toml");
            std::fs::write(
                &manifest,
                crate::shell::test_support::canonical_manifest(
                    &format!("id = \"{READER}\"\nversion = \"1.0.0\"\nkind = \"app\""),
                    &format!(
                        "[entrypoint]\nbin = \"bin/app\"\ntype = \"gui\"\n\
                         [continuity]\nrequired = {required}\n\
                         semantic-schema = \"{schema}\"\n"
                    ),
                ),
            )
            .unwrap();
            std::fs::write(
                dir.path().join("registry.toml"),
                format!(
                    "[[entry]]\nid = '{READER}'\npayload_root = {:?}\nmanifest_path = {:?}\n",
                    payload.display().to_string(),
                    manifest.display().to_string()
                ),
            )
            .unwrap();
        }
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_APPS_DATA_DIR", dir.path());
        (dir, env)
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn an_uninstalled_third_party_app_roams_a_tagged_semantic_payload() {
        let (_store, _env) = reader_store(None);
        assert!(validate_app_metadata(
            READER,
            ContinuitySemanticSchema::PublicationPositionV1.wire_tag(),
            PAGE_42,
        ));
        // The tag is checked against the payload, not trusted.
        assert!(!validate_app_metadata(
            READER,
            ContinuitySemanticSchema::WebLocationV1.wire_tag(),
            PAGE_42,
        ));
        assert!(!validate_app_metadata(
            READER,
            ContinuitySemanticSchema::PublicationPositionV1.wire_tag(),
            br#"{"book_id":"../x","chapter":3,"page":42}"#,
        ));
        // Untagged, nothing says what the payload means.
        assert!(!validate_app_metadata(
            READER,
            ContinuitySemanticSchema::LEGACY_UNTAGGED_TAG,
            PAGE_42,
        ));
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn an_installed_app_that_declares_a_different_schema_is_refused() {
        let (_store, _env) = reader_store(Some("web-location-v1"));
        assert!(!validate_app_metadata(
            READER,
            ContinuitySemanticSchema::PublicationPositionV1.wire_tag(),
            PAGE_42,
        ));
        assert!(validate_app_metadata(
            READER,
            ContinuitySemanticSchema::WebLocationV1.wire_tag(),
            br#"{"url":"https://docs.example/guide"}"#,
        ));
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn legacy_untagged_first_party_payload_still_accepted() {
        let (_store, _env) = reader_store(None);
        assert!(validate_app_metadata(
            "so.tilo.ebook",
            ContinuitySemanticSchema::LEGACY_UNTAGGED_TAG,
            PAGE_42,
        ));
        assert!(validate_app_metadata(
            "so.tilo.ebook",
            ContinuitySemanticSchema::PublicationPositionV1.wire_tag(),
            PAGE_42,
        ));
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn launch_drops_resume_when_installed_schema_disagrees() {
        use crate::shell::launcher;
        let checkpoint = ui::app::compose_structural_continuation_metadata(
            ui::view::View {
                scroll: vec![24.0],
                ..ui::view::View::default()
            },
            Some(ui::collaboration::ContinuationMetadata {
                schema_version: ContinuitySemanticSchema::PublicationPositionV1.wire_tag(),
                bytes: PAGE_42.to_vec(),
            }),
        )
        .unwrap();
        let launch_with = |schema: Option<&str>, required: &str| {
            let (_store, _env) = reader_store_requiring(schema, required);
            let room = RoomId::new();
            launcher::clear_pending_placements();
            launcher::set_pending_placement(
                READER,
                crate::shell::tab_binding::Placement::IntoTab(41),
            );
            launcher::stage_continuation_resume(
                room,
                41,
                checkpoint.schema_version,
                checkpoint.bytes.clone(),
                4_100,
            );
            let staged = launcher::take_staged_resume_for_package(READER, room);
            launcher::clear_pending_placements();
            let (resume, record) = staged.expect("the tab keeps its launch");
            assert_eq!(record, 4_100, "the source record still reopens");
            resume.map(|(schema, bytes)| {
                assert_eq!(schema, ui::app::STRUCTURAL_CONTINUATION_SCHEMA);
                let facets = ui::app::structural_continuation_facets(&bytes).unwrap();
                assert!(facets.presentation());
                (facets.semantic(), bytes)
            })
        };
        let both = r#"["presentation", "semantic"]"#;
        let presentation = r#"["presentation"]"#;

        let (semantic, bytes) = launch_with(Some("publication-position-v1"), both)
            .expect("an agreeing manifest receives the checkpoint");
        assert!(semantic, "an agreeing manifest receives the position");
        assert_eq!(bytes, checkpoint.bytes);
        let (semantic, _) = launch_with(Some("web-location-v1"), presentation)
            .expect("the presentation checkpoint survives");
        assert!(!semantic, "a disagreeing manifest never sees the payload");
        assert_eq!(
            launch_with(Some("web-location-v1"), both),
            None,
            "a manifest requiring the semantic facet never receives a presentation-only resume",
        );
        let (semantic, _) = launch_with(None, both).expect("the presentation checkpoint survives");
        assert!(!semantic, "a launch with no signed manifest cannot agree");
    }

    #[test]
    fn realtime_schema_facets_mirror_the_toolkit_contract() {
        use launcher::manifest::continuity_schema as schema;
        for bits in 0u8..16 {
            let [microphone, camera, screen_share, share_audio] =
                [0, 1, 2, 3].map(|bit| bits & (1 << bit) != 0);
            let toolkit = ui::realtime_continuity::RealtimeMediaFacets {
                microphone,
                camera,
                screen_share,
                share_audio,
            };
            let json = serde_json::to_vec(&toolkit).unwrap();
            let mirrored: schema::RealtimeMediaFacets = serde_json::from_slice(&json).unwrap();
            assert_eq!(serde_json::to_vec(&mirrored).unwrap(), json);
            assert_eq!(mirrored.valid(), toolkit.valid(), "{json:?}");
            let controls = ui::realtime_continuity::RealtimeControlIntent {
                microphone_muted: microphone,
                camera_muted: camera,
                screen_share_paused: screen_share,
                share_audio_muted: share_audio,
            };
            let json = serde_json::to_vec(&controls).unwrap();
            let mirrored: schema::RealtimeControlIntent = serde_json::from_slice(&json).unwrap();
            assert_eq!(serde_json::to_vec(&mirrored).unwrap(), json);
        }
    }
}
