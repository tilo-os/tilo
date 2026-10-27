//! Public files this device keeps one copy of for every app and workspace
//! (a book catalogue), through the app's capability. The daemon decides
//! which files exist and where they come from; an app keeps one by name,
//! reads it, and releases it. A released copy stays on the device until
//! the device needs its room.

use std::io;

use runtime_fs::blobs::{BlobClient, BlobError, ReadOutcome};

use crate::PublicState;

/// One public file, by the name the daemon knows it under.
#[derive(Clone)]
pub struct PublicCopy {
    client: BlobClient,
    name: &'static str,
}

fn io_error(error: &BlobError) -> io::Error {
    io::Error::other(error.to_string())
}

impl PublicCopy {
    pub(crate) fn new(client: BlobClient, name: &'static str) -> Self {
        Self { client, name }
    }

    /// Keep a copy on this device for this app; the first keeper's fetches it.
    ///
    /// # Errors
    ///
    /// The daemon knows no public file by this name, or is out of reach.
    pub fn keep(&self) -> io::Result<()> {
        self.client
            .public_keep(self.name)
            .map_err(|error| io_error(&error))
    }

    /// This app no longer keeps it.
    ///
    /// # Errors
    ///
    /// As [`Self::keep`].
    pub fn release(&self) -> io::Result<()> {
        self.client
            .public_release(self.name)
            .map_err(|error| io_error(&error))
    }

    /// The current copy's hash, when it is wholly here: what tells a reader
    /// that keeps something built from the copy whether it changed, without
    /// reading it.
    ///
    /// # Errors
    ///
    /// As [`Self::keep`].
    pub fn here(&self) -> io::Result<Option<String>> {
        let read = self
            .client
            .public_read(self.name, 0, 0)
            .map_err(|error| io_error(&error))?;
        Ok(match read.outcome {
            ReadOutcome::Ready { .. } => read.hash,
            ReadOutcome::Pending | ReadOutcome::Unavailable => None,
        })
    }

    /// The current copy.
    ///
    /// # Errors
    ///
    /// As [`Self::keep`], or the bytes did not match their hash.
    pub fn read(&self) -> io::Result<PublicState> {
        Ok(
            match self
                .client
                .public_read_all(self.name)
                .map_err(|error| io_error(&error))?
            {
                Ok((bytes, hash)) => PublicState::Here { bytes, hash },
                Err(ReadOutcome::Pending) => PublicState::Arriving,
                Err(_) => PublicState::NotKept,
            },
        )
    }
}
