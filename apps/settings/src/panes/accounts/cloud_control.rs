//! What the cloud account service says about its mounts, and the requests.
//! Settings sends it.

use std::path::PathBuf;
use std::time::Duration;

use runtime_fs::cloud::{CloudClient, CloudMount, CloudRequest};

use crate::channel::{Receiver, TryRecvError};

const REFRESH: Duration = Duration::from_secs(10);

pub(crate) struct CloudMounts {
    socket: Option<PathBuf>,
    /// The mounts the service last listed; `None` before it answered or
    /// while it is not running.
    known: Option<Vec<CloudMount>>,
    listing: Option<Receiver<Option<Vec<CloudMount>>>>,
    next: Option<ui::time::Instant>,
}

impl CloudMounts {
    pub(crate) fn live() -> Self {
        Self::at(runtime_fs::cloud::default_socket())
    }

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
