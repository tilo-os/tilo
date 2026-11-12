//! `cargo web`: the web desktop, built by the bundle's own build (`build.sh --bundle desktop`) and
//! served by the local hosting stack (`os/containers/tilo-sh/stack.sh`), which runs production's
//! entrypoints and site layout. After it serves, the page is read back as a browser would start
//! it: the entry page, every module it and its modules name, and the runtime's WebAssembly. A
//! missing piece fails the command, so a bundle that would not start is never left serving.

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Instant;

use crate::{repo_root, seconds, state_home, target_dir};

const HELP: &str = "\
cargo web: the web desktop on this machine, served by the local tilo.sh stack.

Builds the desktop bundle (crates/browser/tilo-browser-runtime/scripts/build.sh
--bundle desktop; only what changed rebuilds) and serves it with the stack
production's site runs on (os/containers/tilo-sh/stack.sh: the relay, the
front door and the web entry, all on 127.0.0.1). A running stack keeps running
and only the web entry restarts on the new bundle. Then it loads the page the
way a browser starts it (the entry page, each module it imports, the runtime
module and its WebAssembly) and fails, naming what is missing, if any of it
does not load. Prints the address to open; the stack keeps running in the
background until `cargo web down`.

Usage: cargo web [OPTIONS]
       cargo web down [--wipe] | status | logs NAME | browser | env

Options:
  --release   build the bundle as it ships (optimised, wasm-opt): minutes
              longer; the default is the development profile (build.sh
              --profile quick), rebuilt in place
  --fresh     start the stack on new state (a new Lounge, a new door)
  --bundle DIR  serve this built desktop bundle (build.sh --bundle desktop's
              output, such as CI's web-desktop-bundle) instead of building one
  --check     smoke test on a stack of its own (its own state and ports):
              build, serve, load the page and its runtime module, stop

The compile runs on t (or the cargo drop-in TILO_SH_CARGO names, such as
rcargo). Needs wasm-bindgen at the pinned version, the wasm32 target, curl,
bwrap, openssl and python3; `stack.sh help` says more about the stack.

Examples:
  cargo web                 # then open the printed https://tilo.localhost:… address
  cargo web --check
  cargo web logs web
  cargo web down
";

pub fn main(args: &[String]) -> Result<ExitCode, String> {
    let repo = repo_root()?;
    let stack = repo.join("os/containers/tilo-sh/stack.sh");
    if let Some(first) = args.first()
        && matches!(
            first.as_str(),
            "down" | "status" | "logs" | "browser" | "env"
        )
    {
        return pass_to_stack(&stack, args);
    }
    let (mut release, mut fresh, mut check) = (false, false, false);
    let mut given = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--bundle" => {
                let dir = args.next().ok_or("--bundle needs a directory")?;
                given = Some(
                    std::env::current_dir()
                        .map_err(|e| e.to_string())?
                        .join(dir),
                );
            }
            "--release" => release = true,
            "--fresh" => fresh = true,
            "--check" => check = true,
            "-h" | "--help" | "help" => {
                print!("{HELP}");
                return Ok(ExitCode::SUCCESS);
            }
            other => return Err(format!("unknown option {other} (cargo web --help)")),
        }
    }
    let began = Instant::now();
    let mut stack_env: Vec<(&str, String)> = Vec::new();
    let state = if check {
        // A stack of its own, beside any the developer runs.
        let state = target_dir(&repo).join("tilo-local-check/web-stack");
        stack_env.push(("TILO_SH_STATE", state.display().to_string()));
        stack_env.push((
            "TILO_SH_PORT_BASE",
            std::env::var("TILO_LOCAL_CHECK_PORT_BASE").unwrap_or_else(|_| "24300".into()),
        ));
        stack_env.push(("TILO_SH_VM_HOST", "0".into()));
        // The VM host is the machine's own; the check's stack neither starts nor stops it.
        stack_env.push((
            "TILO_SH_VM_HOST_STATE",
            state.join("vm-host").display().to_string(),
        ));
        fresh = true;
        state
    } else {
        std::env::var_os("TILO_SH_STATE").map_or_else(|| state_home("tilo-sh-local"), PathBuf::from)
    };

    let bundle = match given {
        Some(bundle) if bundle.join("build.json").is_file() => bundle,
        Some(bundle) => {
            return Err(format!(
                "{} holds no built bundle (build.json)",
                bundle.display()
            ));
        }
        None => build_bundle(&repo, release)?,
    };
    let built = began.elapsed();
    stack_env.push(("TILO_SH_BUNDLE", bundle.display().to_string()));

    let stack_command = |args: &[&str]| {
        let mut command = Command::new("bash");
        command.arg(&stack).args(args);
        for (key, value) in &stack_env {
            command.env(key, value);
        }
        command
    };
    serve(&stack_command, fresh, &state)?;
    let origin = site_origin(stack_command(&["env"]))?;
    let ca = state.join("local-ca.pem");
    let loaded = verify(&origin, &ca);
    if check {
        let _ = stack_command(&["down"]).stdout(Stdio::null()).status();
    }
    let loaded = loaded?;
    println!("ok: {loaded}");
    println!(
        "ok: the web desktop is served at {origin}/ (bundle {} in {}, ready {} after start)",
        bundle.display(),
        seconds(built),
        seconds(began.elapsed())
    );
    if check {
        println!("ok: cargo web --check passed; its stack is stopped again");
    } else {
        println!(
            "    open {origin}/ in a browser with WebGPU (trust {} or accept the warning);\n    \
             cargo web status | logs web | down",
            ca.display()
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// `stack.sh ARGS`, for the stack's own commands.
fn pass_to_stack(stack: &Path, args: &[String]) -> Result<ExitCode, String> {
    let status = Command::new("bash")
        .arg(stack)
        .args(args)
        .status()
        .map_err(|error| error.to_string())?;
    Ok(if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Build the desktop bundle into the target directory: the development profile updated in place
/// (only what changed is redone), or the shipped profile from scratch.
fn build_bundle(repo: &Path, release: bool) -> Result<PathBuf, String> {
    let out = target_dir(repo).join(if release {
        "tilo-local/web-bundle-release"
    } else {
        "tilo-local/web-bundle"
    });
    let mut args: Vec<String> = vec!["--bundle".into(), "desktop".into()];
    let updating = !release && out.join("build.json").is_file();
    if release {
        let _ = std::fs::remove_dir_all(&out);
    } else {
        args.extend(["--profile".into(), "quick".into()]);
        if updating {
            args.push("--update".into());
        } else {
            let _ = std::fs::remove_dir_all(&out);
        }
    }
    std::fs::create_dir_all(&out).map_err(|error| format!("{}: {error}", out.display()))?;
    eprintln!(
        "==> building the desktop bundle (build.sh {}{})",
        args.join(" "),
        if updating { ", in place" } else { "" }
    );
    let began = Instant::now();
    let status = Command::new("bash")
        .arg(repo.join("crates/browser/tilo-browser-runtime/scripts/build.sh"))
        .arg(&out)
        .args(&args)
        // The stack's own cargo: t's cached build, or the drop-in TILO_SH_CARGO names.
        .env("CARGO", repo.join("os/containers/tilo-sh/cargo.sh"))
        .status()
        .map_err(|error| error.to_string())?;
    if !status.success() {
        return Err("the desktop bundle did not build (above)".into());
    }
    if !out.join("build.json").is_file() {
        return Err(format!(
            "build.sh succeeded but left no build.json in {}",
            out.display()
        ));
    }
    eprintln!("==> bundle built in {}", seconds(began.elapsed()));
    Ok(out)
}

/// Bring the stack up on the bundle, or restart only its web entry when it already serves.
fn serve(stack: &dyn Fn(&[&str]) -> Command, fresh: bool, state: &Path) -> Result<(), String> {
    let serving = !fresh && web_up(stack(&["status"]));
    let status = if serving {
        eprintln!("==> the stack is up: restarting the web entry on the new bundle");
        stack(&["restart", "web"]).status()
    } else {
        eprintln!("==> starting the stack (os/containers/tilo-sh/stack.sh up)");
        let mut up = vec!["up"];
        if fresh {
            up.push("--fresh");
        }
        stack(&up).status()
    }
    .map_err(|error| error.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "the stack did not come up (above; logs: {}/logs)",
            state.display()
        ))
    }
}

/// Whether `stack.sh status` says the web entry answers.
fn web_up(mut status: Command) -> bool {
    status.stderr(Stdio::null()).output().is_ok_and(|out| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|line| line.split_whitespace().take(2).eq(["web", "up"]))
    })
}

/// The site's address, from `stack.sh env`.
fn site_origin(mut env: Command) -> Result<String, String> {
    let out = env
        .stderr(Stdio::inherit())
        .output()
        .map_err(|error| error.to_string())?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("export TILO_SITE="))
        .map(|origin| {
            origin
                .trim()
                .trim_matches('\'')
                .trim_matches('"')
                .to_owned()
        })
        .ok_or_else(|| "stack.sh env names no TILO_SITE".into())
}

/// One file of the site: its status, type and body.
struct Fetched {
    code: u16,
    kind: String,
    body: Vec<u8>,
}

/// `url` as a browser asks for it: the site answers a command-line client at `/` with its
/// installer script, as `curl https://tilo.sh | sh` gets it.
fn fetch(url: &str, ca: &Path, accept: &str) -> Result<Fetched, String> {
    let out = Command::new("curl")
        .args(["-sS", "--max-time", "30", "--compressed"])
        .args([
            "-A",
            "Mozilla/5.0 (X11; Linux x86_64) tilo-local page check",
        ])
        .args(["-H", &format!("Accept: {accept}"), "--cacert"])
        .arg(ca)
        .args(["-w", "\n%{http_code} %{content_type}", url])
        .output()
        .map_err(|error| format!("curl: {error}"))?;
    if !out.status.success() {
        return Err(format!(
            "{url}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let mut body = out.stdout;
    let split = body
        .iter()
        .rposition(|byte| *byte == b'\n')
        .ok_or_else(|| format!("{url}: no status from curl"))?;
    let trailer = body.split_off(split);
    let trailer = String::from_utf8_lossy(trailer.get(1..).unwrap_or_default()).into_owned();
    let (code, kind) = trailer.split_once(' ').unwrap_or((&trailer, ""));
    Ok(Fetched {
        code: code.parse().unwrap_or(0),
        kind: kind.to_owned(),
        body,
    })
}

/// `reference` resolved against `base` (a URL ending in a file or a `/`).
fn resolve(base: &str, reference: &str) -> String {
    if reference.starts_with("http://") || reference.starts_with("https://") {
        return reference.to_owned();
    }
    let (scheme, rest) = base.split_once("://").unwrap_or(("https", base));
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let mut parts: Vec<&str> = if reference.starts_with('/') {
        Vec::new()
    } else {
        // The directory `base` is in: its path without the file at the end.
        let mut dir: Vec<&str> = path.split('/').collect();
        dir.pop();
        dir.into_iter().filter(|part| !part.is_empty()).collect()
    };
    for part in reference.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    format!("{scheme}://{host}/{}", parts.join("/"))
}

/// The module and WebAssembly files `text` (a page or a module) loads: what a static or dynamic
/// import, a `new URL(…, import.meta.url)` or a page's `href`/`src` names.
fn named_files(text: &str) -> Vec<String> {
    const LEADS: [&str; 6] = ["from", "import(", "import", "new URL(", "href=", "src="];
    let mut found = Vec::new();
    for lead in LEADS {
        let mut rest = text;
        while let Some((_, after)) = rest.split_once(lead) {
            rest = after;
            let after = after.trim_start();
            let Some(quote) = after.chars().next().filter(|c| "\"'`".contains(*c)) else {
                continue;
            };
            let Some((piece, _)) = after.get(1..).and_then(|inside| inside.split_once(quote))
            else {
                continue;
            };
            if names_a_file(piece) && !found.iter().any(|seen| seen == piece) {
                found.push(piece.to_owned());
            }
        }
    }
    found
}

/// Whether `piece` is a relative path to a module or WebAssembly file.
fn names_a_file(piece: &str) -> bool {
    let wanted = ["mjs", "js", "wasm"]
        .iter()
        .any(|ext| has_extension(piece, ext));
    let plain = !piece.is_empty()
        && piece.len() < 200
        && piece
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "./_-".contains(c));
    let relative = piece.starts_with("./")
        || piece.starts_with("../")
        || piece.starts_with('/')
        || piece.starts_with("pkg/");
    wanted && plain && relative
}

fn has_extension(url: &str, extension: &str) -> bool {
    Path::new(url)
        .extension()
        .is_some_and(|found| found.eq_ignore_ascii_case(extension))
}

/// Load the page as a browser starts it; what loaded, or what did not.
fn verify(origin: &str, ca: &Path) -> Result<String, String> {
    let page_url = format!("{origin}/");
    let page = fetch(&page_url, ca, "text/html")?;
    if page.code != 200 || !page.kind.starts_with("text/html") {
        return Err(format!(
            "{page_url} answered {} {} instead of the entry page",
            page.code, page.kind
        ));
    }
    let mut queue: VecDeque<(String, String)> = named_files(&String::from_utf8_lossy(&page.body))
        .into_iter()
        .map(|file| (resolve(&page_url, &file), page_url.clone()))
        .collect();
    let mut seen = BTreeSet::new();
    let (mut modules, mut missing, mut runtime_js, mut runtime_wasm) =
        (0_usize, Vec::new(), None, None);
    while let Some((url, from)) = queue.pop_front() {
        if !url.starts_with(origin) || !seen.insert(url.clone()) || seen.len() > 2000 {
            continue;
        }
        let file = fetch(&url, ca, "*/*")?;
        if file.code != 200 {
            missing.push(format!("{url} ({}, named by {from})", file.code));
            continue;
        }
        if has_extension(&url, "wasm") {
            if !file.body.starts_with(b"\0asm") {
                missing.push(format!("{url} is not WebAssembly (named by {from})"));
            } else if url.ends_with("/pkg/tilo_browser_runtime_bg.wasm") {
                runtime_wasm = Some(file.body.len());
            }
            continue;
        }
        if !file.kind.contains("javascript") {
            missing.push(format!(
                "{url} is served as {:?}, not JavaScript",
                file.kind
            ));
            continue;
        }
        modules = modules.saturating_add(1);
        if url.ends_with("/pkg/tilo_browser_runtime.js") {
            runtime_js = Some(url.clone());
            // The glue fetches its module beside itself (`new URL(…, import.meta.url)`).
            queue.push_back((resolve(&url, "tilo_browser_runtime_bg.wasm"), url.clone()));
        }
        for named in named_files(&String::from_utf8_lossy(&file.body)) {
            queue.push_back((resolve(&url, &named), url.clone()));
        }
    }
    if runtime_js.is_none() {
        missing.push("no module names the runtime module pkg/tilo_browser_runtime.js".into());
    }
    if runtime_wasm.is_none() {
        missing.push(
            "the runtime's WebAssembly (pkg/tilo_browser_runtime_bg.wasm) did not load".into(),
        );
    }
    if !missing.is_empty() {
        return Err(format!(
            "the page would not start; missing or broken:\n  {}",
            missing.join("\n  ")
        ));
    }
    Ok(format!(
        "the page loads: {modules} modules, the runtime module and its WebAssembly ({} bytes)",
        runtime_wasm.unwrap_or(0)
    ))
}

#[cfg(test)]
mod tests {
    use super::{named_files, resolve};

    #[test]
    fn resolves_like_a_browser() {
        let page = "https://tilo.localhost:24120/";
        assert_eq!(
            resolve(page, "./v/abc/entry.mjs"),
            "https://tilo.localhost:24120/v/abc/entry.mjs"
        );
        let module = "https://tilo.localhost:24120/v/abc/host.mjs";
        assert_eq!(
            resolve(module, "./pkg/tilo_browser_runtime.js"),
            "https://tilo.localhost:24120/v/abc/pkg/tilo_browser_runtime.js"
        );
        assert_eq!(
            resolve(module, "../x.mjs"),
            "https://tilo.localhost:24120/v/x.mjs"
        );
        assert_eq!(
            resolve(module, "/a.js"),
            "https://tilo.localhost:24120/a.js"
        );
    }

    #[test]
    fn finds_the_modules_a_page_names() {
        let text = r#"<link rel="modulepreload" href="./v/a/entry.mjs"><script type="module" src='./v/a/host.mjs'></script>
            // the page's own words, which a quote in them does not confuse
            import init,* as runtime from './pkg/tilo_browser_runtime.js';
            const split = await import('./pkg/__wasm_split.js'); const words = "not ./a/module.js please";
            const imports = { "./tilo_browser_runtime_bg.js": import0 };
            const url = new URL("compute/worker.mjs", import.meta.url);"#;
        assert_eq!(
            named_files(text),
            [
                "./pkg/tilo_browser_runtime.js",
                "./pkg/__wasm_split.js",
                "./v/a/entry.mjs",
                "./v/a/host.mjs"
            ]
        );
    }
}
