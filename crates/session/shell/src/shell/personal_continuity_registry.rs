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
    if schema == ui::app::GENERIC_VIEW_CONTINUATION_SCHEMA {
        return ui::app::validate_generic_continuation_metadata(bytes);
    }
    if schema != 1 {
        return false;
    }
    match app_id {
        "dev.qos.browser" => serde_json::from_slice::<BrowserResume>(bytes)
            .is_ok_and(|resume| portable_http_url(&resume.url)),
        "dev.qos.ebook" => serde_json::from_slice::<EbookResume>(bytes).is_ok_and(|resume| {
            let _ = (resume.chapter, resume.page);
            portable_id(&resume.book_id)
        }),
        "dev.qos.music" => serde_json::from_slice::<MusicResume>(bytes).is_ok_and(|resume| {
            let _ = resume.position_ms;
            resume.paused && portable_id(&resume.track_id)
        }),
        _ => false,
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
        // Reuse the launch target validator: a process cannot smuggle a local
        // pathname into the registry key either.
        LaunchIntent::app(app_id.clone())?;
        let resume = ResumeMetadata::new(schema_version, ResumeSensitivity::Syncable, bytes)?;
        if !validate_app_metadata(&app_id, schema_version, resume.bytes()) {
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

/// The shell broker and network share this process-local registry. It is not
/// persisted: authoritative app CRDT state repopulates it after launch.
pub fn global_registry() -> Arc<Mutex<ContinuationRegistry>> {
    static REGISTRY: OnceLock<Arc<Mutex<ContinuationRegistry>>> = OnceLock::new();
    Arc::clone(REGISTRY.get_or_init(|| Arc::new(Mutex::new(ContinuationRegistry::default()))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_only_changed_safe_reports() {
        let room = RoomId::new();
        let mut reports = ContinuationRegistry::default();
        assert!(
            reports
                .report(
                    room,
                    "dev.qos.browser",
                    AppInstanceId(11),
                    1,
                    br#"{"url":"https://qos.dev"}"#.to_vec()
                )
                .unwrap()
        );
        assert!(
            !reports
                .report(
                    room,
                    "dev.qos.browser",
                    AppInstanceId(11),
                    1,
                    br#"{"url":"https://qos.dev"}"#.to_vec()
                )
                .unwrap()
        );
        assert_eq!(reports.len(), 1);
        assert!(
            reports
                .resume_for(room, "dev.qos.browser", AppInstanceId(11))
                .is_some()
        );
    }

    #[test]
    fn rejects_pathful_reports_before_they_enter_the_registry() {
        let error = ContinuationRegistry::default()
            .report(
                RoomId::new(),
                "dev.qos.browser",
                AppInstanceId(11),
                1,
                b"file:///home/alice/private".to_vec(),
            )
            .unwrap_err();
        assert_eq!(error, ContinuityError::LocalPathInResumeState);
    }

    #[test]
    fn rejects_unknown_opaque_and_structurally_invalid_app_metadata() {
        let room = RoomId::new();
        let mut reports = ContinuationRegistry::default();
        assert_eq!(
            reports.report(
                room,
                "dev.qos.mail",
                AppInstanceId(1),
                1,
                br#"{"password":"secret"}"#.to_vec(),
            ),
            Err(ContinuityError::InvalidResumePayload)
        );
        assert_eq!(
            reports.report(
                room,
                "dev.qos.browser",
                AppInstanceId(2),
                1,
                br#"{"url":"https://alice:secret@example.test"}"#.to_vec(),
            ),
            Err(ContinuityError::InvalidResumePayload)
        );
        assert_eq!(reports.len(), 0);
    }

    #[test]
    fn two_browser_instances_keep_distinct_urls() {
        let room = RoomId::new();
        let mut reports = ContinuationRegistry::default();
        reports
            .report(
                room,
                "dev.qos.browser",
                AppInstanceId(11),
                1,
                br#"{"url":"https://a.test"}"#.to_vec(),
            )
            .unwrap();
        reports
            .report(
                room,
                "dev.qos.browser",
                AppInstanceId(22),
                1,
                br#"{"url":"https://b.test"}"#.to_vec(),
            )
            .unwrap();
        reports.bind_tab(AppInstanceId(101), AppInstanceId(11));
        reports.bind_tab(AppInstanceId(202), AppInstanceId(22));
        let launch = LaunchIntent::app("dev.qos.browser").unwrap();
        assert_eq!(reports.len(), 2);
        assert_ne!(
            reports.resume_for_tab(room, AppInstanceId(101), &launch),
            reports.resume_for_tab(room, AppInstanceId(202), &launch),
        );
    }

    #[test]
    fn clear_removes_only_the_certified_instance_projection() {
        let room = RoomId::new();
        let mut reports = ContinuationRegistry::default();
        for (instance, url) in [
            (AppInstanceId(11), br#"{"url":"https://a.test"}"#.as_slice()),
            (AppInstanceId(22), br#"{"url":"https://b.test"}"#.as_slice()),
        ] {
            reports
                .report(room, "dev.qos.browser", instance, 1, url.to_vec())
                .unwrap();
        }

        assert!(reports.clear(room, "dev.qos.browser", AppInstanceId(11)));
        assert!(
            reports
                .resume_for(room, "dev.qos.browser", AppInstanceId(11))
                .is_none()
        );
        assert!(
            reports
                .resume_for(room, "dev.qos.browser", AppInstanceId(22))
                .is_some()
        );
        assert!(!reports.clear(room, "dev.qos.browser", AppInstanceId(11)));
    }

    #[test]
    fn remote_refresh_does_not_replace_a_live_tab_record() {
        let room = RoomId::new();
        let launch = LaunchIntent::app("dev.qos.browser").unwrap();
        let mut reports = ContinuationRegistry::default();
        reports.bind_tab(AppInstanceId(10), AppInstanceId(101));
        reports
            .report(
                room,
                "dev.qos.browser",
                AppInstanceId(101),
                1,
                br#"{"url":"https://local.test"}"#.to_vec(),
            )
            .unwrap();
        reports.store_remote_resume(
            room,
            AppInstanceId(10),
            "dev.qos.browser".into(),
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
        assert_eq!(resolved.resume.bytes(), br#"{"url":"https://local.test"}"#);
    }

    #[test]
    fn removing_a_closed_remote_tab_drops_its_staged_checkpoint() {
        let room = RoomId::new();
        let tab = AppInstanceId(10);
        let launch = LaunchIntent::app("dev.qos.browser").unwrap();
        let mut reports = ContinuationRegistry::default();
        reports.store_remote_resume(
            room,
            tab,
            "dev.qos.browser".into(),
            ResumeMetadata::new(
                1,
                ResumeSensitivity::Syncable,
                br#"{"url":"https://closed.test"}"#.to_vec(),
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
}
