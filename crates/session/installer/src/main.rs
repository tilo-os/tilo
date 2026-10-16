// Thin binary entry point for the Tilo graphical installer — all logic
// lives in the library (mirrors greeter/src/main.rs).
//
// Modes:
//   * default — build an `InstallerApp`, hand it to `ui::App`, run the
//     live winit event loop. Launched off the live media under `cage`:
//         cage -s -- /usr/bin/tilo-installer
//   * `--shots <PATH> [--scenario <name>]` plus the standard capture options
//     (`--viewport`, `--text-scale`, `--theme`, …, see
//     `ui::launch::LaunchArgs`) — render one screen into a PNG at `<PATH>`
//     and exit. No window, no event loop. `<name>` is one of network, disk,
//     disk-alongside, account, keyboard, keyboard-default, options,
//     options-open, profile-locale-repair, media, media-chosen, review,
//     review-alongside, install, finish, finish-recovery,
//     finish-recovery-saved, failed and failed-recovery; `options-open`
//     renders the Options step with the time-zone dropdown popped open;
//     `keyboard` seeds a non-default (German) layout so the preview shows the
//     remapped keys. Defaults to `disk`.
//
// A live run also answers the tilo-control action protocol (actions list,
// actions invoke, state-dump) on its console endpoints — see
// crates/session/console-control for where they listen and why only there.

use std::sync::{Arc, Mutex};

use installer::InstallerApp;
use installer::installer_app::{ProxySlot, Step};
use ui::widget::Widget;

/// Default offscreen viewport for `--shots`.
const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);

/// # Errors
///
/// Returns an error for a malformed command line or an unknown `--scenario`;
/// the live event-loop path does not return.
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _detached = ui::wake::WakeScope::enter_detached();
    tracing_subscriber::fmt::init();

    let args = ui::launch::LaunchArgs::from_env(&[])?;
    if let Some(out) = &args.shots {
        let name = args.scenario.as_deref().unwrap_or("disk");
        let widget = build_shot(name).ok_or_else(|| format!("unknown --scenario {name:?}"))?;
        ui::clock::freeze_for_capture();
        let viewport = args.viewport.unwrap_or(DEFAULT_SHOTS_VIEWPORT);
        let mut frame = ui::headless::FrameHarness::new_with_theme(
            ui::State::new(),
            widget,
            viewport,
            ui::launch::shot_theme(&args),
        );
        frame.settle_for_capture(ui::launch::CAPTURE_SETTLE_BOUND);
        frame.render_png(out);
        println!("tilo-installer: wrote shot to {}", out.display());
        return Ok(());
    }

    let wake_scope = ui::wake::WakeScope::new();
    let _wake_entry = wake_scope.enter();
    // Shared slot the event loop fills with its proxy once running; the
    // install worker thread reads it to stream progress back in.
    let proxy_slot: ProxySlot = Arc::new(Mutex::new(None));
    let console = console_control::ConsoleServer::new();
    let widget = InstallerApp::new()
        .with_proxy(proxy_slot.clone())
        .with_console(console.clone());
    let state = ui::State::new();
    let mut app = ui::App::from_scoped_parts(wake_scope, state, Box::new(widget));
    // Hand the keyboard to the first field/row at startup so the wizard
    // is usable without a click...
    app.autofocus_first = true;
    // ...and re-focus the first field of each step after the per-step
    // rebuild (opt-in; leaves shell/greeter behaviour untouched).
    app.autofocus_on_structure_change = true;
    let slot = proxy_slot;
    app.run_with_setup(move |proxy| {
        if let Ok(mut g) = slot.lock() {
            *g = Some(proxy.clone());
        }
        serve_console(&console, proxy);
    });
    Ok(())
}

/// Start the console endpoints that exist on this machine: the private
/// runtime socket, and the virtio port when a hypervisor attached one. Each
/// queued request wakes the event loop, which answers it on the UI thread.
fn serve_console(
    console: &console_control::ConsoleServer,
    proxy: winit::event_loop::EventLoopProxy<ui::app::UserEvent>,
) {
    let waker: console_control::server::Waker = Arc::new(move || {
        let _ = proxy.send_event(ui::app::UserEvent::External(
            installer::installer_app::CONSOLE_WAKE.to_owned(),
        ));
    });
    console.serve_discovered(installer::operation::PROVIDER, &waker);
}

/// Build the widget for a `--shots --scenario <name>` render. The shot app
/// carries sample account details so input fields show representative text.
/// Returns `None` for an unrecognised name.
fn build_shot(name: &str) -> Option<Box<dyn Widget>> {
    if name == "disk-alongside" {
        let mut app = InstallerApp::for_shot(Step::Disk);
        app.shot_alongside();
        return Some(Box::new(app));
    }
    if name == "review-alongside" {
        let mut app = InstallerApp::for_shot(Step::Review);
        app.shot_alongside_review();
        return Some(Box::new(app));
    }
    // `options-open` renders the Options step with the time-zone dropdown
    // popped open, capturing the overlay popup state.
    if name == "options-open" {
        let mut app = InstallerApp::for_shot(Step::Options);
        app.shot_open_timezone();
        return Some(Box::new(app));
    }
    if name == "profile-locale-repair" {
        let mut app = InstallerApp::for_shot(Step::Options);
        app.shot_locale_repair();
        return Some(Box::new(app));
    }

    // `keyboard` seeds a non-default layout so the visual preview clearly
    // shows the remapped keys (y/z swap + ü ö ä for German QWERTZ).
    if name == "keyboard" {
        let mut app = InstallerApp::for_shot(Step::Keyboard);
        app.shot_keymap("German (QWERTZ)");
        return Some(Box::new(app));
    }

    if name == "media-chosen" {
        let mut app = InstallerApp::for_shot(Step::Media);
        app.shot_codecs_chosen();
        return Some(Box::new(app));
    }

    if name == "finish-recovery" {
        let mut app = InstallerApp::for_shot(Step::Install);
        app.shot_finished_with_recovery();
        return Some(Box::new(app));
    }
    if name == "finish-recovery-saved" {
        let mut app = InstallerApp::for_shot(Step::Install);
        app.shot_finished_with_saved_recovery();
        return Some(Box::new(app));
    }
    if name == "failed-recovery" {
        let mut app = InstallerApp::for_shot(Step::Install);
        app.shot_failed_with_recovery();
        return Some(Box::new(app));
    }

    let (step, terminal): (Step, Option<bool>) = match name {
        "network" => (Step::Network, None),
        "disk" => (Step::Disk, None),
        "account" => (Step::Account, None),
        "keyboard-default" => (Step::Keyboard, None),
        "options" => (Step::Options, None),
        "media" => (Step::Media, None),
        "review" => (Step::Review, None),
        "install" => (Step::Install, None),
        "finish" => (Step::Install, Some(true)),
        "failed" => (Step::Install, Some(false)),
        _ => return None,
    };
    let mut app = InstallerApp::for_shot(step);
    match terminal {
        Some(true) => app.shot_finished(),
        Some(false) => app.shot_failed(),
        None => {}
    }
    Some(Box::new(app))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_and_profile_repair_shot_states_are_renderable() {
        // Phase `recovery_visual_states_are_renderable`.
        {
            for scenario in [
                "finish-recovery",
                "finish-recovery-saved",
                "failed-recovery",
            ] {
                assert!(build_shot(scenario).is_some(), "missing {scenario}");
            }
        }
        // Phase `transported_profile_repair_state_is_renderable`.
        {
            assert!(build_shot("profile-locale-repair").is_some());
        }
    }
}
