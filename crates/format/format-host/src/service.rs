//! Cap'n Proto `FormatRouter::Server` implementation. Routes RPC calls onto
//! the shared [`Router`]. The transport (Unix socket listener) lives in
//! `transport.rs`.
//!
//! capnp 0.26 generates async server methods on `Rc<Self>` returning an
//! `impl Future<Output = Result<(), capnp::Error>>`; we route everything to a
//! `tokio::task::spawn_blocking` so the wasmtime calls (which can block for up
//! to the 10-s wallclock budget) don't stall the capnp event loop.

use std::rc::Rc;

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

impl format_router::Server for FormatRouterService {
    fn detect(
        self: capnp::capability::Rc<Self>,
        params: format_router::DetectParams,
        mut results: format_router::DetectResults,
    ) -> impl core::future::Future<Output = Result<(), Error>> + 'static {
        let router = self.router.clone();
        async move {
            let p = params.get().map_err(|e| Error::failed(e.to_string()))?;
            let head = p
                .get_head()
                .map_err(|e| Error::failed(e.to_string()))?
                .to_vec();
            let hint = p
                .get_hint()
                .map_err(|e| Error::failed(e.to_string()))?
                .to_str()
                .map_err(|e| Error::failed(e.to_string()))?
                .to_string();
            let (detected, provider_id) =
                tokio::task::spawn_blocking(move || router.detect(&head, &hint))
                    .await
                    .map_err(|e| Error::failed(format!("join: {e}")))?;
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
            let p = params.get().map_err(|e| Error::failed(e.to_string()))?;
            let bytes = p
                .get_bytes()
                .map_err(|e| Error::failed(e.to_string()))?
                .to_vec();
            let provider_id = p
                .get_provider_id()
                .map_err(|e| Error::failed(e.to_string()))?
                .to_str()
                .map_err(|e| Error::failed(e.to_string()))?
                .to_string();
            let out = tokio::task::spawn_blocking(move || router.import_as(&bytes, &provider_id))
                .await
                .map_err(|e| Error::failed(format!("join: {e}")))?
                .map_err(|e| Error::failed(format!("{}: {}", e.kind, e.message)))?;
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
            let p = params.get().map_err(|e| Error::failed(e.to_string()))?;
            let canonical = p
                .get_canonical()
                .map_err(|e| Error::failed(e.to_string()))?
                .to_vec();
            let provider_id = p
                .get_provider_id()
                .map_err(|e| Error::failed(e.to_string()))?
                .to_str()
                .map_err(|e| Error::failed(e.to_string()))?
                .to_string();
            let out =
                tokio::task::spawn_blocking(move || router.export_as(&canonical, &provider_id))
                    .await
                    .map_err(|e| Error::failed(format!("join: {e}")))?
                    .map_err(|e| Error::failed(format!("{}: {}", e.kind, e.message)))?;
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
            let mut out = results.get().init_providers(ids.len() as u32);
            for (i, id) in ids.iter().enumerate() {
                out.set(i as u32, id.as_str());
            }
            Ok(())
        }
    }
}

// Make `Rc<Self>` usable. capnp uses its own `Rc`-style smart pointer shape.
// We don't need extra glue beyond the `Server` impl above.
#[allow(dead_code)]
fn _ensure_rc_compat() {
    fn _accept<T: format_router::Server + 'static>(_: Rc<T>) {}
}
