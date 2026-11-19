//! Why a module was not loaded or a call into it failed: one type every engine reports in, so a
//! host tells its person the same thing whichever engine ran the guest.

/// What [`Failure::OutOfFuel`] says on every engine. A host that reports a spent budget in its
/// own words still says this one thing about why.
pub const OUT_OF_FUEL: &str = "all fuel consumed by WebAssembly";

/// What [`Failure::MemoryLimit`] says on every engine: wasm's own text for a growth past the
/// limit, which wasmi raises as a trap and wasmtime as the error its limiter returns.
pub const GROWTH_LIMITED: &str = "growth operation limited";

/// Why a module was refused or a call failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Failure {
    /// The guest stopped with wasm's trap text.
    #[error("trap: {0}")]
    Trap(String),
    /// The call spent its whole budget.
    #[error("out of fuel: {0}")]
    OutOfFuel(String),
    /// The guest grew its memory or a table past what its instance may hold.
    #[error("memory limit: {0}")]
    MemoryLimit(String),
    /// The module does not export the function the caller asked for.
    #[error("module has no export `{0}`")]
    NoExport(String),
    /// The module could not be instantiated, linked or given its first fuel.
    #[error("cannot link module: {0}")]
    Link(String),
    /// The bytes are not a wasm module.
    #[error("not a wasm module: {0}")]
    Invalid(String),
    /// The module imports something its host does not admit, named `module::name`.
    #[error("a module that may import nothing imports {0}")]
    Imports(String),
}

/// What a trap says, the same on every engine: wasm's trap texts, which wasmi and wasmtime share
/// apart from wasmi naming the index of an uninitialized table element.
pub(crate) fn trap_text(text: &str) -> String {
    if text == "uninitialized element 2" {
        "uninitialized element".to_owned()
    } else {
        text.to_owned()
    }
}
