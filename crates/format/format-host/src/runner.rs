//! WASM Component Model host. Loads one `.wasm` component per registered
//! provider, holds a precompiled module in memory, and runs detect / import /
//! export under per-call resource limits.
//!
//! Resource caps:
//! * **Memory** — `ResourceLimiter::memory_growing` rejects growth past
//!   `MAX_MEMORY_BYTES` (256 MiB). A guest that asks for more gets a clean
//!   trap.
//! * **Wallclock** — `Engine::increment_epoch()` is bumped once per
//!   `EPOCH_TICK` by a Tokio task; each Store sets a deadline `MAX_TICKS`
//!   ahead (= 10 s). A guest that loops forever is interrupted at the next
//!   epoch bump.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use format_provider_wit::CodecError;
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{ResourceTableError, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

/// 256 MiB per call. The host rejects any growth beyond this in
/// `WasiHostState::memory_growing`.
pub const MAX_MEMORY_BYTES: usize = 256 * 1024 * 1024;
/// Wallclock budget per call.
pub const MAX_WALLCLOCK: Duration = Duration::from_secs(10);
/// Epoch tick rate. Picking 1 Hz keeps overhead negligible and gives us 1-s
/// resolution on the wallclock budget.
pub const EPOCH_TICK: Duration = Duration::from_secs(1);
/// Ticks-ahead deadline corresponding to [`MAX_WALLCLOCK`].
pub const MAX_TICKS: u64 = MAX_WALLCLOCK.as_secs() / EPOCH_TICK.as_secs();

// Generate host-side Component Model bindings from the WIT package shipped by
// `format-provider-wit`. The host imports the generated `Provider`
// world's `format_provider` exports.
wasmtime::component::bindgen!({
    path: "../format-provider-wit/wit",
    world: "provider",
});

// The bindgen macro emits a top-level `Provider` struct (from the world name)
// with a method to instantiate the component and accessors for the exported
// interface(s). We pull a short alias for the interface module so the call
// sites stay readable.
use exports::qos::format_provider::format_provider as wit_iface;

/// Per-Store state. Holds the WASI table + ctx (so we can hand WASI capabilities
/// to providers if/when they need them) and the resource limiter that enforces
/// our memory ceiling.
pub struct WasiHostState {
    table: ResourceTable,
    wasi: WasiCtx,
    limits: StoreLimits,
}

impl WasiView for WasiHostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// A loaded provider component. Cheap to clone (everything inside is `Arc`).
#[derive(Clone)]
pub struct LoadedComponent {
    engine: Engine,
    component: Component,
    linker: Arc<Linker<WasiHostState>>,
}

impl LoadedComponent {
    /// Build an engine wired for the component model + per-call interruption.
    ///
    /// # Errors
    ///
    /// Returns an error if the Wasmtime engine configuration or creation fails.
    pub fn engine() -> anyhow::Result<Engine> {
        let mut cfg = wasmtime::Config::new();
        cfg.wasm_component_model(true);
        cfg.epoch_interruption(true);
        // On-disk compilation cache. Best-effort: if the cache dir can't be
        // created (read-only homes, sandbox, etc.) we fall back to no cache —
        // the host stays usable, just slower.
        if let Some(cache_dir) = cache_dir() {
            let _ = std::fs::create_dir_all(&cache_dir);
            let mut cache_conf = wasmtime::CacheConfig::new();
            cache_conf.with_directory(cache_dir);
            if let Ok(cache) = wasmtime::Cache::new(cache_conf) {
                cfg.cache(Some(cache));
            }
        }
        Engine::new(&cfg).map_err(|e| anyhow::anyhow!("building wasmtime engine: {e:#}"))
    }

    /// Load a component from a `.wasm` file.
    ///
    /// # Errors
    ///
    /// Returns an error if reading the file or parsing it as a component fails.
    pub fn from_path(engine: &Engine, path: &Path) -> anyhow::Result<Self> {
        let component = Component::from_file(engine, path)
            .map_err(|e| anyhow::anyhow!("loading {}: {e:#}", path.display()))?;
        Self::wrap(engine.clone(), component)
    }

    /// Load a component from in-memory bytes (used by tests + `.wat` fixtures).
    ///
    /// # Errors
    ///
    /// Returns an error if parsing the bytes as a component fails.
    pub fn from_bytes(engine: &Engine, bytes: &[u8]) -> anyhow::Result<Self> {
        let component = Component::new(engine, bytes)
            .map_err(|e| anyhow::anyhow!("loading component bytes: {e:#}"))?;
        Self::wrap(engine.clone(), component)
    }

    fn wrap(engine: Engine, component: Component) -> anyhow::Result<Self> {
        let mut linker: Linker<WasiHostState> = Linker::new(&engine);
        // WASI imports — some component bindgen artifacts unconditionally
        // pull in WASI even when the guest doesn't use it; wire it up
        // pre-emptively. Best-effort: in older wasmtime-wasi builds the
        // function may live behind a different module path.
        let _ = wasmtime_wasi::p2::add_to_linker_sync(&mut linker);
        Ok(Self {
            engine,
            component,
            linker: Arc::new(linker),
        })
    }

    /// Build a fresh Store with our resource caps applied.
    fn new_store(&self) -> Store<WasiHostState> {
        let limits = StoreLimitsBuilder::new()
            .memory_size(MAX_MEMORY_BYTES)
            .build();
        let wasi = WasiCtxBuilder::new().inherit_stderr().build();
        let state = WasiHostState {
            table: ResourceTable::new(),
            wasi,
            limits,
        };
        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limits);
        // 10-s wallclock budget. The background ticker bumps the engine epoch
        // once per second; if the guest spins past 10 bumps, this trap fires.
        store.set_epoch_deadline(MAX_TICKS);
        store.epoch_deadline_trap();
        store
    }

    /// Instantiate + call `detect`.
    ///
    /// # Errors
    ///
    /// Returns a `CodecError` if component instantiation fails or if the guest
    /// component returns an error.
    pub fn detect(&self, head: &[u8]) -> Result<wit_iface::Detected, CodecError> {
        let mut store = self.new_store();
        let bindings = Provider::instantiate(&mut store, &self.component, &self.linker)
            .map_err(|e| CodecError::trap(format!("instantiate: {e:#}")))?;
        let iface = bindings.qos_format_provider_format_provider();
        iface
            .call_detect(&mut store, head)
            .map_err(|e| classify_trap(&e))
    }

    /// Instantiate + call `import-doc`. Returns the canonical CRDT bytes.
    ///
    /// # Errors
    ///
    /// Returns a `CodecError` if component instantiation fails or if the guest
    /// component returns an error.
    pub fn import_doc(&self, bytes: &[u8]) -> Result<Vec<u8>, CodecError> {
        let mut store = self.new_store();
        let bindings = Provider::instantiate(&mut store, &self.component, &self.linker)
            .map_err(|e| CodecError::trap(format!("instantiate: {e:#}")))?;
        let iface = bindings.qos_format_provider_format_provider();
        match iface.call_import_doc(&mut store, bytes) {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(CodecError {
                kind: e.kind,
                message: e.message,
            }),
            Err(trap) => Err(classify_trap(&trap)),
        }
    }

    /// Instantiate + call `export-doc`. Returns the file-format bytes.
    ///
    /// # Errors
    ///
    /// Returns a `CodecError` if component instantiation fails or if the guest
    /// component returns an error.
    pub fn export_doc(&self, canonical: &[u8]) -> Result<Vec<u8>, CodecError> {
        let mut store = self.new_store();
        let bindings = Provider::instantiate(&mut store, &self.component, &self.linker)
            .map_err(|e| CodecError::trap(format!("instantiate: {e:#}")))?;
        let iface = bindings.qos_format_provider_format_provider();
        match iface.call_export_doc(&mut store, canonical) {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(CodecError {
                kind: e.kind,
                message: e.message,
            }),
            Err(trap) => Err(classify_trap(&trap)),
        }
    }
}

/// Map a wasmtime trap to a structured `CodecError`. The epoch-interruption
/// path surfaces as a trap whose Display contains `"interrupt"`; memory growth
/// past the ceiling fails inside the limiter and bubbles back as an
/// out-of-bounds trap. Either way, the host stays up.
fn classify_trap<E: std::fmt::Display + std::fmt::Debug>(e: &E) -> CodecError {
    let msg = format!("{e:#?}");
    let lo = msg.to_lowercase();
    if lo.contains("interrupt") || lo.contains("epoch") {
        CodecError::resource_limit(format!("wallclock budget exhausted: {msg}"))
    } else if lo.contains("memory") || lo.contains("out of bounds") {
        CodecError::resource_limit(format!("memory cap hit: {msg}"))
    } else {
        CodecError::trap(msg)
    }
}

/// Cache dir for wasmtime's on-disk compilation cache. Lives under
/// `$XDG_CACHE_HOME/qos/wasmtime/` (≈`~/.cache/qos/wasmtime/`).
fn cache_dir() -> Option<std::path::PathBuf> {
    dirs::cache_dir().map(|d| d.join("qos").join("wasmtime"))
}

/// Spawn a Tokio task that bumps the engine's epoch counter once per
/// [`EPOCH_TICK`]. One ticker per engine is enough; the wallclock budget is
/// applied per Store via `set_epoch_deadline`.
pub fn spawn_epoch_ticker(engine: Engine) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(EPOCH_TICK);
        loop {
            interval.tick().await;
            engine.increment_epoch();
        }
    });
}

// `ResourceTableError` lives in the wasi-side bindings; pull it through so
// the `WasiHostState` impl above compiles cleanly with the resource table.
#[allow(dead_code)]
type _ResourceTableUnused = ResourceTableError;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_builds_with_component_model() {
        let _ = LoadedComponent::engine().unwrap();
    }
}
