//! The action door's route for offloadable invocations on one device
//! ([`tilo_actions::offload::ActionOffload`]), over its [`Offloader`].
//!
//! The door hands over an admitted invocation of an offloadable action with
//! the content its arguments name. The route runs it on the best-placed
//! other device only when every input is document media the invoking room
//! reaches on this device ([`DocumentMedia::contains`]): content the room's
//! apps could read themselves, never a path, never bytes another room put
//! here. The outputs are kept as that room's media
//! ([`DocumentMedia::store`]), so the app that asked reads them by the same
//! `tilo.content` addresses a local run returns. In every other case — a
//! room this device places no compute for, an input not here, no better
//! device, a worker failing or going away — the answer is
//! [`Offloaded::RunHere`] with the reason, and the door runs the app's own
//! handler.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use fs::document_media::DocumentMedia;
use runtime_fs::blobs::BlobScope;
use tilo_actions::offload::{ActionOffload, OffloadFuture, OffloadRequest, Offloaded};

use crate::offload::{Offloader, RanOn};

/// Local runs awaited at once before the oldest are forgotten.
const MAX_RUNNING_HERE: usize = 64;

/// Offloadable invocations of this device's rooms, run through its
/// [`Offloader`].
pub struct OffloadRoute {
    offloader: Arc<Offloader>,
    media: Arc<DocumentMedia>,
    /// Room → the relay workspace its compute jobs are placed in.
    workspaces: HashMap<String, u64>,
    /// Input bytes of requests answered [`Offloaded::RunHere`], until the
    /// door reports their local run.
    running_here: Mutex<HashMap<(String, Vec<String>), u64>>,
}

impl OffloadRoute {
    /// Route the offloadable invocations of the rooms in `workspaces`
    /// (room → relay workspace) through `offloader`, binding inputs from
    /// and keeping outputs in `media`.
    #[must_use]
    pub fn new(
        offloader: Arc<Offloader>,
        media: Arc<DocumentMedia>,
        workspaces: HashMap<String, u64>,
    ) -> Self {
        Self {
            offloader,
            media,
            workspaces,
            running_here: Mutex::default(),
        }
    }

    async fn route(&self, request: &OffloadRequest) -> Result<Offloaded, String> {
        let workspace_id = *self
            .workspaces
            .get(&request.room)
            .ok_or("this device places no compute for the room")?;
        let scope = BlobScope::new(request.room.clone(), request.app_id.clone())
            .map_err(|error| format!("the room's content is not addressable: {error}"))?;
        let mut inputs = Vec::with_capacity(request.inputs.len());
        for (index, hex) in request.inputs.iter().enumerate() {
            let mut digest = [0; 32];
            hex::decode_to_slice(hex, &mut digest)
                .map_err(|_| format!("input {index} is not a content hash"))?;
            if !self.media.contains(&scope, digest.into()).await {
                return Err(format!(
                    "input {index} is not the room's content on this device"
                ));
            }
            inputs.push(digest);
        }
        let key = (request.room.clone(), request.inputs.clone());
        if let Ok(bytes) = self.offloader.input_bytes(&inputs).await {
            let mut running = self
                .running_here
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            // A local run the door never reports (it failed) is forgotten
            // rather than kept forever.
            if running.len() >= MAX_RUNNING_HERE {
                running.clear();
            }
            running.insert(key.clone(), bytes);
        }
        let run = self
            .offloader
            .offload(workspace_id, &request.offload, &inputs)
            .await?;
        let RanOn::Worker(worker) = run.ran_on else {
            return Err("the run was not offloaded".into());
        };
        for digest in &run.outputs {
            let bytes = self
                .offloader
                .content(*digest)
                .await?
                .ok_or("an output is no longer here")?;
            self.media
                .store(&scope, bytes)
                .await
                .map_err(|error| format!("an output could not be kept: {error}"))?;
        }
        self.running_here
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&key);
        Ok(Offloaded::Ran {
            output: run.output,
            device: hex::encode(worker),
            elapsed: run.elapsed,
        })
    }
}

impl ActionOffload for OffloadRoute {
    fn offload(&self, request: OffloadRequest) -> OffloadFuture<'_> {
        Box::pin(async move {
            match self.route(&request).await {
                Ok(ran) => ran,
                Err(reason) => {
                    tracing::info!(
                        app = %request.offload.app,
                        entrypoint = %request.offload.entrypoint,
                        %reason,
                        "offloadable action runs here"
                    );
                    Offloaded::RunHere { reason }
                }
            }
        })
    }

    fn ran_here(&self, request: &OffloadRequest, elapsed: Duration) {
        let bytes = self
            .running_here
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&(request.room.clone(), request.inputs.clone()));
        if let Some(bytes) = bytes {
            self.offloader
                .record_local(&request.offload, elapsed, bytes);
        }
    }
}
