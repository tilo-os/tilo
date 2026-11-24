//! The wasm engine: compiles modules and runs calls into them, for the rest of the runtime.
//!
//! * One engine per process: every runtime (every workspace's host, the surface, a block)
//!   compiles into it, and a module is compiled once per process, cached by the content hash of
//!   its bytes. Each runtime keeps its own instances (and so its own stores and limits).
//! * Each module normally has one instance. A call that arrives while the module's
//!   instance is already running (an action in a module invoking an action of the same module)
//!   gets a second instance from the same compiled module, which costs microseconds; guests
//!   keep no state between calls, so instances are interchangeable.
//! * Runaway code is stopped by fuel, a fixed budget per call. Memory growth beyond the
//!   per-instance limit traps. Engine construction, the limits, the refuel and the text of every
//!   failure are `tilo-wasm-host`'s, shared with every other wasm engine in the tree; what is
//!   here is the runtime's own: caching, pooling, profiling and import dispatch.
//! * Imports reach the host through the [`Host`] trait; the engine knows nothing of the ABI
//!   beyond the signatures in [`crate::abi`].
//! * A module may also be a linked set ([`Engine::compile_linked`]): modules instantiated into
//!   one store, the first with the imports the engine links and each after it with the exports of the
//!   ones before it, as the page links a split bundle. An app's model is one: the model runtime
//!   every app shares, compiled once per process, then the app's own module. Each module of a
//!   set is compiled, cached and tiered on its own, by its own hash; the set moves to compiled
//!   code once all of its modules are.
//!
//! With the `wasmtime` feature, which every shipped build has, the engine is tiered
//! ([`EngineKind::Tiered`], `engine/tiering.rs`): a module runs on wasmi, the interpreter, from
//! the moment it is loaded, while Cranelift compiles it on a thread of its own, and calls move
//! to the compiled module between calls once it is ready. Compiled modules are kept on disk
//! ([`keep_compiled_in`]), so a module compiled once starts compiled. Without the feature the
//! engine is wasmi alone. A process can also run on one engine only, wasmi, Cranelift or Winch,
//! to compare them (docs/tilolang-engines.md). What differs between engines is
//! behind [`Backend`]: making a module, linking the imports, instantiating with the limits, and one
//! call. Caching, pooling, profiling, import dispatch and how errors are reported are written
//! once, here, so a call gives the same result on every engine, and the same before and after
//! a module moves to compiled code.

#[cfg(all(test, all(feature = "wasmtime", not(target_arch = "wasm32"))))]
mod tests;
#[cfg(all(feature = "wasmtime", not(target_arch = "wasm32")))]
mod tiering;
mod wasmi_backend;
#[cfg(all(feature = "wasmtime", not(target_arch = "wasm32")))]
mod wasmtime_backend;
#[cfg(all(feature = "wasmtime", not(target_arch = "wasm32")))]
mod wasmtime_host;

use std::cell::{Cell, Ref, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;

use tilo_wasm_host::Failure;

use crate::abi;
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
    /// Unwind the guest because a run stopped at an action it invokes.
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

    /// Run the import a module of another ABI calls, by its module and name. `memory` is the
    /// caller's exported memory, as [`Self::import`] is given it.
    ///
    /// A device runs an app's model as a module of its own (`tilo-model-engine`), whose
    /// imports are the record's, not the `tl` table's. A host that answers for such an ABI
    /// names it with [`Self::guest_abi`]; that ABI's imports are linked, and each is answered
    /// here.
    ///
    /// # Errors
    ///
    /// A [`HostFail`] traps or suspends the guest.
    fn guest_import(
        &self,
        module: &str,
        name: &str,
        _memory: &[u8],
        _args: &[Wasm],
    ) -> Result<GuestAnswer, HostFail> {
        Err(HostFail::Trap(format!(
            "this host imports no {module}::{name}"
        )))
    }

    /// The imports a module of its own ABI calls, as `(module, name, parameters, result)` in
    /// the signs [`crate::abi`] writes: `i` (i32, which also carries handles and bools), `l`
    /// (i64) and `f` (f64). They are linked beside the `tl` table, so a module of this ABI
    /// loads with no import missing. An empty slice, the default, links none: the engine runs
    /// packages and nothing else.
    fn guest_abi(&self) -> &'static [(&'static str, &'static str, &'static str, &'static str)] {
        &[]
    }
}

/// What an import of another ABI answers.
#[derive(Clone, Debug, PartialEq)]
pub enum GuestAnswer {
    /// Nothing.
    Nothing,
    /// This value, as the import's result.
    Value(Wasm),
    /// These bytes, copied to `offset` of the guest's memory; and this value, if any, as the
    /// import's result — the shape an import that answers a `(pointer, length)` uses.
    Bytes {
        offset: u32,
        bytes: Vec<u8>,
        result: Option<Wasm>,
    },
}

/// Why a call into a module failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CallError {
    /// The shared wasm engine layer refused the module or the call: a trap, a spent budget, a
    /// refused growth, a missing export or an unloadable module.
    #[error(transparent)]
    Wasm(#[from] Failure),
    /// Execution stopped at an action it invokes; the host resolves the action and calls again.
    #[error("the run is suspended")]
    Suspended,
}

/// Per-call and per-instance limits, as the shared wasm engine layer names them.
pub use tilo_wasm_host::Limits as EngineLimits;

/// The limits every runtime starts with: a call may burn 2 billion instructions and take half
/// a minute, and an instance may reach 64 MiB of linear memory. The counts are the engines' own
/// defaults, written down so that no module that loaded before is refused now: a linked set
/// makes one instance per module.
pub const DEFAULT_LIMITS: EngineLimits = EngineLimits {
    fuel_per_call: 2_000_000_000,
    // Fuel is what stops a guest that computes, within about a second of compiled code (the
    // interpreter's deadline is not enforced). The clock is the backstop for a call whose time
    // goes elsewhere: a host function that takes long, or waits, still counts against it, so
    // it is generous enough for a slow host call and still bounded.
    deadline: Some(std::time::Duration::from_secs(30)),
    max_memory_bytes: 64 * 1024 * 1024,
    max_memories: 10_000,
    max_tables: 10_000,
    max_instances: 10_000,
};

/// How wasmi turns wasm into what it runs. The compiling engines compile every function when
/// the module is compiled. The shared wasm engine layer's choice, re-exported.
pub use tilo_wasm_host::wasmi::Translation;

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
        if cfg!(all(feature = "wasmtime", not(target_arch = "wasm32"))) {
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
            Self::Tiered | Self::Cranelift | Self::Winch => {
                cfg!(all(feature = "wasmtime", not(target_arch = "wasm32")))
            }
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
        if !cfg!(all(feature = "wasmtime", not(target_arch = "wasm32"))) {
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
        #[cfg(all(feature = "wasmtime", not(target_arch = "wasm32")))]
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

// ---- the boundary --------------------------------------------------------------------------------

/// What a call carries into the guest's store while it runs: who answers the imports, and the
/// profile, if one is on.
struct Cx {
    host: Rc<dyn Host>,
    profile: Option<Rc<Cell<EngineProfile>>>,
    /// What each unit costs, while a profile is on: the fuel a call spends is charged to the
    /// unit that made it.
    costs: Option<Rc<crate::cost::Recorder>>,
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

    fn compile(&self, bytes: &[u8]) -> Result<Self::Module, Failure>;
    fn exports(module: &Self::Module) -> Vec<String>;
    /// Import `(module, name, params, result)` of another ABI, answered by
    /// [`Host::guest_import`].
    fn linker(&self, imports: &[Imports]) -> Result<Self::Linker, String>;
    /// An instance of `modules` with fuel for one call, their start functions called: the first
    /// linked with `linker`, each after it with every import answered by the export of that
    /// name of the latest module before it that has one ([`Engine::compile_linked`]).
    fn instantiate(
        &self,
        linker: &Self::Linker,
        modules: &[Self::Module],
        limits: &EngineLimits,
    ) -> Result<Self::Instance, Failure>;
    /// Call `export` with `cx` in the store, giving it a whole call's budget from `limits`, and
    /// take `cx` out again.
    fn call(
        &self,
        instance: &mut Self::Instance,
        export: &str,
        args: &[Wasm],
        limits: &EngineLimits,
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
    let started = cx.profile.as_ref().map(|_| tilo_clock::now());
    let result = cx.host.import(id, memory, args);
    if let (Some(profile), Some(started)) = (&cx.profile, started) {
        let mut p = profile.get();
        p.imports = p.imports.saturating_add(1);
        p.in_imports = p.in_imports.saturating_add(started.elapsed());
        profile.set(p);
    }
    result
}

/// [`import`] for an import of another ABI, with the guest's memory to write an answer into.
fn guest_import(
    cx: Option<&Cx>,
    module: &str,
    name: &str,
    memory: &[u8],
    args: &[Wasm],
) -> Result<GuestAnswer, HostFail> {
    let cx = cx.ok_or_else(|| HostFail::Trap("import called outside a host call".into()))?;
    let started = cx.profile.as_ref().map(|_| tilo_clock::now());
    let result = cx.host.guest_import(module, name, memory, args);
    if let (Some(profile), Some(started)) = (&cx.profile, started) {
        let mut p = profile.get();
        p.imports = p.imports.saturating_add(1);
        p.in_imports = p.in_imports.saturating_add(started.elapsed());
        profile.set(p);
    }
    result
}

/// [`GuestAnswer`] as the engine answers a caller: what it puts in the results, and the bytes
/// to copy into the guest's memory.
pub(crate) fn guest_answer(answer: GuestAnswer) -> (Option<Wasm>, Option<(u32, Vec<u8>)>) {
    match answer {
        GuestAnswer::Nothing => (None, None),
        GuestAnswer::Value(value) => (Some(value), None),
        GuestAnswer::Bytes {
            offset,
            bytes,
            result,
        } => (result, Some((offset, bytes))),
    }
}

/// The `tl` import table, one runtime's imports: what a [`Backend::Linker`] links for a host
/// that executes packages, and beside them the imports of an ABI another device's modules
/// use ([`Engine::link`]).
pub type Imports = (&'static str, &'static str, &'static str, &'static str);

/// The imports [`Backend::linker`] links when no host named an ABI of its own: the `tl` table.
fn package_imports() -> Vec<Imports> {
    abi::IMPORTS
        .iter()
        .map(|(_, name, params, result)| (abi::MODULE, *name, *params, *result))
        .collect()
}

/// An import that did not return, carried through the engine back to [`Backend::call`].
#[derive(Debug)]
struct Unwind(HostFail);

impl std::fmt::Display for Unwind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            HostFail::Trap(message) => f.write_str(message),
            HostFail::Suspend => f.write_str("run suspended"),
        }
    }
}

impl std::error::Error for Unwind {}

impl From<&Unwind> for CallError {
    fn from(unwind: &Unwind) -> Self {
        match &unwind.0 {
            HostFail::Trap(message) => Self::Wasm(Failure::Trap(message.clone())),
            HostFail::Suspend => Self::Suspended,
        }
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

    #[cfg(all(feature = "wasmtime", not(target_arch = "wasm32")))]
    fn drop_module(&self, hash: &ModuleHash) {
        self.modules
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(hash);
    }
}

enum Shared {
    Wasmi(Compiler<wasmi_backend::Wasmi>),
    #[cfg(all(feature = "wasmtime", not(target_arch = "wasm32")))]
    Wasmtime(Compiler<wasmtime_backend::Wasmtime>),
    #[cfg(all(feature = "wasmtime", not(target_arch = "wasm32")))]
    Tiered(Box<Tiers>),
}

/// The tiered engine's two tiers: wasmi, which runs a module at once, and Cranelift.
#[cfg(all(feature = "wasmtime", not(target_arch = "wasm32")))]
struct Tiers {
    base: Compiler<wasmi_backend::Wasmi>,
    top: tiering::Tiering,
}

#[cfg(all(feature = "wasmtime", not(target_arch = "wasm32")))]
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
            #[cfg(all(feature = "wasmtime", not(target_arch = "wasm32")))]
            EngineKind::Cranelift | EngineKind::Winch => {
                wasmtime_backend::Wasmtime::new(config.kind)
                    .map(|backend| Shared::Wasmtime(Compiler::new(backend)))
            }
            #[cfg(all(feature = "wasmtime", not(target_arch = "wasm32")))]
            EngineKind::Tiered => Tiers::new(config.translation)
                .map(Box::new)
                .map(Shared::Tiered),
            #[cfg(not(all(feature = "wasmtime", not(target_arch = "wasm32"))))]
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
    /// The module, or the modules of a linked set in the order they are instantiated.
    modules: Vec<B::Module>,
    idle: Vec<B::Instance>,
}

/// One module of what [`Engine::compile_linked`] compiles: its hash and its bytes.
type Part<'a> = (ModuleHash, &'a [u8]);

/// One runtime's instances of the modules it loaded, on one engine.
struct Pools<B: Backend> {
    compiler: &'static Compiler<B>,
    /// The imports of the host's own ABI, beside the `tl` table ([`Engine::link`]): empty
    /// until a host that runs modules of another ABI names it.
    abi: Cell<&'static [Imports]>,
    /// The linker, and the ABI it was linked for: made again when [`Self::abi`] moves.
    linker: RefCell<(&'static [Imports], B::Linker)>,
    modules: RefCell<HashMap<ModuleHash, Compiled<B>>>,
}

impl<B: Backend> Pools<B> {
    fn new(compiler: &'static Compiler<B>) -> Result<Self, String> {
        const NONE: &[Imports] = &[];
        Ok(Self {
            abi: Cell::new(NONE),
            linker: RefCell::new((NONE, compiler.backend.linker(&package_imports())?)),
            compiler,
            modules: RefCell::new(HashMap::new()),
        })
    }

    /// Link the host's own ABI ([`Engine::link`]) from now on.
    fn link(&self, abi: &'static [Imports]) -> Result<(), String> {
        self.abi.set(abi);
        // The linker is made again by the next load, for both this ABI and the `tl` table.
        let mut imports = package_imports();
        imports.extend_from_slice(abi);
        let linker = self.compiler.backend.linker(&imports)?;
        *self.linker.borrow_mut() = (abi, linker);
        Ok(())
    }

    /// The linker that links the `tl` table and the host's ABI, made now when it is not the
    /// one that was.
    fn linker(&self) -> Result<Ref<'_, B::Linker>, CallError> {
        let abi = self.abi.get();
        {
            let linked = self.linker.borrow();
            if std::ptr::eq(linked.0, abi) {
                return Ok(Ref::map(linked, |(_, linker)| linker));
            }
        }
        let mut imports = package_imports();
        imports.extend_from_slice(abi);
        let linker = self
            .compiler
            .backend
            .linker(&imports)
            .map_err(|why| CallError::from(Failure::Link(why)))?;
        *self.linker.borrow_mut() = (abi, linker);
        Ok(Ref::map(self.linker.borrow(), |(_, linker)| linker))
    }

    /// Compile `parts` (one module, or a linked set's) for `hash`, each part taken from the
    /// process's cache when it holds it.
    fn compile(
        &self,
        hash: &ModuleHash,
        parts: &[Part<'_>],
        limits: &EngineLimits,
        compiled_in: impl FnOnce(Duration),
    ) -> Result<(), CallError> {
        if self.has(hash) {
            return Ok(());
        }
        let mut modules = Vec::with_capacity(parts.len());
        let mut compiling: Option<Duration> = None;
        for (part, bytes) in parts {
            let module = if let Some(module) = self.compiler.known(part) {
                module
            } else {
                let started = tilo_clock::now();
                let module = self.compiler.backend.compile(bytes)?;
                compiling = Some(
                    compiling
                        .unwrap_or_default()
                        .saturating_add(started.elapsed()),
                );
                self.compiler.keep(part.clone(), module.clone());
                module
            };
            modules.push(module);
        }
        if let Some(took) = compiling {
            compiled_in(took);
        }
        self.adopt(hash, modules, limits)
    }

    /// Use `modules` for `hash` from now on, with its first instance made.
    fn adopt(
        &self,
        hash: &ModuleHash,
        modules: Vec<B::Module>,
        limits: &EngineLimits,
    ) -> Result<(), CallError> {
        let first = {
            let linker = self.linker()?;
            self.compiler
                .backend
                .instantiate(&linker, &modules, limits)?
        };
        self.modules.borrow_mut().insert(
            hash.clone(),
            Compiled {
                modules,
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
    #[cfg(all(feature = "wasmtime", not(target_arch = "wasm32")))]
    fn remove(&self, hash: &ModuleHash) {
        self.modules.borrow_mut().remove(hash);
    }

    fn exports(&self, module: &ModuleHash) -> Vec<String> {
        self.modules
            .borrow()
            .get(module)
            .map_or_else(Vec::new, |c| {
                c.modules.iter().flat_map(B::exports).collect()
            })
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
                .ok_or_else(|| Failure::Link(format!("module {module} is not compiled")))?;
            (c.modules.clone(), c.idle.pop())
        };
        if let Some(instance) = idle {
            return Ok(instance);
        }
        let linker = self.linker()?;
        self.compiler
            .backend
            .instantiate(&linker, &compiled, limits)
            .map_err(CallError::from)
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
        let outcome = self
            .compiler
            .backend
            .call(&mut instance, export, args, limits, cx);
        // A trapped instance may have been left mid-way through its own memory; it is dropped.
        if tilo_wasm_host::reusable(&outcome)
            && let Some(c) = self.modules.borrow_mut().get_mut(module)
        {
            c.idle.push(instance);
        }
        outcome
    }
}

/// One runtime's instances on the tiered engine: each module's on wasmi until its compiled
/// module is ready, then on Cranelift.
#[cfg(all(feature = "wasmtime", not(target_arch = "wasm32")))]
struct TieredPools {
    tiering: &'static tiering::Tiering,
    base: Pools<wasmi_backend::Wasmi>,
    top: Pools<wasmtime_backend::Wasmtime>,
    /// The modules of each linked set, by the set's hash: each is compiled on its own, and the
    /// set moves to compiled code once all of them are. A single module is its own part.
    sets: RefCell<HashMap<ModuleHash, Vec<ModuleHash>>>,
}

#[cfg(all(feature = "wasmtime", not(target_arch = "wasm32")))]
impl TieredPools {
    fn new(tiers: &'static Tiers) -> Result<Self, String> {
        Ok(Self {
            tiering: &tiers.top,
            base: Pools::new(&tiers.base)?,
            top: Pools::new(&tiers.top.compiler)?,
            sets: RefCell::new(HashMap::new()),
        })
    }

    fn compile(
        &self,
        hash: &ModuleHash,
        parts: &[Part<'_>],
        limits: &EngineLimits,
        compiled_in: impl FnOnce(Duration),
    ) -> Result<(), CallError> {
        if self.has(hash) {
            return Ok(());
        }
        if parts.len() != 1 || parts.first().is_some_and(|(part, _)| part != hash) {
            self.sets.borrow_mut().insert(
                hash.clone(),
                parts.iter().map(|(part, _)| part.clone()).collect(),
            );
        }
        // Compiled earlier, in this process or (from the disk cache) in an earlier one: it
        // starts compiled.
        let started = tilo_clock::now();
        let ready: Option<Vec<_>> = parts
            .iter()
            .map(|(part, _)| self.tiering.ready(part))
            .collect();
        if let Some(modules) = ready {
            let adopted = self.top.adopt(hash, modules, limits);
            compiled_in(started.elapsed());
            return adopted;
        }
        self.base.compile(hash, parts, limits, compiled_in)?;
        for (part, bytes) in parts {
            self.tiering.request(part, bytes);
        }
        Ok(())
    }

    /// The modules `hash` is made of: a linked set's, or the module itself.
    fn parts(&self, hash: &ModuleHash) -> Vec<ModuleHash> {
        self.sets
            .borrow()
            .get(hash)
            .cloned()
            .unwrap_or_else(|| vec![hash.clone()])
    }

    fn has(&self, hash: &ModuleHash) -> bool {
        self.top.has(hash) || self.base.has(hash)
    }

    /// Move the module to its compiled code if that is ready. This happens between calls
    /// into it: a call still running on wasmi keeps its instance, which is dropped when it
    /// ends.
    fn tier_up(&self, hash: &ModuleHash, limits: &EngineLimits) {
        // A set whose compiled modules did not instantiate stays on wasmi (`reject`).
        if self.top.has(hash) || !self.base.has(hash) || self.tiering.failure(hash).is_some() {
            return;
        }
        let Some(modules) = self
            .parts(hash)
            .iter()
            .map(|part| self.tiering.compiled(part))
            .collect::<Option<Vec<_>>>()
        else {
            return;
        };
        match self.top.adopt(hash, modules, limits) {
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

    fn link(&self, abi: &'static [Imports]) -> Result<(), String> {
        self.base.link(abi)?;
        self.top.link(abi)
    }
}

enum Pooled {
    Wasmi(Pools<wasmi_backend::Wasmi>),
    #[cfg(all(feature = "wasmtime", not(target_arch = "wasm32")))]
    Wasmtime(Pools<wasmtime_backend::Wasmtime>),
    #[cfg(all(feature = "wasmtime", not(target_arch = "wasm32")))]
    Tiered(Box<TieredPools>),
}

/// The same expression for whichever engine the process runs: [`Pools`] and [`TieredPools`]
/// have the same methods.
macro_rules! on_engine {
    ($pooled:expr, $pools:ident => $body:expr) => {
        match $pooled {
            Pooled::Wasmi($pools) => $body,
            #[cfg(all(feature = "wasmtime", not(target_arch = "wasm32")))]
            Pooled::Wasmtime($pools) => $body,
            #[cfg(all(feature = "wasmtime", not(target_arch = "wasm32")))]
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
    /// What each unit costs, while a profile is on.
    costs: RefCell<Option<Rc<crate::cost::Recorder>>>,
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
            #[cfg(all(feature = "wasmtime", not(target_arch = "wasm32")))]
            Shared::Wasmtime(compiler) => Pooled::Wasmtime(Pools::new(compiler)?),
            #[cfg(all(feature = "wasmtime", not(target_arch = "wasm32")))]
            Shared::Tiered(tiers) => Pooled::Tiered(Box::new(TieredPools::new(tiers)?)),
        };
        Ok(Self {
            kind,
            pooled,
            limits,
            profile: RefCell::new(None),
            costs: RefCell::new(None),
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
            #[cfg(all(feature = "wasmtime", not(target_arch = "wasm32")))]
            Pooled::Tiered(pools) => pools.tier(module),
            _ => on_engine!(&self.pooled, pools => pools.has(module)).then_some(self.kind),
        }
    }

    /// Wait until the tiered engine's compile of the module has ended, so that its next call
    /// runs compiled (or, if the compile failed, stays on wasmi): for benchmarks and tests.
    /// The compile thread ends the wait when it finishes the module, either way. Returns at
    /// once on another engine or for a module not being compiled.
    pub fn settle_compile(&self, module: &ModuleHash) {
        #[cfg(all(feature = "wasmtime", not(target_arch = "wasm32")))]
        if let Pooled::Tiered(pools) = &self.pooled {
            for part in pools.parts(module) {
                pools.tiering.settle(&part);
            }
        }
        #[cfg(not(all(feature = "wasmtime", not(target_arch = "wasm32"))))]
        let _ = module;
    }

    /// Why the tiered engine could not compile the module, which then stays on wasmi; `None`
    /// while it compiles, once it compiled, and on any other engine.
    #[must_use]
    pub fn compile_failure(&self, module: &ModuleHash) -> Option<String> {
        match &self.pooled {
            #[cfg(all(feature = "wasmtime", not(target_arch = "wasm32")))]
            Pooled::Tiered(pools) => pools.tiering.failure(module).or_else(|| {
                pools
                    .parts(module)
                    .iter()
                    .find_map(|part| pools.tiering.failure(part))
            }),
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

    /// Charge the fuel of every call to the unit that made it, as `costs` counts them; cheap
    /// while its profile is off.
    pub(crate) fn watch_costs(&self, costs: Rc<crate::cost::Recorder>) {
        *self.costs.borrow_mut() = Some(costs);
    }

    fn count(&self, f: impl FnOnce(&mut EngineProfile)) {
        if let Some(profile) = self.profile.borrow().as_ref() {
            let mut p = profile.get();
            f(&mut p);
            profile.set(p);
        }
    }

    /// Compile (or fetch from the cache) the module with these bytes, whose imports `host`
    /// answers: the `tl` table and, when it names one, the imports of its own ABI
    /// ([`Host::guest_abi`]).
    pub fn compile(&self, bytes: &[u8]) -> Result<ModuleHash, CallError> {
        let hash = ModuleHash::of_bytes(bytes);
        let parts = [(hash.clone(), bytes)];
        let counted =
            |took: Duration| self.count(|p| p.compiling = p.compiling.saturating_add(took));
        on_engine!(&self.pooled, pools => pools.compile(&hash, &parts, &self.limits, counted))?;
        Ok(hash)
    }

    /// Compile (or fetch from the cache) `modules` as one linked set, called like one module:
    /// an instance of the set instantiates the first module with the imports the engine links,
    /// then each after it with every import answered by the export of that name of the latest
    /// module before it that has one, as the page links a split bundle (its modules import by
    /// name the memory, table, globals and functions the start module exports). A call looks
    /// its export up in the same order. Each module is compiled once per process, cached by its
    /// own hash, so the modules several sets share are compiled once; the set is named by the
    /// hashes of its modules, in order.
    ///
    /// # Errors
    ///
    /// A [`Failure::Link`] for a module that does not compile, an empty set, or a first
    /// instance that cannot be made (an import nothing before it exports).
    pub fn compile_linked(&self, modules: &[&[u8]]) -> Result<ModuleHash, CallError> {
        if modules.is_empty() {
            return Err(Failure::Link("a linked set of no modules".into()).into());
        }
        let parts: Vec<Part<'_>> = modules
            .iter()
            .map(|bytes| (ModuleHash::of_bytes(bytes), *bytes))
            .collect();
        let mut set = blake3::Hasher::new();
        set.update(b"tilolang.linked\0");
        for (part, _) in &parts {
            set.update(part.0.as_bytes());
            set.update(b"\0");
        }
        let hash = ModuleHash(set.finalize().to_hex().to_string());
        let counted =
            |took: Duration| self.count(|p| p.compiling = p.compiling.saturating_add(took));
        on_engine!(&self.pooled, pools => pools.compile(&hash, &parts, &self.limits, counted))?;
        Ok(hash)
    }

    /// Link `abi`'s imports beside the `tl` table, from now on: the imports a host that runs
    /// modules of its own ABI ([`Host::guest_abi`]) answers ([`Host::guest_import`]). A module
    /// loaded after this links with them; one loaded before is dropped and loaded again.
    pub fn link(&self, abi: &'static [Imports]) -> Result<(), String> {
        on_engine!(&self.pooled, pools => pools.link(abi))
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
        let started = profile.as_ref().map(|_| tilo_clock::now());
        let costs = self
            .costs
            .borrow()
            .as_ref()
            .filter(|costs| costs.on())
            .map(Rc::clone);
        let cx = Cx {
            host,
            profile,
            costs,
        };
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
