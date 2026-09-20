use crate::release::InternalRelease;
use ui::install_artifacts::{Phase as WorkerPhase, Status, Token, Client as Worker, PreferenceState, PreferenceDurability};
use crate::setup::model::{Capabilities, Experience};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::collections::VecDeque;

const MAX_PREFERENCE_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SelectionKey {
    schema: u32,
    release_id: String,
    architecture: String,
    manifest_sha256: String,
}

impl SelectionKey {
    fn from_release(release: &InternalRelease) -> Self {
        Self {
            schema: 1,
            release_id: release.release_id().into(),
            architecture: release.architecture().into(),
            manifest_sha256: release.manifest_sha256().into(),
        }
    }

    fn encode(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        serde_json::to_vec(self).map_err(|error| error.to_string())
    }

    fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_PREFERENCE_BYTES {
            return Err("Saved download selection exceeds its size limit.".into());
        }
        let key: Self = serde_json::from_slice(bytes)
            .map_err(|error| format!("Saved download selection is invalid: {error}"))?;
        key.validate()?;
        Ok(key)
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema != 1
            || self.release_id.is_empty()
            || self.release_id.len() > 128
            || self.architecture.is_empty()
            || self.architecture.len() > 32
            || self.manifest_sha256.len() != 64
            || !self
                .manifest_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("Saved download selection has an unsupported value.".into());
        }
        Ok(())
    }

    fn matches(&self, release: &InternalRelease) -> bool {
        self.release_id == release.release_id()
            && self.architecture == release.architecture()
            && self.manifest_sha256 == release.manifest_sha256()
    }
}

fn offered_selection(
    releases: Vec<InternalRelease>,
    host_architecture: &str,
) -> Result<SelectionKey, String> {
    let mut host = releases
        .into_iter()
        .filter(|release| release.architecture() == host_architecture);
    let release = host
        .next()
        .ok_or("No Full Tilo download is offered for this computer.")?;
    if host.next().is_some() {
        return Err(
            "More than one Full Tilo download is offered for this computer; selection policy is unresolved."
                .into(),
        );
    }
    Ok(SelectionKey::from_release(&release))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DownloadPresentation {
    pub(super) phase: Option<WorkerPhase>,
    pub(super) title: String,
    pub(super) details: String,
    pub(super) pending: bool,
}

impl Default for DownloadPresentation {
    fn default() -> Self {
        Self {
            phase: None,
            title: "Full Tilo download".into(),
            details: "Choose Download Tilo after reviewing this setup.".into(),
            pending: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingCommand {
    Pause,
    Cancel,
    Release,
    Discard,
}

pub(super) struct InternalDownloadFlow {
    capabilities: Capabilities,
    preference_loaded: bool,
    pending_key: Option<(Token, SelectionKey)>,
    preference_warning: bool,
    saved: Option<SelectionKey>,
    offered: Option<SelectionKey>,
    catalog_error: Option<String>,
    worker: Option<Worker>,
    token: Option<Token>,
    presentation: DownloadPresentation,
    pending: Option<PendingCommand>,
    revision: u64,
    #[cfg(test)]
    catalogs: VecDeque<Vec<InternalRelease>>,
    #[cfg(test)]
    host_architecture: String,
}

impl InternalDownloadFlow {
    #[cfg(not(test))]
    pub(super) fn new(capabilities: Capabilities) -> Result<Self, String> {
        if ui::install_artifacts::availability() == ui::install_artifacts::Availability::UnsupportedHost {
            return Err("Full Tilo download is unavailable on this computer. Installation has not started.".into());
        }
        let worker = Worker::open_legacy_imager().map_err(|error| error.to_string())?;
        let mut flow = Self {
            capabilities,
            preference_loaded: false,
            pending_key: None,
            preference_warning: false,
            saved: None,
            offered: None,
            catalog_error: None,
            worker: Some(worker),
            token: None,
            presentation: DownloadPresentation::default(),
            pending: None,
            revision: 0,
            #[cfg(test)]
            catalogs: VecDeque::new(),
            #[cfg(test)]
            host_architecture: ui::install_artifacts::catalog_architecture().into(),
        };
        flow.refresh_catalog();
        Ok(flow)
    }

    #[cfg(test)]
    pub(super) fn unavailable_fixture(capabilities: Capabilities) -> Self {
        let mut flow = Self {
            capabilities,
            preference_loaded: true,
            pending_key: None,
            preference_warning: false,
            saved: None,
            offered: None,
            catalog_error: None,
            worker: None,
            token: None,
            presentation: DownloadPresentation::default(),
            pending: None,
            revision: 0,
            catalogs: VecDeque::from([Vec::new()]),
            host_architecture: ui::install_artifacts::catalog_architecture().into(),
        };
        flow.refresh_catalog();
        flow
    }

    #[cfg(test)]
    fn fixture(
        capabilities: Capabilities,
        worker: Worker,
        catalogs: Vec<Vec<InternalRelease>>,
        host_architecture: &str,
    ) -> Self {
        let mut flow = Self {
            capabilities,
            preference_loaded: false,
            pending_key: None,
            preference_warning: false,
            saved: None,
            offered: None,
            catalog_error: None,
            worker: Some(worker),
            token: None,
            presentation: DownloadPresentation::default(),
            pending: None,
            revision: 0,
            catalogs: catalogs.into(),
            host_architecture: host_architecture.into(),
        };
        flow.refresh_catalog();
        flow
    }

    #[cfg(test)]
    pub(in crate::setup) fn fixture_at(
        capabilities: Capabilities,
        worker: Worker,
        catalogs: Vec<Vec<InternalRelease>>,
        host_architecture: &str,
    ) -> Self {
        Self::fixture(
            capabilities,
            worker,
            catalogs,
            host_architecture,
        )
    }

    fn host_architecture(&self) -> &str {
        #[cfg(test)]
        {
            &self.host_architecture
        }
        #[cfg(not(test))]
        {
            ui::install_artifacts::catalog_architecture()
        }
    }

    fn current_catalog(&mut self) -> Result<Vec<InternalRelease>, String> {
        #[cfg(test)]
        if let Some(catalog) = self.catalogs.pop_front() {
            return Ok(catalog);
        }
        crate::release::bundled_internal_releases()
    }

    fn refresh_catalog(&mut self) {
        let architecture = self.host_architecture().to_string();
        match self
            .current_catalog()
            .and_then(|releases| offered_selection(releases, &architecture))
        {
            Ok(key) => {
                self.offered = Some(key);
                self.catalog_error = None;
            }
            Err(error) => {
                self.offered = None;
                self.catalog_error = Some(error);
            }
        }
    }

    fn resolve(&mut self, key: &SelectionKey) -> Result<InternalRelease, String> {
        if !self.capabilities.supports(Experience::FullOs) {
            return Err("Full Tilo download is not supported on this computer.".into());
        }
        let architecture = self.host_architecture().to_string();
        let mut releases = self
            .current_catalog()?
            .into_iter()
            .filter(|release| release.architecture() == architecture);
        let release = releases
            .next()
            .ok_or("The selected Full Tilo download is no longer offered.")?;
        if releases.next().is_some() {
            return Err(
                "More than one Full Tilo download is offered for this computer; selection policy is unresolved."
                    .into(),
            );
        }
        if !key.matches(&release) {
            return Err(
                "The selected Full Tilo download changed. Review the current download before continuing."
                    .into(),
            );
        }
        Ok(release)
    }

    fn ensure_worker(&mut self) -> Result<&Worker, String> {
        if self.worker.is_none() {
            self.worker = Some(Worker::open_legacy_imager().map_err(|error| error.to_string())?);
        }
        self.worker
            .as_ref()
            .ok_or("Download worker is unavailable.".into())
    }

    pub(super) fn offered_selection(&self) -> Option<SelectionKey> {
        self.offered.clone()
    }

    pub(super) fn catalog_error(&self) -> Option<&str> {
        self.catalog_error.as_deref()
    }

    pub(super) fn can_resume(&self) -> bool {
        self.preference_loaded
            && self.capabilities.supports(Experience::FullOs)
            && self
                .saved
                .as_ref()
                .zip(self.offered.as_ref())
                .is_some_and(|(saved, offered)| saved == offered)
            && self.pending.is_none()
            && (self.token.is_none()
                || matches!(
                    self.presentation.phase,
                    Some(
                        WorkerPhase::Paused
                            | WorkerPhase::Cancelled
                            | WorkerPhase::Failed
                            | WorkerPhase::Released
                            | WorkerPhase::Discarded
                    )
                ))
    }

    pub(super) fn can_start(&self) -> bool {
        self.preference_loaded
            && self.capabilities.supports(Experience::FullOs)
            && self.offered.is_some()
            && self.saved.as_ref() != self.offered.as_ref()
            && (self.token.is_none()
                || (self.pending_key.as_ref().is_some_and(|(token, _)| {
                    self.token.as_ref() == Some(token)
                }) && matches!(
                    self.presentation.phase,
                    Some(WorkerPhase::Failed | WorkerPhase::Cancelled | WorkerPhase::Paused)
                )))
            && self.pending.is_none()
    }

    pub(super) fn can_pause(&self) -> bool {
        self.pending.is_none()
            && matches!(
                self.presentation.phase,
                Some(
                    WorkerPhase::SavingPreference
                        | WorkerPhase::Authenticating
                        | WorkerPhase::Acquiring
                        | WorkerPhase::Preparing
                        | WorkerPhase::Downloading
                )
            )
    }

    pub(super) fn can_cancel(&self) -> bool {
        self.can_pause()
    }

    pub(super) fn can_release(&self) -> bool {
        self.pending.is_none() && self.presentation.phase == Some(WorkerPhase::Ready)
    }

    pub(super) fn can_discard(&self) -> bool {
        self.pending.is_none()
            && matches!(
                self.presentation.phase,
                Some(
                    WorkerPhase::Paused
                        | WorkerPhase::Cancelled
                        | WorkerPhase::Failed
                        | WorkerPhase::Released
                )
            )
    }

    pub(super) fn start_selected(&mut self, key: SelectionKey) -> Result<(), String> {
        if !self.can_start() {
            return Err("A Full Tilo download cannot start in its current state.".into());
        }
        let release = self.resolve(&key)?;
        let bytes = key.encode()?;
        let token = self.ensure_worker()?.start(release, bytes).map_err(|error| error.to_string())?;
        self.pending_key = Some((token.clone(), key));
        self.token = Some(token);
        self.pending = None;
        self.sync_current_status();
        Ok(())
    }

    pub(super) fn resume(&mut self) -> Result<(), String> {
        if self.pending.is_some() || !self.can_resume() {
            return Err("This saved download cannot be resumed yet.".into());
        }
        let key = self
            .saved
            .clone()
            .ok_or("There is no saved download selection to resume.")?;
        let release = self.resolve(&key)?;
        let token = self.ensure_worker()?.resume(release).map_err(|error| error.to_string())?;
        self.token = Some(token);
        self.sync_current_status();
        Ok(())
    }

    pub(super) fn pause(&mut self) -> Result<(), String> {
        self.stop(false)
    }

    pub(super) fn cancel(&mut self) -> Result<(), String> {
        self.stop(true)
    }

    fn stop(&mut self, cancel: bool) -> Result<(), String> {
        if self.pending.is_some()
            || !matches!(
                self.presentation.phase,
                Some(
                    WorkerPhase::SavingPreference
                        | WorkerPhase::Authenticating
                        | WorkerPhase::Acquiring
                        | WorkerPhase::Preparing
                        | WorkerPhase::Downloading
                )
            )
        {
            return Err("The download is not currently active.".into());
        }
        let (worker, token) = self
            .worker
            .as_ref()
            .zip(self.token.as_ref())
            .ok_or("The active download is unavailable.")?;
        if cancel {
            worker.cancel(token).map_err(|error| error.to_string())?;
            self.set_pending(PendingCommand::Cancel, "Cancelling download…");
        } else {
            worker.pause(token).map_err(|error| error.to_string())?;
            self.set_pending(PendingCommand::Pause, "Pausing download…");
        }
        Ok(())
    }

    pub(super) fn discard(&mut self) -> Result<(), String> {
        if self.pending.is_some()
            || !matches!(
                self.presentation.phase,
                Some(
                    WorkerPhase::Paused
                        | WorkerPhase::Cancelled
                        | WorkerPhase::Failed
                        | WorkerPhase::Released
                )
            )
        {
            return Err("Unfinished download data is not available to discard.".into());
        }
        let key = self
            .saved
            .clone()
            .ok_or("There is no saved download selection to discard.")?;
        let release = self.resolve(&key)?;
        let target = self
            .token
            .clone()
            .ok_or("The prior download operation is unavailable.")?;
        let token = self
            .worker
            .as_ref()
            .ok_or("The download worker is unavailable.")?
            .discard(&target, release).map_err(|error| error.to_string())?;
        self.token = Some(token);
        self.set_pending(
            PendingCommand::Discard,
            "Discarding unfinished download data…",
        );
        Ok(())
    }

    pub(super) fn release(&mut self) -> Result<(), String> {
        if self.pending.is_some() || self.presentation.phase != Some(WorkerPhase::Ready) {
            return Err("The verified download is not available to keep for later.".into());
        }
        let (worker, token) = self
            .worker
            .as_ref()
            .zip(self.token.as_ref())
            .ok_or("The verified download is no longer held.")?;
        worker.release(token).map_err(|error| error.to_string())?;
        self.set_pending(PendingCommand::Release, "Keeping download for later…");
        Ok(())
    }

    fn set_pending(&mut self, pending: PendingCommand, title: &str) {
        self.pending = Some(pending);
        self.presentation.pending = true;
        self.presentation.title = title.into();
        self.revision = self.revision.wrapping_add(1);
    }

    fn sync_preference(&mut self) -> bool {
        if self.preference_loaded { return false; }
        let Some(worker) = &self.worker else { return false; };
        match worker.preference() {
            PreferenceState::Loading => return false,
            PreferenceState::Loaded(bytes) => {
                self.saved = bytes.as_deref().and_then(|bytes| SelectionKey::decode(bytes).ok());
                self.presentation = DownloadPresentation::default();
            }
            PreferenceState::Failed(failure) => {
                self.presentation.details = format!("Saved download selection could not be read: {}", failure.message);
            }
        }
        self.preference_loaded = true;
        self.revision = self.revision.wrapping_add(1);
        true
    }
    fn sync_current_status(&mut self) -> bool {
        let Some(status) = self.worker.as_ref().and_then(Worker::status) else {
            return false;
        };
        self.accept_status(status)
    }

    fn accept_status(&mut self, status: Status) -> bool {
        if self.token.as_ref() != Some(&status.token) {
            return false;
        }
        let mut acknowledged = false;
        if let Some(commit) = status.preference_commit {
            if self.pending_key.as_ref().is_some_and(|(token, _)| *token == status.token) {
                self.saved = self.pending_key.take().map(|(_, key)| key);
                acknowledged = true;
            }
            self.preference_warning |= commit.durability == PreferenceDurability::NamespaceUnproven;
        }
        if let Some(pending) = self.pending {
            let expected = match pending {
                PendingCommand::Pause => status.phase == WorkerPhase::Paused,
                PendingCommand::Cancel => status.phase == WorkerPhase::Cancelled,
                PendingCommand::Release => status.phase == WorkerPhase::Released,
                PendingCommand::Discard => status.phase == WorkerPhase::Discarded,
            };
            let failed = status.phase == WorkerPhase::Failed;
            if !expected && !failed {
                if acknowledged { self.revision = self.revision.wrapping_add(1); }
                self.append_preference_warning();
                return acknowledged;
            }
            self.pending = None;
        }
        let mut next = presentation_from_status(&status);
        if self.preference_warning { append_preference_warning(&mut next); }
        if next == self.presentation {
            if acknowledged { self.revision = self.revision.wrapping_add(1); }
            return acknowledged;
        }
        self.presentation = next;
        self.revision = self.revision.wrapping_add(1);
        true
    }

    fn append_preference_warning(&mut self) {
        if self.preference_warning { append_preference_warning(&mut self.presentation); }
    }
    pub(super) fn poll(&mut self) -> bool {
        let loaded = self.sync_preference();
        self.sync_current_status() || loaded
    }
    #[cfg(test)]
    pub(in crate::setup) fn wait_preference(&mut self) {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !self.preference_loaded {
            self.poll();
            assert!(std::time::Instant::now() < until, "preference load did not settle");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    pub(super) fn presentation(&self) -> &DownloadPresentation {
        &self.presentation
    }

    pub(super) fn revision(&self) -> u64 {
        self.revision
    }

    pub(super) fn has_pending_operation(&self) -> bool {
        !self.preference_loaded
            || self.pending.is_some()
            || matches!(
                self.presentation.phase,
                Some(
                    WorkerPhase::SavingPreference
                        | WorkerPhase::Authenticating
                        | WorkerPhase::Acquiring
                        | WorkerPhase::Preparing
                        | WorkerPhase::Downloading
                        | WorkerPhase::Consuming
                )
            )
    }
}

fn append_preference_warning(presentation: &mut DownloadPresentation) {
    const WARNING: &str = " The saved selection may not survive power loss on this computer.";
    if !presentation.details.contains(WARNING) { presentation.details.push_str(WARNING); }
}
fn presentation_from_status(status: &Status) -> DownloadPresentation {
    presentation_from_facts(
        status.phase,
        status.committed,
        status.progress,
        status.finals,
        status.residue,
        status.uncertain,
        status.error.as_deref(),
    )
}

fn presentation_from_facts(
    phase: WorkerPhase,
    committed: [u64; 2],
    progress: Option<ui::install_artifacts::Progress>,
    finals: [bool; 2],
    residue: bool,
    uncertain: bool,
    error: Option<&str>,
) -> DownloadPresentation {
    let (title, summary) = match phase {
        WorkerPhase::SavingPreference => (
            "Saving download selection",
            "Saving the selected download before contacting the release server.",
        ),
        WorkerPhase::Authenticating => (
            "Downloading Tilo",
            "Checking the selected Tilo release against this signed app catalog.",
        ),
        WorkerPhase::Acquiring | WorkerPhase::Preparing => (
            "Downloading Tilo",
            "Checking retained download data before continuing.",
        ),
        WorkerPhase::Downloading => (
            "Downloading Tilo",
            "Downloading verified Tilo installation data.",
        ),
        WorkerPhase::Ready => (
            "Download ready",
            "Download verified. Installation has not started.",
        ),
        WorkerPhase::Paused => (
            "Download paused",
            "The download is paused. Resume will recheck any retained download data before continuing.",
        ),
        WorkerPhase::Cancelled => (
            "Download cancelled",
            "Unfinished download data or completed components may remain. Discarding unfinished data is a separate action.",
        ),
        WorkerPhase::Failed => ("Download failed", "The download did not finish."),
        WorkerPhase::Released => (
            "Kept for later",
            "Verified download data was retained. Installation has not started.",
        ),
        WorkerPhase::Discarded => (
            "Unfinished download data discarded",
            "Owned unfinished data was discarded for this selection. Completed components or other data may remain.",
        ),
        WorkerPhase::Consuming | WorkerPhase::Consumed => (
            "Download unavailable",
            "Protected staging is not available in this setup flow.",
        ),
    };
    let mut details = summary.to_string();
    if let Some(progress) = progress {
        details.push_str(&format!(
            " Component {} of 2: {} bytes received in this attempt; {} bytes verified.",
            progress.component.saturating_add(1).min(2),
            progress.received,
            progress.verified
        ));
    }
    if committed != [0, 0] {
        details.push_str(&format!(
            " Retained download data: component 1 has {} bytes; component 2 has {} bytes.",
            committed[0], committed[1]
        ));
    }
    let final_count = finals.iter().filter(|retained| **retained).count();
    if final_count > 0 {
        let noun = if final_count == 1 {
            "component is"
        } else {
            "components are"
        };
        details.push_str(&format!(
            " {final_count} completed and verified {noun} retained."
        ));
    }
    if residue {
        details
            .push_str(" Some download data remains and will be checked before another operation.");
    }
    if uncertain {
        details.push_str(
            " The saved download state may have changed; it will be checked again before another operation.",
        );
    }
    if let Some(error) = error {
        details.push_str(" Error: ");
        details.push_str(error);
    }
    DownloadPresentation {
        phase: Some(phase),
        title: title.into(),
        details,
        pending: false,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::release::InternalRelease;
    use tilo_install_artifacts::test_support::{manifest_bytes, manifest_value};
    use ui::install_artifacts::Phase;
    use tilo_install_artifacts::test_support::{Server, response};
    use crate::setup::model::{Capabilities, HostOs};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    use std::time::{Duration, Instant};

    fn release() -> InternalRelease {
        let bytes = manifest_bytes(&manifest_value());
        Server::new(Vec::new()).selection(&bytes)
    }

    fn supported() -> Capabilities {
        Capabilities {
            host: HostOs::Linux,
            desktop_session: false,
            internal_dual_boot: true,
        }
    }

    fn wait_for(flow: &mut InternalDownloadFlow, phase: Phase) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            flow.poll();
            if flow.presentation().phase == Some(phase) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {phase:?}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    // Catches accepting a preference that can smuggle authority beyond the exact bounded key.
    #[test]
    fn saved_selection_is_bounded_strict_and_versioned() {
        let key = SelectionKey::from_release(&release());
        let bytes = key.encode().unwrap();
        assert_eq!(SelectionKey::decode(&bytes).unwrap(), key);

        let mut extra: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        extra["manifest_url"] = serde_json::json!("https://evil.test/manifest.json");
        assert!(SelectionKey::decode(&serde_json::to_vec(&extra).unwrap()).is_err());
        assert!(SelectionKey::decode(&vec![b' '; MAX_PREFERENCE_BYTES + 1]).is_err());

        let mut old: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        old["schema"] = serde_json::json!(0);
        assert!(SelectionKey::decode(&serde_json::to_vec(&old).unwrap()).is_err());
    }

    // Catches selecting array order when the embedded catalog has zero or several host entries.
    #[test]
    fn offered_selection_requires_exactly_one_current_host_entry() {
        assert!(offered_selection(Vec::new(), "x86_64").is_err());
        assert!(offered_selection(vec![release(), release()], "x86_64").is_err());

        let key = offered_selection(vec![release()], "x86_64").unwrap();
        assert_eq!(key.release_id, "fixture-only");
        assert_eq!(key.architecture, "x86_64");
    }

    // Catches persisting authority-bearing fields in the app's advisory representation.
    #[test]
    fn preference_round_trip_contains_only_the_advisory_key() {
        let key = SelectionKey::from_release(&release());
        let bytes = key.encode().unwrap();
        assert_eq!(SelectionKey::decode(&bytes).unwrap(),key);
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value.as_object().unwrap().len(),4);
        let mut replacement = key; replacement.release_id = "fixture-replacement".into();
        assert_eq!(SelectionKey::decode(&replacement.encode().unwrap()).unwrap(),replacement);
    }

    // Catches malformed saved JSON becoming a resumable key.
    #[test]
    fn invalid_preference_is_ignored_and_write_failure_is_reported() {
        let bytes = manifest_bytes(&manifest_value()); let server = Server::new(vec![]);
        let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("selection.json");
        std::fs::write(&path,br#"{"schema":1,"release_id":"x"}"#).unwrap();
        let (worker, observed) = server.client_with_preference_fault(&dir.path().join("cache"),&path,
            tilo_install_artifacts::test_support::PreferenceFault::BeforePublish);
        let mut flow = InternalDownloadFlow::fixture(supported(),worker,
            vec![vec![server.selection(&bytes)],vec![server.selection(&bytes)]],"x86_64");
        flow.wait_preference(); assert!(flow.saved.is_none()); assert!(!flow.can_resume());
        flow.start_selected(flow.offered_selection().unwrap()).unwrap();
        observed.recv_timeout(Duration::from_secs(5)).unwrap(); wait_for(&mut flow,Phase::Failed);
        assert!(flow.saved.is_none()); assert!(flow.presentation.details.contains("Error:"));
        assert!(server.requests().is_empty());
    }

    // Catches detaching from the real worker, claiming installation, or polling held Ready forever.
    #[test]
    fn real_worker_reaches_held_ready_and_keeps_verified_data_for_later() {
        let bytes = manifest_bytes(&manifest_value());
        let server = Server::new(vec![
            response(200, "", &bytes),
            response(200, "", &vec![b'E'; 4096]),
            response(200, "", &vec![b'R'; 8192]),
        ]);
        let root = tempfile::tempdir().unwrap();
        let preference_root = tempfile::tempdir().unwrap();
        let key = SelectionKey::from_release(&server.selection(&bytes));
        let worker = server.client(root.path(), &preference_root.path().join("selection.json"));
        let mut flow = InternalDownloadFlow::fixture(
            supported(),
            worker,
            vec![vec![server.selection(&bytes)], vec![server.selection(&bytes)]],
            "x86_64",
        );
        flow.wait_preference();

        assert_eq!(flow.offered_selection(), Some(key.clone()));
        flow.start_selected(key).unwrap();
        assert!(flow.has_pending_operation());
        wait_for(&mut flow, Phase::Ready);
        assert_eq!(flow.presentation().title, "Download ready");
        assert!(
            flow.presentation()
                .details
                .contains("Download verified. Installation has not started.")
        );
        assert!(!flow.has_pending_operation());

        flow.release().unwrap();
        assert!(flow.has_pending_operation());
        wait_for(&mut flow, Phase::Released);
        assert_eq!(flow.presentation().title, "Kept for later");
        assert!(!flow.has_pending_operation());
        assert_eq!(server.consumptions(), 0, "No staging consumer may run");
    }

    // Catches a duplicate Start replacing an active token or repeating selection side effects.
    #[test]
    fn duplicate_start_during_active_download_preserves_token_preference_catalog_and_requests() {
        use std::os::unix::fs::MetadataExt;

        let bytes = manifest_bytes(&manifest_value());
        let (blocked_tx, blocked_rx) = mpsc::sync_channel(0);
        let (continue_tx, continue_rx) = mpsc::sync_channel(0);
        let mut request = 0;
        let server = Server::on_request(
            vec![
                response(200, "", &bytes),
                response(200, "", &vec![b'E'; 4096]),
                response(200, "", &vec![b'R'; 8192]),
                response(200, "", &bytes),
                response(200, "", &vec![b'E'; 4096]),
                response(200, "", &vec![b'R'; 8192]),
            ],
            move || {
                request += 1;
                if request == 2 {
                    blocked_tx.send(()).unwrap();
                    continue_rx.recv().unwrap();
                }
            },
        );
        let root = tempfile::tempdir().unwrap();
        let preference_root = tempfile::tempdir().unwrap();
        let preference_path = preference_root.path().join("selection.json");
        let key = SelectionKey::from_release(&server.selection(&bytes));
        let worker = server.client(root.path(), &preference_root.path().join("selection.json"));
        let mut flow = InternalDownloadFlow::fixture(
            supported(),
            worker,
            vec![
                vec![server.selection(&bytes)],
                vec![server.selection(&bytes)],
                vec![server.selection(&bytes)],
            ],
            "x86_64",
        );
        flow.wait_preference();
        flow.start_selected(key.clone()).unwrap();
        blocked_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let token = flow.token.clone().unwrap();
        let preference_inode = std::fs::metadata(&preference_path).unwrap().ino();
        let catalogs = flow.catalogs.len();
        let requests = server.requests().len();

        let duplicate = flow.start_selected(key);
        let token_after = flow.token.clone().unwrap();
        let preference_inode_after = std::fs::metadata(&preference_path).unwrap().ino();
        let catalogs_after = flow.catalogs.len();
        let requests_after = server.requests().len();
        continue_tx.send(()).unwrap();

        assert_eq!(
            duplicate.unwrap_err(),
            "A Full Tilo download cannot start in its current state."
        );
        assert_eq!(token_after, token);
        assert_eq!(preference_inode_after, preference_inode);
        assert_eq!(catalogs_after, catalogs);
        assert_eq!(requests_after, requests);
        wait_for(&mut flow, Phase::Ready);
        assert_eq!(server.requests().len(), 3);
        assert_eq!(server.consumptions(), 0, "No staging consumer may run");
    }

    // Catches a stale Start replacing the token and leases held at verified Ready.
    #[test]
    fn stale_start_at_held_ready_preserves_token_and_request_count() {
        use std::os::unix::fs::MetadataExt;

        let bytes = manifest_bytes(&manifest_value());
        let server = Server::new(vec![
            response(200, "", &bytes),
            response(200, "", &vec![b'E'; 4096]),
            response(200, "", &vec![b'R'; 8192]),
            response(200, "", &bytes),
            response(200, "", &vec![b'E'; 4096]),
            response(200, "", &vec![b'R'; 8192]),
        ]);
        let root = tempfile::tempdir().unwrap();
        let preference_root = tempfile::tempdir().unwrap();
        let preference_path = preference_root.path().join("selection.json");
        let key = SelectionKey::from_release(&server.selection(&bytes));
        let worker = server.client(root.path(), &preference_root.path().join("selection.json"));
        let mut flow = InternalDownloadFlow::fixture(
            supported(),
            worker,
            vec![
                vec![server.selection(&bytes)],
                vec![server.selection(&bytes)],
                vec![server.selection(&bytes)],
            ],
            "x86_64",
        );
        flow.wait_preference();
        flow.start_selected(key.clone()).unwrap();
        wait_for(&mut flow, Phase::Ready);
        let token = flow.token.clone().unwrap();
        let preference_inode = std::fs::metadata(&preference_path).unwrap().ino();
        let catalogs = flow.catalogs.len();
        let requests = server.requests().len();

        assert_eq!(
            flow.start_selected(key).unwrap_err(),
            "A Full Tilo download cannot start in its current state."
        );
        assert_eq!(flow.token.as_ref(), Some(&token));
        assert_eq!(
            std::fs::metadata(&preference_path).unwrap().ino(),
            preference_inode
        );
        assert_eq!(flow.catalogs.len(), catalogs);
        assert_eq!(server.requests().len(), requests);

        flow.release().unwrap();
        wait_for(&mut flow, Phase::Released);
        assert_eq!(server.requests().len(), 3);
        assert_eq!(server.consumptions(), 0, "No staging consumer may run");
    }

    // Catches retaining a token/handle across restart or starting network work implicitly.
    #[test]
    fn pause_close_and_reconstruct_resume_with_a_new_token() {
        let bytes = manifest_bytes(&manifest_value());
        let (blocked_tx, blocked_rx) = mpsc::sync_channel(0);
        let (continue_tx, continue_rx) = mpsc::sync_channel(0);
        let mut request = 0;
        let server = Server::on_request(
            vec![
                response(200, "", &bytes),
                response(200, "", &vec![b'E'; 4096]),
            ],
            move || {
                request += 1;
                if request == 2 {
                    blocked_tx.send(()).unwrap();
                    continue_rx.recv().unwrap();
                }
            },
        );
        let root = tempfile::tempdir().unwrap();
        let preference_root = tempfile::tempdir().unwrap();
        let key = SelectionKey::from_release(&server.selection(&bytes));
        let worker = server.client(root.path(), &preference_root.path().join("selection.json"));
        let mut flow = InternalDownloadFlow::fixture(
            supported(),
            worker,
            vec![vec![server.selection(&bytes)], vec![server.selection(&bytes)]],
            "x86_64",
        );
        flow.wait_preference();
        flow.start_selected(key).unwrap();
        blocked_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let first_token = flow.token.clone().unwrap();
        flow.pause().unwrap();
        assert!(flow.presentation().pending);
        continue_tx.send(()).unwrap();
        wait_for(&mut flow, Phase::Paused);
        drop(flow);
        assert_eq!(server.consumptions(), 0, "No staging consumer may run");
        drop(server);

        let resumed_server = Server::new(vec![
            response(200, "", &bytes),
            response(200, "", &vec![b'E'; 4096]),
            response(200, "", &vec![b'R'; 8192]),
        ]);
        let resumed_worker =
            resumed_server.client(root.path(), &preference_root.path().join("selection.json"));
        let mut resumed = InternalDownloadFlow::fixture(
            supported(),
            resumed_worker,
            vec![
                vec![resumed_server.selection(&bytes)],
                vec![resumed_server.selection(&bytes)],
            ],
            "x86_64",
        );
        resumed.wait_preference();
        assert!(resumed.can_resume());
        assert!(!resumed.can_start());
        assert!(resumed_server.requests().is_empty());
        resumed.resume().unwrap();
        let second_token = resumed.token.clone().unwrap();
        assert_ne!(first_token, second_token);
        wait_for(&mut resumed, Phase::Ready);
        assert_eq!(resumed_server.consumptions(), 0, "No staging consumer may run");
    }

    // Catches a queued Ready/progress snapshot defeating cancel or discard reusing its token.
    #[test]
    fn accepted_cancel_stays_pending_until_cancelled_then_discard_uses_new_token() {
        let bytes = manifest_bytes(&manifest_value());
        let (blocked_tx, blocked_rx) = mpsc::sync_channel(0);
        let (continue_tx, continue_rx) = mpsc::sync_channel(0);
        let mut request = 0;
        let server = Server::on_request(
            vec![
                response(200, "", &bytes),
                response(200, "", &vec![b'E'; 4096]),
                response(200, "", &bytes),
            ],
            move || {
                request += 1;
                if request == 2 {
                    blocked_tx.send(()).unwrap();
                    continue_rx.recv().unwrap();
                }
            },
        );
        let root = tempfile::tempdir().unwrap();
        let preference_root = tempfile::tempdir().unwrap();
        let key = SelectionKey::from_release(&server.selection(&bytes));
        let worker = server.client(root.path(), &preference_root.path().join("selection.json"));
        let mut flow = InternalDownloadFlow::fixture(
            supported(),
            worker,
            vec![
                vec![server.selection(&bytes)],
                vec![server.selection(&bytes)],
                vec![server.selection(&bytes)],
            ],
            "x86_64",
        );
        flow.wait_preference();
        flow.start_selected(key).unwrap();
        blocked_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let transfer_token = flow.token.clone().unwrap();
        flow.cancel().unwrap();
        flow.poll();
        assert!(flow.presentation().pending);
        assert_eq!(flow.presentation().title, "Cancelling download…");
        continue_tx.send(()).unwrap();
        wait_for(&mut flow, Phase::Cancelled);
        let cancelled = flow.worker.as_ref().unwrap().status().unwrap();

        flow.discard().unwrap();
        let cleanup_token = flow.token.clone().unwrap();
        assert_ne!(transfer_token, cleanup_token);
        assert!(!flow.accept_status(cancelled));
        assert!(flow.presentation().pending);
        wait_for(&mut flow, Phase::Discarded);
        assert!(flow.presentation().details.contains("may remain"));
        assert_eq!(server.consumptions(), 0, "No staging consumer may run");
    }

    // Catches leaving held readers/root leases behind when the setup view closes at Ready.
    #[test]
    fn dropping_held_ready_releases_leases_and_retains_verified_components() {
        let bytes = manifest_bytes(&manifest_value());
        let server = Server::new(vec![
            response(200, "", &bytes),
            response(200, "", &vec![b'E'; 4096]),
            response(200, "", &vec![b'R'; 8192]),
        ]);
        let root = tempfile::tempdir().unwrap();
        let preference_root = tempfile::tempdir().unwrap();
        let key = SelectionKey::from_release(&server.selection(&bytes));
        let worker = server.client(root.path(), &preference_root.path().join("selection.json"));
        let mut flow = InternalDownloadFlow::fixture(
            supported(),
            worker,
            vec![vec![server.selection(&bytes)], vec![server.selection(&bytes)]],
            "x86_64",
        );
        flow.wait_preference();
        flow.start_selected(key).unwrap();
        wait_for(&mut flow, Phase::Ready);
        let first_token = flow.token.clone().unwrap();
        drop(flow);
        assert_eq!(server.consumptions(), 0, "No staging consumer may run");
        drop(server);

        let reopened_server = Server::new(vec![response(200, "", &bytes)]);
        let reopened_worker =
            reopened_server.client(root.path(), &preference_root.path().join("selection.json"));
        let mut reopened = InternalDownloadFlow::fixture(
            supported(),
            reopened_worker,
            vec![
                vec![reopened_server.selection(&bytes)],
                vec![reopened_server.selection(&bytes)],
            ],
            "x86_64",
        );
        reopened.wait_preference();
        assert!(reopened.can_resume());
        reopened.resume().unwrap();
        assert_ne!(first_token, reopened.token.clone().unwrap());
        wait_for(&mut reopened, Phase::Ready);
        assert_eq!(reopened_server.requests().len(), 1);
        assert_eq!(reopened_server.consumptions(), 0, "No staging consumer may run");
    }

    // Catches dropping the controller while leaving an in-flight worker detached.
    #[test]
    fn dropping_during_authentication_waits_for_cooperative_worker_shutdown() {
        let bytes = manifest_bytes(&manifest_value());
        let (entered_tx, entered_rx) = mpsc::sync_channel(0);
        let (continue_tx, continue_rx) = mpsc::sync_channel(0);
        let request_returned = Arc::new(AtomicBool::new(false));
        let returned = Arc::clone(&request_returned);
        let server = Server::on_request(vec![response(200, "", &bytes)], move || {
            entered_tx.send(()).unwrap();
            continue_rx.recv().unwrap();
            returned.store(true, Ordering::Release);
        });
        let root = tempfile::tempdir().unwrap();
        let preference_root = tempfile::tempdir().unwrap();
        let key = SelectionKey::from_release(&server.selection(&bytes));
        let worker = server.client(root.path(), &preference_root.path().join("selection.json"));
        let mut flow = InternalDownloadFlow::fixture(
            supported(),
            worker,
            vec![vec![server.selection(&bytes)], vec![server.selection(&bytes)]],
            "x86_64",
        );
        flow.wait_preference();
        flow.start_selected(key).unwrap();
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let unblock = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(25));
            continue_tx.send(()).unwrap();
        });
        drop(flow);
        unblock.join().unwrap();
        assert!(request_returned.load(Ordering::Acquire));
        assert_eq!(server.consumptions(), 0, "No staging consumer may run");
    }

    // Catches catalog drift, ambiguity, or capability fixtures issuing a worker command.
    #[test]
    fn changed_ambiguous_or_unsupported_selection_never_starts_the_worker() {
        let bytes = manifest_bytes(&manifest_value());
        let mut changed = bytes.clone();
        changed.push(b' ');
        let server = Server::new(Vec::new());
        let root = tempfile::tempdir().unwrap();
        let preference_root = tempfile::tempdir().unwrap();
        let key = SelectionKey::from_release(&server.selection(&bytes));
        let worker = server.client(root.path(), &preference_root.path().join("selection.json"));
        let mut changed_flow = InternalDownloadFlow::fixture(
            supported(),
            worker,
            vec![vec![server.selection(&bytes)], vec![server.selection(&changed)]],
            "x86_64",
        );
        changed_flow.wait_preference();
        assert!(changed_flow.start_selected(key.clone()).is_err());
        assert!(changed_flow.token.is_none());
        assert!(server.requests().is_empty());

        let worker = server.client(root.path(), &preference_root.path().join("selection.json"));
        let mut ambiguous = InternalDownloadFlow::fixture(
            supported(),
            worker,
            vec![
                vec![server.selection(&bytes)],
                vec![server.selection(&bytes), server.selection(&bytes)],
            ],
            "x86_64",
        );
        ambiguous.wait_preference();
        assert!(ambiguous.start_selected(key.clone()).is_err());
        assert!(ambiguous.token.is_none());

        let worker = server.client(root.path(), &preference_root.path().join("selection.json"));
        let mut unsupported_capabilities = supported();
        unsupported_capabilities.internal_dual_boot = false;
        let mut unsupported = InternalDownloadFlow::fixture(
            unsupported_capabilities,
            worker,
            vec![vec![server.selection(&bytes)], vec![server.selection(&bytes)]],
            "x86_64",
        );
        unsupported.wait_preference();
        assert!(unsupported.start_selected(key).is_err());
        assert!(unsupported.token.is_none());
        assert!(server.requests().is_empty());
        assert_eq!(server.consumptions(), 0, "No staging consumer may run");
    }

    // Catches normalizing away the coordinator's R1/R2 uncertainty and boundary errors.
    #[test]
    fn terminal_wording_retains_bytes_components_residue_uncertainty_and_original_error() {
        for phase in [Phase::Failed, Phase::Cancelled] {
            let presentation = presentation_from_facts(
                phase,
                [17, 29],
                None,
                [true, false],
                true,
                true,
                Some("journal normalization failed; held-reader boundary: readback changed"),
            );
            assert!(presentation.details.contains("17 bytes"));
            assert!(presentation.details.contains("29 bytes"));
            assert!(
                presentation
                    .details
                    .contains("1 completed and verified component")
            );
            assert!(presentation.details.contains("Some download data remains"));
            assert!(presentation.details.contains("may have changed"));
            assert!(
                presentation
                    .details
                    .contains("journal normalization failed")
            );
            assert!(
                presentation
                    .details
                    .contains("held-reader boundary: readback changed")
            );
            assert!(!presentation.details.contains("acknowledged prefix"));
            assert!(!presentation.details.contains("private cache"));
            assert_ne!(presentation.title, "Download ready");
        }
    }

    // Catches residue=false being upgraded into a claim that all retained data is absent.
    #[test]
    fn false_residue_makes_no_global_absence_claim() {
        let presentation = presentation_from_facts(
            Phase::Discarded,
            [0, 0],
            None,
            [false, false],
            false,
            false,
            None,
        );
        assert!(!presentation.details.contains("no download data"));
        assert!(!presentation.details.contains("cache cleared"));
        assert!(presentation.details.contains("may remain"));
    }

    // Catches a pause with no established retained facts claiming data or completed components.
    #[test]
    fn paused_zero_fact_presentation_is_neutral_about_retained_data() {
        let presentation = presentation_from_facts(
            Phase::Paused,
            [0, 0],
            None,
            [false, false],
            false,
            false,
            None,
        );
        assert_eq!(presentation.title, "Download paused");
        assert_eq!(
            presentation.details,
            "The download is paused. Resume will recheck any retained download data before continuing."
        );
    }

    // Catches neutral pause copy dropping facts the worker did establish.
    #[test]
    fn paused_presentation_retains_known_bytes_components_residue_uncertainty_and_error() {
        let presentation = presentation_from_facts(
            Phase::Paused,
            [512, 0],
            None,
            [true, false],
            true,
            true,
            Some("pause boundary changed"),
        );
        assert!(presentation.details.contains("component 1 has 512 bytes"));
        assert!(
            presentation
                .details
                .contains("1 completed and verified component")
        );
        assert!(presentation.details.contains("Some download data remains"));
        assert!(presentation.details.contains("may have changed"));
        assert!(presentation.details.contains("pause boundary changed"));
    }
    #[test]
    fn preference_loading_gates_commands_and_commit_ack_survives_coalescing() {
        let bytes = manifest_bytes(&manifest_value()); let server = Server::new(vec![response(404,"",b"")]);
        let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("selection.json");
        let (worker, observed, release) = server.client_with_preference_hold(&dir.path().join("cache"),&path);
        let key = SelectionKey::from_release(&server.selection(&bytes));
        let mut flow = InternalDownloadFlow::fixture(supported(),worker,
            vec![vec![server.selection(&bytes)],vec![server.selection(&bytes)]],"x86_64");
        assert!(!flow.can_start()); assert!(!flow.can_resume());
        assert!(flow.start_selected(key.clone()).is_err()); assert!(server.requests().is_empty());
        flow.wait_preference(); assert!(flow.can_start());
        flow.start_selected(key.clone()).unwrap();
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(flow.saved.is_none()); assert!(flow.pending_key.is_some());
        release.send(()).unwrap();
        // Deliberately skip every intermediate status, then deliver only Failed.
        let until = Instant::now()+Duration::from_secs(10);
        let status = loop {
            let status = flow.worker.as_ref().unwrap().status().unwrap();
            if status.phase == Phase::Failed { break status; }
            assert!(Instant::now()<until); std::thread::sleep(Duration::from_millis(2));
        };
        assert!(status.preference_commit.is_some()); assert!(flow.accept_status(status.clone()));
        assert_eq!(flow.saved,Some(key)); assert!(flow.pending_key.is_none());
        let revision = flow.revision(); assert!(!flow.accept_status(status)); assert_eq!(flow.revision(),revision);
    }

    #[test]
    fn foreign_ack_is_refused_and_namespace_warning_survives_later_token_statuses() {
        use ui::install_artifacts::PreferenceCommit;
        let bytes = manifest_bytes(&manifest_value()); let server = Server::new(vec![response(404,"",b"")]);
        let dir = tempfile::tempdir().unwrap();
        let (worker, observed, release) = server.client_with_preference_hold(&dir.path().join("cache"),&dir.path().join("selection.json"));
        let mut flow = InternalDownloadFlow::fixture(supported(),worker,
            vec![vec![server.selection(&bytes)],vec![server.selection(&bytes)]],"x86_64");
        flow.wait_preference(); flow.start_selected(flow.offered_selection().unwrap()).unwrap();
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        let other_server = Server::new(vec![response(404,"",b"")]);
        let other = other_server.client(&dir.path().join("other-cache"),&dir.path().join("other.json"));
        other.start(other_server.selection(&bytes),b"other".to_vec()).unwrap();
        let until = Instant::now()+Duration::from_secs(10);
        let mut foreign = loop {
            let status = other.status().unwrap();
            if status.phase == Phase::Failed { break status; }
            assert!(Instant::now()<until); std::thread::sleep(Duration::from_millis(2));
        };
        assert!(!flow.accept_status(foreign.clone())); assert!(flow.saved.is_none());
        // A DTO projection of the real Windows success result, not native Windows evidence.
        foreign.token = flow.token.clone().unwrap();
        foreign.preference_commit = Some(PreferenceCommit { durability: PreferenceDurability::NamespaceUnproven });
        assert!(flow.accept_status(foreign.clone())); assert!(flow.saved.is_some());
        assert!(flow.presentation.details.contains("may not survive power loss"));
        foreign.preference_commit = None; foreign.phase = Phase::Released;
        flow.accept_status(foreign);
        assert!(flow.presentation.details.contains("may not survive power loss"));
        release.send(()).unwrap();
    }

    #[test]
    fn cancel_while_saving_keeps_pending_key_until_commit_and_stops_network() {
        let bytes = manifest_bytes(&manifest_value()); let server = Server::new(vec![]);
        let dir = tempfile::tempdir().unwrap();
        let (worker, observed, release) = server.client_with_preference_hold(&dir.path().join("cache"),&dir.path().join("selection.json"));
        let mut flow = InternalDownloadFlow::fixture(supported(),worker,
            vec![vec![server.selection(&bytes)],vec![server.selection(&bytes)]],"x86_64");
        flow.wait_preference(); let key = flow.offered_selection().unwrap(); flow.start_selected(key.clone()).unwrap();
        observed.recv_timeout(Duration::from_secs(5)).unwrap(); flow.poll();
        assert_eq!(flow.presentation.phase,Some(Phase::SavingPreference)); assert!(flow.can_cancel());
        flow.cancel().unwrap(); assert!(flow.saved.is_none()); release.send(()).unwrap();
        wait_for(&mut flow,Phase::Cancelled); assert_eq!(flow.saved,Some(key));
        assert!(flow.pending_key.is_none()); assert!(server.requests().is_empty());
    }

    #[test]
    fn saved_key_is_reresolved_against_changed_catalog_before_resume() {
        let bytes = manifest_bytes(&manifest_value()); let mut changed = bytes.clone(); changed.push(b' ');
        let server = Server::new(vec![]); let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("selection.json");
        let key = SelectionKey::from_release(&server.selection(&bytes)); std::fs::write(&path,key.encode().unwrap()).unwrap();
        let worker = server.client(&dir.path().join("cache"),&path);
        let mut flow = InternalDownloadFlow::fixture(supported(),worker,
            vec![vec![server.selection(&bytes)],vec![server.selection(&changed)]],"x86_64");
        flow.wait_preference(); assert!(flow.can_resume()); assert!(flow.resume().is_err());
        assert!(flow.token.is_none()); assert!(server.requests().is_empty());
        assert_eq!(std::fs::read(path).unwrap(),key.encode().unwrap());
    }

    // Catches a failed save consuming the only Start opportunity without committing the key.
    #[test]
    fn terminal_uncommitted_preference_failure_allows_explicit_retry() {
        use ui::install_artifacts::{PreferenceNamespace, PreferencePublication};
        let bytes = manifest_bytes(&manifest_value());
        let server = Server::new(vec![response(404, "", b"")]);
        let directory = tempfile::tempdir().unwrap();
        let cache = directory.path().join("cache");
        let preference = directory.path().join("selection.json");
        let key = SelectionKey::from_release(&server.selection(&bytes));
        let mut old_key = key.clone(); old_key.release_id = "previous-selection".into();
        std::fs::write(&preference, old_key.encode().unwrap()).unwrap();
        let worker = server.client(&cache, &preference);
        let mut flow = InternalDownloadFlow::fixture(supported(), worker,
            vec![vec![server.selection(&bytes)]; 3], "x86_64");
        flow.wait_preference();
        assert_eq!(flow.saved, Some(old_key.clone()));
        // A real directory collision refuses publication without a fake storage backend.
        std::fs::remove_file(&preference).unwrap();
        std::fs::create_dir(&preference).unwrap();
        flow.start_selected(key.clone()).unwrap();
        wait_for(&mut flow, Phase::Failed);
        let failed_token = flow.token.clone().unwrap();
        let failed = flow.worker.as_ref().unwrap().status().unwrap();
        let facts = flow.worker.as_ref().unwrap().preference();
        let PreferenceState::Failed(failure) = &facts else { panic!("save failure missing") };
        assert_eq!(failure.publication, PreferencePublication::NotPublished);
        assert_eq!(failure.namespace, PreferenceNamespace::Durable);
        assert!(failed.preference_commit.is_none());
        assert_eq!(failed.error.as_deref(), Some(failure.message.as_str()));
        assert_eq!(flow.saved, Some(old_key.clone()));
        assert_eq!(flow.pending_key.as_ref(), Some(&(failed_token.clone(), key.clone())));
        assert!(server.requests().is_empty()); assert!(!cache.exists());
        let presentation = flow.presentation.clone();
        flow.poll();
        assert_eq!(flow.presentation, presentation);
        assert_eq!(flow.worker.as_ref().unwrap().preference(), facts);
        assert_eq!(flow.token.as_ref(), Some(&failed_token));
        assert!(flow.can_start(), "a terminal uncommitted save must allow an explicit retry");
        assert!(!flow.can_resume());
        // Only this explicit repair/retry replaces the failed attempt's token.
        std::fs::remove_dir(&preference).unwrap();
        flow.start_selected(key.clone()).unwrap();
        let retry = flow.token.clone().unwrap();
        assert_ne!(retry, failed_token);
        assert!(!flow.accept_status(failed));
        wait_for(&mut flow, Phase::Failed); // deliberate later 404, after preference commit
        let retried = flow.worker.as_ref().unwrap().status().unwrap();
        assert_eq!(retried.token, retry); assert!(retried.preference_commit.is_some());
        assert_eq!(flow.saved, Some(key.clone())); assert!(flow.pending_key.is_none());
        assert_eq!(std::fs::read(preference).unwrap(), key.encode().unwrap());
        assert_eq!(server.requests().len(), 1); assert!(!cache.exists());
    }

    // Catches cancellation before save admission stranding the pending key forever.
    #[test]
    fn terminal_uncommitted_preference_cancellation_allows_explicit_retry() {
        let bytes = manifest_bytes(&manifest_value());
        let server = Server::new(vec![response(404, "", b"")]);
        let directory = tempfile::tempdir().unwrap();
        let cache = directory.path().join("cache");
        let preference = directory.path().join("selection.json");
        let (worker, observed, release) = server.client_with_preference_hold(&cache, &preference);
        // Hold a real older operation so this flow's first save stays queued.
        let older = worker.start(server.selection(&bytes), b"older advisory bytes".to_vec()).unwrap();
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut flow = InternalDownloadFlow::fixture(supported(), worker,
            vec![vec![server.selection(&bytes)]; 3], "x86_64");
        flow.wait_preference(); let key = flow.offered_selection().unwrap();
        flow.start_selected(key.clone()).unwrap();
        let cancelled_token = flow.token.clone().unwrap();
        assert_ne!(cancelled_token, older);
        flow.cancel().unwrap(); release.send(()).unwrap();
        wait_for(&mut flow, Phase::Cancelled);
        let cancelled = flow.worker.as_ref().unwrap().status().unwrap();
        assert!(cancelled.preference_commit.is_none()); assert!(flow.saved.is_none());
        assert_eq!(flow.pending_key.as_ref(), Some(&(cancelled_token.clone(), key.clone())));
        assert!(server.requests().is_empty()); assert!(!cache.exists());
        assert_eq!(std::fs::read(&preference).unwrap(), b"older advisory bytes");
        let facts = flow.worker.as_ref().unwrap().preference();
        let presentation = flow.presentation.clone(); flow.poll();
        assert_eq!(flow.presentation, presentation);
        assert_eq!(flow.worker.as_ref().unwrap().preference(), facts);
        assert_eq!(flow.token.as_ref(), Some(&cancelled_token));
        assert!(flow.can_start(), "cancellation without a commit must allow an explicit retry");
        flow.start_selected(key.clone()).unwrap();
        let retry = flow.token.clone().unwrap(); assert_ne!(retry, cancelled_token);
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(flow.saved.is_none(), "only the retry commit may advance saved");
        assert!(!flow.accept_status(cancelled));
        release.send(()).unwrap(); wait_for(&mut flow, Phase::Failed);
        let status = flow.worker.as_ref().unwrap().status().unwrap();
        assert_eq!(status.token, retry); assert!(status.preference_commit.is_some());
        assert_eq!(flow.saved, Some(key.clone())); assert!(flow.pending_key.is_none());
        assert_eq!(std::fs::read(preference).unwrap(), key.encode().unwrap());
        assert_eq!(server.requests().len(), 1); assert!(!cache.exists());
    }

}
