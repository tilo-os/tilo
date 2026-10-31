//! The wasm engine: compiles modules and runs calls into them, for the rest of the runtime.
//!
//! * One engine per process: every runtime (every room's host, the surface, a block)
//!   compiles into it, and a module is compiled once per process, cached by the content hash of
//!   its bytes. Each runtime keeps its own instances (and so its own stores and limits).
//! * Each module normally has one instance. A call that arrives while the module's
//!   instance is already running (a flow in a module invoking an action of the same module)
//!   gets a second instance from the same compiled module, which costs microseconds; guests
//!   keep no state between calls, so instances are interchangeable.
//! * Runaway code is stopped by fuel, a fixed budget per call. Memory growth beyond the
//!   per-instance limit traps.
//! * Imports reach the host through the [`Host`] trait; the engine knows nothing of the ABI
//!   beyond the signatures in [`crate::abi`].
//!
//! With the `wasmtime` feature, which every shipped build has, the engine is tiered
//! ([`EngineKind::Tiered`], `engine/tiering.rs`): a module runs on wasmi, the interpreter, from
//! the moment it is loaded, while Cranelift compiles it on a thread of its own, and calls move
//! to the compiled module between calls once it is ready. Compiled modules are kept on disk
//! ([`keep_compiled_in`]), so a module compiled once starts compiled. Without the feature the
//! engine is wasmi alone. A process can also run on one engine only, wasmi, Cranelift or Winch,
//! to compare them (docs/tilolang-engines.md). What differs between engines is behind
//! [`Backend`]: making a module, linking the imports, instantiating with the limits, and one
//! call. Caching, pooling, profiling, import dispatch and how errors are reported are written
//! once, here, so a call gives the same result on every engine, and the same before and after
//! a module moves to compiled code.

#[cfg(all(test, feature = "wasmtime"))]
mod tests;
#[cfg(feature = "wasmtime")]
mod tiering;
mod wasmi_backend;
#[cfg(feature = "wasmtime")]
mod wasmtime_backend;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use crate::ids::ModuleHash;

/// A wasm scalar crossing the boundary. Handles and bools travel as `I32`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Wasm {
    I32(i32),
    I64(i64),
    F64(f64),
}

/// Why a host import did not return a value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostFail {
    /// Trap the guest with this message.
    Trap(String),
    /// Unwind the guest because a flow is waiting for a decision.
    Suspend,
}

/// The host side of the imports.
pub trait Host {
    /// Run import number `id` of [`crate::abi::IMPORTS`]. `memory` is the caller's exported
    /// memory, or empty when it has none.
    ///
    /// # Errors
    ///
    /// A [`HostFail`] traps or suspends the guest.
    fn import(&self, id: usize, memory: &[u8], args: &[Wasm]) -> Result<Option<Wasm>, HostFail>;
}

/// Why a call into a module failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CallError {
    #[error("trap: {0}")]
    Trap(String),
    #[error("the flow is suspended")]
    Suspended,
    #[error("out of fuel: {0}")]
    OutOfFuel(String),
    #[error("memory limit: {0}")]
    MemoryLimit(String),
    #[error("module has no export `{0}`")]
    NoExport(String),
    #[error("cannot link module: {0}")]
    Link(String),
}

/// What a [`CallError::OutOfFuel`] and a [`CallError::MemoryLimit`] say, on every engine.
const OUT_OF_FUEL: &str = "all fuel consumed by WebAssembly";
const GROWTH_LIMITED: &str = "growth operation limited";

/// Per-call and per-instance limits.
#[derive(Clone, Copy, Debug)]
pub struct EngineLimits {
    /// Fuel for one call into a module, nested calls excluded.
    pub fuel_per_call: u64,
    /// Linear memory an instance may reach.
    pub max_memory_bytes: usize,
}

impl Default for EngineLimits {
    fn default() -> Self {
        Self {
            fuel_per_call: 2_000_000_000,
            max_memory_bytes: 64 * 1024 * 1024,
        }
    }
}

/// How wasmi turns wasm into what it runs. The compiling engines compile every function when
/// the module is compiled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Translation {
    /// Validate a module when it is compiled, translate each function on its first call.
    #[default]
    Lazy,
    /// Validate and translate every function when the module is compiled.
    Eager,
}

/// Which engine runs the process's wasm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineKind {
    /// wasmi first, then Cranelift once a module is compiled (the `wasmtime` feature): what a
    /// shipped build runs.
    Tiered,
    /// The wasmi interpreter.
    Wasmi,
    /// wasmtime with its optimizing compiler, Cranelift (the `wasmtime` feature).
    Cranelift,
    /// wasmtime with its baseline compiler, Winch (the `wasmtime` feature).
    Winch,
}

impl Default for EngineKind {
    /// Tiered when the build has wasmtime, else wasmi.
    fn default() -> Self {
        if cfg!(feature = "wasmtime") {
            Self::Tiered
        } else {
            Self::Wasmi
        }
    }
}

impl EngineKind {
    /// Every engine, built in or not.
    pub const ALL: [Self; 4] = [Self::Tiered, Self::Wasmi, Self::Cranelift, Self::Winch];

    /// The name a person types: `tiered`, `wasmi`, `cranelift`, `winch`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Tiered => "tiered",
            Self::Wasmi => "wasmi",
            Self::Cranelift => "cranelift",
            Self::Winch => "winch",
        }
    }

    /// The engine of this name.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }

    /// Whether this build has the engine.
    #[must_use]
    pub const fn built_in(self) -> bool {
        match self {
            Self::Wasmi => true,
            Self::Tiered | Self::Cranelift | Self::Winch => cfg!(feature = "wasmtime"),
        }
    }
}

/// How the process's engine is made.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EngineConfig {
    pub kind: EngineKind,
    /// How wasmi translates; the compiling engines ignore it.
    pub translation: Translation,
}

/// The variable that picks the engine of a build with the `wasmtime` feature, when nothing
/// called [`configure`]: a name of [`EngineKind::name`]. It lets the runtime's own test suites
/// run on each engine. A build without the feature has only wasmi and does not read it.
pub const ENGINE_VARIABLE: &str = "TILOLANG_ENGINE";

impl EngineConfig {
    /// The configuration [`ENGINE_VARIABLE`] asks for, or why it names none.
    fn from_environment() -> Result<Self, String> {
        if !cfg!(feature = "wasmtime") {
            return Ok(Self::default());
        }
        let Ok(name) = std::env::var(ENGINE_VARIABLE) else {
            return Ok(Self::default());
        };
        let kind = EngineKind::from_name(&name).ok_or_else(|| {
            format!("{ENGINE_VARIABLE}={name} names no engine (tiered, wasmi, cranelift, winch)")
        })?;
        Ok(Self {
            kind,
            ..Self::default()
        })
    }
}

/// Make the process's engine, before any runtime exists. Returns the configuration in effect:
/// once the engine is made it stays as it was made.
///
/// # Errors
///
/// The engine is not built in ([`EngineKind::built_in`]) or cannot be made on this machine.
pub fn configure(config: EngineConfig) -> Result<EngineConfig, String> {
    if !config.kind.built_in() {
        return Err(format!(
            "this build has no {} engine (build with the `wasmtime` feature)",
            config.kind.name()
        ));
    }
    let process = PROCESS.get_or_init(|| Process::new(config));
    process.engine.as_ref().map_err(Clone::clone)?;
    Ok(process.config)
}

/// The configuration of the process's engine, making it from [`ENGINE_VARIABLE`] if no
/// [`configure`] came first.
#[must_use]
pub fn configured() -> EngineConfig {
    process().config
}

/// Forget the modules the process compiled, so the next runtime that loads one compiles it
/// again: how a benchmark measures a cold start in a warm process. Runtimes keep what they
/// already loaded; compiled modules kept on disk stay there.
pub fn forget_compiled() {
    match shared() {
        Ok(Shared::Wasmi(p)) => p.forget(),
        #[cfg(feature = "wasmtime")]
        Ok(Shared::Wasmtime(p)) => p.forget(),
        #[cfg(feature = "wasmtime")]
        Ok(Shared::Tiered(t)) => {
            t.base.forget();
            t.top.forget();
        }
        Err(_) => {}
    }
}

/// Keep the modules the tiered engine compiles in `dir`, and start modules found there on
/// compiled code: a process calls it once, before its first runtime loads a module, with a
/// directory that is its own (a tilolang host keeps them in its storage). Without it compiled
/// modules last as long as the process. Other engines keep nothing on disk.
///
/// # Errors
///
/// The tiered engine already read its cache from another directory, or the process's engine
/// could not be made.
pub fn keep_compiled_in(dir: PathBuf) -> Result<(), String> {
    match shared()? {
        #[cfg(feature = "wasmtime")]
        Shared::Tiered(t) => t.top.keep_in(dir),
        _ => {
            drop(dir);
            Ok(())
        }
    }
}

/// Where the time of the calls into wasm went, while a profile is on
/// ([`Engine::profile`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EngineProfile {
    /// Calls into a module.
    pub calls: u64,
    /// Time inside those calls, imports included.
    pub in_calls: Duration,
    /// Of that, time in the host's imports.
    pub in_imports: Duration,
    /// Imports called.
    pub imports: u64,
    /// Time compiling modules (only modules new to the process are compiled).
    pub compiling: Duration,
}

// ---- the seam --------------------------------------------------------------------------------

/// What a call carries into the guest's store while it runs: who answers the imports, and the
/// profile, if one is on.
struct Cx {
    host: Rc<dyn Host>,
    profile: Option<Rc<Cell<EngineProfile>>>,
}

/// An engine, as the rest of this module uses it. Everything above it is written once.
trait Backend: Send + Sync + Sized + 'static {
    /// A compiled module, cheap to clone and shared by every runtime in the process.
    type Module: Clone + Send;
    /// The import table bound to [`import`], one per runtime.
    type Linker;
    /// A store holding one instance of a module, its limits, and the export functions it
    /// looked up.
    type Instance;

    fn compile(&self, bytes: &[u8]) -> Result<Self::Module, String>;
    fn exports(module: &Self::Module) -> Vec<String>;
    fn linker(&self) -> Result<Self::Linker, String>;
    /// An instance with fuel for one call, its start function run.
    fn instantiate(
        &self,
        linker: &Self::Linker,
        module: &Self::Module,
        limits: &EngineLimits,
    ) -> Result<Self::Instance, CallError>;
    /// Call `export` with `cx` in the store and `fuel` to spend, and take `cx` out again.
    fn call(
        instance: &mut Self::Instance,
        export: &str,
        args: &[Wasm],
        fuel: u64,
        cx: Cx,
    ) -> Result<Option<Wasm>, CallError>;
}

/// The store-side half of every import, for every engine: run the host's import with the
/// caller's memory, and count it.
fn import(
    cx: Option<&Cx>,
    id: usize,
    memory: &[u8],
    args: &[Wasm],
) -> Result<Option<Wasm>, HostFail> {
    let cx = cx.ok_or_else(|| HostFail::Trap("import called outside a host call".into()))?;
    let started = cx.profile.as_ref().map(|_| Instant::now());
    let result = cx.host.import(id, memory, args);
    if let (Some(profile), Some(started)) = (&cx.profile, started) {
        let mut p = profile.get();
        p.imports = p.imports.saturating_add(1);
        p.in_imports = p.in_imports.saturating_add(started.elapsed());
        profile.set(p);
    }
    result
}

/// An import that did not return, carried through the engine back to [`Backend::call`].
#[derive(Debug)]
struct Unwind(HostFail);

impl std::fmt::Display for Unwind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            HostFail::Trap(message) => f.write_str(message),
            HostFail::Suspend => f.write_str("flow suspended"),
        }
    }
}

impl std::error::Error for Unwind {}

impl From<&Unwind> for CallError {
    fn from(unwind: &Unwind) -> Self {
        match &unwind.0 {
            HostFail::Trap(message) => Self::Trap(message.clone()),
            HostFail::Suspend => Self::Suspended,
        }
    }
}

/// What a trap says, the same on every engine: wasm's trap texts, which wasmi and wasmtime
/// share apart from wasmi's "uninitialized element 2".
fn trap_text(text: &str) -> String {
    if text == "uninitialized element 2" {
        "uninitialized element".to_owned()
    } else {
        text.to_owned()
    }
}

// ---- the process -----------------------------------------------------------------------------

/// The process's engine and the modules compiled into it.
struct Compiler<B: Backend> {
    backend: B,
    modules: Mutex<HashMap<ModuleHash, B::Module>>,
}

impl<B: Backend> Compiler<B> {
    fn new(backend: B) -> Self {
        Self {
            backend,
            modules: Mutex::new(HashMap::new()),
        }
    }

    fn known(&self, hash: &ModuleHash) -> Option<B::Module> {
        self.modules
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(hash)
            .cloned()
    }

    fn keep(&self, hash: ModuleHash, module: B::Module) {
        self.modules
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(hash, module);
    }

    fn forget(&self) {
        self.modules
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    #[cfg(feature = "wasmtime")]
    fn drop_module(&self, hash: &ModuleHash) {
        self.modules
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(hash);
    }
}

enum Shared {
    Wasmi(Compiler<wasmi_backend::Wasmi>),
    #[cfg(feature = "wasmtime")]
    Wasmtime(Compiler<wasmtime_backend::Wasmtime>),
    #[cfg(feature = "wasmtime")]
    Tiered(Box<Tiers>),
}

/// The tiered engine's two tiers: wasmi, which runs a module at once, and Cranelift.
#[cfg(feature = "wasmtime")]
struct Tiers {
    base: Compiler<wasmi_backend::Wasmi>,
    top: tiering::Tiering,
}

#[cfg(feature = "wasmtime")]
impl Tiers {
    fn new(translation: Translation) -> Result<Self, String> {
        Ok(Self {
            base: Compiler::new(wasmi_backend::Wasmi::new(translation)),
            top: tiering::Tiering::new()?,
        })
    }
}

struct Process {
    config: EngineConfig,
    /// The engine, or why it could not be made (Winch on a machine it does not support).
    engine: Result<Shared, String>,
}

impl Process {
    fn new(config: EngineConfig) -> Self {
        let engine = match config.kind {
            EngineKind::Wasmi => Ok(Shared::Wasmi(Compiler::new(wasmi_backend::Wasmi::new(
                config.translation,
            )))),
            #[cfg(feature = "wasmtime")]
            EngineKind::Cranelift | EngineKind::Winch => {
                wasmtime_backend::Wasmtime::new(config.kind)
                    .map(|backend| Shared::Wasmtime(Compiler::new(backend)))
            }
            #[cfg(feature = "wasmtime")]
            EngineKind::Tiered => Tiers::new(config.translation)
                .map(Box::new)
                .map(Shared::Tiered),
            #[cfg(not(feature = "wasmtime"))]
            EngineKind::Tiered | EngineKind::Cranelift | EngineKind::Winch => Err(format!(
                "this build has no {} engine (build with the `wasmtime` feature)",
                config.kind.name()
            )),
        };
        Self { config, engine }
    }
}

static PROCESS: OnceLock<Process> = OnceLock::new();

fn process() -> &'static Process {
    PROCESS.get_or_init(|| match EngineConfig::from_environment() {
        Ok(config) => Process::new(config),
        // Every runtime then fails to start, saying why.
        Err(why) => Process {
            config: EngineConfig::default(),
            engine: Err(why),
        },
    })
}

fn shared() -> Result<&'static Shared, String> {
    process().engine.as_ref().map_err(Clone::clone)
}

// ---- one runtime's engine --------------------------------------------------------------------

struct Compiled<B: Backend> {
    module: B::Module,
    idle: Vec<B::Instance>,
}

/// One runtime's instances of the modules it loaded, on one engine.
struct Pools<B: Backend> {
    compiler: &'static Compiler<B>,
    linker: B::Linker,
    modules: RefCell<HashMap<ModuleHash, Compiled<B>>>,
}

impl<B: Backend> Pools<B> {
    fn new(compiler: &'static Compiler<B>) -> Result<Self, String> {
        Ok(Self {
            linker: compiler.backend.linker()?,
            compiler,
            modules: RefCell::new(HashMap::new()),
        })
    }

    fn compile(
        &self,
        hash: &ModuleHash,
        bytes: &[u8],
        limits: &EngineLimits,
        compiled_in: impl FnOnce(Duration),
    ) -> Result<(), CallError> {
        if self.has(hash) {
            return Ok(());
        }
        let module = if let Some(module) = self.compiler.known(hash) {
            module
        } else {
            let started = Instant::now();
            let module = self
                .compiler
                .backend
                .compile(bytes)
                .map_err(CallError::Link)?;
            compiled_in(started.elapsed());
            self.compiler.keep(hash.clone(), module.clone());
            module
        };
        self.adopt(hash, module, limits)
    }

    /// Run `module` for `hash` from now on, with its first instance made.
    fn adopt(
        &self,
        hash: &ModuleHash,
        module: B::Module,
        limits: &EngineLimits,
    ) -> Result<(), CallError> {
        let first = self
            .compiler
            .backend
            .instantiate(&self.linker, &module, limits)?;
        self.modules.borrow_mut().insert(
            hash.clone(),
            Compiled {
                module,
                idle: vec![first],
            },
        );
        Ok(())
    }

    fn has(&self, hash: &ModuleHash) -> bool {
        self.modules.borrow().contains_key(hash)
    }

    /// Drop the module and its idle instances; one still running is dropped when its call
    /// ends.
    #[cfg(feature = "wasmtime")]
    fn remove(&self, hash: &ModuleHash) {
        self.modules.borrow_mut().remove(hash);
    }

    fn exports(&self, module: &ModuleHash) -> Vec<String> {
        self.modules
            .borrow()
            .get(module)
            .map_or_else(Vec::new, |c| B::exports(&c.module))
    }

    fn cached(&self) -> usize {
        self.modules.borrow().len()
    }

    fn checkout(
        &self,
        module: &ModuleHash,
        limits: &EngineLimits,
    ) -> Result<B::Instance, CallError> {
        let (compiled, idle) = {
            let mut modules = self.modules.borrow_mut();
            let c = modules
                .get_mut(module)
                .ok_or_else(|| CallError::Link(format!("module {module} is not compiled")))?;
            (c.module.clone(), c.idle.pop())
        };
        match idle {
            Some(instance) => Ok(instance),
            None => self
                .compiler
                .backend
                .instantiate(&self.linker, &compiled, limits),
        }
    }

    fn call(
        &self,
        module: &ModuleHash,
        export: &str,
        args: &[Wasm],
        limits: &EngineLimits,
        cx: Cx,
    ) -> Result<Option<Wasm>, CallError> {
        let mut instance = self.checkout(module, limits)?;
        let outcome = B::call(&mut instance, export, args, limits.fuel_per_call, cx);
        // A trapped instance may have been left mid-way through its own memory; it is dropped.
        if outcome.is_ok()
            && let Some(c) = self.modules.borrow_mut().get_mut(module)
        {
            c.idle.push(instance);
        }
        outcome
    }
}

/// One runtime's instances on the tiered engine: each module's on wasmi until its compiled
/// module is ready, then on Cranelift.
#[cfg(feature = "wasmtime")]
struct TieredPools {
    tiering: &'static tiering::Tiering,
    base: Pools<wasmi_backend::Wasmi>,
    top: Pools<wasmtime_backend::Wasmtime>,
}

#[cfg(feature = "wasmtime")]
impl TieredPools {
    fn new(tiers: &'static Tiers) -> Result<Self, String> {
        Ok(Self {
            tiering: &tiers.top,
            base: Pools::new(&tiers.base)?,
            top: Pools::new(&tiers.top.compiler)?,
        })
    }

    fn compile(
        &self,
        hash: &ModuleHash,
        bytes: &[u8],
        limits: &EngineLimits,
        compiled_in: impl FnOnce(Duration),
    ) -> Result<(), CallError> {
        if self.has(hash) {
            return Ok(());
        }
        // Compiled earlier, in this process or (from the disk cache) in an earlier one: it
        // starts compiled.
        let started = Instant::now();
        if let Some(module) = self.tiering.ready(hash) {
            let adopted = self.top.adopt(hash, module, limits);
            compiled_in(started.elapsed());
            return adopted;
        }
        self.base.compile(hash, bytes, limits, compiled_in)?;
        self.tiering.request(hash, bytes);
        Ok(())
    }

    fn has(&self, hash: &ModuleHash) -> bool {
        self.top.has(hash) || self.base.has(hash)
    }

    /// Move the module to its compiled code if that is ready. This happens between calls
    /// into it: a call still running on wasmi keeps its instance, which is dropped when it
    /// ends.
    fn tier_up(&self, hash: &ModuleHash, limits: &EngineLimits) {
        if self.top.has(hash) || !self.base.has(hash) {
            return;
        }
        let Some(module) = self.tiering.compiled(hash) else {
            return;
        };
        match self.top.adopt(hash, module, limits) {
            Ok(()) => self.base.remove(hash),
            Err(error) => self.tiering.reject(hash, error.to_string()),
        }
    }

    fn tier(&self, hash: &ModuleHash) -> Option<EngineKind> {
        if self.top.has(hash) {
            Some(EngineKind::Cranelift)
        } else if self.base.has(hash) {
            Some(EngineKind::Wasmi)
        } else {
            None
        }
    }

    fn exports(&self, hash: &ModuleHash) -> Vec<String> {
        if self.top.has(hash) {
            self.top.exports(hash)
        } else {
            self.base.exports(hash)
        }
    }

    fn cached(&self) -> usize {
        self.top.cached().saturating_add(self.base.cached())
    }

    fn call(
        &self,
        hash: &ModuleHash,
        export: &str,
        args: &[Wasm],
        limits: &EngineLimits,
        cx: Cx,
    ) -> Result<Option<Wasm>, CallError> {
        self.tier_up(hash, limits);
        if self.top.has(hash) {
            self.top.call(hash, export, args, limits, cx)
        } else {
            self.base.call(hash, export, args, limits, cx)
        }
    }
}

enum Pooled {
    Wasmi(Pools<wasmi_backend::Wasmi>),
    #[cfg(feature = "wasmtime")]
    Wasmtime(Pools<wasmtime_backend::Wasmtime>),
    #[cfg(feature = "wasmtime")]
    Tiered(TieredPools),
}

/// The same expression for whichever engine the process runs: [`Pools`] and [`TieredPools`]
/// have the same methods.
macro_rules! on_engine {
    ($pooled:expr, $pools:ident => $body:expr) => {
        match $pooled {
            Pooled::Wasmi($pools) => $body,
            #[cfg(feature = "wasmtime")]
            Pooled::Wasmtime($pools) => $body,
            #[cfg(feature = "wasmtime")]
            Pooled::Tiered($pools) => $body,
        }
    };
}

/// One runtime's view of the process's engine: its instances of the modules it loaded, and
/// its limits.
pub struct Engine {
    kind: EngineKind,
    pooled: Pooled,
    limits: EngineLimits,
    profile: RefCell<Option<Rc<Cell<EngineProfile>>>>,
}

impl Engine {
    /// An engine with fuel metering and the full import table linked.
    ///
    /// # Errors
    ///
    /// The process's engine could not be made, or the import table is inconsistent, which is a
    /// bug.
    pub fn new(limits: EngineLimits) -> Result<Self, String> {
        Self::on(shared()?, process().config.kind, limits)
    }

    fn on(shared: &'static Shared, kind: EngineKind, limits: EngineLimits) -> Result<Self, String> {
        let pooled = match shared {
            Shared::Wasmi(compiler) => Pooled::Wasmi(Pools::new(compiler)?),
            #[cfg(feature = "wasmtime")]
            Shared::Wasmtime(compiler) => Pooled::Wasmtime(Pools::new(compiler)?),
            #[cfg(feature = "wasmtime")]
            Shared::Tiered(tiers) => Pooled::Tiered(TieredPools::new(tiers)?),
        };
        Ok(Self {
            kind,
            pooled,
            limits,
            profile: RefCell::new(None),
        })
    }

    /// Which engine this is.
    #[must_use]
    pub const fn kind(&self) -> EngineKind {
        self.kind
    }

    /// Which engine runs the module's calls now: on the tiered engine, wasmi until its
    /// compiled module is ready and a call moved it there, then Cranelift; on any other, the
    /// engine. `None` for a module not loaded.
    #[must_use]
    pub fn tier(&self, module: &ModuleHash) -> Option<EngineKind> {
        match &self.pooled {
            #[cfg(feature = "wasmtime")]
            Pooled::Tiered(pools) => pools.tier(module),
            _ => on_engine!(&self.pooled, pools => pools.has(module)).then_some(self.kind),
        }
    }

    /// Wait until the tiered engine's compile of the module has ended, so that its next call
    /// runs compiled (or, if the compile failed, stays on wasmi): for benchmarks and tests.
    /// The compile thread ends the wait when it finishes the module, either way. Returns at
    /// once on another engine or for a module not being compiled.
    pub fn settle_compile(&self, module: &ModuleHash) {
        #[cfg(feature = "wasmtime")]
        if let Pooled::Tiered(pools) = &self.pooled {
            pools.tiering.settle(module);
        }
        #[cfg(not(feature = "wasmtime"))]
        let _ = module;
    }

    /// Why the tiered engine could not compile the module, which then stays on wasmi; `None`
    /// while it compiles, once it compiled, and on any other engine.
    #[must_use]
    pub fn compile_failure(&self, module: &ModuleHash) -> Option<String> {
        match &self.pooled {
            #[cfg(feature = "wasmtime")]
            Pooled::Tiered(pools) => pools.tiering.failure(module),
            _ => {
                let _ = module;
                None
            }
        }
    }

    /// Start counting where the time of calls goes (or stop, with `false`); see
    /// [`Self::profiled`].
    pub fn profile(&self, on: bool) {
        *self.profile.borrow_mut() = on.then(|| Rc::new(Cell::new(EngineProfile::default())));
    }

    /// What the profile counted since it started.
    pub fn profiled(&self) -> Option<EngineProfile> {
        self.profile.borrow().as_ref().map(|p| p.get())
    }

    fn count(&self, f: impl FnOnce(&mut EngineProfile)) {
        if let Some(profile) = self.profile.borrow().as_ref() {
            let mut p = profile.get();
            f(&mut p);
            profile.set(p);
        }
    }

    /// Compile (or fetch from the cache) the module with these bytes.
    ///
    /// # Errors
    ///
    /// Fails when the bytes are not a valid module or do not link against the ABI.
    pub fn compile(&self, bytes: &[u8]) -> Result<ModuleHash, CallError> {
        let hash = ModuleHash::of_bytes(bytes);
        let counted =
            |took: Duration| self.count(|p| p.compiling = p.compiling.saturating_add(took));
        on_engine!(&self.pooled, pools => pools.compile(&hash, bytes, &self.limits, counted))?;
        Ok(hash)
    }

    /// The names of the module's exports.
    pub fn exports(&self, module: &ModuleHash) -> Vec<String> {
        on_engine!(&self.pooled, pools => pools.exports(module))
    }

    /// How many compiled modules the cache holds.
    pub fn cached_modules(&self) -> usize {
        on_engine!(&self.pooled, pools => pools.cached())
    }

    /// Call `export` of `module` with `args`. Fuel is reset for the call.
    ///
    /// # Errors
    ///
    /// A [`CallError`] for a trap, a suspension, exhausted limits or a missing export.
    pub fn call(
        &self,
        module: &ModuleHash,
        export: &str,
        args: &[Wasm],
        host: Rc<dyn Host>,
    ) -> Result<Option<Wasm>, CallError> {
        let profile = self.profile.borrow().clone();
        let started = profile.as_ref().map(|_| Instant::now());
        let cx = Cx { host, profile };
        let outcome =
            on_engine!(&self.pooled, pools => pools.call(module, export, args, &self.limits, cx));
        if let Some(started) = started {
            self.count(|p| {
                p.calls = p.calls.saturating_add(1);
                p.in_calls = p.in_calls.saturating_add(started.elapsed());
            });
        }
        outcome
    }
}
