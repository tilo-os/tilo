//! The device's side of an app's model: one WebAssembly module per app
//! version, from the app's package, executed without building the app.
//!
//! A model module (`model.wasm` in a `.tapp`'s payload) imports nothing but
//! the calls [`ABI`] names and exports [`EXPORT`]. This crate compiles the
//! bytes on the engine the rest of the system executes WebAssembly on
//! ([`tilolang_runtime::engine`]) and calls it with one JSON request: the
//! genesis of a new resource, or one action with its arguments, naming the
//! resource kind it is for so one module serves every resource its app
//! declares. What comes back is the invocation the action API recorded, which
//! a peer replays, and the Automerge changes it wrote.
//!
//! The engine links only [`ABI`]: a module importing anything else fails to
//! link, which is how "no imports beyond the device's" is enforced rather
//! than checked.
//!
//! Compiled modules are kept by the engine's process-wide cache, so the second
//! action of an installed app costs a call, not a compile.
//!
//! # Split modules
//!
//! An app built in this repository carries only its own code: every app's
//! model is linked into one module and split (tools/ci/app-models.sh,
//! `wasm-split --models`) into the model runtime the apps share (automerge,
//! std, the resource layer, `serde_json`), which the shared runtime bundle
//! carries once, and one module per app. Such a module names the runtime it
//! was split against in its [`RUNTIME_SECTION`], and the export to call. It
//! is linked after that runtime ([`Engine::compile_linked`]): the runtime's
//! start module with [`ABI`], its shared modules, then the app's module, each
//! import answered by the exports of the modules before it. The runtime is
//! found by its hash ([`Runtime::find`]); a module whose runtime is not here
//! is refused, naming the runtime it needs and the ones that are. A module
//! with no such section (an app built alone, `tilo-app model`) is whole and
//! is called as it is.

#![deny(rust_2018_idioms)]

mod runtime;

use std::cell::RefCell;
use std::rc::Rc;

pub use runtime::{RUNTIME_DIR, RUNTIME_SECTION, RUNTIME_VARIABLE, Runtime, runtime_dirs};

use tilolang_runtime::engine::{Engine, EngineLimits, Host, HostFail, Imports, Wasm};
use tilolang_runtime::ids::ModuleHash;

/// The functions a model module imports, as `(module, name, parameters, result)`.
pub const ABI: &[Imports] = &[
    ("tilo_model", "request_length", "", "i"),
    ("tilo_model", "request", "ii", "i"),
    ("tilo_model", "answer", "ii", ""),
];

/// The export a model module is called through.
pub const EXPORT: &str = "tilo_model_apply";

/// Why a model could not be loaded, asked or understood.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("the model module: {0}")]
    Load(String),
    #[error("the model runtime: {0}")]
    Runtime(String),
    #[error("the model: {0}")]
    Call(String),
    #[error("the model refused: {0}")]
    Refused(String),
    #[error("the model answered nothing")]
    Silent,
    #[error("the model's answer: {0}")]
    Answer(String),
}

/// One app version's model, compiled and ready to apply its actions.
pub struct Model {
    engine: Engine,
    module: ModuleHash,
    /// What the module is called through: [`EXPORT`], or a split module's
    /// entry in the runtime's start module.
    export: String,
}

/// What the device asks a model to do, as the module's ABI carries it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// Make a new resource's genesis: the module derives the actor its
    /// identity names ([`tilo_resource::genesis::actor`]), so every holder of
    /// it writes the same change.
    Initialize,
    /// Apply the request's action to the request's document.
    Apply,
}

/// One request to a model.
#[derive(Debug)]
pub struct Request<'a> {
    /// What the model is asked to do.
    pub op: Op,
    /// The resource kind the request is for: an app's module serves every
    /// resource the app declares, and routes the request by this
    /// ([`tilo_resource::Resource::KIND`]).
    pub kind: &'a str,
    /// The resource's document id: the empty string for what an app keeps per
    /// workspace, or a named document's id. The module reads it as
    /// [`Op::Initialize`]'s identity.
    pub id: &'a str,
    /// The workspace the resource is minted in.
    pub home: &'a str,
    /// The bytes of the document the action acts on; empty for
    /// [`Op::Initialize`].
    pub bytes: &'a [u8],
    /// The invoking member ([`tilo_action_policy::MemberId::of_actor_bytes`]:
    /// a person's key, `agent:<id>` or `bot:<id>`).
    pub member: &'a str,
    /// Who acts and for whom, as the device's action API recorded it, when the
    /// member acts for someone ([`tilo_actions::Acting`]): an agent a person's
    /// session handed the action to. `None` for a member acting for themselves.
    pub acting: Option<serde_json::Value>,
    /// The action's id in the resource's vocabulary.
    pub action: &'a str,
    /// Its arguments, as the action API binds them.
    pub arguments: serde_json::Map<String, serde_json::Value>,
    /// The origin clock, milliseconds since the Unix epoch.
    pub now: i64,
}

/// What a model answered.
#[derive(Debug)]
pub struct Answer {
    /// The bytes of the resource's document, for [`Op::Initialize`].
    pub document: Option<Vec<u8>>,
    /// The changes the action added, against the bytes it was given.
    pub changes: Option<Vec<u8>>,
    /// The invocation as the action API recorded it: what a peer replays.
    pub envelope: Option<serde_json::Value>,
}

impl Model {
    /// Compile `bytes`, an app's `model.wasm`: a whole module, or a split one
    /// linked after the runtime it names, found by [`Runtime::find`].
    ///
    /// # Errors
    /// Returns [`ModelError::Load`] when the engine refuses the module, which
    /// is what an import the device does not answer looks like, and
    /// [`ModelError::Runtime`] when the runtime a split module names is not
    /// here.
    pub fn compile(bytes: &[u8]) -> Result<Self, ModelError> {
        match runtime::named(bytes)? {
            None => {
                let engine = Self::engine()?;
                let module = engine
                    .compile(bytes)
                    .map_err(|error| ModelError::Load(error.to_string()))?;
                Ok(Self {
                    engine,
                    module,
                    export: EXPORT.to_owned(),
                })
            }
            Some(named) => {
                let runtime = Runtime::find(&named.runtime)?;
                Self::link(bytes, &runtime, named.entry)
            }
        }
    }

    /// Compile `bytes`, a split app module, linked after `runtime`.
    ///
    /// # Errors
    /// As [`Self::compile`]; a module that is not split, or was split against
    /// another runtime, is [`ModelError::Runtime`], naming both.
    pub fn compile_with(bytes: &[u8], runtime: &Runtime) -> Result<Self, ModelError> {
        let named = runtime::named(bytes)?.ok_or_else(|| {
            ModelError::Runtime(format!(
                "the module is whole: it links against no runtime, not {}",
                runtime.hash()
            ))
        })?;
        if named.runtime != runtime.hash() {
            return Err(ModelError::Runtime(format!(
                "the module was split against model runtime {}, not {} ({})",
                named.runtime,
                runtime.hash(),
                runtime.dir().display()
            )));
        }
        Self::link(bytes, runtime, named.entry)
    }

    fn link(bytes: &[u8], runtime: &Runtime, export: String) -> Result<Self, ModelError> {
        let engine = Self::engine()?;
        let mut modules: Vec<&[u8]> = runtime.modules().collect();
        modules.push(bytes);
        let module = engine
            .compile_linked(&modules)
            .map_err(|error| ModelError::Load(error.to_string()))?;
        Ok(Self {
            engine,
            module,
            export,
        })
    }

    fn engine() -> Result<Engine, ModelError> {
        let engine = Engine::new(EngineLimits::default()).map_err(ModelError::Load)?;
        engine.link(ABI).map_err(ModelError::Load)?;
        Ok(engine)
    }

    /// Ask the model to do `request`.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the module traps, spends its fuel, imports
    /// something the device does not answer, answers nothing, or refuses the
    /// action (in the module's words).
    pub fn ask(&self, request: &Request<'_>) -> Result<Answer, ModelError> {
        let serving = Rc::new(Serving {
            request: encode(request)?,
            answer: RefCell::new(None),
        });
        let outcome = self
            .engine
            .call(&self.module, &self.export, &[], serving.clone());
        outcome.map_err(|error| ModelError::Call(error.to_string()))?;
        let bytes = serving
            .answer
            .borrow_mut()
            .take()
            .ok_or(ModelError::Silent)?;
        let answer: Wire = serde_json::from_slice(&bytes).map_err(|error| {
            ModelError::Answer(format!("{error}: {}", String::from_utf8_lossy(&bytes)))
        })?;
        if let Some(error) = answer.error {
            return Err(ModelError::Refused(error));
        }
        Ok(Answer {
            document: answer.document.as_deref().map(decode).transpose()?,
            changes: answer.changes.as_deref().map(decode).transpose()?,
            envelope: answer.envelope,
        })
    }
}

/// The hash of the model runtime `module` was split against, or `None` for a
/// whole module, which links against nothing.
///
/// # Errors
/// Bytes that are not a WebAssembly module, or whose runtime section does not
/// read.
pub fn linked_runtime(module: &[u8]) -> Result<Option<String>, ModelError> {
    Ok(runtime::named(module)?.map(|named| named.runtime))
}

/// The request as the module reads it.
fn encode(request: &Request<'_>) -> Result<Vec<u8>, ModelError> {
    let base = base64::engine::general_purpose::STANDARD;
    let wire = serde_json::json!({
        "op": match request.op {
            Op::Initialize => "initialize",
            Op::Apply => "apply",
        },
        "kind": request.kind,
        "id": document_id(request.id),
        "home": request.home,
        "document": base64::Engine::encode(&base, request.bytes),
        "member": request.member,
        "acting": request.acting,
        "action": request.action,
        "arguments": request.arguments,
        "now": request.now,
    });
    serde_json::to_vec(&wire).map_err(|error| ModelError::Answer(error.to_string()))
}

/// The document id as the module reads it: the empty route is what an app
/// keeps per workspace, whose ownership key is the package primary's.
pub fn document_id(id: &str) -> String {
    if id.is_empty() {
        "@tilo:package-primary".to_owned()
    } else {
        id.to_owned()
    }
}

/// The module's answer, as it writes it.
#[derive(Debug, serde::Deserialize)]
struct Wire {
    #[serde(default)]
    document: Option<String>,
    #[serde(default)]
    changes: Option<String>,
    #[serde(default)]
    envelope: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<String>,
}

fn decode(text: &str) -> Result<Vec<u8>, ModelError> {
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, text)
        .map_err(|error| ModelError::Answer(error.to_string()))
}

/// This side of one call: the request the module reads and the answer it
/// writes.
struct Serving {
    request: Vec<u8>,
    answer: RefCell<Option<Vec<u8>>>,
}

impl Host for Serving {
    fn import(&self, id: usize, _: &[u8], _: &[Wasm]) -> Result<Option<Wasm>, HostFail> {
        Err(HostFail::Trap(format!("the model called import {id}")))
    }

    fn guest_abi(&self) -> &'static [Imports] {
        ABI
    }

    fn guest_import(
        &self,
        module: &str,
        name: &str,
        memory: &[u8],
        args: &[Wasm],
    ) -> Result<tilolang_runtime::engine::GuestAnswer, HostFail> {
        if module != "tilo_model" {
            return Err(HostFail::Trap(format!(
                "the model imports {module}::{name}"
            )));
        }
        match (name, args) {
            ("request_length", []) => Ok(tilolang_runtime::engine::GuestAnswer::Value(Wasm::I32(
                i32::try_from(self.request.len()).unwrap_or(i32::MAX),
            ))),
            ("request", [Wasm::I32(offset), Wasm::I32(capacity)]) => {
                let capacity = usize::try_from(*capacity).unwrap_or(0);
                let count = capacity.min(self.request.len());
                let bytes = self.request.get(..count).unwrap_or_default().to_vec();
                Ok(tilolang_runtime::engine::GuestAnswer::Bytes {
                    offset: u32::try_from(*offset).unwrap_or(u32::MAX),
                    bytes,
                    result: Some(Wasm::I32(i32::try_from(count).unwrap_or(i32::MAX))),
                })
            }
            ("answer", [Wasm::I32(offset), Wasm::I32(length)]) => {
                let start = usize::try_from(*offset).unwrap_or(usize::MAX);
                let end = start
                    .checked_add(usize::try_from(*length).unwrap_or(usize::MAX))
                    .ok_or_else(|| {
                        HostFail::Trap("the model's answer lies outside its memory".into())
                    })?;
                let answer = memory
                    .get(start..end)
                    .ok_or_else(|| {
                        HostFail::Trap("the model's answer lies outside its memory".into())
                    })?
                    .to_vec();
                *self.answer.borrow_mut() = Some(answer);
                Ok(tilolang_runtime::engine::GuestAnswer::Nothing)
            }
            _ => Err(HostFail::Trap(format!(
                "the model called {module}::{name} wrongly"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A module with no code whose runtime section names `runtime`.
    fn split_against(runtime: &str) -> Vec<u8> {
        let data = format!(r#"{{"runtime":"{runtime}","entry":"tilo_model_apply_x"}}"#);
        let mut contents = vec![u8::try_from(RUNTIME_SECTION.len()).unwrap()];
        contents.extend_from_slice(RUNTIME_SECTION.as_bytes());
        contents.extend_from_slice(data.as_bytes());
        let mut module = b"\0asm\x01\0\0\0".to_vec();
        module.push(0);
        module.push(u8::try_from(contents.len()).unwrap());
        module.extend_from_slice(&contents);
        module
    }

    #[test]
    fn a_module_split_against_another_runtime_is_refused_naming_both() {
        let dir = tempfile::tempdir().unwrap();
        let runtime_dir = dir.path().join(RUNTIME_DIR);
        std::fs::create_dir(&runtime_dir).unwrap();
        let main = b"\0asm\x01\0\0\0".to_vec();
        std::fs::write(runtime_dir.join("main.wasm"), &main).unwrap();
        let mut hasher = blake3::Hasher::new();
        hasher.update(&(main.len() as u64).to_le_bytes());
        hasher.update(&main);
        let hash = hasher.finalize().to_hex().to_string();
        std::fs::write(
            runtime_dir.join("runtime.json"),
            serde_json::json!({"schema": 1, "hash": hash, "modules": ["main.wasm"]}).to_string(),
        )
        .unwrap();
        let runtime = Runtime::read(&runtime_dir).unwrap();

        let other = "f".repeat(64);
        let module = split_against(&other);
        assert_eq!(linked_runtime(&module).unwrap(), Some(other.clone()));
        let Err(refused) = Model::compile_with(&module, &runtime) else {
            panic!("a module split against another runtime linked");
        };
        let refused = refused.to_string();
        assert!(
            refused.contains(&other) && refused.contains(&hash),
            "{refused}"
        );

        // Nothing here holds the runtime it names: refused, naming it.
        let Err(missing) = Model::compile(&module) else {
            panic!("a module whose runtime is not here linked");
        };
        assert!(missing.to_string().contains(&other), "{missing}");
    }
}
