//! The providers of the person's seat, served on the device's one dispatch over the shell's link
//! ([`runtime_collab::ShellClient::provide`]) under the ids apps have always called them by
//! ([`runtime_collab::wire::shell::SEAT_PROVIDERS`]): the workspace's activity as the shell shows
//! it, the machines the person planned, their settings on this computer, and the account it is
//! signed in to. Each needs what only this session holds, so the daemon admits a
//! call, under the app whose window offers it, and hands it here to carry out. What the daemon
//! answers itself (the person's devices, tickets and identities under `tilo.device`) it serves
//! alone: the session registers none of it beside it, nor the workspace's model providers, which
//! the person's services answer and list for the person on this device.
//!
//! What each offers is published to the daemon as it stands when served, and again whenever the
//! session says it moved ([`SeatServed::moved`]): Settings' published sections are the ones whose
//! steps change while the session runs.

use std::sync::Arc;

use runtime_collab::ShellClient;
use tilo_actions::remote::{ActionProvider, Admits, CataloguePage, CatalogueRow};
use tokio::sync::watch;

use crate::activity_provider::ActivityWire;
use crate::machine_steps::MachineSteps;
use crate::server_account::ServerAccountApi;
use crate::settings_steps::SettingsSteps;

/// The seat's providers, as the session made them.
pub(crate) struct SeatProviders {
    /// The workspace's activity as this session's shell shows it.
    pub(crate) activity: Arc<ActivityWire>,
    /// The machines planned in the person's Servers workspace.
    pub(crate) machines: Arc<MachineSteps>,
    /// The person's settings sections, where this session publishes them.
    pub(crate) settings: Option<Arc<SettingsSteps>>,
}

/// The seat's providers as the daemon reaches them: what tells it they offer something new.
#[derive(Clone)]
pub(crate) struct SeatServed(Arc<watch::Sender<u64>>);

impl SeatServed {
    /// What the seat's providers offer moved: each is published to the daemon again.
    pub(crate) fn moved(&self) {
        self.0
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }
}

/// Serve `parts` on the device's dispatch over `shell`, each under its id, for as long as the
/// link lasts.
pub(crate) fn serve_on(shell: &ShellClient, parts: SeatProviders) -> SeatServed {
    let (moved, _) = watch::channel(0_u64);
    let served = SeatServed(Arc::new(moved));
    let SeatProviders {
        activity,
        machines,
        settings,
    } = parts;
    provide(shell, &served, activity);
    provide(shell, &served, machines);
    provide(shell, &served, ServerAccountApi::session());
    if let Some(settings) = settings {
        provide(shell, &served, settings);
    }
    served
}

/// Serve `provider` on the device's dispatch over `shell`, publishing what it offers again each
/// time `served` says it moved.
fn provide<T: Admits + Send + Sync + 'static>(
    shell: &ShellClient,
    served: &SeatServed,
    provider: Arc<T>,
) {
    let runtime = match tilo_rt::try_app() {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::warn!(%error, provider = T::PROVIDER, "the device's dispatch reaches none of this provider's steps");
            return;
        }
    };
    let shell = shell.clone();
    let mut moved = served.0.subscribe();
    runtime.spawn(async move {
        let answers = Arc::clone(&provider) as Arc<dyn ActionProvider + Send + Sync>;
        let listing = match shell
            .provide(T::PROVIDER, &page(provider.as_ref(), 0), answers, tilo_action_calls::serve)
            .await
        {
            Ok(listing) => listing,
            Err(error) => {
                tracing::warn!(%error, provider = T::PROVIDER, "the device's dispatch reaches none of this provider's steps");
                return;
            }
        };
        while moved.changed().await.is_ok() {
            let generation = *moved.borrow_and_update();
            if let Err(error) = listing.publish(&page(provider.as_ref(), generation)).await {
                tracing::warn!(%error, provider = T::PROVIDER, "the daemon is handed this provider's steps no more");
                return;
            }
        }
    });
}

/// Every step `provider` offers now, as the daemon lists them.
fn page<T: Admits>(provider: &T, generation: u64) -> CataloguePage {
    CataloguePage {
        revision: generation.to_string(),
        rows: provider
            .rows()
            .into_iter()
            .map(|placed| CatalogueRow::for_placed_action(T::PROVIDER, T::APP, placed))
            .collect(),
        exhausted: true,
        ..CataloguePage::default()
    }
}
