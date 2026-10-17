//! Explicit preparation-worker producer through the existing owned helper.
//! Registering original groups creates no writable app projection/capability.
use super::*;
use crate::setup::{
    home_capture::{
        Rendezvous, RetainedCallerDeclaration, RpcReply, State, controller::Controller,
    },
    home_capture_helper::{RetainedRequest, publish_retained_request},
};
use std::{process::Stdio, sync::atomic::AtomicBool, time::Duration};
impl MigrationIntent {
    pub fn register_retained_caller(
        &self,
        setup_binding: &str,
        person: &[u8; 32],
        cancel: &AtomicBool,
    ) -> io::Result<RetainedCallerDeclaration> {
        crate::setup::migration::check_cancel(cancel)?;
        let plan = self.for_media()?;
        let home = self.grant.source.decode().map_err(io::Error::other)?;
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()?;
        let (channel, listener) = Rendezvous::create(directory.path()).map_err(|error| {
            io::Error::new(error.kind(), format!("retained caller rendezvous: {error}"))
        })?;
        let request =
            RetainedRequest::new(&home, plan.clone(), channel, setup_binding.into(), *person)?;
        let _request = publish_retained_request(directory.path(), &request)?;
        self.source_pin.validate()?;
        let child = std::process::Command::new("pkexec")
            .arg(registration_executable()?)
            .arg(crate::setup::home_capture_helper::ARG)
            .arg(directory.path().join("request.json"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(registration_stderr())
            .spawn()?;
        let mut controller =
            Controller::attach_with_rpc(child, directory, &plan.operation_id, listener)?;
        let result = (|| {
            let ticket = controller.register_retained_caller_async(self.source_pin.clone())?;
            let mut declaration = None;
            let mut released = false;
            loop {
                crate::setup::migration::check_cancel(cancel)?;
                if let Some(status) = controller.poll().map_err(|error| {
                    io::Error::new(error.kind(), format!("retained helper status: {error}"))
                })? {
                    if status.expires_unix.is_some()
                        || !matches!(status.state, State::Intended | State::Released)
                    {
                        return Err(error(
                            "Retained registration unexpectedly created a capture",
                        ));
                    }
                    released |= status.state == State::Released;
                }
                if declaration.is_none()
                    && let Some(reply) = ticket.poll().map_err(|error| {
                        io::Error::new(error.kind(), format!("retained helper RPC: {error}"))
                    })?
                {
                    match reply {
                        RpcReply::RetainedCallerRegistered {
                            declaration: returned,
                        } => {
                            returned.validate_binding(&plan, setup_binding, person)?;
                            declaration = Some(returned);
                        }
                        _ => return Err(error("Retained original groups were not acknowledged")),
                    }
                }
                if released && let Some(declaration) = declaration {
                    return Ok(declaration);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        })();
        controller.cancel_now();
        let stopped = controller.cancel_and_wait();
        #[cfg(all(test, target_os = "linux"))]
        if std::env::var_os("TILO_NATIVE_SHARED_FILES_ACTOR_ROOT").is_some() {
            eprintln!(
                "native retained result: {:?}; retirement: {:?}",
                result.as_ref().map(|_| ()),
                stopped
            );
        }
        // Always retire the owned helper before returning a receipt or error.
        let declaration = result?;
        stopped?;
        crate::setup::migration::check_cancel(cancel)?;
        if self.for_media()? != plan {
            return Err(error("Retained selection changed after registration"));
        }
        Ok(declaration)
    }
}

fn registration_stderr() -> Stdio {
    #[cfg(all(test, target_os = "linux"))]
    if std::env::var_os("TILO_NATIVE_SHARED_FILES_ACTOR_ROOT").is_some() {
        // The native owner bounds the actor's combined diagnostic output.
        return Stdio::inherit();
    }
    Stdio::null()
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
