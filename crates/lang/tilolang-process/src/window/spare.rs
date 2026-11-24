//! A spare window process: the session starts one per workspace before any tilolang app's window
//! is asked for, so opening one starts no process.
//!
//! It is started for no app. It holds no package, no app data and no app identity: the launcher
//! hands it the runtimes, the fonts, its workspace's render service, the filtered accessibility bus
//! and its link (`launcher::run::tilolang_window`). The session sends the Wayland connection it
//! made for it ([`ToProcess::Display`]); the process connects and prepares everything a window
//! needs but the window, and waits on its link. When a tilolang app's window is asked for, the
//! session binds it to that app and workspace ([`ToProcess::Bind`]): the window it then opens is
//! the app's, and shows the instance the app's tilolang process runs. A bound process serves only
//! that app in that workspace, as the link of a launch of it would: later windows of the app there
//! open in it, and no other app's.
//!
//! What the compositor shows of the process's windows is what the session bound its connection
//! to, never what the process says it is.

use std::os::fd::IntoRawFd as _;
use std::os::unix::fs::FileExt as _;

use runtime_portal::process_link::{Binding, Descriptor, Packet as _, ToProcess};

/// Run the spare the session started, on the link the launcher handed it: wait for its display,
/// prepare, wait for the app it is bound to, and run that app's windows.
pub fn run() -> std::process::ExitCode {
    let Some(link) = runtime_portal::process_link::take() else {
        eprintln!("tilolang: a spare window was handed no link");
        return std::process::ExitCode::from(2);
    };
    let wayland = match ToProcess::receive(&link) {
        Ok(Some(ToProcess::Display { wayland })) => wayland,
        // The session let it go before it drew anything.
        Ok(None) => return std::process::ExitCode::SUCCESS,
        Ok(Some(other)) => {
            eprintln!("tilolang: a spare window was sent {other:?} before its display");
            return std::process::ExitCode::from(2);
        }
        Err(error) => {
            eprintln!("tilolang: a spare window's link: {error}");
            return std::process::ExitCode::from(2);
        }
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "the spare's display connection arrives on its link after it starts, before any \
                  thread does: this is the process's launcher environment, which the Wayland \
                  client reads"
    )]
    // SAFETY: no thread has started yet. The Wayland client takes the descriptor this names as its
    // connection, and owns it from there.
    unsafe {
        std::env::set_var("WAYLAND_SOCKET", wayland.into_raw_fd().to_string());
    }
    ui::launch::warm_window_process();
    let Bind {
        binding,
        module,
        shell_ui,
        surface,
    } = match bound(&link) {
        Ok(Some(bound)) => bound,
        Ok(None) => return std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tilolang: a spare window: {error}");
            return std::process::ExitCode::from(2);
        }
    };
    let Binding {
        app,
        surface_hash,
        window,
        ..
    } = *binding;
    let bound = match read_module(&app, module) {
        Ok(bound) => bound,
        Err(error) => {
            eprintln!("tilolang: {app}'s module: {error}");
            return std::process::ExitCode::from(2);
        }
    };
    if let (Some(surface), Some(hash)) = (surface, surface_hash.as_deref())
        && let Err(error) = register_surface(&app, surface, hash)
    {
        eprintln!("tilolang: {app}'s surface: {error}");
        return std::process::ExitCode::from(2);
    }
    if let Err(error) = super::bind(bound) {
        eprintln!("tilolang: {error}");
        return std::process::ExitCode::from(2);
    }
    ui::window_link::adopt(link, window, shell_ui);
    ui::launch::run_or_exit(&super::launch_spec());
    std::process::ExitCode::SUCCESS
}

/// What the session bound the spare to: the app, its module and its first window's connections.
struct Bind {
    binding: Box<Binding>,
    module: Descriptor,
    shell_ui: Option<Descriptor>,
    surface: Option<Descriptor>,
}

/// What the session bound the spare to, with its first window's connections: the patch
/// connection that came before it is kept for the window. `None` when the session let the spare
/// go unbound.
fn bound(link: &Descriptor) -> Result<Option<Bind>, String> {
    loop {
        match ToProcess::receive(link).map_err(|error| format!("its link: {error}"))? {
            None => return Ok(None),
            Some(ToProcess::Patch { instance, patch }) => {
                ui::window_link::patch_arrived(instance, patch);
            }
            Some(ToProcess::Bind {
                binding,
                module,
                shell_ui,
                surface,
            }) => {
                return Ok(Some(Bind {
                    binding,
                    module,
                    shell_ui,
                    surface,
                }));
            }
            Some(other) => return Err(format!("it was sent {other:?} before it was bound")),
        }
    }
}

/// Register the surface `app` publishes, read from `surface`, which hashes to `hash`.
fn register_surface(app: &str, surface: Descriptor, hash: &str) -> Result<(), String> {
    let bytes = read_whole(surface).map_err(|error| error.to_string())?;
    ui::app_surface::register_named(app, &bytes, hash)
}

/// What the windows of `app` show of it, read from its module as its signed package ships it:
/// its name, its icon and its translations, as a window of its installed package reads them.
fn read_module(app: &str, module: Descriptor) -> Result<super::Bound, String> {
    let wasm = read_whole(module).map_err(|error| error.to_string())?;
    let meta = tilolang_tilo::tilolang::PackageMeta::from_wasm(&wasm)
        .ok_or("it is not a tilolang module")?;
    if meta.app != app {
        return Err(format!("it is the module of {}", meta.app));
    }
    Ok(super::Bound {
        app: meta.app,
        title: meta.title,
        icon: meta.icon,
        catalogues: tilolang_tilo::tilolang::meta::locales_from_wasm(&wasm),
    })
}

/// The whole of the sealed file `fd`, read at its offsets: the session hands each spare a copy
/// of the same file, whose shared position no read moves.
fn read_whole(fd: Descriptor) -> std::io::Result<Vec<u8>> {
    let file = std::fs::File::from(fd);
    let len = usize::try_from(file.metadata()?.len()).map_err(std::io::Error::other)?;
    let mut bytes = vec![0; len];
    file.read_exact_at(&mut bytes, 0)?;
    Ok(bytes)
}
