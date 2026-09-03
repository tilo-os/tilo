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
        let report = AppContinuationReport {
            room,
            app_id: app_id.clone(),
            instance_id: instance_id.clone(),
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
                    br#"{\"url\":\"https://qos.dev\"}"#.to_vec()
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
                    br#"{\"url\":\"https://qos.dev\"}"#.to_vec()
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
    fn two_browser_instances_keep_distinct_urls() {
        let room = RoomId::new();
        let mut reports = ContinuationRegistry::default();
        reports
            .report(
                room,
                "dev.qos.browser",
                AppInstanceId(11),
                1,
                br#"{\"url\":\"https://a.test\"}"#.to_vec(),
            )
            .unwrap();
        reports
            .report(
                room,
                "dev.qos.browser",
                AppInstanceId(22),
                1,
                br#"{\"url\":\"https://b.test\"}"#.to_vec(),
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
}
