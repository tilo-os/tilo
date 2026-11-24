//! The device's image model, served by this computer's one daemon: what a picture shows, the
//! words in it, the objects and pets it holds and the faces it groups, and the library analysis
//! that writes what each picture shows into the device's index.
//!
//! The service is a provider on the daemon's dispatch ([`Providers`]), so an app calls its typed
//! actions by the addresses it publishes (`vision:vision.image-model#describe`) whichever app the
//! call names, and reads the answer as the action's value: there is no connection of its own and
//! no second path for one language. The work is the device's: the model gateway, a picture's bytes
//! from the blob service this daemon serves, and the index the daemon keeps.
//!
//! [`Providers`]: tilo_workspace_service::workspaces::providers::Providers

use std::path::Path;
use std::sync::{Arc, Mutex};

use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, ResourceSearch,
};
use tilo_actions::{
    ActionInvoker, ActionOutput, InvocationId, InvocationUpdate, ResourceSpec,
};
use tilo_lock::MutexRecover as _;
use tilo_service::{Service, Started};
use tilo_vision_service::index::VisionIndex;
use tilo_vision_service::{AnalysisSink, ImageSource, LocalVision, VisionService};

use super::search::DeviceSearch;

/// The application whose grants decide the model's actions: the package it answers as.
pub(crate) const APP: &str = tilo_vision_service::APP;

/// The provider a call to the model's resources names.
pub(crate) const PROVIDER: &str = "so.tilo.vision";

/// The device's image model, as the daemon serves it to the apps it runs.
pub(crate) struct ImageModel {
    /// The service answers one call at a time; the provider is asked from the dispatch's threads.
    service: Mutex<VisionService>,
}

impl ImageModel {
    /// Start the model over the device's pictures: their bytes from the blob service at
    /// `blob_socket`, the index `search` answers, and `person` as the one whose pictures it reads.
    ///
    /// # Errors
    ///
    /// The blob service does not answer, or the device's search is not served yet.
    pub(crate) fn start(
        blob_socket: &Path,
        search: &Arc<DeviceSearch>,
        person: &str,
    ) -> Result<Self, String> {
        let search = search
            .search()
            .ok_or_else(|| "the device's search is not served yet".to_owned())?;
        let client = runtime_fs::blobs::BlobClient::connect_session(blob_socket)
            .map_err(|error| format!("the blob service did not answer: {error}"))?;
        let store = tilo_content::ContentStore::from_backend(Arc::new(
            tilo_content_device::DeviceBackend::new(client),
        ));
        let index = Arc::new(VisionIndex {
            search: search as Arc<dyn ResourceSearch>,
            caller: Caller::local_person(person),
        });
        let wake = tilo_service::LoopWake::default();
        let mut service = VisionService::new(
            Arc::new(LocalVision::local()),
            Arc::clone(&index) as Arc<dyn ImageSource>,
            index as Arc<dyn AnalysisSink>,
            wake.handle(),
        );
        service.own_person(person, &[]);
        service.lend_content(&mut |_app| store.clone());
        Ok(Self {
            service: Mutex::new(service),
        })
    }
}

impl ActionProvider for ImageModel {
    fn serves(&self, provider: &str) -> bool {
        provider == PROVIDER
    }

    fn published(&self) -> Vec<ResourceSpec> {
        self.service.lock_recover().resources()
    }

    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        let resources = self.service.lock_recover().resources();
        Pending::ready(Ok(CataloguePage::published(PROVIDER, &resources, &query)))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let id = request.invocation;
        let InvokeArgument::Json(arguments) = request.argument else {
            return Pending::ready(Err(ActionCallError::InvalidRequest {
                reason: "the image model reads its arguments as JSON".to_owned(),
            }));
        };
        let address = &request.target.address;
        let answer = self.service.lock_recover().invoke_as(
            &ActionInvoker::admitted(caller.acting()),
            &address.resource,
            &address.action,
            &arguments,
        );
        let update = match answer {
            Ok(Started::Done(output)) => InvocationUpdate::Succeeded {
                invocation: id,
                output,
            },
            // A run the model took lands its progress in the reading; the caller hears it was
            // taken rather than waiting on the library to finish.
            Ok(Started::Accepted) => InvocationUpdate::Succeeded {
                invocation: id,
                output: ActionOutput::default(),
            },
            Ok(Started::Later(_)) => {
                return Pending::ready(Err(ActionCallError::Unavailable {
                    reason: "the image model answers at once".to_owned(),
                }));
            }
            Err(error) => {
                return Pending::ready(Err(ActionCallError::from_api(&error)));
            }
        };
        Pending::ready(Ok(InvocationState::new(id, update).receipt()))
    }

    fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
        // Every answer is the receipt: the model runs nothing a caller follows.
        None
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownInvocation))
    }
}
