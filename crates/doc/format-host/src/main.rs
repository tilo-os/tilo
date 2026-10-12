//! Entry point for the format-router daemon. Reads the registry, builds the
//! router, spins up the inotify watcher, then enters the
//! Cap'n Proto accept loop on the Unix socket.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use format_host::{
    default_registry_path,
    router::{Router, load_into},
    runner, transport,
    watch::spawn_watcher,
};

fn main() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    let registry_path = default_registry_path()
        .ok_or_else(|| anyhow::anyhow!("no config directory for the provider registry"))?;
    let socket_path = transport::default_socket_path()
        .ok_or_else(|| anyhow::anyhow!("XDG_RUNTIME_DIR is not set"))?;
    tracing::info!(
        registry = %registry_path.display(),
        socket   = %socket_path.display(),
        "tilo-format-host starting"
    );

    // capnp-rpc + tokio-LocalSet (one task per connection, single-thread Vat).
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        let router = Router::new(runner::engine());
        // Best-effort initial load; an absent registry just means "no providers
        // loaded" and the host stays up.
        if let Err(e) = load_into(&router, &registry_path) {
            tracing::warn!(error = %e, "initial registry load failed; continuing empty");
        }
        spawn_watcher(router.clone(), registry_path.clone())?;
        let listener = transport::bind(&socket_path)?;
        transport::serve(listener, router).await
    })
}
