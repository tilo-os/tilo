use crate::release::InternalRelease;
use crate::setup::internal_cache::coordinator::{Phase as WorkerPhase, Status, Token, Worker};
use crate::setup::internal_install_flow::{StagingLink, staging_consumer};
use crate::setup::model::{Capabilities, Experience};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

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

struct SelectionPreference {
    path: PathBuf,
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
    Consume,
}

pub(super) struct InternalDownloadFlow {
    capabilities: Capabilities,
    preference: SelectionPreference,
    saved: Option<SelectionKey>,
    offered: Option<SelectionKey>,
    catalog_error: Option<String>,
    worker: Option<Worker>,
    token: Option<Token>,
    presentation: DownloadPresentation,
    pending: Option<PendingCommand>,
    revision: u64,
    /// Consumer for verified downloads: the staging helper handoff.
    staging: Option<StagingLink>,
    /// The current operation was handed to the staging consumer, so a failure
    /// is an installation failure over retained verified data.
    consumed: bool,
    #[cfg(test)]
    catalogs: VecDeque<Vec<InternalRelease>>,
    #[cfg(test)]
    host_architecture: String,
}

impl InternalDownloadFlow {
    #[cfg(not(test))]
    pub(super) fn new(capabilities: Capabilities, staging: StagingLink) -> Result<Self, String> {
        let preference = SelectionPreference::live()?;
        let saved = preference.load()?;
        let mut flow = Self {
            capabilities,
            preference,
            saved,
            offered: None,
            catalog_error: None,
            worker: None,
            token: None,
            presentation: DownloadPresentation::default(),
            pending: None,
            revision: 0,
            staging: Some(staging),
            consumed: false,
            #[cfg(test)]
            catalogs: VecDeque::new(),
            #[cfg(test)]
            host_architecture: std::env::consts::ARCH.into(),
        };
        flow.refresh_catalog();
        Ok(flow)
    }

    #[cfg(test)]
    pub(super) fn unavailable_fixture(capabilities: Capabilities) -> Self {
        let temporary = tempfile::tempdir().expect("temporary setup preference");
        let preference = SelectionPreference::at(temporary.path().join("selection.json"));
        let mut flow = Self {
            capabilities,
            preference,
            saved: None,
            offered: None,
            catalog_error: None,
            worker: None,
            token: None,
            presentation: DownloadPresentation::default(),
            pending: None,
            revision: 0,
            staging: None,
            consumed: false,
            catalogs: VecDeque::from([Vec::new()]),
            host_architecture: std::env::consts::ARCH.into(),
        };
        flow.refresh_catalog();
        flow
    }

    #[cfg(test)]
    fn fixture(
        capabilities: Capabilities,
        preference: SelectionPreference,
        worker: Worker,
        catalogs: Vec<Vec<InternalRelease>>,
        host_architecture: &str,
    ) -> Self {
        let saved = preference.load().unwrap_or(None);
        let mut flow = Self {
            capabilities,
            preference,
            saved,
            offered: None,
            catalog_error: None,
            worker: Some(worker),
            token: None,
            presentation: DownloadPresentation::default(),
            pending: None,
            revision: 0,
            staging: None,
            consumed: false,
            catalogs: catalogs.into(),
            host_architecture: host_architecture.into(),
        };
        flow.refresh_catalog();
        flow
    }

    #[cfg(test)]
    pub(in crate::setup) fn fixture_at(
        capabilities: Capabilities,
        preference_path: PathBuf,
        worker: Worker,
        catalogs: Vec<Vec<InternalRelease>>,
        host_architecture: &str,
    ) -> Self {
        Self::fixture(
            capabilities,
            SelectionPreference::at(preference_path),
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
            std::env::consts::ARCH
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
            self.worker = Some(match self.staging.clone() {
                Some(link) => Worker::spawn(staging_consumer(link))?,
                None => Worker::spawn(|_, _, _, _| {
                    Err(
                        "Protected staging is not available; verified download data was retained."
                            .into(),
                    )
                })?,
            });
        }
        self.worker
            .as_ref()
            .ok_or("Download worker is unavailable.".into())
    }

    /// Follow a capability change (the host probe finished).
    pub(super) fn set_capabilities(&mut self, capabilities: Capabilities) {
        self.capabilities = capabilities;
        self.revision = self.revision.wrapping_add(1);
    }

    /// Show a verified (or already consumed) download for `--shots`; no
    /// worker or data exists.
    pub(super) fn pose_for_shot(&mut self, consumed: bool) {
        self.presentation = presentation_from_facts(
            if consumed {
                WorkerPhase::Consumed
            } else {
                WorkerPhase::Ready
            },
            [0, 0],
            None,
            [true, true],
            false,
            false,
            None,
        );
        self.revision = self.revision.wrapping_add(1);
    }

    /// A verified download is held and can be handed to the staging helper.
    pub(super) fn is_ready(&self) -> bool {
        self.pending.is_none() && self.presentation.phase == Some(WorkerPhase::Ready)
    }

    /// Hand the held verified download to the staging consumer.
    pub(super) fn consume(&mut self) -> Result<(), String> {
        if !self.is_ready() {
            return Err(crate::setup::strings::t(
                "imager-setup-internal-error-download",
            ));
        }
        let (worker, token) = self
            .worker
            .as_ref()
            .zip(self.token.as_ref())
            .ok_or("The verified download is no longer held.")?;
        worker.consume(token)?;
        self.consumed = true;
        self.set_pending(
            PendingCommand::Consume,
            &crate::setup::strings::t("imager-setup-internal-download-consuming"),
        );
        Ok(())
    }

    /// The installation failure the worker reported for a consumed download.
    pub(super) fn consume_failure(&self) -> Option<&str> {
        (self.consumed
            && self.pending.is_none()
            && matches!(
                self.presentation.phase,
                Some(WorkerPhase::Failed | WorkerPhase::Cancelled)
            ))
        .then_some(self.presentation.details.as_str())
    }

    pub(super) fn offered_selection(&self) -> Option<SelectionKey> {
        self.offered.clone()
    }

    pub(super) fn catalog_error(&self) -> Option<&str> {
        self.catalog_error.as_deref()
    }

    pub(super) fn can_resume(&self) -> bool {
        self.capabilities.supports(Experience::FullOs)
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
                            | WorkerPhase::Consumed
                    )
                ))
    }

    pub(super) fn can_start(&self) -> bool {
        self.capabilities.supports(Experience::FullOs)
            && self.offered.is_some()
            && self.saved.as_ref() != self.offered.as_ref()
            && self.token.is_none()
            && self.pending.is_none()
    }

    pub(super) fn can_pause(&self) -> bool {
        self.pending.is_none()
            && matches!(
                self.presentation.phase,
                Some(
                    WorkerPhase::Authenticating
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
        self.preference.save(&key)?;
        let token = self.ensure_worker()?.start(release)?;
        self.saved = Some(key);
        self.token = Some(token);
        self.consumed = false;
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
        let token = self.ensure_worker()?.start(release)?;
        self.token = Some(token);
        self.consumed = false;
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
                    WorkerPhase::Authenticating
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
            worker.cancel(token)?;
            self.set_pending(PendingCommand::Cancel, "Cancelling download…");
        } else {
            worker.pause(token)?;
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
            .discard(&target, release)?;
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
        worker.release(token)?;
        self.set_pending(PendingCommand::Release, "Keeping download for later…");
        Ok(())
    }

    fn set_pending(&mut self, pending: PendingCommand, title: &str) {
        self.pending = Some(pending);
        self.presentation.pending = true;
        self.presentation.title = title.into();
        self.revision = self.revision.wrapping_add(1);
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
        if let Some(pending) = self.pending {
            let expected = match pending {
                PendingCommand::Pause => status.phase == WorkerPhase::Paused,
                PendingCommand::Cancel => status.phase == WorkerPhase::Cancelled,
                PendingCommand::Release => status.phase == WorkerPhase::Released,
                PendingCommand::Discard => status.phase == WorkerPhase::Discarded,
                PendingCommand::Consume => {
                    matches!(status.phase, WorkerPhase::Consuming | WorkerPhase::Consumed)
                }
            };
            let failed = status.phase == WorkerPhase::Failed;
            if !expected && !failed {
                return false;
            }
            self.pending = None;
        }
        let mut next = presentation_from_status(&status);
        if self.consumed && status.phase == WorkerPhase::Failed {
            // The download itself stayed verified; the installer helper stopped.
            next.title = crate::setup::strings::t("imager-setup-internal-download-kept");
            next.details = status.error.unwrap_or_default();
        }
        if next == self.presentation {
            return false;
        }
        self.presentation = next;
        self.revision = self.revision.wrapping_add(1);
        true
    }

    pub(super) fn poll(&mut self) -> bool {
        self.sync_current_status()
    }

    pub(super) fn presentation(&self) -> &DownloadPresentation {
        &self.presentation
    }

    pub(super) fn revision(&self) -> u64 {
        self.revision
    }

    pub(super) fn has_pending_operation(&self) -> bool {
        self.pending.is_some()
            || matches!(
                self.presentation.phase,
                Some(
                    WorkerPhase::Authenticating
                        | WorkerPhase::Acquiring
                        | WorkerPhase::Preparing
                        | WorkerPhase::Downloading
                        | WorkerPhase::Consuming
                )
            )
    }
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
    progress: Option<crate::setup::internal_download::Progress>,
    finals: [bool; 2],
    residue: bool,
    uncertain: bool,
    error: Option<&str>,
) -> DownloadPresentation {
    let consumption: (String, String);
    let (title, summary) = match phase {
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
        WorkerPhase::Consuming => {
            consumption = (
                crate::setup::strings::t("imager-setup-internal-download-consuming"),
                crate::setup::strings::t("imager-setup-internal-download-consuming-detail"),
            );
            (consumption.0.as_str(), consumption.1.as_str())
        }
        WorkerPhase::Consumed => {
            consumption = (
                crate::setup::strings::t("imager-setup-internal-download-consumed"),
                crate::setup::strings::t("imager-setup-internal-download-consumed-detail"),
            );
            (consumption.0.as_str(), consumption.1.as_str())
        }
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

impl SelectionPreference {
    #[cfg(not(test))]
    fn live() -> Result<Self, String> {
        let root = tilo_paths::data_local_dir()
            .ok_or("A device-local settings folder is unavailable on this computer.")?;
        Ok(Self::at(
            root.join("tilo")
                .join("setup")
                .join("internal-download-selection.json"),
        ))
    }

    fn at(path: PathBuf) -> Self {
        Self { path }
    }

    fn load(&self) -> Result<Option<SelectionKey>, String> {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "Saved download selection could not be read: {error}"
                ));
            }
        };
        if file
            .metadata()
            .map_err(|error| format!("Saved download selection could not be inspected: {error}"))?
            .len()
            > MAX_PREFERENCE_BYTES as u64
        {
            return Ok(None);
        }
        let mut bytes = Vec::with_capacity(MAX_PREFERENCE_BYTES.min(512));
        Read::by_ref(&mut file)
            .take((MAX_PREFERENCE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("Saved download selection could not be read: {error}"))?;
        if bytes.len() > MAX_PREFERENCE_BYTES {
            return Ok(None);
        }
        Ok(SelectionKey::decode(&bytes).ok())
    }

    fn save(&self, key: &SelectionKey) -> Result<(), String> {
        let bytes = key.encode()?;
        let parent = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)
            .map_err(|error| format!("Download selection could not be saved: {error}"))?;
        let name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("Download selection path has no ordinary file name.")?;
        let write_result = (|| -> Result<(), std::io::Error> {
            let mut file = tempfile::Builder::new()
                .prefix(&format!(".{name}."))
                .suffix(".tmp")
                .tempfile_in(parent)?;
            file.write_all(&bytes)?;
            file.as_file().sync_all()?;
            file.persist(&self.path).map_err(|error| error.error)?;
            if let Ok(directory) = File::open(parent) {
                let _ = directory.sync_all();
            }
            Ok(())
        })();
        if let Err(error) = write_result {
            return Err(format!("Download selection could not be saved: {error}"));
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::release::InternalRelease;
    use crate::setup::internal_bundle;
    use crate::setup::internal_cache::coordinator::{Phase, Worker};
    use crate::setup::internal_download::tests::{Server, response};
    use crate::setup::model::{Capabilities, HostOs};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    use std::time::Duration;

    fn release() -> InternalRelease {
        let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
        InternalRelease::transport_fixture(&bytes, "https://code.tilo.so/releases/manifest.json")
    }

    fn supported() -> Capabilities {
        Capabilities {
            host: HostOs::Linux,
            desktop_session: false,
            internal_dual_boot: true,
        }
    }

    fn wait_for(flow: &mut InternalDownloadFlow, phase: Phase) {
        tilo_test_env::wait_until(format_args!("the download flow to reach {phase:?}"), || {
            flow.poll();
            flow.presentation().phase == Some(phase)
        });
    }

    /// Saved and offered selection, preference round trip and failures.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn internal_download_selection_and_preference_are_strict_and_advisory() {
        // Phase `saved_selection_is_bounded_strict_and_versioned`.
        // Catches accepting a preference that can smuggle authority beyond the exact bounded key.
        {
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

        // Phase `offered_selection_requires_exactly_one_current_host_entry`.
        // Catches selecting array order when the embedded catalog has zero or several host entries.
        {
            assert!(offered_selection(Vec::new(), "x86_64").is_err());
            assert!(offered_selection(vec![release(), release()], "x86_64").is_err());

            let key = offered_selection(vec![release()], "x86_64").unwrap();
            assert_eq!(key.release_id, "fixture-only");
            assert_eq!(key.architecture, "x86_64");
        }

        // Phase `preference_round_trip_contains_only_the_advisory_key`.
        // Catches persisting worker authority or leaving a partially written restart record.
        {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("setup").join("internal-download.json");
            let preference = SelectionPreference::at(path.clone());
            let key = SelectionKey::from_release(&release());

            preference.save(&key).unwrap();
            assert_eq!(preference.load().unwrap(), Some(key.clone()));
            let mut replacement = key;
            replacement.release_id = "fixture-replacement".into();
            preference.save(&replacement).unwrap();
            assert_eq!(preference.load().unwrap(), Some(replacement));
            let value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(value.as_object().unwrap().len(), 4);
            assert_eq!(
                std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
                1
            );
        }

        // Phase `invalid_preference_is_ignored_and_write_failure_is_reported`.
        // Catches malformed local state becoming authority or silently accepting a failed write.
        {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("selection.json");
            std::fs::write(&path, br#"{"schema":1,"release_id":"x"}"#).unwrap();
            let preference = SelectionPreference::at(path);
            assert_eq!(preference.load().unwrap(), None);

            let blocker = temp.path().join("not-a-directory");
            std::fs::write(&blocker, b"ordinary file").unwrap();
            let failed = SelectionPreference::at(blocker.join("selection.json"));
            assert!(
                failed
                    .save(&SelectionKey::from_release(&release()))
                    .is_err()
            );
        }

        // Phase `changed_ambiguous_or_unsupported_selection_never_starts_the_worker`.
        // Catches catalog drift, ambiguity, or capability fixtures issuing a worker command.
        {
            let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
            let mut changed = bytes.clone();
            changed.push(b' ');
            let server = Server::new(Vec::new());
            let root = tempfile::tempdir().unwrap();
            let preference_root = tempfile::tempdir().unwrap();
            let key = SelectionKey::from_release(&server.release(&bytes));
            let worker = Worker::fixture_for_test(root.path(), server.transport(), |_, _, _, _| {
                Err("Protected staging is unavailable.".into())
            });
            let mut changed_flow = InternalDownloadFlow::fixture(
                supported(),
                SelectionPreference::at(preference_root.path().join("changed.json")),
                worker,
                vec![vec![server.release(&bytes)], vec![server.release(&changed)]],
                "x86_64",
            );
            assert!(changed_flow.start_selected(key.clone()).is_err());
            assert!(changed_flow.token.is_none());
            assert!(server.requests().is_empty());

            let worker = Worker::fixture_for_test(root.path(), server.transport(), |_, _, _, _| {
                Err("Protected staging is unavailable.".into())
            });
            let mut ambiguous = InternalDownloadFlow::fixture(
                supported(),
                SelectionPreference::at(preference_root.path().join("ambiguous.json")),
                worker,
                vec![
                    vec![server.release(&bytes)],
                    vec![server.release(&bytes), server.release(&bytes)],
                ],
                "x86_64",
            );
            assert!(ambiguous.start_selected(key.clone()).is_err());
            assert!(ambiguous.token.is_none());

            let worker = Worker::fixture_for_test(root.path(), server.transport(), |_, _, _, _| {
                Err("Protected staging is unavailable.".into())
            });
            let mut unsupported_capabilities = supported();
            unsupported_capabilities.internal_dual_boot = false;
            let mut unsupported = InternalDownloadFlow::fixture(
                unsupported_capabilities,
                SelectionPreference::at(preference_root.path().join("unsupported.json")),
                worker,
                vec![vec![server.release(&bytes)], vec![server.release(&bytes)]],
                "x86_64",
            );
            assert!(unsupported.start_selected(key).is_err());
            assert!(unsupported.token.is_none());
            assert!(server.requests().is_empty());
        }
    }

    /// Real worker, duplicate and stale starts.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn internal_download_worker_start_is_idempotent_and_holds_verified_data() {
        // Phase `real_worker_reaches_held_ready_and_keeps_verified_data_for_later`.
        // Catches detaching from the real worker, claiming installation, or polling held Ready forever.
        {
            let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
            let server = Server::new(vec![
                response(200, "", &bytes),
                response(200, "", &vec![b'E'; 4096]),
                response(200, "", &vec![b'R'; 8192]),
            ]);
            let root = tempfile::tempdir().unwrap();
            let preference_root = tempfile::tempdir().unwrap();
            let key = SelectionKey::from_release(&server.release(&bytes));
            let worker = Worker::fixture_for_test(root.path(), server.transport(), |_, _, _, _| {
                Err("Protected staging is not available in this build.".into())
            });
            let mut flow = InternalDownloadFlow::fixture(
                supported(),
                SelectionPreference::at(preference_root.path().join("selection.json")),
                worker,
                vec![vec![server.release(&bytes)], vec![server.release(&bytes)]],
                "x86_64",
            );

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
        }

        // Phase `duplicate_start_during_active_download_preserves_token_preference_catalog_and_requests`.
        // Catches a duplicate Start replacing an active token or repeating selection side effects.
        {
            use std::os::unix::fs::MetadataExt;

            let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
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
            let key = SelectionKey::from_release(&server.release(&bytes));
            let worker = Worker::fixture_for_test(root.path(), server.transport(), |_, _, _, _| {
                Err("Protected staging is unavailable.".into())
            });
            let mut flow = InternalDownloadFlow::fixture(
                supported(),
                SelectionPreference::at(preference_path.clone()),
                worker,
                vec![
                    vec![server.release(&bytes)],
                    vec![server.release(&bytes)],
                    vec![server.release(&bytes)],
                ],
                "x86_64",
            );
            flow.start_selected(key.clone()).unwrap();
            tilo_test_env::recv("the download to block", &blocked_rx);
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
        }

        // Phase `stale_start_at_held_ready_preserves_token_and_request_count`.
        // Catches a stale Start replacing the token and leases held at verified Ready.
        {
            use std::os::unix::fs::MetadataExt;

            let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
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
            let key = SelectionKey::from_release(&server.release(&bytes));
            let worker = Worker::fixture_for_test(root.path(), server.transport(), |_, _, _, _| {
                Err("Protected staging is unavailable.".into())
            });
            let mut flow = InternalDownloadFlow::fixture(
                supported(),
                SelectionPreference::at(preference_path.clone()),
                worker,
                vec![
                    vec![server.release(&bytes)],
                    vec![server.release(&bytes)],
                    vec![server.release(&bytes)],
                ],
                "x86_64",
            );
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
        }
    }

    /// Pause, cancel, discard and drop.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn internal_download_pause_cancel_and_drop_manage_tokens_and_leases() {
        // Phase `pause_close_and_reconstruct_resume_with_a_new_token`.
        // Catches retaining a token/handle across restart or starting network work implicitly.
        {
            let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
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
            let preference_path = preference_root.path().join("selection.json");
            let key = SelectionKey::from_release(&server.release(&bytes));
            let worker = Worker::fixture_for_test(root.path(), server.transport(), |_, _, _, _| {
                Err("Protected staging is unavailable.".into())
            });
            let mut flow = InternalDownloadFlow::fixture(
                supported(),
                SelectionPreference::at(preference_path.clone()),
                worker,
                vec![vec![server.release(&bytes)], vec![server.release(&bytes)]],
                "x86_64",
            );
            flow.start_selected(key).unwrap();
            tilo_test_env::recv("the download to block", &blocked_rx);
            let first_token = flow.token.clone().unwrap();
            flow.pause().unwrap();
            assert!(flow.presentation().pending);
            continue_tx.send(()).unwrap();
            wait_for(&mut flow, Phase::Paused);
            drop(flow);
            drop(server);

            let resumed_server = Server::new(vec![
                response(200, "", &bytes),
                response(200, "", &vec![b'E'; 4096]),
                response(200, "", &vec![b'R'; 8192]),
            ]);
            let resumed_worker =
                Worker::fixture_for_test(root.path(), resumed_server.transport(), |_, _, _, _| {
                    Err("Protected staging is unavailable.".into())
                });
            let mut resumed = InternalDownloadFlow::fixture(
                supported(),
                SelectionPreference::at(preference_path),
                resumed_worker,
                vec![
                    vec![resumed_server.release(&bytes)],
                    vec![resumed_server.release(&bytes)],
                ],
                "x86_64",
            );
            assert!(resumed.can_resume());
            assert!(!resumed.can_start());
            assert!(resumed_server.requests().is_empty());
            resumed.resume().unwrap();
            let second_token = resumed.token.clone().unwrap();
            assert_ne!(first_token, second_token);
            wait_for(&mut resumed, Phase::Ready);
        }

        // Phase `accepted_cancel_stays_pending_until_cancelled_then_discard_uses_new_token`.
        // Catches a queued Ready/progress snapshot defeating cancel or discard reusing its token.
        {
            let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
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
            let key = SelectionKey::from_release(&server.release(&bytes));
            let worker = Worker::fixture_for_test(root.path(), server.transport(), |_, _, _, _| {
                Err("Protected staging is unavailable.".into())
            });
            let mut flow = InternalDownloadFlow::fixture(
                supported(),
                SelectionPreference::at(preference_root.path().join("selection.json")),
                worker,
                vec![
                    vec![server.release(&bytes)],
                    vec![server.release(&bytes)],
                    vec![server.release(&bytes)],
                ],
                "x86_64",
            );
            flow.start_selected(key).unwrap();
            tilo_test_env::recv("the download to block", &blocked_rx);
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
        }

        // Phase `dropping_held_ready_releases_leases_and_retains_verified_components`.
        // Catches leaving held readers/root leases behind when the setup view closes at Ready.
        {
            let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
            let server = Server::new(vec![
                response(200, "", &bytes),
                response(200, "", &vec![b'E'; 4096]),
                response(200, "", &vec![b'R'; 8192]),
            ]);
            let root = tempfile::tempdir().unwrap();
            let preference_root = tempfile::tempdir().unwrap();
            let key = SelectionKey::from_release(&server.release(&bytes));
            let worker = Worker::fixture_for_test(root.path(), server.transport(), |_, _, _, _| {
                Err("Protected staging is unavailable.".into())
            });
            let mut flow = InternalDownloadFlow::fixture(
                supported(),
                SelectionPreference::at(preference_root.path().join("selection.json")),
                worker,
                vec![vec![server.release(&bytes)], vec![server.release(&bytes)]],
                "x86_64",
            );
            flow.start_selected(key).unwrap();
            wait_for(&mut flow, Phase::Ready);
            let first_token = flow.token.clone().unwrap();
            drop(flow);
            drop(server);

            let reopened_server = Server::new(vec![response(200, "", &bytes)]);
            let reopened_worker =
                Worker::fixture_for_test(root.path(), reopened_server.transport(), |_, _, _, _| {
                    Err("Protected staging is unavailable.".into())
                });
            let mut reopened = InternalDownloadFlow::fixture(
                supported(),
                SelectionPreference::at(preference_root.path().join("selection.json")),
                reopened_worker,
                vec![
                    vec![reopened_server.release(&bytes)],
                    vec![reopened_server.release(&bytes)],
                ],
                "x86_64",
            );
            assert!(reopened.can_resume());
            reopened.resume().unwrap();
            assert_ne!(first_token, reopened.token.clone().unwrap());
            wait_for(&mut reopened, Phase::Ready);
            assert_eq!(reopened_server.requests().len(), 1);
        }

        // Phase `dropping_during_authentication_waits_for_cooperative_worker_shutdown`.
        // Catches dropping the controller while leaving an in-flight worker detached.
        {
            let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
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
            let key = SelectionKey::from_release(&server.release(&bytes));
            let worker = Worker::fixture_for_test(root.path(), server.transport(), |_, _, _, _| {
                Err("Protected staging is unavailable.".into())
            });
            let mut flow = InternalDownloadFlow::fixture(
                supported(),
                SelectionPreference::at(preference_root.path().join("selection.json")),
                worker,
                vec![vec![server.release(&bytes)], vec![server.release(&bytes)]],
                "x86_64",
            );
            flow.start_selected(key).unwrap();
            tilo_test_env::recv("the download request", &entered_rx);
            let unblock = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(25));
                continue_tx.send(()).unwrap();
            });
            drop(flow);
            unblock.join().unwrap();
            assert!(request_returned.load(Ordering::Acquire));
        }
    }

    /// Terminal and paused presentation.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn internal_download_wording_reports_only_known_retained_facts() {
        // Phase `terminal_wording_retains_bytes_components_residue_uncertainty_and_original_error`.
        // Catches normalizing away the coordinator's R1/R2 uncertainty and boundary errors.
        {
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

        // Phase `false_residue_makes_no_global_absence_claim`.
        // Catches residue=false being upgraded into a claim that all retained data is absent.
        {
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

        // Phase `paused_zero_fact_presentation_is_neutral_about_retained_data`.
        // Catches a pause with no established retained facts claiming data or completed components.
        {
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

        // Phase `paused_presentation_retains_known_bytes_components_residue_uncertainty_and_error`.
        // Catches neutral pause copy dropping facts the worker did establish.
        {
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
    }
}
