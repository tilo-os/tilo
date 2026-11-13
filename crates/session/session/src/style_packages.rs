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

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Map, Value, json};
use tilo_actions::{
    ActionAddress, ActionOutput, ActionParam, ActionSpec, DoorError, EncodedAction, OperationKind,
    ParamType, ResourceSpec,
};
use tilo_lock::MutexRecover as _;
use tilo_session_core::door::Published;
use ui::host_operation::ResourceOperationStart;

use crate::i18n::t;

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
    let mut spec = ResourceSpec::new(RESOURCE, KIND, t("session-desk-styles"));
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
        _look: &mut tilo_session_core::door::Look<'_>,
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

/// "Make it mine" for the shipped style `style` (`so.tilo.shell:strips`): the shipped style
/// rewritten into the person's own short package in their workspace, compiled against what this
/// session publishes, signed with their key and installed. Answers the package id, and the style
/// the copy offers.
///
/// The copy is small: one `.tl` file whose one view is the shipped style's own body, its calls
/// the shell's (`shell.strips_view`), so a person and their agent read and edit a file where
/// every line is theirs. The shipped package is never copied whole, so an update to it never
/// overwrites this file.
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
    let view = shipped_view(view)?;
    let dir = package_dir(COPY_ID)?;
    write_copy(&dir, &view)?;
    let installed = build_and_install(&dir, COPY_ID)?;
    let own = format!("{installed}:{}", view.name);
    Ok((
        installed,
        format!("{own} is yours: the switch offers it, and the desk draws it"),
    ))
}

/// The shipped style `named` (`strips`, or empty for it) as the shell this build carries declares
/// it: its name, the doc line the switch lists it by, and its body.
fn shipped_view(named: &str) -> Result<ShippedView, String> {
    let named = if named.is_empty() { "strips" } else { named };
    for (_, text) in tilo_surface_host::shell_part::shell_sources() {
        if let Some(view) = view_in(text, named) {
            return Ok(view);
        }
    }
    Err(format!(
        "`{}:{named}` is not a style this build ships",
        tilo_surface_host::shell_part::SHELL_APP
    ))
}

/// The view `named` as one of the shipped shell's source files declares it: its doc line, and
/// the body between its signature's braces, which draws the desk, with the shell's own names
/// qualified (`shell.strips_view`) so the copy compiles as a package of its own.
fn view_in(text: &str, named: &str) -> Option<ShippedView> {
    let lines: Vec<&str> = text.lines().collect();
    let at = lines
        .iter()
        .position(|line| line.trim_start().starts_with(&format!("view {named}(")))?;
    // The view's doc line is the `///` block above it; its first line names the style.
    let mut first = None;
    for line in lines.get(..at).unwrap_or_default().iter().rev() {
        match line.trim_start().strip_prefix("///") {
            Some(words) => first = Some(words.trim().to_owned()),
            None => break,
        }
    }
    let body = lines.get(at.saturating_add(1)..)?;
    let end = body.iter().position(|line| *line == "}")?;
    let body = trim_blank_edges(body.get(..end)?).join("\n");
    Some(ShippedView {
        name: named.to_owned(),
        line: first.unwrap_or_default(),
        body: rewrite_shell_names(&body),
    })
}

/// The style's body with the shell's own names qualified: the shipped styles write
/// `strips_view(a, viewing, …)`, which is the shell's view and the shell's root; in a package of
/// its own the same code says `shell.strips_view(a, shell.viewing, …)`. A name the shell does
/// not declare (`fill`, `space`, `layers`, `a`, `lists`) stands as it was.
fn rewrite_shell_names(body: &str) -> String {
    let declared = shell_names();
    let mut out = String::with_capacity(body.len());
    let mut word = String::new();
    // The character before the word being read: `.` means the word is a field, not a name.
    let mut before = '\0';
    let flush = |word: &mut String, before: char, out: &mut String| {
        if word.is_empty() {
            return;
        }
        let dotted = before == '.';
        if !dotted && declared.iter().any(|name| name == word) {
            out.push_str("shell.");
        }
        out.push_str(word);
        word.clear();
    };
    for c in body.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            word.push(c);
        } else {
            flush(&mut word, before, &mut out);
            out.push(c);
            before = c;
        }
    }
    flush(&mut word, before, &mut out);
    out
}

/// The names the shell package declares by their plain names: its views (its templates too), its
/// functions, types, enums and named roots, as a style's copied body may write them.
fn shell_names() -> &'static [String] {
    static NAMES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| {
        let mut names: Vec<String> = Vec::new();
        let mut push = |name: &str| {
            if !name.is_empty() && !names.iter().any(|held| held == name) {
                names.push(name.to_owned());
            }
        };
        for (_, text) in tilo_surface_host::shell_part::shell_sources() {
            for line in text.lines() {
                let line = line.trim_start();
                let line = line.strip_prefix("template ").unwrap_or(line);
                // `view name(`, `fn name(`, `type name`, `enum name`, `root name`.
                for (keyword, takes_paren) in [
                    ("view ", true),
                    ("fn ", true),
                    ("type ", false),
                    ("enum ", false),
                    ("root ", false),
                ] {
                    let Some(rest) = line.strip_prefix(keyword) else {
                        continue;
                    };
                    let end = if takes_paren {
                        rest.find('(').unwrap_or(rest.len())
                    } else {
                        rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                            .unwrap_or(rest.len())
                    };
                    push(rest.get(..end).unwrap_or("").trim());
                }
            }
        }
        names
    })
}

/// The lines of `body` without the blank lines at either end.
fn trim_blank_edges<'a>(body: &'a [&'a str]) -> &'a [&'a str] {
    let start = body
        .iter()
        .position(|line| !line.trim().is_empty())
        .unwrap_or(body.len());
    let end = body
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map_or(start, |end| end.saturating_add(1));
    body.get(start..end).unwrap_or_default()
}

/// A shipped style, as the shell this build carries declares it.
struct ShippedView {
    /// The view's own name, as its declaration writes it.
    name: String,
    /// The view's doc line, in its author's words: the name the switch lists it by.
    line: String,
    /// The body between the signature's braces, the shell's names qualified.
    body: String,
}

/// Write the person's own style package into `dir`: a small package of its own that `use`s the
/// shell and declares one view of `shell.Arrangement`, the shipped style `view` rebuilt through
/// `shell.desk_style(...)` with the style's own places inside it.
fn write_copy(dir: &Path, view: &ShippedView) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    // The ids lock is the compiler's to fill: a package of its own has ids of its own.
    std::fs::write(dir.join("app.ids"), "")
        .map_err(|error| format!("{}: {error}", dir.join("app.ids").display()))?;
    std::fs::write(dir.join("app.tl"), style_source(view))
        .map_err(|error| format!("{}: {error}", dir.join("app.tl").display()))?;
    Ok(())
}

/// The source of the person's style package: its `app` header, its use of the shell, and the one
/// view, named after the shipped style's own.
fn style_source(view: &ShippedView) -> String {
    format!(
        "app {COPY_ID} \"{COPY_TITLE}\"\n\
         \n\
         // Make it mine: the shipped `{package}:{name}` as the person's own style. Edit it, then\n\
         // install it: the desk draws what this file says.\n\
         use {package} as shell\n\
         \n\
         /// {line}\n\
         view {name}(a: shell.Arrangement) {{\n\
         {body}\n\
         }}\n",
        package = tilo_surface_host::shell_part::SHELL_APP,
        name = view.name,
        line = view.line,
        body = view.body,
    )
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
                .map_or_else(|| package.to_owned(), |b| b.compiled.meta.app.clone());
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

/// Compile the package in `dir` against the shell this session runs and what this session
/// publishes. A desk style names the arrangement as `shell.Arrangement` (`use so.tilo.shell as
/// shell`) and calls the shell's `template view`s, so the shell's own compiled package stands in
/// `published`: only its metadata carries the package's templates, and it is read from the
/// compiled shell itself, so it names the actions, types and templates the desk's own code calls.
fn compile(dir: &Path) -> Result<Vec<tilolang_tilo::tilolang::source::BuiltPackage>, String> {
    let mut published = tilolang_tilo::catalogue::published_packages(
        &tilolang_tilo::tilolang::source::search_of(dir, &[]),
    )?;
    // The shell's compiled package replaces any description the catalogue found for it, and goes
    // first: the resolver takes the first package of an id it meets.
    let mut shell = Vec::new();
    for package in shell_packages()? {
        published.retain(|meta| meta.app != package.meta.app);
        shell.push(package.meta);
    }
    shell.extend(published);
    let published = shell;
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
    // The package compiled against the shell this session runs (its metadata carries the
    // templates a style calls), then delivered and signed the way `tilolang build` delivers
    // any package.
    let shell = shell_packages()?;
    let built = compile(dir)?;
    let Some(person) = built
        .iter()
        .find(|built| built.compiled.meta.app == package)
        .or_else(|| built.last())
    else {
        return Err(format!("`{package}` compiled nothing"));
    };
    let options = tilolang_cli::package::BuildOptions {
        dir: dir.to_path_buf(),
        search: Vec::new(),
        out,
        tilo_runtime: tilolang_cli::package::DEFAULT_TILO_RUNTIME.to_owned(),
        tilolang_runtime: tilolang_cli::package::DEFAULT_TILOLANG_RUNTIME.to_owned(),
        version: "0.1.0".to_owned(),
    };
    let app = tilolang_cli::package::deliver_compiled(
        &person.compiled,
        &shell.iter().collect::<Vec<_>>(),
        &options,
        &identity,
    )?;
    let catalogue = package_store::Catalog::from_env().map_err(|error| error.to_string())?;
    let root = catalogue.user().root().to_path_buf();
    let store = package_store::PackageStore::at(root);
    let tapp = std::fs::File::open(&app.tapp)
        .map_err(|error| format!("{}: {error}", app.tapp.display()))?;
    let source = app.tapp.display().to_string();
    let installed = app.app;
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
        .map_err(|error| format!("{installed}: {error}"))?;
    // The `tapp`s were the install's transport: the store holds what they carried, so the build
    // directory goes, and a person's cache keeps no second copy of what they installed.
    let out = build_out(package);
    if let Err(error) = std::fs::remove_dir_all(&out) {
        tilo_failure::ignored!(
            "the install itself succeeded; a cache directory left behind costs space the next build reuses, and nothing the person was doing depended on its removal",
            tracing::debug!(%error, "a style build's leftovers stay in the cache")
        );
    }
    Ok(installed)
}

/// Where a build's payloads and `.tapp`s go: the person's cache, never their workspace or the
/// store, and removed once the install has taken what they carried.
fn build_out(package: &str) -> PathBuf {
    tilo_paths::xdg::cache_home()
        .or_else(|| tilo_paths::home().map(|home| home.join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("tilo")
        .join("style-builds")
        .join(package)
}
