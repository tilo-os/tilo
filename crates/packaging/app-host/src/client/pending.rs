//! Cooperative reads on the same authenticated app-host connection.

use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::task::Poll;

use super::{
    BlockingClient, CatalogueError, FlatpakStatus, InstalledPackage, ListedPackage, PackageAbout,
    WirePinning, catalogue_refusal, parse_id, text_result,
};

type ReadFuture<T, E> = Pin<Box<dyn Future<Output = Result<T, E>>>>;

/// One typed read polled on the client that created it.
/// Dropping it stops observing that response without opening another transport.
#[must_use]
pub struct PendingRead<T, E = String> {
    future: Option<ReadFuture<T, E>>,
}

impl<T, E> PendingRead<T, E> {
    fn new(future: impl Future<Output = Result<T, E>> + 'static) -> Self {
        Self {
            future: Some(Box::pin(future)),
        }
    }

    /// Pump the existing connection once and return a completed response.
    /// Subsequent polls after completion return `None`.
    pub fn poll(&mut self, host: &BlockingClient) -> Option<Result<T, E>> {
        let future = self.future.as_mut()?;
        let outcome = host.local.block_on(&host.runtime, async {
            tokio::task::yield_now().await;
            poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx))).await
        });
        match outcome {
            Poll::Ready(result) => {
                self.future = None;
                Some(result)
            }
            Poll::Pending => None,
        }
    }
}

impl BlockingClient {
    pub(super) fn complete<T, E: From<String>>(&self, mut read: PendingRead<T, E>) -> Result<T, E> {
        let future = read
            .future
            .take()
            .ok_or_else(|| E::from("app-host read already completed".to_string()))?;
        self.local.block_on(&self.runtime, future)
    }

    /// Start bounded app-side catalogue work on this client's owned runtime.
    /// It has no host capability beyond the closure's ordinary sandbox.
    pub fn background_read<T: Send + 'static>(
        &self,
        read: impl FnOnce() -> Result<T, String> + Send + 'static,
    ) -> PendingRead<T> {
        let task = self.runtime.spawn_blocking(read);
        PendingRead::new(async move {
            task.await
                .map_err(|_| "catalogue read worker stopped".to_string())?
        })
    }

    /// Begin the same typed list read without waiting for its response.
    pub fn begin_list(&self) -> PendingRead<Vec<ListedPackage>> {
        let client = self.client.clone();
        PendingRead::new(async move {
            let response = client
                .list_request()
                .send()
                .promise
                .await
                .map_err(|error| error.to_string())?;
            let result = response.get().map_err(|e| e.to_string())?;
            let entries = result.get_entries().map_err(|e| e.to_string())?;
            let mut out = Vec::with_capacity(entries.len() as usize);
            for entry in entries {
                let manifest_text = entry
                    .get_manifest_text()
                    .map_err(|e| e.to_string())?
                    .to_string()
                    .map_err(|e| e.to_string())?;
                let manifest = tilo_app_package::Manifest::from_toml_str(&manifest_text)
                    .map_err(|e| format!("installed manifest: {e}"))?;
                let text = |value: Result<capnp::text::Reader<'_>, capnp::Error>| {
                    text_result(value).map(std::path::PathBuf::from)
                };
                let id = parse_id(entry.get_id().map_err(|e| e.to_string())?)?;
                let about = PackageAbout::from_full_manifest(&manifest);
                let package = InstalledPackage {
                    id: manifest.package.id,
                    version: manifest.package.version,
                    package_id_hex: id.to_hex(),
                    payload_root: text(entry.get_payload_root())?,
                    manifest_path: text(entry.get_manifest_path())?,
                    publisher_key_hex: manifest
                        .publisher
                        .identity_bytes()
                        .map(hex::encode)
                        .unwrap_or_default(),
                    pinned: entry.get_pinning().map_err(|e| e.to_string())? == WirePinning::Pinned,
                    installed_at: 0,
                    last_used: entry.get_last_used_unix(),
                    attested_by: String::new(),
                    cli_commands: manifest.cli.map(|cli| cli.commands).unwrap_or_default(),
                    runtime: manifest.package.runtime.unwrap_or_default(),
                    source: String::new(),
                    held: false,
                    payload_bytes: entry.get_size_bytes(),
                    system: !entry.get_user_managed(),
                };
                out.push(ListedPackage { package, about });
            }
            Ok(out)
        })
    }
    /// Begin the same typed repositories read without waiting for its response.
    pub fn begin_repositories(&self) -> PendingRead<String> {
        let client = self.client.clone();
        PendingRead::new(async move {
            let request = client.repositories_request();
            text_result(
                request
                    .send()
                    .promise
                    .await
                    .map_err(|error| error.to_string())?
                    .get()
                    .map_err(|e| e.to_string())?
                    .get_json(),
            )
        })
    }
    /// Begin the same typed registry catalogue read without waiting for its response.
    pub fn begin_registry_catalogue(&self, registry: &str) -> PendingRead<String, CatalogueError> {
        let client = self.client.clone();
        let registry = registry.to_owned();
        PendingRead::new(async move {
            let mut request = client.registry_catalogue_request();
            request.get().set_registry(&registry);
            let response = request
                .send()
                .promise
                .await
                .map_err(|error| failed(&error))?;
            let results = response.get().map_err(|error| failed(&error))?;
            if let Some(refused) = catalogue_refusal(results.get_refusal(), results.get_detail()) {
                return Err(refused);
            }
            let entries = text_result(results.get_entries())?;
            if entries.len() > 4 * 1024 * 1024 {
                return Err(CatalogueError::Failed(
                    "verified repository catalogue exceeds the response limit".into(),
                ));
            }
            Ok(entries)
        })
    }
    /// Begin reading the catalogue the host last verified from `registry`,
    /// which needs no network and so answers while a fresh read is pending.
    pub fn begin_last_verified_catalogue(
        &self,
        registry: &str,
    ) -> PendingRead<String, CatalogueError> {
        let client = self.client.clone();
        let registry = registry.to_owned();
        PendingRead::new(async move {
            let mut request = client.last_verified_catalogue_request();
            request.get().set_registry(&registry);
            let response = request
                .send()
                .promise
                .await
                .map_err(|error| failed(&error))?;
            let results = response.get().map_err(|error| failed(&error))?;
            if let Some(refused) = catalogue_refusal(results.get_refusal(), results.get_detail()) {
                return Err(refused);
            }
            let entries = text_result(results.get_entries())?;
            if entries.len() > 4 * 1024 * 1024 {
                return Err(CatalogueError::Failed(
                    "verified repository catalogue exceeds the response limit".into(),
                ));
            }
            Ok(entries)
        })
    }
    /// Begin the same typed flatpak catalogue read without waiting for its response.
    pub fn begin_flatpak_catalogue(
        &self,
        remote: &str,
        user_managed: bool,
    ) -> PendingRead<String, CatalogueError> {
        let client = self.client.clone();
        let remote = remote.to_owned();
        PendingRead::new(async move {
            let mut request = client.flatpak_catalogue_request();
            request.get().set_remote(&remote);
            request.get().set_user_managed(user_managed);
            let response = request
                .send()
                .promise
                .await
                .map_err(|error| failed(&error))?;
            let results = response.get().map_err(|error| failed(&error))?;
            if let Some(refused) = catalogue_refusal(results.get_refusal(), results.get_detail()) {
                return Err(refused);
            }
            Ok(text_result(results.get_rows())?)
        })
    }
    /// Begin the same typed flatpak status read without waiting for its response.
    pub fn begin_flatpak_status(&self) -> PendingRead<FlatpakStatus> {
        let client = self.client.clone();
        PendingRead::new(async move {
            let response = client
                .flatpak_status_request()
                .send()
                .promise
                .await
                .map_err(|error| error.to_string())?;
            let result = response.get().map_err(|e| e.to_string())?;
            Ok(FlatpakStatus {
                available: result.get_available(),
                installed: text_result(result.get_installed())?,
                remotes: text_result(result.get_remotes())?,
            })
        })
    }
}

fn failed(error: &capnp::Error) -> CatalogueError {
    CatalogueError::Failed(error.to_string())
}
