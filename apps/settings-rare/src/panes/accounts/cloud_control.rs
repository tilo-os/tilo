//! What the cloud account service says about its mounts, and the requests
//! Settings sends it.
//!
//! The storage daemon's cloud service mounts each enabled file account at
//! `~/Remote/<label>` and serves the mounts' control on its socket
//! (`runtime_fs::cloud`). Settings offers sync now, pause and resume on an
//! account exactly while the service lists it as mounted, so an action
//! disappears rather than fails. The listing is read on a worker, never on
//! the window's thread, and carries no credential.

use std::path::PathBuf;
use std::time::Duration;

use runtime_fs::cloud::{CloudClient, CloudMount, CloudRequest};

use crate::channel::{Receiver, TryRecvError};

/// How often an open Settings window asks the service again.
const REFRESH: Duration = Duration::from_secs(10);

pub(crate) struct CloudMounts {
    /// Where the service listens; `None` asks nothing (samples, tests).
    socket: Option<PathBuf>,
    /// The mounts the service last listed; `None` before it answered or
    /// while it is not running.
    known: Option<Vec<CloudMount>>,
    listing: Option<Receiver<Option<Vec<CloudMount>>>>,
    next: Option<ui::time::Instant>,
}

impl CloudMounts {
    /// The person's own service, at its usual socket.
    pub(crate) fn live() -> Self {
        Self::at(runtime_fs::cloud::default_socket())
    }

    /// The service at `socket`, or none.
    pub(crate) fn at(socket: Option<PathBuf>) -> Self {
        Self {
            socket,
            known: None,
            listing: None,
            next: None,
        }
    }

    /// A fixed listing that is never refreshed (tests).
    #[cfg(test)]
    pub(crate) fn known(mounts: Vec<CloudMount>) -> Self {
        Self {
            known: Some(mounts),
            ..Self::at(None)
        }
    }

    /// `account`'s mount, while the service lists it.
    pub(crate) fn mount(&self, account: &str) -> Option<&CloudMount> {
        self.known
            .as_ref()?
            .iter()
            .find(|mount| mount.account == account)
    }

    /// Ask the service again at the next poll; a listing already on its
    /// way may predate the change and is dropped.
    pub(crate) fn invalidate(&mut self) {
        self.listing = None;
        self.next = None;
    }

    /// Take in a finished listing and start the next one when due; whether
    /// what is known changed.
    pub(crate) fn poll(&mut self) -> bool {
        if let Some(listing) = &self.listing {
            let answer = match listing.try_recv() {
                Ok(answer) => answer,
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => None,
            };
            self.listing = None;
            self.next = Some(ui::time::now() + REFRESH);
            let changed = answer != self.known;
            self.known = answer;
            return changed;
        }
        let due = self.next.is_none_or(|next| ui::time::now() >= next);
        if let (true, Some(socket)) = (due, self.socket.clone()) {
            let (answer, listing) = crate::channel::channel();
            let started = crate::device_read::thread(
                "settings-cloud-mounts",
                Box::new(move || {
                    let mounts = CloudClient::connect(&socket)
                        .and_then(|mut client| client.mounts())
                        .ok();
                    let _ = answer.send(mounts);
                }),
            );
            if started.is_ok() {
                self.listing = Some(listing);
            } else {
                self.next = Some(ui::time::now() + REFRESH);
            }
        }
        false
    }

    /// `request` as work for a worker thread.
    ///
    /// # Errors
    ///
    /// Why the service cannot be asked.
    pub(crate) fn request(
        &self,
        request: CloudRequest,
    ) -> Result<impl FnOnce() -> Result<(), String> + Send + 'static, String> {
        let socket = self
            .socket
            .clone()
            .ok_or_else(|| crate::i18n::t("accounts-cloud-unavailable"))?;
        Ok(move || {
            CloudClient::connect(&socket)
                .and_then(|mut client| client.control(&request))
                .map_err(|error| error.to_string())
        })
    }
}
