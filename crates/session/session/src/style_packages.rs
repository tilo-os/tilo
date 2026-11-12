//! A person's own desk styles as packages in their workspace (`docs/designs/app-language/
//! shell.md` §6, "Make it mine" and "Agents make styles with the same commands a person types").
//!
//! A desk style is a tilolang package, and a person makes one the way they make any app: they
//! write a package, install it, pick it. This is where that happens for a style: the session
//! offers the same three commands a person types at `tilolang` — `check`, and an install that
//! compiles and signs — as actions on its own door, so a shell view, the canvas chat's agent and
//! the REPL run the one path.
//!
//! * **Make it mine** copies a shipped style's package source from the copy this build carries
//!   (`tilo_surface_host::shell_part::shell_sources`) into the person's workspace under their own
//!   package id ([`COPY_ID`]), installs it, and leaves the shipped package untouched: updates to
//!   the shipped style never reach the copy.
//! * **check** compiles a package of the workspace against what this session publishes and
//!   answers its diagnostics; nothing is written.
//! * **install** compiles the package, signs it with the person's development key, and installs
//!   it into their store. Installing code is the person's decision, so it is asked on the
//!   system's sheet first ([`crate::questions`]): the person's own press of it is their answer,
//!   and an agent's step in a turn is asked by the broker before it arrives here.
//!
//! The person's workspace packages live under `$XDG_DATA_HOME/tilo/packages` (one directory per
//! package id), never in the store: the store holds what is installed, the workspace holds the
//! source the person owns. A copy is installed *from* the workspace, so editing it and installing
//! it again is the whole loop.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Map, Value, json};
use tilo_actions::{
    ActionAddress, ActionOutput, ActionParam, ActionSpec, DoorError, EncodedAction, OperationKind,
    ParamType, ResourceSpec,
};
use tilo_lock::MutexRecover as _;
use tilo_surface_host::Published;
use ui::host_operation::ResourceOperationStart;

/// The resource the person's style packages are made, checked and installed on.
pub const RESOURCE: &str = "session-authoring";

/// What kind of resource [`RESOURCE`] is.
pub const KIND: &str = "session.authoring";

/// The package id a "make it mine" copy of the shipped shell is installed as: the person's own,
/// so it never collides with the shipped `so.tilo.shell`, and an update to the shipped style
/// never reaches it.
pub const COPY_ID: &str = "mine.shell";

/// The words the copy's `app` header carries, so the switch and the store show it as the
/// person's own.
const COPY_TITLE: &str = "My desk";

/// The developer override naming the person's workspace packages directory (tests and a
/// development session). A shipped build ignores it, as it ignores every `TILO_*` override.
const WORKSPACE_ENV: &str = "TILO_PACKAGES_DIR";

/// The person's workspace packages directory: where a package they own keeps its source, one
/// directory per package id.
///
/// # Errors
/// No home, so there is nowhere to keep them.
pub fn workspace_root() -> Result<PathBuf, String> {
    if let Some(dir) = package_store::dev_env::dev_override(WORKSPACE_ENV) {
        return Ok(PathBuf::from(dir));
    }
    tilo_paths::xdg::data_home()
        .map(|data| data.join("tilo").join("packages"))
        .ok_or_else(|| "no home: there is nowhere to keep your packages".to_owned())
}

/// The directory the package `package` keeps its source in.
fn package_dir(package: &str) -> Result<PathBuf, String> {
    let name = package.trim();
    if name.is_empty() || name.contains('/') || name.contains('\\') || name == "." || name == ".." {
        return Err(format!("`{package}` is not a package's id"));
    }
    Ok(workspace_root()?.join(name))
}

/// What a command of this resource did, as its reading last left it: what the person, their
/// agent or the REPL reads back after an `Accepted` step.
#[derive(Clone, Debug, Default)]
struct Done {
    /// The last step, by its action id.
    action: String,
    /// Whether it succeeded.
    ok: bool,
    /// The package it was about.
    package: String,
    /// What it answered: the diagnostics `check` read, or what `install` did.
    answer: String,
}

/// The person's style packages, as the session's door publishes them: `make-it-mine`, `check`
/// and `install`, each the command it names, over the person's workspace.
pub(crate) struct Served {
    /// What the last step answered, guarded off the loop: a `check` or an `install` compiles and
    /// signs, which is a second's work that must not hold the session's frame.
    done: Mutex<Done>,
}

impl Default for Served {
    fn default() -> Self {
        Self::new()
    }
}

impl Served {
    pub(crate) const fn new() -> Self {
        Self {
            done: Mutex::new(Done {
                action: String::new(),
                ok: false,
                package: String::new(),
                answer: String::new(),
            }),
        }
    }
}

/// The four commands, described as the broker lists them.
pub(crate) fn resource() -> ResourceSpec {
    let result = tilo_actions::ResultSpec::Json {
        schema: json!({"type": "object"}),
    };
    let make = ActionSpec::typed_operation(
        "make-it-mine",
        "Make it mine",
        "Copies a shipped desk style's source into your workspace as your own package, with the \
         same parts, and installs it: the shipped style never overwrites it.",
        OperationKind::Effect,
        KIND,
        vec![ActionParam::required_owned(
            "style",
            ParamType::Text,
            "The shipped style, by its view reference (`so.tilo.shell:strips`)",
        )],
        |_| EncodedAction {
            type_id: KIND.into(),
            payload: b"make-it-mine".to_vec(),
        },
    );
    let check = ActionSpec::typed_operation(
        "check",
        "Check a style package",
        "Compiles a package in your workspace against the installed shell and session and \
         answers its diagnostics; nothing is written.",
        OperationKind::View,
        KIND,
        vec![ActionParam::required_owned(
            "package",
            ParamType::Text,
            "The package, by its id (`mine.shell`)",
        )],
        |_| EncodedAction {
            type_id: KIND.into(),
            payload: b"check".to_vec(),
        },
    );
    let install = ActionSpec::typed_operation(
        "install",
        "Install a style package",
        "Compiles a package in your workspace, signs it with your development key, and installs \
         it, so its styles are offered without a restart.",
        OperationKind::Effect,
        KIND,
        vec![ActionParam::required_owned(
            "package",
            ParamType::Text,
            "The package, by its id (`mine.shell`)",
        )],
        |_| EncodedAction {
            type_id: KIND.into(),
            payload: b"install".to_vec(),
        },
    );
    let mut spec = ResourceSpec::new(RESOURCE, KIND, "Desk styles");
    for mut action in [make, check, install] {
        // Installing code is the person's decision: the broker holds an agent's step that
        // installs or copies a package for the person's answer on the system's sheet
        // (`shell.md` §6, "Agents make styles with the same commands a person types"). The
        // person's own press is their answer, as it is for every action they take themselves.
        if action.id == "install" || action.id == "make-it-mine" {
            action.type_mut().contract.kind = tilo_actions::ActionKind::Outside;
            action.type_mut().contract.risk = tilo_actions::RiskLevel::Sensitive;
        }
        action.type_mut().contract.result = result.clone();
        spec = spec.with_action(action);
    }
    spec
}

impl Published for Served {
    fn resources(&mut self) -> Vec<ResourceSpec> {
        vec![resource()]
    }

    fn revision(&self) -> Option<u64> {
        // What it offers never changes; what it answers is read when asked.
        Some(0)
    }

    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &Map<String, Value>,
        _look: &mut tilo_surface_host::Look<'_>,
    ) -> Result<ResourceOperationStart, DoorError> {
        if address.resource != RESOURCE {
            return Err(DoorError::Unknown(address.clone()));
        }
        let text = |name: &str| -> Result<String, DoorError> {
            arguments
                .get(name)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| {
                    DoorError::InvalidArguments(format!("`{}` takes `{name}`", address.action))
                })
        };
        let answer = match address.action.as_str() {
            "make-it-mine" => make_it_mine(&text("style")?),
            "check" => check(&text("package")?),
            "install" => install(&text("package")?),
            _ => return Err(DoorError::Unknown(address.clone())),
        };
        let (package, answer) = match answer {
            Ok((package, answer)) => (package, Ok(answer)),
            Err(why) => (String::new(), Err(why)),
        };
        let done = {
            let mut done = self.done.lock_recover();
            done.action.clone_from(&address.action);
            done.ok = answer.is_ok();
            done.package = package;
            done.answer = match &answer {
                Ok(text) => text.clone(),
                Err(why) => why.clone(),
            };
            Done {
                action: done.action.clone(),
                ok: done.ok,
                package: done.package.clone(),
                answer: done.answer.clone(),
            }
        };
        let value = json!({
            "action": done.action,
            "ok": done.ok,
            "package": done.package,
            "answer": done.answer,
        });
        match answer {
            Ok(_) => Ok(ResourceOperationStart::Completed(ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            })),
            Err(reason) => Err(DoorError::Failed(reason)),
        }
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        Vec::new()
    }
}

/// "Make it mine" for the shipped style `style` (`so.tilo.shell:strips`): the shipped shell's
/// source written into the person's workspace as their own package, compiled against what this
/// session publishes, signed with their key and installed. Answers the package id and the style
/// reference the copy offers.
///
/// Only the shipped shell's own styles have a source to copy: another package's style is already
/// the person's (or another author's), and copying it would be a second package claiming its
/// types. Its own is the package the copy is of.
///
/// # Errors
/// The style is not the shipped shell's, the source cannot be written, or it does not compile.
fn make_it_mine(style: &str) -> Result<(String, String), String> {
    let (package, view) = style
        .split_once(':')
        .map_or((style, ""), |(package, view)| (package, view));
    if package != tilo_surface_host::shell_part::SHELL_APP {
        return Err(format!(
            "`{style}` is not a shipped style: only the styles this build ships can be copied, and \
             another package's style is already its author's"
        ));
    }
    let dir = package_dir(COPY_ID)?;
    write_copy(&dir)?;
    let installed = build_and_install(&dir, COPY_ID)?;
    let style = if view.is_empty() {
        format!("{installed}:strips")
    } else {
        format!("{installed}:{view}")
    };
    Ok((
        installed.clone(),
        format!("{installed} is yours; its `{style}` is offered in the switch"),
    ))
}

/// Write the shipped shell's source into `dir` as the person's own package: every file of the
/// package this build ships, and its source catalogue, with the `app` header renamed to the
/// person's package id and the words it is listed by. Nothing else of the shipped package is
/// read, so an update to it never reaches this copy.
fn write_copy(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir.join("locales"))
        .map_err(|error| format!("{}: {error}", dir.display()))?;
    for (name, text) in tilo_surface_host::shell_part::shell_sources() {
        let text = if *name == "app.tl" {
            copy_header(text)
        } else {
            (*text).to_owned()
        };
        std::fs::write(dir.join(name), text)
            .map_err(|error| format!("{}: {error}", dir.join(name).display()))?;
    }
    std::fs::write(
        dir.join("locales").join("en-US.ftl"),
        tilo_surface_host::shell_part::shell_catalogue(),
    )
    .map_err(|error| format!("{}: {error}", dir.join("locales/en-US.ftl").display()))?;
    // The ids lock is the compiler's to draw for the copy's own declarations: a package of its
    // own has ids of its own, and no id the shipped package was compiled with is reused.
    std::fs::write(dir.join("app.ids"), "")
        .map_err(|error| format!("{}: {error}", dir.join("app.ids").display()))?;
    Ok(())
}

/// The shipped shell's `app` header, as the person's own package declares it: `app so.tilo.shell
/// "Shell" (…)` becomes `app mine.shell "My desk" (…)`, and every other line stands as it was.
fn copy_header(text: &str) -> String {
    text.split_inclusive('\n')
        .map(|line| {
            if line.trim_start().starts_with("app ") {
                format!(
                    "app {COPY_ID} \"{COPY_TITLE}\" (icon: .monitor, category: .system, records: \
                     .own)\n"
                )
            } else {
                line.to_owned()
            }
        })
        .collect()
}

/// Check the package `package` of the person's workspace: compile it against what this session
/// publishes, and answer its diagnostics in the compiler's own words. Nothing is written.
///
/// # Errors
/// The package is not in the workspace, or its directory does not read.
fn check(package: &str) -> Result<(String, String), String> {
    let dir = package_dir(package)?;
    if !dir.is_dir() {
        return Err(format!("`{package}` is not in your workspace"));
    }
    match compile(&dir) {
        Ok(built) => {
            let app = built
                .get(built.len().saturating_sub(1))
                .map_or(package.to_owned(), |b| b.compiled.meta.app.clone());
            Ok((app, "no errors".to_owned()))
        }
        Err(error) => Err(error),
    }
}

/// Install the package `package` of the person's workspace: compile it, sign it with the
/// person's development key, and put it in their store, so its styles are offered without a
/// restart.
///
/// # Errors
/// The package is not in the workspace, does not compile, or the store refuses it.
fn install(package: &str) -> Result<(String, String), String> {
    let dir = package_dir(package)?;
    if !dir.is_dir() {
        return Err(format!("`{package}` is not in your workspace"));
    }
    let installed = build_and_install(&dir, package)?;
    Ok((
        installed.clone(),
        format!("{installed} is installed; its styles are in the switch"),
    ))
}

/// Compile the package in `dir` and the packages it uses against what this session publishes.
///
/// A desk style names the arrangement as `shell.Arrangement` (`use so.tilo.shell as shell`), so
/// the shell's own package is offered as published metadata beside the platform's: a style
/// compiles against the shell this session runs, whichever build installed it, without needing
/// the shell's source beside it in the workspace. The metadata is read from the compiled shell
/// itself, so it names the actions and types the desk's own code calls, never a second reading
/// of the source.
fn compile(dir: &Path) -> Result<Vec<tilolang_tilo::tilolang::source::BuiltPackage>, String> {
    let mut published = tilolang_tilo::catalogue::published_packages(
        &tilolang_tilo::tilolang::source::search_of(dir, &[]),
    )
    .map_err(|error| error.to_string())?;
    for package in shell_packages()? {
        if !published.iter().any(|meta| meta.app == package.meta.app) {
            published.push(package.meta);
        }
    }
    let mut rng = tilolang_tilo::tilolang::OsEntropy::default();
    let built = tilolang_tilo::tilolang::source::compile_package(dir, &[], &published, &mut rng)
        .map_err(|error| error.to_string())?;
    Ok(built)
}

/// The shell's package as this build runs it: the store's installed `so.tilo.shell` when the
/// person has one, else the built-in copy, each compiled against this session's description —
/// the same packages `crate::shell::compile` runs, which is what the desk draws with.
fn shell_packages() -> Result<Vec<tilolang_tilo::tilolang::Compiled>, String> {
    crate::shell::compile()
}

/// Build the package in `dir` into a signed `.tapp` with the person's development key, and
/// install it into their store; answers the package id the store holds it as.
fn build_and_install(dir: &Path, package: &str) -> Result<String, String> {
    let identity_path = tilolang_cli::state_dir().join("publisher.json");
    let identity = tilolang_cli::identity_at(&identity_path)?;
    let out = build_out(package);
    std::fs::create_dir_all(&out).map_err(|error| format!("{}: {error}", out.display()))?;
    let built = tilolang_cli::package::build(
        &tilolang_cli::package::BuildOptions {
            dir: dir.to_path_buf(),
            search: Vec::new(),
            out,
            tilo_runtime: tilolang_cli::package::DEFAULT_TILO_RUNTIME.to_owned(),
            tilolang_runtime: tilolang_cli::package::DEFAULT_TILOLANG_RUNTIME.to_owned(),
            version: "0.1.0".to_owned(),
        },
        &identity,
    )?;
    let catalogue = package_store::Catalog::from_env().map_err(|error| error.to_string())?;
    let root = catalogue.user().root().to_path_buf();
    let store = package_store::PackageStore::at(root);
    let mut installed = package.to_owned();
    for app in built {
        let tapp = std::fs::File::open(&app.tapp)
            .map_err(|error| format!("{}: {error}", app.tapp.display()))?;
        let source = app.tapp.display().to_string();
        let id = app.app.clone();
        store
            .install(
                std::io::BufReader::new(tapp),
                move |_meta| {
                    Ok(package_store::Admission {
                        pinned: false,
                        attested_by: String::new(),
                        source,
                        held: false,
                    })
                },
                None,
            )
            .map_err(|error| format!("{id}: {error}"))?;
        installed = id;
    }
    Ok(installed)
}

/// Where a build's payloads and `.tapp`s go: the person's cache, never their workspace or the
/// store, so a check leaves nothing behind and an install keeps only what it installed.
fn build_out(package: &str) -> PathBuf {
    tilo_paths::xdg::cache_home()
        .or_else(|| tilo_paths::home().map(|home| home.join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("tilo")
        .join("style-builds")
        .join(package)
}

/// The packages of the person's workspace, by their id: a reading of what they own.
///
/// # Errors
/// The workspace does not read.
pub fn owned() -> Result<BTreeMap<String, PathBuf>, String> {
    let root = workspace_root()?;
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Ok(BTreeMap::new());
    };
    let mut out = BTreeMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir()
            && let Some(name) = path.file_name().and_then(|name| name.to_str())
        {
            out.insert(name.to_owned(), path);
        }
    }
    Ok(out)
}
