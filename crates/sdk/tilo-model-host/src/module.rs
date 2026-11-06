//! One Rust app's model module, loaded and run by the document host.
//!
//! `docs/architecture/apps.md` §1–§2: an app's model and verbs are a WebAssembly module that
//! runs in the system's document host, with no imports beyond the host's. This is that host's
//! side of it: the module's bytes, the engine that runs them, and the [`Host`] that answers
//! the module's imports with the record it was asked about.
//!
//! An app's code is about as trustworthy as a random web page's, so this is the whole of what
//! it can reach: one request in, one answer out, and no clock, key, file, network or actor it
//! did not receive. What the module is handed is the record's document — which the host
//! opened — and what it answers is the document it made, which the host decides whether to
//! keep.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_model_abi::{Answer, IMPORTS, MODULE, Refusal, Request, export};
use tilolang_runtime::engine::{Engine, GuestAnswer, Host as WasmHost, HostFail, Wasm};

/// Why a module could not be loaded or its call did not answer.
#[derive(Debug, thiserror::Error)]
pub enum ModuleError {
    /// The engine refused the bytes: they are not a module, or they import what this host
    /// does not answer.
    #[error("{0}")]
    Load(String),
    /// The module has no export for the record's kind.
    #[error("the module of {0} exports nothing for a {1} record (`{2}`)")]
    NoExport(String, String, String),
    /// The call trapped, ran out of its budget, or ended without answering.
    #[error("{0}")]
    Call(String),
    /// The model refused the call, in its own words.
    #[error("{0}")]
    Refused(String),
    /// The module answered something the host does not read as an answer.
    #[error("the module of {0} answered what is not one: {1}")]
    Malformed(String, String),
}

/// One app's model module: its bytes, compiled once, and the engine that runs its exports.
pub struct Module {
    /// The app whose code this is, as its package names it.
    app: String,
    /// The compiled module, by the hash of its bytes.
    hash: tilolang_runtime::ids::ModuleHash,
    /// The engine, which the host keeps for every module it runs.
    engine: Rc<Engine>,
}

/// What a call answered, between the module's `answer` import and the host reading it.
enum Answered {
    Success(Answer),
    Refused(Refusal),
    /// The module failed without saying why (a trap before it refused).
    Nothing,
}

impl Module {
    /// The module `wasm` of the app `app`, loaded into `engine`.
    ///
    /// # Errors
    /// The bytes are not a module, or they import what this host does not answer: a model
    /// module reaches nothing but [`IMPORTS`], and anything else is refused here rather than
    /// bounded later.
    pub fn load(app: &str, wasm: &[u8], engine: Rc<Engine>) -> Result<Self, ModuleError> {
        engine
            .link(IMPORTS)
            .map_err(|error| ModuleError::Load(format!("{app}: {error}")))?;
        let hash = engine
            .compile(wasm)
            .map_err(|error| ModuleError::Load(format!("{app}: {error}")))?;
        Ok(Self {
            app: app.to_owned(),
            hash,
            engine,
        })
    }

    /// The app whose code this is.
    #[must_use]
    pub fn app(&self) -> &str {
        &self.app
    }

    /// The exports the module answers, by name.
    #[must_use]
    pub fn exports(&self) -> Vec<String> {
        self.engine.exports(&self.hash)
    }

    /// Whether the module answers the record `kind`'s calls.
    #[must_use]
    pub fn serves(&self, kind: &str) -> bool {
        self.exports().iter().any(|name| name == &export(kind))
    }

    /// Ask the module for `request`, which is about the record of `kind` this app keeps: the
    /// module runs the model's own code and the host answers what it asked for — the
    /// document, the time, the entropy.
    ///
    /// # Errors
    /// The module has no export for the kind, trapped, or refused; a refusal is the model's
    /// own words, and nothing of it is kept.
    pub fn run(&self, kind: &str, request: &Request) -> Result<Answer, ModuleError> {
        let symbol = export(kind);
        if !self.serves(kind) {
            return Err(ModuleError::NoExport(
                self.app.clone(),
                kind.to_owned(),
                symbol,
            ));
        }
        let asked = serde_json::to_vec(request)
            .map_err(|error| ModuleError::Malformed(self.app.clone(), error.to_string()))?;
        // The engine keeps its host for the call's whole length, so the call owns
        // everything it answers with; what it publishes is read back afterwards.
        let state = Rc::new(RefCell::new(Saying::default()));
        let host = Call {
            app: self.app.clone(),
            request: asked,
            state: Rc::clone(&state),
        };
        let outcome = self.engine.call(&self.hash, &symbol, &[], Rc::new(host));
        // The call's answer is what its imports published, whether or not the trap that ends a
        // refusal reached the host first.
        let published = state.borrow_mut().answered.take();
        let logged = state.borrow().logged.join("; ");
        match published {
            Some(Answered::Success(answer)) => Ok(answer),
            Some(Answered::Refused(refused)) => Err(ModuleError::Refused(refused.message)),
            Some(Answered::Nothing) | None => match outcome {
                Ok(_) => Err(ModuleError::Call(format!(
                    "{}'s module answered nothing for {kind}",
                    self.app
                ))),
                // A trap is a panic in the model: what it said to the log before it is why.
                Err(error) if logged.is_empty() => {
                    Err(ModuleError::Call(format!("{}: {error}", self.app)))
                }
                Err(error) => Err(ModuleError::Call(format!(
                    "{}: {error} (it said: {logged})",
                    self.app
                ))),
            },
        }
    }
}

/// What one call answered and said. A trapped model's last words are the only account of why.
#[derive(Default)]
struct Saying {
    answered: Option<Answered>,
    logged: Vec<String>,
}

/// One call into a module: the request to hand it, and what it answered and said.
struct Call {
    app: String,
    request: Vec<u8>,
    state: Rc<RefCell<Saying>>,
}

impl WasmHost for Call {
    fn import(&self, id: usize, _memory: &[u8], _args: &[Wasm]) -> Result<Option<Wasm>, HostFail> {
        Err(HostFail::Trap(format!(
            "{}'s model called the package import {id}, which this host does not answer",
            self.app
        )))
    }

    fn guest_abi(&self) -> &'static [(&'static str, &'static str, &'static str, &'static str)] {
        IMPORTS
    }

    fn guest_import(
        &self,
        module: &str,
        name: &str,
        memory: &[u8],
        args: &[Wasm],
    ) -> Result<GuestAnswer, HostFail> {
        if module != MODULE {
            return Err(HostFail::Trap(format!(
                "{}'s model imported {module}::{name}",
                self.app
            )));
        }
        match name {
            "request" => answer_bytes(args, &self.request),
            "entropy" => entropy(memory, usize_of(args.first())?, usize_of(args.get(1))?),
            "answer" => {
                let bytes = read(memory, args)?;
                let answer: Answer = serde_json::from_slice(&bytes).map_err(|error| {
                    HostFail::Trap(format!(
                        "{}'s model answered what is not one: {error}",
                        self.app
                    ))
                })?;
                self.state.borrow_mut().answered = Some(Answered::Success(answer));
                Ok(GuestAnswer::Nothing)
            }
            "fail" => {
                let bytes = read(memory, args)?;
                let refused: Refusal = serde_json::from_slice(&bytes).map_err(|error| {
                    HostFail::Trap(format!(
                        "{}'s model refused in what is not one: {error}",
                        self.app
                    ))
                })?;
                self.state.borrow_mut().answered = Some(Answered::Refused(refused));
                Ok(GuestAnswer::Nothing)
            }
            "log" => {
                let bytes = read(memory, args)?;
                let line = String::from_utf8_lossy(&bytes).into_owned();
                tracing::debug!(app = %self.app, "{line}");
                self.state.borrow_mut().logged.push(line);
                Ok(GuestAnswer::Nothing)
            }
            other => Err(HostFail::Trap(format!(
                "{MODULE}::{other} is not this host's"
            ))),
        }
    }
}

/// `bytes` written into the module's buffer at the pointer it gave, or the negated size it
/// needs when the buffer is too small.
fn answer_bytes(args: &[Wasm], bytes: &[u8]) -> Result<GuestAnswer, HostFail> {
    let pointer = usize_of(args.first())?;
    let capacity = usize_of(args.get(1))?;
    if bytes.len() > capacity {
        let need = i32::try_from(bytes.len())
            .map_err(|_| HostFail::Trap("the request is larger than a buffer can be".into()))?;
        return Ok(GuestAnswer::Value(Wasm::I32(-need)));
    }
    Ok(GuestAnswer::Bytes {
        offset: u32::try_from(pointer).map_err(|_| HostFail::Trap("a bad pointer".into()))?,
        bytes: bytes.to_vec(),
        result: Some(Wasm::I32(i32::try_from(bytes.len()).unwrap_or(i32::MAX))),
    })
}

/// `count` bytes of entropy, written where the caller points, and how many were written.
///
/// The bytes come from the system's random source: this host is the session, which has one.
/// The module has none of its own, so a new document's actor id is drawn here, and the change
/// the host replays from the same request is the one the module made.
fn entropy(memory: &[u8], pointer: usize, count: usize) -> Result<GuestAnswer, HostFail> {
    // Nothing to fill is nothing to ask for, and it needs no memory either.
    if count == 0 {
        return Ok(GuestAnswer::Value(Wasm::I32(0)));
    }
    pointer
        .checked_add(count)
        .filter(|&end| end <= memory.len())
        .ok_or_else(|| {
            HostFail::Trap("the module asked for entropy outside its memory".to_owned())
        })?;
    let offset = u32::try_from(pointer).map_err(|_| HostFail::Trap("a bad pointer".into()))?;
    let mut bytes = vec![0_u8; count];
    getrandom::fill(&mut bytes)
        .map_err(|error| HostFail::Trap(format!("this host has no entropy: {error}")))?;
    Ok(GuestAnswer::Bytes {
        offset,
        bytes,
        result: Some(Wasm::I32(i32::try_from(count).unwrap_or(i32::MAX))),
    })
}

/// The bytes of a `(pointer, length)` pair in the module's memory.
fn read(memory: &[u8], args: &[Wasm]) -> Result<Vec<u8>, HostFail> {
    let pointer = usize_of(args.first())?;
    let length = usize_of(args.get(1))?;
    let end = pointer
        .checked_add(length)
        .filter(|&end| end <= memory.len())
        .ok_or_else(|| HostFail::Trap("the module read outside its memory".to_owned()))?;
    Ok(memory.get(pointer..end).unwrap_or_default().to_vec())
}

fn usize_of(value: Option<&Wasm>) -> Result<usize, HostFail> {
    match value {
        Some(Wasm::I32(value)) => usize::try_from(*value)
            .map_err(|_| HostFail::Trap("a negative pointer or length".to_owned())),
        _ => Err(HostFail::Trap(
            "an import's pointer is not a number".to_owned(),
        )),
    }
}
