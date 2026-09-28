//! Web builds: the same package, packed around a WebAssembly side module.
//!
//! An app author writes one `tilo-app.toml`. The web build of that package
//! keeps the authored manifest — id, version, publisher, sandbox, providers —
//! and the packer adds what only the build knows, read from the module
//! itself:
//!
//! - the toolkit-core ABI the module was linked against, from its
//!   [`RUNTIME_ABI_SECTION`] custom section;
//! - the post-MVP WebAssembly features it uses, from LLVM's
//!   `target_features` custom section.
//!
//! [`web_manifest`] records both in the manifest's `[target]` block, points
//! the entrypoint at the module and drops native-only fields. The result is
//! packed and signed exactly like a native package; see [`crate::pack()`].

use std::path::PathBuf;

use crate::id::Kind;
use crate::manifest::{Entrypoint, EntrypointType, IsaRequirements, Manifest, Target};

/// Custom section holding the toolkit-core build ABI id a module links
/// against (or the core provides), as UTF-8:
/// `tilo-runtime/<version>+wasm.<32 hex>`.
pub const RUNTIME_ABI_SECTION: &str = "tilo.runtime_abi";

const TARGET_FEATURES_SECTION: &str = "target_features";
const WASM_MAGIC: &[u8; 4] = b"\0asm";
const WASM_VERSION: [u8; 4] = [1, 0, 0, 0];

/// What a module says about the build that produced it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WasmModuleInfo {
    /// The [`RUNTIME_ABI_SECTION`] contents, when present.
    pub runtime_abi: Option<String>,
    /// Features the module uses (`+`/`=` entries of `target_features`), when
    /// the section is present.
    pub features: Option<Vec<String>>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WebBuildError {
    #[error("not a WebAssembly 1.0 binary module")]
    NotWasm,
    #[error("WebAssembly module is truncated or malformed at byte {0}")]
    Malformed(usize),
    #[error("custom section {0} appears more than once")]
    DuplicateSection(&'static str),
    #[error("{RUNTIME_ABI_SECTION} section is not a `tilo-runtime/<version>+wasm.<32 hex>` id")]
    BadRuntimeAbi,
    #[error(
        "the module carries no {RUNTIME_ABI_SECTION} section; build it against the toolkit core \
         or pass the ABI explicitly"
    )]
    MissingRuntimeAbi,
    #[error("the module was linked against ABI {module}, not {requested}")]
    RuntimeAbiMismatch { module: String, requested: String },
    #[error(
        "the module carries no target_features section; pass the canonical wasm isa explicitly"
    )]
    MissingFeatures,
    #[error("web isa: {0}")]
    Isa(String),
    #[error("web runtime: {0}")]
    Runtime(String),
    #[error("a web app needs `package.runtime = \"tilo-runtime/<version>\"` in its manifest")]
    AppWithoutRuntime,
    #[error("only kind=app and kind=runtime packages have a web build")]
    UnsupportedKind,
    #[error("the manifest names runtime {declared}, but the module was built for {module}")]
    RuntimeReleaseMismatch { declared: String, module: String },
    #[error("module path {0:?} must be a plain `.wasm` file name")]
    BadModulePath(String),
}

/// Read the build facts a module carries. Only section framing is parsed;
/// the code itself is the loader's business.
///
/// # Errors
///
/// Returns [`WebBuildError::NotWasm`] without the WebAssembly 1.0 header,
/// [`WebBuildError::Malformed`] for broken section framing,
/// [`WebBuildError::DuplicateSection`] for a repeated build section and
/// [`WebBuildError::BadRuntimeAbi`] for a malformed ABI id.
pub fn inspect_wasm_module(bytes: &[u8]) -> Result<WasmModuleInfo, WebBuildError> {
    if bytes.get(..4) != Some(WASM_MAGIC.as_slice())
        || bytes.get(4..8) != Some(WASM_VERSION.as_slice())
    {
        return Err(WebBuildError::NotWasm);
    }
    let mut info = WasmModuleInfo::default();
    let mut at = 8;
    while at < bytes.len() {
        let id = *bytes.get(at).ok_or(WebBuildError::Malformed(at))?;
        let (size, body) = read_leb_u32(bytes, at.saturating_add(1))?;
        let end = body
            .checked_add(size as usize)
            .filter(|end| *end <= bytes.len())
            .ok_or(WebBuildError::Malformed(at))?;
        if id == 0 {
            let (name_len, name_at) = read_leb_u32(bytes, body)?;
            let payload_at = name_at
                .checked_add(name_len as usize)
                .filter(|payload_at| *payload_at <= end)
                .ok_or(WebBuildError::Malformed(body))?;
            let name = bytes.get(name_at..payload_at).unwrap_or_default();
            let payload = bytes.get(payload_at..end).unwrap_or_default();
            if name == RUNTIME_ABI_SECTION.as_bytes() {
                if info.runtime_abi.is_some() {
                    return Err(WebBuildError::DuplicateSection(RUNTIME_ABI_SECTION));
                }
                let abi = std::str::from_utf8(payload).map_err(|_| WebBuildError::BadRuntimeAbi)?;
                if registry_types::parse_web_runtime(abi).is_err() {
                    return Err(WebBuildError::BadRuntimeAbi);
                }
                info.runtime_abi = Some(abi.to_string());
            } else if name == TARGET_FEATURES_SECTION.as_bytes() {
                if info.features.is_some() {
                    return Err(WebBuildError::DuplicateSection(TARGET_FEATURES_SECTION));
                }
                info.features = Some(read_target_features(bytes, payload_at, end)?);
            }
        }
        at = end;
    }
    Ok(info)
}

/// `target_features`: a vector of `(prefix byte, name)`; `+` means used and
/// `=` used-and-required-of-every-linked-object, `-` means disallowed.
fn read_target_features(
    bytes: &[u8],
    start: usize,
    end: usize,
) -> Result<Vec<String>, WebBuildError> {
    let (count, mut at) = read_leb_u32(bytes, start)?;
    let mut features = Vec::new();
    for _ in 0..count {
        let prefix = *bytes
            .get(at)
            .filter(|_| at < end)
            .ok_or(WebBuildError::Malformed(at))?;
        let (len, name_at) = read_leb_u32(bytes, at.saturating_add(1))?;
        let name_end = name_at
            .checked_add(len as usize)
            .filter(|name_end| *name_end <= end)
            .ok_or(WebBuildError::Malformed(at))?;
        let name = std::str::from_utf8(bytes.get(name_at..name_end).unwrap_or_default())
            .map_err(|_| WebBuildError::Malformed(name_at))?;
        if matches!(prefix, b'+' | b'=') && !features.iter().any(|known| known == name) {
            features.push(name.to_string());
        }
        at = name_end;
    }
    if at != end {
        return Err(WebBuildError::Malformed(at));
    }
    Ok(features)
}

fn read_leb_u32(bytes: &[u8], mut at: usize) -> Result<(u32, usize), WebBuildError> {
    let start = at;
    let mut value: u32 = 0;
    for shift in [0u32, 7, 14, 21, 28] {
        let byte = *bytes.get(at).ok_or(WebBuildError::Malformed(start))?;
        at = at.saturating_add(1);
        let low = u32::from(byte & 0x7f);
        if shift == 28 && low > 0x0f {
            return Err(WebBuildError::Malformed(start));
        }
        value |= low.wrapping_shl(shift);
        if byte & 0x80 == 0 {
            return Ok((value, at));
        }
    }
    Err(WebBuildError::Malformed(start))
}

/// How a web build is described beyond what its module says.
#[derive(Clone, Debug, Default)]
pub struct WebBuildOptions<'a> {
    /// Canonical wasm `isa`, replacing the module's `target_features`.
    pub isa: Option<&'a str>,
    /// ABI id for a module without [`RUNTIME_ABI_SECTION`]; must agree with
    /// the section when both exist.
    pub runtime_abi: Option<&'a str>,
    /// For `kind = "runtime"`: the release the core is expected to provide,
    /// `tilo-runtime/<version>`; checked against the ABI id.
    pub provides: Option<&'a str>,
}

/// The target a module is built for: its canonical isa and full runtime.
///
/// # Errors
///
/// Returns a [`WebBuildError`] when the ABI or features are missing,
/// disagree with an explicit value, or are not canonical.
pub fn web_target(
    manifest: &Manifest,
    module_path: &str,
    info: &WasmModuleInfo,
    options: &WebBuildOptions<'_>,
) -> Result<Target, WebBuildError> {
    if module_path.contains('/') || !crate::package::is_safe_path(module_path) {
        return Err(WebBuildError::BadModulePath(module_path.to_string()));
    }
    let abi = match (info.runtime_abi.as_deref(), options.runtime_abi) {
        (Some(module), Some(requested)) if module != requested => {
            return Err(WebBuildError::RuntimeAbiMismatch {
                module: module.to_string(),
                requested: requested.to_string(),
            });
        }
        (Some(abi), _) | (None, Some(abi)) => abi,
        (None, None) => return Err(WebBuildError::MissingRuntimeAbi),
    };
    let isa = match options.isa {
        Some(isa) => {
            registry_types::parse_wasm_isa(isa)
                .map_err(|error| WebBuildError::Isa(error.to_string()))?;
            isa.to_string()
        }
        None => registry_types::canonical_wasm_isa(
            info.features
                .as_deref()
                .ok_or(WebBuildError::MissingFeatures)?
                .iter()
                .map(String::as_str),
        )
        .map_err(|error| WebBuildError::Isa(error.to_string()))?,
    };
    let runtime = registry_types::parse_web_runtime(abi)
        .map_err(|error| WebBuildError::Runtime(error.to_string()))?;
    let declared = match manifest.package.kind {
        Kind::App => Some(
            manifest
                .package
                .runtime
                .as_deref()
                .ok_or(WebBuildError::AppWithoutRuntime)?,
        ),
        Kind::Runtime => options.provides,
        Kind::Service | Kind::FormatProvider | Kind::Plugin => {
            return Err(WebBuildError::UnsupportedKind);
        }
    };
    if let Some(declared) = declared
        && declared != runtime.release()
    {
        return Err(WebBuildError::RuntimeReleaseMismatch {
            declared: declared.to_string(),
            module: abi.to_string(),
        });
    }
    Ok(Target {
        platform: registry_types::WEB_PLATFORM.into(),
        architecture: registry_types::WASM32_ARCHITECTURE.into(),
        isa,
        runtime: abi.to_string(),
        module: module_path.to_string(),
    })
}

/// The manifest of the web build of `authored`: the authored package with
/// `[target]` set, the entrypoint pointing at the module, and native-only
/// fields (`[package.isa]`, native binaries, splash segments, PATH commands)
/// removed.
///
/// # Errors
///
/// See [`web_target`].
pub fn web_manifest(
    authored: &Manifest,
    module_path: &str,
    info: &WasmModuleInfo,
    options: &WebBuildOptions<'_>,
) -> Result<Manifest, WebBuildError> {
    let target = web_target(authored, module_path, info, options)?;
    let mut manifest = authored.clone();
    manifest.package.isa = IsaRequirements::default();
    manifest.cli = None;
    manifest.entrypoint = match manifest.package.kind {
        Kind::App => Some(Entrypoint {
            bin: PathBuf::from(module_path),
            r#type: EntrypointType::Gui,
            gui_bin: None,
            cli_bin: None,
            splash_segments: Vec::new(),
            splash_budget_ms: authored
                .entrypoint
                .as_ref()
                .map_or(100, |entry| entry.splash_budget_ms),
        }),
        _ => None,
    };
    manifest.target = Some(target);
    Ok(manifest)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A minimal module: header plus the given custom sections.
    pub(crate) fn module(sections: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut out = Vec::from(*WASM_MAGIC);
        out.extend_from_slice(&WASM_VERSION);
        // A type section with no types, so non-custom sections are skipped.
        out.extend_from_slice(&[1, 1, 0]);
        for (name, payload) in sections {
            let mut body = leb(name.len());
            body.extend_from_slice(name.as_bytes());
            body.extend_from_slice(payload);
            out.push(0);
            out.extend(leb(body.len()));
            out.extend(body);
        }
        out
    }

    pub(crate) fn features(names: &[&str]) -> Vec<u8> {
        let mut out = leb(names.len());
        for name in names {
            out.push(b'+');
            out.extend(leb(name.len()));
            out.extend_from_slice(name.as_bytes());
        }
        out
    }

    fn leb(mut value: usize) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let byte = u8::try_from(value & 0x7f).unwrap();
            value >>= 7;
            if value == 0 {
                out.push(byte);
                return out;
            }
            out.push(byte | 0x80);
        }
    }

    const ABI: &str = "tilo-runtime/2026.07+wasm.00112233445566778899aabbccddeeff";

    pub(crate) fn authored(kind: &str, runtime: Option<&str>) -> Manifest {
        let runtime = runtime.map_or(String::new(), |r| format!("runtime = \"{r}\""));
        Manifest::from_toml_str(&format!(
            r#"
[package]
id = "so.tilo.books"
version = "0.1.0"
kind = "{kind}"
{runtime}

[package.isa]
x86_64 = "v3"

[publisher]
identity = "ed25519:{zero}"
signature = ""

[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "bin/books"
type = "gui"
splash-segments = ["bin/books[0x0:0x10]"]

[cli]
commands = ["books"]
"#,
            zero = "00".repeat(32)
        ))
        .unwrap()
    }

    #[test]
    fn module_sections_carry_the_abi_and_features() {
        let bytes = module(&[
            ("producers", vec![0]),
            (RUNTIME_ABI_SECTION, ABI.as_bytes().to_vec()),
            (
                TARGET_FEATURES_SECTION,
                features(&["sign-ext", "mutable-globals", "sign-ext"]),
            ),
        ]);
        let info = inspect_wasm_module(&bytes).unwrap();
        assert_eq!(info.runtime_abi.as_deref(), Some(ABI));
        assert_eq!(
            info.features,
            Some(vec!["sign-ext".to_string(), "mutable-globals".to_string()])
        );
        assert_eq!(
            inspect_wasm_module(&module(&[])).unwrap(),
            WasmModuleInfo::default()
        );
    }

    #[test]
    fn malformed_modules_are_refused() {
        assert_eq!(inspect_wasm_module(b"\0asm"), Err(WebBuildError::NotWasm));
        assert_eq!(
            inspect_wasm_module(b"\x7fELF\x02\x01\x01\0"),
            Err(WebBuildError::NotWasm)
        );
        let mut truncated = module(&[(RUNTIME_ABI_SECTION, ABI.as_bytes().to_vec())]);
        truncated.pop();
        assert!(matches!(
            inspect_wasm_module(&truncated),
            Err(WebBuildError::Malformed(_))
        ));
        let twice = module(&[
            (RUNTIME_ABI_SECTION, ABI.as_bytes().to_vec()),
            (RUNTIME_ABI_SECTION, ABI.as_bytes().to_vec()),
        ]);
        assert_eq!(
            inspect_wasm_module(&twice),
            Err(WebBuildError::DuplicateSection(RUNTIME_ABI_SECTION))
        );
        let upper = module(&[(RUNTIME_ABI_SECTION, ABI.to_uppercase().into_bytes())]);
        assert_eq!(
            inspect_wasm_module(&upper),
            Err(WebBuildError::BadRuntimeAbi)
        );
    }

    #[test]
    fn the_web_manifest_is_the_authored_package_with_a_target() {
        let manifest = authored("app", Some("tilo-runtime/2026.07"));
        let info = WasmModuleInfo {
            runtime_abi: Some(ABI.into()),
            features: Some(vec!["sign-ext".into(), "bulk-memory".into()]),
        };
        let web =
            web_manifest(&manifest, "books.wasm", &info, &WebBuildOptions::default()).unwrap();
        let target = web.target.clone().unwrap();
        assert_eq!(target.isa, "wasm32-mvp+bulk-memory+sign-ext");
        assert_eq!(target.runtime, ABI);
        assert_eq!(target.module, "books.wasm");
        assert_eq!(web.package.id, manifest.package.id);
        assert_eq!(web.sandbox, manifest.sandbox);
        assert_eq!(web.package.isa, IsaRequirements::default());
        assert!(web.cli.is_none());
        let entry = web.entrypoint.as_ref().unwrap();
        assert_eq!(entry.bin, PathBuf::from("books.wasm"));
        assert!(entry.splash_segments.is_empty());
        assert_eq!(crate::validate_manifest(&web), Ok(()));
        assert_eq!(
            crate::validate_manifest_with_payload(&web, &["books.wasm".into()]),
            Ok(())
        );
        assert_eq!(
            crate::validate_manifest_with_payload(&web, &["other.wasm".into()]),
            Err(crate::ManifestError::TargetModuleMissing(
                "books.wasm".into()
            ))
        );
        let reparsed = Manifest::from_toml_str(&web.to_toml_string().unwrap()).unwrap();
        assert_eq!(reparsed, web);
    }

    #[test]
    fn explicit_build_facts_must_agree_with_the_module() {
        let manifest = authored("app", Some("tilo-runtime/2026.07"));
        let info = WasmModuleInfo {
            runtime_abi: Some(ABI.into()),
            features: None,
        };
        assert_eq!(
            web_target(&manifest, "books.wasm", &info, &WebBuildOptions::default()),
            Err(WebBuildError::MissingFeatures)
        );
        let isa = WebBuildOptions {
            isa: Some("wasm32-mvp+mutable-globals"),
            ..WebBuildOptions::default()
        };
        assert_eq!(
            web_target(&manifest, "books.wasm", &info, &isa)
                .unwrap()
                .isa,
            "wasm32-mvp+mutable-globals"
        );
        let other_abi = WebBuildOptions {
            runtime_abi: Some("tilo-runtime/2026.07+wasm.ffeeddccbbaa99887766554433221100"),
            ..isa.clone()
        };
        assert!(matches!(
            web_target(&manifest, "books.wasm", &info, &other_abi),
            Err(WebBuildError::RuntimeAbiMismatch { .. })
        ));
        let unordered = WebBuildOptions {
            isa: Some("wasm32-mvp+sign-ext+bulk-memory"),
            ..WebBuildOptions::default()
        };
        assert!(matches!(
            web_target(&manifest, "books.wasm", &info, &unordered),
            Err(WebBuildError::Isa(_))
        ));
        assert!(matches!(
            web_target(&manifest, "nested/books.wasm", &info, &isa),
            Err(WebBuildError::BadModulePath(_))
        ));
        let anonymous = WasmModuleInfo::default();
        assert_eq!(
            web_target(&manifest, "books.wasm", &anonymous, &isa),
            Err(WebBuildError::MissingRuntimeAbi)
        );
        let no_runtime = authored("app", None);
        assert_eq!(
            web_target(&no_runtime, "books.wasm", &info, &isa),
            Err(WebBuildError::AppWithoutRuntime)
        );
        let other_release = authored("app", Some("tilo-runtime/2026.08"));
        assert!(matches!(
            web_target(&other_release, "books.wasm", &info, &isa),
            Err(WebBuildError::RuntimeReleaseMismatch { .. })
        ));
        let not_web = WasmModuleInfo {
            runtime_abi: None,
            features: None,
        };
        let bare_release = WebBuildOptions {
            runtime_abi: Some("tilo-runtime/2026.07"),
            ..isa.clone()
        };
        assert!(matches!(
            web_target(&manifest, "books.wasm", &not_web, &bare_release),
            Err(WebBuildError::Runtime(_))
        ));
    }

    #[test]
    fn the_toolkit_core_provides_a_runtime() {
        let manifest = authored("runtime", None);
        let info = WasmModuleInfo {
            runtime_abi: Some(ABI.into()),
            features: Some(vec!["mutable-globals".into()]),
        };
        let web = web_manifest(&manifest, "core.wasm", &info, &WebBuildOptions::default()).unwrap();
        assert!(web.entrypoint.is_none());
        assert_eq!(web.target.as_ref().unwrap().runtime, ABI);
        assert_eq!(crate::validate_manifest(&web), Ok(()));
        let provides = |release| WebBuildOptions {
            provides: Some(release),
            ..WebBuildOptions::default()
        };
        assert!(
            web_target(
                &manifest,
                "core.wasm",
                &info,
                &provides("tilo-runtime/2026.07")
            )
            .is_ok()
        );
        assert!(matches!(
            web_target(
                &manifest,
                "core.wasm",
                &info,
                &provides("tilo-runtime/2026.08")
            ),
            Err(WebBuildError::RuntimeReleaseMismatch { .. })
        ));
    }

    #[test]
    fn a_hand_written_target_is_validated() {
        let manifest = authored("app", Some("tilo-runtime/2026.07"));
        let info = WasmModuleInfo {
            runtime_abi: Some(ABI.into()),
            features: Some(Vec::new()),
        };
        let web =
            web_manifest(&manifest, "books.wasm", &info, &WebBuildOptions::default()).unwrap();

        let mut native_isa = web.clone();
        native_isa.package.isa = manifest.package.isa.clone();
        assert_eq!(
            crate::validate_manifest(&native_isa),
            Err(crate::ManifestError::TargetWithNativeIsa)
        );
        let mut other_release = web.clone();
        other_release.package.runtime = Some("tilo-runtime/2026.08".into());
        assert!(matches!(
            crate::validate_manifest(&other_release),
            Err(crate::ManifestError::TargetRuntimeMismatch { .. })
        ));
        let mut native_entry = web.clone();
        native_entry.entrypoint = manifest.entrypoint;
        assert!(matches!(
            crate::validate_manifest(&native_entry),
            Err(crate::ManifestError::TargetEntrypointMismatch(_))
        ));
        let mut native_target = web.clone();
        if let Some(target) = native_target.target.as_mut() {
            target.platform = registry_types::TILO_OS_PLATFORM.into();
            target.architecture = "x86_64".into();
            target.isa = "x86_64-v3".into();
        }
        assert!(matches!(
            crate::validate_manifest(&native_target),
            Err(crate::ManifestError::BadTarget(_))
        ));
        let mut service = web;
        service.package.kind = Kind::Service;
        assert_eq!(
            crate::validate_manifest(&service),
            Err(crate::ManifestError::TargetKindUnsupported)
        );
    }
}
