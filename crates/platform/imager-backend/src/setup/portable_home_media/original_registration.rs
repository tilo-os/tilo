//! Original-source registration uses the existing admitted helper, never
//! locally sampled NSS facts or a fabricated immutable content epoch.
use super::*;
use crate::setup::{
    home_capture::{Rendezvous, RpcReply, SourceWitness, State, controller::Controller},
    home_capture_helper::{OriginalRequest, publish_original_request},
    migration_handoff::Plan,
};
use std::os::unix::fs::MetadataExt as _;
use std::{process::Stdio, time::Duration};

pub struct Registered {
    pub witness: SourceWitness,
    pub mounted: Option<crate::setup::home_capture::MountedWitness>,
    pub plan: Plan,
    pub original_caller: crate::setup::home_capture::OriginalCallerFacts,
}
impl HomeSource {
    /// Called only on the preparation worker. Permission remains the original
    /// held folder pin; registration freezes no bytes and promises no export.
    pub fn register_original_source(
        &self,
        plan: &Plan,
        cancel: &AtomicBool,
    ) -> io::Result<Registered> {
        check_cancel(cancel)?;
        self.validate_offline_scope(plan)?;
        self.validate_original_roots(cancel)?;
        let home = self
            .granted_path
            .as_ref()
            .ok_or_else(|| changed("Original permission grant is missing"))?;
        let request = tempfile::tempdir()?;
        let (rendezvous, listener) = Rendezvous::create(request.path())?;
        let mut claim = OriginalRequest::new(home, plan.clone(), rendezvous)?;
        let _held = publish_original_request(request.path(), &mut claim)?;
        check_cancel(cancel)?;
        self.pin.validate()?;
        let child = std::process::Command::new("pkexec")
            .arg(registration_executable()?)
            .arg(crate::setup::home_capture_helper::ARG)
            .arg(request.path().join("request.json"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut controller =
            Controller::attach_with_rpc(child, request, &plan.operation_id, listener)?;
        let result = wait_registration(
            &mut controller,
            &claim.plan,
            self.original_roots.as_ref(),
            cancel,
        );
        controller.cancel_now();
        let cleanup = controller.cancel_and_wait();
        let registered = result?;
        cleanup?;
        check_cancel(cancel)?;
        self.validate_offline_scope(plan)?;
        self.validate_original_roots(cancel)?;
        Ok(registered)
    }
}
pub fn register_applications(
    home: &Path,
    pin: &PinnedDirectory,
    plan: &Plan,
    cancel: &AtomicBool,
) -> io::Result<Registered> {
    if plan.categories.home || !plan.categories.flatpak {
        return Err(changed("Applications-only original source scope differs"));
    }
    register_staging_copy(home, pin, plan, cancel)
}
/// Same canonical registration from the retained original directory grant.
pub fn register_staging_copy(
    home: &Path,
    pin: &PinnedDirectory,
    plan: &Plan,
    cancel: &AtomicBool,
) -> io::Result<Registered> {
    check_cancel(cancel)?;
    pin.validate()?;
    if !(plan.categories.home || plan.categories.flatpak)
        || plan.home_policy != crate::setup::migration_handoff::HomePolicy::Copy
        || pin.file.metadata()?.uid() != plan.home_uid()
    {
        return Err(changed("Original copy source scope differs"));
    }
    let current =
        crate::setup::migration_handoff::capture(home, &plan.operation_id, plan.categories)?;
    if current.home != plan.home {
        return Err(changed("Original copy source identity changed"));
    }
    let request = tempfile::tempdir()?;
    let (rendezvous, listener) = Rendezvous::create(request.path())?;
    let mut claim = if plan.categories.home {
        OriginalRequest::new(home, plan.clone(), rendezvous)?
    } else {
        OriginalRequest::new_applications(home, plan.clone(), rendezvous)?
    };
    let _held = publish_original_request(request.path(), &mut claim)?;
    check_cancel(cancel)?;
    pin.validate()?;
    let child = std::process::Command::new("pkexec")
        .arg(registration_executable()?)
        .arg(crate::setup::home_capture_helper::ARG)
        .arg(request.path().join("request.json"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut controller = Controller::attach_with_rpc(child, request, &plan.operation_id, listener)?;
    let result = wait_registration(&mut controller, &claim.plan, None, cancel);
    controller.cancel_now();
    let cleanup = controller.cancel_and_wait();
    let registered = result?;
    cleanup?;
    check_cancel(cancel)?;
    pin.validate()?;
    let current =
        crate::setup::migration_handoff::capture(home, &plan.operation_id, plan.categories)?;
    if current.home != plan.home {
        return Err(changed("Original copy source changed before ACK"));
    }
    Ok(registered)
}
fn wait_registration(
    controller: &mut Controller,
    plan: &Plan,
    roots: Option<&Arc<crate::setup::GrantedHomeRoots>>,
    cancel: &AtomicBool,
) -> io::Result<Registered> {
    let ticket = if let Some(roots) = roots {
        controller.register_original_mounted_async(roots.clone())?
    } else {
        controller.register_original_async()?
    };
    let mut registered = None;
    let mut released = false;
    loop {
        check_cancel(cancel)?;
        if let Some(status) = controller.poll()? {
            if status.expires_unix.is_some()
                || !matches!(status.state, State::Intended | State::Released)
            {
                return Err(changed(
                    "Original registration unexpectedly reported a capture epoch",
                ));
            }
            released |= status.state == State::Released;
        }
        if registered.is_none() {
            if let Some(reply) = ticket.poll()? {
                let (witness, original_caller, mounted) = match (roots, reply) {
                    (
                        None,
                        RpcReply::OriginalRegistered {
                            witness,
                            original_caller,
                        },
                    ) => (witness, original_caller, None),
                    (
                        Some(roots),
                        RpcReply::MountedOriginalRegistered {
                            generation,
                            witness,
                            original_caller,
                        },
                    ) if generation == roots.generation() => {
                        roots.validate(cancel)?;
                        (witness.primary().clone(), original_caller, Some(witness))
                    }
                    _ => return Err(changed("Original registration was not acknowledged")),
                };
                witness.validate_plan(plan)?;
                original_caller.validate(plan.home_uid())?;
                registered = Some(Registered {
                    plan: plan.clone(),
                    witness,
                    original_caller,
                    mounted,
                });
            }
        }
        if released && registered.is_some() {
            return registered.ok_or_else(|| changed("Original registration receipt missing"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn registration_executable() -> std::io::Result<std::path::PathBuf> {
    #[cfg(all(
        any(test, feature = "test-support"),
        target_os = "linux",
        feature = "native-capture-qualification"
    ))]
    if let Some(path) = crate::setup::qualification_registration::current()? {
        return Ok(path);
    }
    std::env::current_exe()
}
