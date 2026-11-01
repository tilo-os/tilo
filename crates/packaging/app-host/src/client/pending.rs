//! Cooperative reads on the same authenticated app-host connection.

use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::task::Poll;

use super::{
    BlockingClient, CatalogueError, FlatpakStatus, InstalledPackage, ListedPackage, PackageAbout,
    catalogue, text, unexpected,
};
use crate::wire::{Answer, Operation, Resource, WirePinning};

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

    #[allow(
        clippy::future_not_send,
        reason = "the connection is shared by Rc on its current-thread runtime"
    )]
    fn begin_read(
        &self,
        resource: Resource,
    ) -> impl Future<Output = Result<Answer, String>> + use<> {
        self.connection.call(Operation::Read(resource))
    }

    /// Begin the same typed list read without waiting for its response.
    pub fn begin_list(&self) -> PendingRead<Vec<ListedPackage>> {
        let answer = self.begin_read(Resource::Packages);
        PendingRead::new(async move {
            let entries = match answer.await? {
                Answer::Packages(entries) => entries,
                other => return Err(unexpected(&other)),
            };
            let mut out = Vec::with_capacity(entries.len());
            for entry in entries {
                let manifest = tilo_app_package::Manifest::from_toml_str(&entry.manifest_text)
                    .map_err(|e| format!("installed manifest: {e}"))?;
                let id = tilo_app_package::PackageId::from_bytes(entry.id);
                let about = PackageAbout::from_full_manifest(&manifest);
                let package = InstalledPackage {
                    id: manifest.package.id,
                    version: manifest.package.version,
                    package_id_hex: id.to_hex(),
                    payload_root: entry.payload_root.into(),
                    manifest_path: entry.manifest_path.into(),
                    publisher_key_hex: manifest
                        .publisher
                        .identity_bytes()
                        .map(hex::encode)
                        .unwrap_or_default(),
                    pinned: entry.pinning == WirePinning::Pinned,
                    installed_at: 0,
                    last_used: entry.last_used_unix,
                    attested_by: String::new(),
                    cli_commands: manifest.cli.map(|cli| cli.commands).unwrap_or_default(),
                    runtime: manifest.package.runtime.unwrap_or_default(),
                    source: String::new(),
                    held: false,
                    payload_bytes: entry.size_bytes,
                    system: !entry.user_managed,
                };
                out.push(ListedPackage { package, about });
            }
            Ok(out)
        })
    }

    /// Begin the same typed repositories read without waiting for its response.
    pub fn begin_repositories(&self) -> PendingRead<String> {
        let answer = self.begin_read(Resource::Repositories);
        PendingRead::new(async move { text(answer.await?) })
    }

    /// Begin the same typed registry catalogue read without waiting for its response.
    pub fn begin_registry_catalogue(&self, registry: &str) -> PendingRead<String, CatalogueError> {
        let answer = self.begin_read(Resource::RegistryCatalogue {
            registry: registry.to_owned(),
        });
        PendingRead::new(async move { bounded_registry(catalogue(answer.await)?) })
    }

    /// Begin reading the catalogue the host last verified from `registry`,
    /// which needs no network and so answers while a fresh read is pending.
    pub fn begin_last_verified_catalogue(
        &self,
        registry: &str,
    ) -> PendingRead<String, CatalogueError> {
        let answer = self.begin_read(Resource::LastVerifiedCatalogue {
            registry: registry.to_owned(),
        });
        PendingRead::new(async move { bounded_registry(catalogue(answer.await)?) })
    }

    /// Begin the same typed flatpak catalogue read without waiting for its response.
    pub fn begin_flatpak_catalogue(
        &self,
        remote: &str,
        user_managed: bool,
    ) -> PendingRead<String, CatalogueError> {
        let answer = self.begin_read(Resource::FlatpakCatalogue {
            remote: remote.to_owned(),
            user_managed,
        });
        PendingRead::new(async move { catalogue(answer.await) })
    }

    /// Begin the same typed flatpak status read without waiting for its response.
    pub fn begin_flatpak_status(&self) -> PendingRead<FlatpakStatus> {
        let answer = self.begin_read(Resource::FlatpakStatus);
        PendingRead::new(async move {
            match answer.await? {
                Answer::FlatpakStatus {
                    available,
                    installed,
                    remotes,
                } => Ok(FlatpakStatus {
                    available,
                    installed,
                    remotes,
                }),
                other => Err(unexpected(&other)),
            }
        })
    }
}

fn bounded_registry(entries: String) -> Result<String, CatalogueError> {
    if entries.len() > 4 * 1024 * 1024 {
        return Err(CatalogueError::Failed(
            "verified repository catalogue exceeds the response limit".into(),
        ));
    }
    Ok(entries)
}
