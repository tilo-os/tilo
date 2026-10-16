//! Cap'n Proto `FormatRouter::Server` implementation. Routes RPC calls onto
//! the shared [`Router`]. The transport (Unix socket listener) lives in
//! `transport.rs`.
//!
//! capnp 0.26 generates async server methods on `Rc<Self>` returning an
//! `impl Future<Output = Result<(), capnp::Error>>`; we route everything to a
//! `tokio::task::spawn_blocking` so provider calls (which can run until their
//! fuel budget is spent) don't stall the capnp event loop.

use capnp::Error;

use crate::format_router_capnp::format_router;
use crate::router::Router;

pub struct FormatRouterService {
    router: Router,
}

impl FormatRouterService {
    pub fn new(router: Router) -> Self {
        Self { router }
    }
}

/// Run a provider call on the blocking pool, so a call that runs until its
/// fuel budget is spent doesn't stall the capnp event loop.
async fn blocking<T: Send + 'static>(
    call: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Error> {
    tokio::task::spawn_blocking(call)
        .await
        .map_err(|e| Error::failed(format!("join: {e}")))
}

// capnp-rpc's generated `Results`/`Params` types are `Rc`-based and never
// `Send` by design — this whole server runs on capnp-rpc's own
// single-threaded local executor, with `spawn_blocking` the only bridge to a
// worker thread. There is no Send-safe alternative shape.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc's Rc-based Results/Params are never Send; this runs on its local executor"
)]
impl format_router::Server for FormatRouterService {
    fn detect(
        self: capnp::capability::Rc<Self>,
        params: format_router::DetectParams,
        mut results: format_router::DetectResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        let router = self.router.clone();
        async move {
            let p = params.get()?;
            let head = p.get_head()?.to_vec();
            let hint = p.get_hint()?.to_str()?.to_string();
            let (detected, provider_id) = blocking(move || router.detect(&head, &hint)).await?;
            let mut out = results.get().init_detected();
            let mut conf = out.reborrow().init_confidence();
            conf.set_tier(detected.confidence.tier);
            conf.set_score(detected.confidence.score);
            let mime = detected.mime.unwrap_or_default();
            out.set_mime(mime.as_str());
            out.set_provider_id(provider_id.as_str());
            Ok(())
        }
    }

    fn import_as(
        self: capnp::capability::Rc<Self>,
        params: format_router::ImportAsParams,
        mut results: format_router::ImportAsResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        let router = self.router.clone();
        async move {
            let p = params.get()?;
            let bytes = p.get_bytes()?.to_vec();
            let provider_id = p.get_provider_id()?.to_str()?.to_string();
            let out = blocking(move || router.import_as(&bytes, &provider_id))
                .await?
                .map_err(|e| Error::failed(e.to_string()))?;
            results.get().set_canonical(&out);
            Ok(())
        }
    }

    fn export_as(
        self: capnp::capability::Rc<Self>,
        params: format_router::ExportAsParams,
        mut results: format_router::ExportAsResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        let router = self.router.clone();
        async move {
            let p = params.get()?;
            let canonical = p.get_canonical()?.to_vec();
            let provider_id = p.get_provider_id()?.to_str()?.to_string();
            let out = blocking(move || router.export_as(&canonical, &provider_id))
                .await?
                .map_err(|e| Error::failed(e.to_string()))?;
            results.get().set_bytes(&out);
            Ok(())
        }
    }

    fn list(
        self: capnp::capability::Rc<Self>,
        _params: format_router::ListParams,
        mut results: format_router::ListResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        let router = self.router.clone();
        async move {
            let ids = router.list();
            let count = u32::try_from(ids.len()).unwrap_or(u32::MAX);
            let mut out = results.get().init_providers(count);
            for (i, id) in ids.iter().enumerate() {
                out.set(u32::try_from(i).unwrap_or(u32::MAX), id.as_str());
            }
            Ok(())
        }
    }
}
