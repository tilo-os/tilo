//! The action door's route for offloadable invocations on one device
//! ([`tilo_actions::offload::ActionOffload`]), over its [`Offloader`].
//!
//! The door hands over an admitted invocation of an offloadable action with
//! the content its arguments name. The route runs it on the best-placed
//! other device only when every input is document media the invoking room
//! reaches on this device, read through the invoking app's scope
//! ([`RoomMedia`](crate::offload::RoomMedia)): content the room's apps could
//! read themselves, never a path, never bytes another room put here. The
//! outputs are kept as that room's media, so the app that asked reads them
//! by the same `tilo.content` addresses a local run returns. In every other case — a
//! room this device places no compute for, an input not here, no better
//! device, a worker failing or going away — the answer is
//! [`Offloaded::RunHere`] with the reason, and the door runs the app's own
//! handler.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use runtime_fs::blobs::BlobScope;
use tilo_actions::offload::{ActionOffload, OffloadFuture, OffloadRequest, Offloaded};

use crate::offload::{Offloader, RanOn};
use tilo_lock::MutexRecover as _;

/// Local runs awaited at once before the oldest are forgotten.
const MAX_RUNNING_HERE: usize = 64;

/// Offloadable invocations of this device's rooms, run through its
/// [`Offloader`].
pub struct OffloadRoute {
    offloader: Arc<Offloader>,
    /// Room → the relay workspace its compute jobs are placed in.
    workspaces: HashMap<String, u64>,
    /// Input bytes of requests answered [`Offloaded::RunHere`], until the
    /// door reports their local run.
    running_here: Mutex<HashMap<(String, Vec<String>), u64>>,
}

impl OffloadRoute {
    /// Route the offloadable invocations of the rooms in `workspaces`
    /// (room → relay workspace) through `offloader`, binding inputs from
    /// and keeping outputs in the rooms' media its content reaches.
    #[must_use]
    pub fn new(offloader: Arc<Offloader>, workspaces: HashMap<String, u64>) -> Self {
        Self {
            offloader,
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
            inputs.push(digest);
        }
        // Every input is the room's media here, or nothing leaves.
        let bytes = self.offloader.input_bytes(&scope, &inputs).await?;
        let key = (request.room.clone(), request.inputs.clone());
        {
            let mut running = self.running_here.lock_recover();
            // A local run the door never reports (it failed) is forgotten
            // rather than kept forever.
            if running.len() >= MAX_RUNNING_HERE {
                running.clear();
            }
            running.insert(key.clone(), bytes);
        }
        // The outputs come back as the room's media.
        let run = self
            .offloader
            .offload(workspace_id, &scope, &request.offload, &inputs)
            .await?;
        let RanOn::Worker(worker) = run.ran_on else {
            return Err("the run was not offloaded".into());
        };
        self.running_here.lock_recover().remove(&key);
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
            .lock_recover()
            .remove(&(request.room.clone(), request.inputs.clone()));
        if let Some(bytes) = bytes {
            self.offloader
                .record_local(&request.offload, elapsed, bytes);
        }
    }
}
