//! Original encrypted export followed by an independent full verifier namespace.
use super::runner::OriginalWorkerRun;
use super::*;
use crate::setup::{
    home_capture::{runtime::TrustedCaptureService, worker},
    portable_home_media::Expectation,
};
use zeroize::Zeroizing;

/// Keeps actual cleanup evidence on failure; a receipt is not finished setup.
pub struct OriginalExportOutcome {
    pub(super) runs: Vec<OriginalWorkerRun>,
    pub result: io::Result<Expectation>,
    pub(super) applications: Option<crate::setup::flatpak_transfer::portable::Expectation>,
}
impl OriginalExportOutcome {
    pub fn cleanup_receipt(
        &self,
    ) -> io::Result<(
        &launcher::owned_service::ConfirmedShutdown,
        &launcher::owned_service::ServiceLeaseKey,
        &Path,
    )> {
        if self.runs.is_empty()
            || self
                .runs
                .iter()
                .any(|run| run.supervised.shutdown.is_none())
        {
            return Err(refused("Original worker shutdown remains unconfirmed"));
        }
        let run = self
            .runs
            .last()
            .ok_or_else(|| refused("Original cleanup custody missing"))?;
        let proof = run
            .supervised
            .shutdown
            .as_ref()
            .ok_or_else(|| refused("Original cleanup proof missing"))?;
        Ok((proof, &run.key, run.working_path()?))
    }
}
impl OfflineOutputTransaction {
    pub fn export_and_verify(
        self: &Arc<Self>,
        password: Zeroizing<Vec<u8>>,
    ) -> OriginalExportOutcome {
        let mut runs = Vec::new();
        let mut applications = None;
        let result = (|| {
            self.check()?;
            // The legacy full-Home command must not launch on an apps-only claim.
            let _ = self.read.output_declaration(&self.cancelled)?;
            self.store
                .lock()
                .map_err(|_| refused("Original output Store lock failed"))?
                .mark_original_output_launching(
                    &self.binding.attempt,
                    &self.medium.files()?.output,
                )?;
            self.original_command(Zeroizing::new(password.to_vec()), None, &mut runs)?;
            let exported = runs
                .last()
                .ok_or_else(|| refused("Original export runner missing"))?;
            let (expected, expected_applications) = match exported.quiescent_response()? {
                worker::Response::ExportOriginal { home, applications } => {
                    (home.clone(), applications.clone())
                }
                _ => return Err(refused("Original export response differs")),
            };
            expected.validate()?;
            let required = self.read.output_flatpak_required(&self.cancelled)?;
            let application_operation = self.read.output_flatpak_operation(&self.cancelled)?;
            if expected_applications.is_some() != required {
                return Err(refused(
                    "Original selected applications were omitted or substituted",
                ));
            }
            let mut sealed = expected.sealed_bytes;
            if let Some(apps) = &expected_applications {
                apps.validate()?;
                if application_operation.as_deref() != Some(apps.operation.as_str())
                    || apps.operation == expected.operation
                    || apps.setup_binding != expected.setup_binding
                {
                    return Err(refused("Original application export binding differs"));
                }
                sealed = sealed
                    .checked_add(apps.sealed_bytes)
                    .ok_or_else(|| refused("Original paired export size overflow"))?;
            }
            if sealed > self.binding.reserve_bytes {
                return Err(refused("Original export exceeds admitted budget"));
            }
            let exit = exported
                .supervised
                .result
                .as_ref()
                .map_err(|_| refused("Original export quiescence missing"))?;
            self.check()?;
            let expectations = super::super::store::OriginalOutputExpectations {
                home: &expected,
                applications: expected_applications.as_ref(),
                reserve_bytes: self.binding.reserve_bytes,
            };
            {
                let store = self
                    .store
                    .lock()
                    .map_err(|_| refused("Original output Store lock failed"))?;
                let output = self.medium.files()?.output;
                store.mark_original_output_exported_paired(
                    &self.binding.attempt,
                    &output,
                    &expectations,
                    exit,
                    &exported.key,
                    exported.working_path()?,
                )?;
                store.mark_original_output_verifying_paired(
                    &self.binding.attempt,
                    &output,
                    &expectations,
                )?;
            }
            // This command is minted again under the actual retained original
            // reader; source/setup/operation/budget are never copied from a DTO.
            self.original_command(
                password,
                Some((expected.clone(), expected_applications.clone())),
                &mut runs,
            )?;
            let verified = runs
                .last()
                .ok_or_else(|| refused("Original verifier runner missing"))?;
            if !matches!(verified.quiescent_response()?, worker::Response::Validated) {
                return Err(refused("Original verifier response differs"));
            }
            let exit = verified
                .supervised
                .result
                .as_ref()
                .map_err(|_| refused("Original verifier quiescence missing"))?;
            self.check()?;
            self.store
                .lock()
                .map_err(|_| refused("Original output Store lock failed"))?
                .mark_original_output_verified_paired(
                    &self.binding.attempt,
                    &self.medium.files()?.output,
                    &expectations,
                    exit,
                    &verified.key,
                    verified.working_path()?,
                )?;
            self.medium.publish_verified_receipt_paired(
                &expectations,
                exit,
                &verified.key,
                verified.working_path()?,
            )?;
            // Canonical bounded receipt is published after the independent
            // verifier has read the entire authenticated encrypted stream.
            // This still does not rewrite/complete the pending setup manifest.
            applications = expected_applications;
            Ok(expected)
        })();
        OriginalExportOutcome {
            runs,
            result,
            applications,
        }
    }
    fn original_command(
        self: &Arc<Self>,
        password: Zeroizing<Vec<u8>>,
        expected: Option<(
            Expectation,
            Option<crate::setup::flatpak_transfer::portable::Expectation>,
        )>,
        runs: &mut Vec<OriginalWorkerRun>,
    ) -> io::Result<()> {
        self.check()?;
        let service = TrustedCaptureService::native()?;
        let runtime = service.root.clone();
        let publisher = service.publisher.clone();
        self.clone()
            .with_writer(&runtime, &publisher, |admission, request, digest| {
                if digest != self.binding.read_claim_digest {
                    return Err(refused("Original command read binding differs"));
                }
                let request = match expected {
                    Some((expected, applications)) => {
                        request.into_validate_selected(expected, applications, password)?
                    }
                    None => request.into_export(password)?,
                };
                let run = self.launch_request(admission, request, service)?;
                // Retain actual cleanup tokens before the read API performs its
                // final permission recheck, including cancellation after child exit.
                runs.push(run);
                Ok(())
            })
    }
}
