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
    pub(in crate::setup) fn register_retained_caller(
        &self,
        setup_binding: &str,
        person: &[u8; 32],
        cancel: &AtomicBool,
    ) -> io::Result<RetainedCallerDeclaration> {
        super::super::migration::check_cancel(cancel)?;
        let plan = self.for_media()?;
        let home = self.grant.source.decode().map_err(io::Error::other)?;
        let directory = tempfile::tempdir()?;
        let (channel, listener) = Rendezvous::create(directory.path())?;
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
            .stderr(Stdio::null())
            .spawn()?;
        let mut controller =
            Controller::attach_with_rpc(child, directory, &plan.operation_id, listener)?;
        let result = (|| {
            let ticket = controller.register_retained_caller_async(self.source_pin.clone())?;
            let mut declaration = None;
            let mut released = false;
            loop {
                super::super::migration::check_cancel(cancel)?;
                if let Some(status) = controller.poll()? {
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
                    && let Some(reply) = ticket.poll()?
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
                if released && declaration.is_some() {
                    return Ok(declaration.unwrap());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        })();
        controller.cancel_now();
        let stopped = controller.cancel_and_wait();
        // Always retire the owned helper before returning a receipt or error.
        let declaration = result?;
        stopped?;
        super::super::migration::check_cancel(cancel)?;
        if self.for_media()? != plan {
            return Err(error("Retained selection changed after registration"));
        }
        Ok(declaration)
    }
}

fn registration_executable() -> std::io::Result<std::path::PathBuf> {
    #[cfg(all(test, target_os = "linux", feature = "native-capture-qualification"))]
    if let Some(path) = crate::setup::qualification_registration::current()? {
        return Ok(path);
    }
    std::env::current_exe()
}
