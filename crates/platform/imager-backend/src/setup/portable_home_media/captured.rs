//! Original permission and immutable epoch remain separate capabilities.
use super::*;
use crate::setup::home_capture::{
    Command, Grant, Rendezvous, Response, RpcReply, Secret, State,
    controller::{Controller, Ticket},
};
use crate::setup::home_capture_helper::{Action, Request};
use std::{process::Stdio, sync::Mutex, time::Duration};

pub(super) struct Captured {
    controller: Mutex<Option<Controller>>,
    summary: ExportSummary,
    entries: usize,
    reservation: u64,
    released: AtomicBool,
}
impl Captured {
    pub(super) fn review(home: &Path, cancel: &AtomicBool) -> io::Result<Option<Self>> {
        let source = PinnedDirectory::open_absolute(home)?;
        let readable = durable::readable_directory(&source.file)?;
        match fs_native::btrfs::subvolume_identity(std::os::fd::AsFd::as_fd(&readable)) {
            Ok(_) => (),
            Err(error) if error.raw_os_error() == Some(libc::ENOTTY) => return Ok(None),
            Err(error) => return Err(error),
        }
        if !crate::setup::home_capture::provider_supported()? {
            return Ok(None);
        }
        check_cancel(cancel)?;
        let operation = uuid::Uuid::new_v4().to_string();
        let grant = Grant::review(home, &operation)?;
        let request = tempfile::tempdir()?;
        let (channel, listener) = Rendezvous::create(request.path())?;
        let claim = Request {
            schema: 1,
            action: Action::Capture,
            grant,
            channel: Some(channel),
        };
        let _held = crate::setup::home_capture_helper::publish_request(request.path(), &claim)?;
        source.validate()?;
        let child = std::process::Command::new("pkexec")
            .arg(std::env::current_exe()?)
            .arg(crate::setup::home_capture_helper::ARG)
            .arg(request.path().join("request.json"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut controller = Controller::attach_with_rpc(child, request, &operation, listener)?;
        let ticket = controller.submit(Command::Review, None)?;
        let result = wait(&mut controller, &ticket, cancel);
        let value = match result {
            Ok(RpcReply::Response {
                response:
                    Response::Review {
                        files,
                        bytes,
                        links,
                        runtime_sockets,
                        sealed_reservation,
                        entries,
                    },
            }) => Self {
                controller: Mutex::new(Some(controller)),
                summary: ExportSummary {
                    files,
                    bytes,
                    links,
                    special_files: 0,
                    runtime_sockets,
                },
                entries,
                reservation: sealed_reservation,
                released: AtomicBool::new(false),
            },
            Ok(_) => {
                controller.cancel_now();
                return Err(changed("Capture review response differs"));
            }
            Err(error) => {
                controller.cancel_now();
                return Err(error);
            }
        };
        source.validate()?;
        Ok(Some(value))
    }
    pub(super) fn available(&self) -> bool {
        !self.released.load(Ordering::Acquire)
    }
    pub(super) fn release(&self) {
        self.released.store(true, Ordering::Release);
        let controller = self
            .controller
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(mut controller) = controller {
            controller.cancel_now();
            std::thread::spawn(move || {
                let _ = controller.cancel_and_wait();
            });
        }
    }
    pub(super) fn summary(&self) -> ExportSummary {
        self.summary.clone()
    }
    pub(super) fn entries(&self) -> usize {
        self.entries
    }
    pub(super) fn reservation(&self) -> u64 {
        self.reservation
    }
    pub(super) fn validate(&self, cancel: &AtomicBool) -> io::Result<()> {
        self.validate_epoch(cancel).map_err(source_unavailable)
    }
    fn validate_epoch(&self, cancel: &AtomicBool) -> io::Result<()> {
        let mut held = self
            .controller
            .lock()
            .map_err(|_| changed("Capture controller unavailable"))?;
        let controller = held
            .as_mut()
            .ok_or_else(|| changed("Capture has been released"))?;
        let ticket = controller.validate_async()?;
        match wait(controller, &ticket, cancel)? {
            RpcReply::Validated => Ok(()),
            _ => Err(changed(
                "Captured home is unavailable; review the files again",
            )),
        }
    }
    pub(super) fn export_reporting(
        &self,
        media: &MediaGrant,
        operation: &str,
        setup_binding: &str,
        password: Zeroizing<Vec<u8>>,
        cancel: &AtomicBool,
        observer: &mut portable_transfer::Observer<'_>,
    ) -> io::Result<Expectation> {
        let command = Command::Export {
            export_operation: operation.into(),
            setup_binding: setup_binding.into(),
            password: Secret::new(password),
        };
        match self.run(command, media, cancel, observer)? {
            Response::Export { expected } => {
                expected.validate()?;
                if expected.operation != operation || expected.setup_binding != setup_binding {
                    return Err(changed("Captured export binding differs"));
                }
                Ok(expected)
            }
            _ => Err(changed("Captured export response differs")),
        }
    }
    pub(super) fn validate_existing_reporting(
        &self,
        media: &MediaGrant,
        expected: &Expectation,
        setup_binding: &str,
        password: &[u8],
        cancel: &AtomicBool,
        observer: &mut portable_transfer::Observer<'_>,
    ) -> io::Result<()> {
        expected.validate()?;
        if expected.setup_binding != setup_binding {
            return Err(changed("Captured package binding differs"));
        }
        let command = Command::ValidateExisting {
            export_operation: expected.operation.clone(),
            expected: expected.clone(),
            setup_binding: setup_binding.into(),
            password: Secret::new(Zeroizing::new(password.to_vec())),
        };
        match self.run(command, media, cancel, observer)? {
            Response::Validated => Ok(()),
            _ => Err(changed("Captured package verification differs")),
        }
    }
    pub(super) fn run(
        &self,
        command: Command,
        media: &MediaGrant,
        cancel: &AtomicBool,
        observer: &mut portable_transfer::Observer<'_>,
    ) -> io::Result<Response> {
        let output = Arc::new(MediaGrant::from_reviewed_pin(media.pin.clone())?);
        let mut held = self
            .controller
            .lock()
            .map_err(|_| changed("Capture controller unavailable"))?;
        let controller = held
            .as_mut()
            .ok_or_else(|| changed("Capture has been released"))?;
        let validation = controller.validate_async().map_err(source_unavailable)?;
        if !matches!(wait(controller, &validation, cancel)?, RpcReply::Validated) {
            return Err(source_unavailable(changed(
                "Capture validation did not complete",
            )));
        }
        let ticket = controller.submit(command, Some(output))?;
        match wait_reporting(controller, &ticket, cancel, observer)? {
            RpcReply::Response { response } => Ok(response),
            _ => Err(changed("Captured home export did not complete")),
        }
    }
}
fn wait(controller: &mut Controller, ticket: &Ticket, cancel: &AtomicBool) -> io::Result<RpcReply> {
    wait_reporting(controller, ticket, cancel, &mut |_| Ok(()))
}
fn wait_reporting(
    controller: &mut Controller,
    ticket: &Ticket,
    cancel: &AtomicBool,
    observer: &mut portable_transfer::Observer<'_>,
) -> io::Result<RpcReply> {
    loop {
        if cancel.load(Ordering::Acquire) {
            controller.cancel_now();
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Capture operation cancelled",
            ));
        }
        if let Some(status) = controller.poll().map_err(source_unavailable)? {
            validate_status(&status)?;
        }
        if let Some(progress) = ticket.progress()? {
            if let Err(error) = observer(progress) {
                controller.cancel_now();
                return Err(error);
            }
        }
        if let Some(value) = ticket.poll()? {
            if matches!(value, RpcReply::Released) {
                return Err(source_unavailable(changed(
                    "Capture epoch has been released",
                )));
            }
            return Ok(value);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
impl Drop for Captured {
    fn drop(&mut self) {
        let held = self
            .controller
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(mut controller) = held {
            controller.cancel_now();
            std::thread::spawn(move || {
                let _ = controller.cancel_and_wait();
            });
        }
    }
}

fn validate_status(status: &crate::setup::home_capture::Status) -> io::Result<()> {
    if status.state == State::Released || status.state == State::Releasing {
        return Err(source_unavailable(changed(
            "Capture epoch has been released",
        )));
    }
    if let Some(expiry) = status.expires_unix {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_secs();
        if now >= expiry {
            return Err(source_unavailable(changed("Capture epoch has expired")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn released_or_expired_status_requires_review_without_claiming_files_changed() {
        let mut status = crate::setup::home_capture::Status {
            operation: uuid::Uuid::new_v4().to_string(),
            state: State::Released,
            expires_unix: None,
        };
        let released = validate_status(&status).unwrap_err();
        assert_eq!(
            source_review_error(&released),
            Some("installer-source-unavailable")
        );
        assert!(!source_changed(&released));
        status.state = State::Ready;
        status.expires_unix = Some(0);
        assert_eq!(
            source_review_error(&validate_status(&status).unwrap_err()),
            Some("installer-source-unavailable")
        );
        assert!(source_review_error(&io::Error::from_raw_os_error(libc::ENOSPC)).is_none());
        assert!(source_review_error(&changed("Ciphertext binding differs")).is_none());
        assert!(
            source_review_error(&source_unavailable(io::Error::new(
                io::ErrorKind::Interrupted,
                "Cancelled"
            )))
            .is_none()
        );
    }
}
