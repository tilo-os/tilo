//! Authenticated, in-session resume metadata reported by native apps.
//!
//! This is deliberately a small cache, not another document store. Apps keep
//! their authoritative state in their own CRDT documents and report only a
//! bounded, path-free continuation projection over the collaboration socket.
//! The network layer can read this registry while capturing a shell layout.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::shell::personal_continuity::{
    AppInstanceId, ContinuityError, LaunchIntent, ResumeMetadata, ResumeSensitivity,
};
use crate::shell::state::RoomId;
use launcher::manifest::{ContinuityContract, ContinuityFacet, ContinuitySemanticSchema};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserResume {
    url: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EbookResume {
    book_id: String,
    chapter: u64,
    page: u64,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MusicResume {
    track_id: String,
    position_ms: u64,
    paused: bool,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ChatResume {
    workspace_id: String,
    channel_id: String,
    media: ui::realtime_continuity::RealtimeMediaFacets,
    controls: ui::realtime_continuity::RealtimeControlIntent,
}

fn portable_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b':'))
}

fn portable_http_url(value: &str) -> bool {
    let Some(rest) = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
    else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    !authority.is_empty()
        && !authority.contains('@')
        && !value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b'\\')
}

pub(crate) fn validate_app_metadata(app_id: &str, schema: u16, bytes: &[u8]) -> bool {
    if schema == ui::app::STRUCTURAL_CONTINUATION_SCHEMA {
        return ui::app::sanitize_structural_continuation_metadata(bytes, |schema, bytes| {
            semantic_schema_for_app(app_id).is_some_and(|portable_schema| {
                validate_portable_semantic_metadata(portable_schema, schema, bytes)
            })
        })
        .is_some();
    }
    if schema == ui::app::GENERIC_VIEW_CONTINUATION_SCHEMA {
        return ui::app::validate_generic_continuation_metadata(bytes);
    }
    semantic_schema_for_app(app_id).is_some_and(|portable_schema| {
        validate_portable_semantic_metadata(portable_schema, schema, bytes)
    })
}

fn semantic_schema_for_app(app_id: &str) -> Option<ContinuitySemanticSchema> {
    match crate::shell::launcher::installed_continuity_contract(app_id) {
        crate::shell::launcher::InstalledContinuityRequirements::Required(contract) => {
            contract.semantic_schema
        }
        crate::shell::launcher::InstalledContinuityRequirements::NotInstalled => {
            semantic_schema_for_builtin(app_id)
        }
        crate::shell::launcher::InstalledContinuityRequirements::Invalid => None,
    }
}

fn semantic_schema_for_builtin(app_id: &str) -> Option<ContinuitySemanticSchema> {
    match app_id {
        "so.tilo.browser" => Some(ContinuitySemanticSchema::WebLocationV1),
        "so.tilo.ebook" => Some(ContinuitySemanticSchema::PublicationPositionV1),
        "so.tilo.music" => Some(ContinuitySemanticSchema::MediaPositionV1),
        "so.tilo.chat" => Some(ContinuitySemanticSchema::RealtimeSessionV1),
        _ => None,
    }
}

fn validate_portable_semantic_metadata(
    portable_schema: ContinuitySemanticSchema,
    schema: u16,
    bytes: &[u8],
) -> bool {
    if schema != 1 {
        return false;
    }
    match portable_schema {
        ContinuitySemanticSchema::WebLocationV1 => serde_json::from_slice::<BrowserResume>(bytes)
            .is_ok_and(|resume| portable_http_url(&resume.url)),
        ContinuitySemanticSchema::PublicationPositionV1 => {
            serde_json::from_slice::<EbookResume>(bytes).is_ok_and(|resume| {
                let _ = (resume.chapter, resume.page);
                portable_id(&resume.book_id)
            })
        }
        ContinuitySemanticSchema::MediaPositionV1 => serde_json::from_slice::<MusicResume>(bytes)
            .is_ok_and(|resume| {
                let _ = resume.position_ms;
                resume.paused && portable_id(&resume.track_id)
            }),
        ContinuitySemanticSchema::RealtimeSessionV1 => serde_json::from_slice::<ChatResume>(bytes)
            .is_ok_and(|resume| {
                let _ = resume.controls;
                resume.media.valid()
                    && resume.media.microphone
                    && !resume.media.share_audio
                    && portable_id(&resume.workspace_id)
                    && portable_id(&resume.channel_id)
            }),
    }
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
    reports: BTreeMap<(RoomId, String, AppInstanceId), AppContinuationReport>,
    tab_instances: BTreeMap<AppInstanceId, AppInstanceId>,
    /// Cold-start offers from another device.  Keeping these separate from
    /// `tab_instances` is essential: an incoming update must not replace the
    /// record identity of a local process already bound into that tab.
    remote_resumes: BTreeMap<(RoomId, String, AppInstanceId), ResolvedContinuation>,
    remote_records: BTreeMap<(RoomId, String, AppInstanceId), AppInstanceId>,
}

impl ContinuationRegistry {
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
        let semantic_schema = semantic_schema_for_builtin(&app_id);
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
                    semantic_schema.is_some_and(|portable_schema| {
                        validate_portable_semantic_metadata(portable_schema, schema, bytes)
                    })
                })
                .ok_or(ContinuityError::InvalidResumePayload)?;
            ResumeMetadata::new(schema_version, ResumeSensitivity::Syncable, bytes)?
        } else {
            let resume = ResumeMetadata::new(schema_version, ResumeSensitivity::Syncable, bytes)?;
            if schema_version != ui::app::GENERIC_VIEW_CONTINUATION_SCHEMA
                && !semantic_schema.is_some_and(|portable_schema| {
                    validate_portable_semantic_metadata(
                        portable_schema,
                        schema_version,
                        resume.bytes(),
                    )
                })
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
}
