//! Thin binary entry point for the qOS on-screen keyboard.
//!
//! Two modes, mirroring the other `ui` apps (e.g. file-explorer):
//!   * default — build an [`OskWidget`], hand it to `ui::App`, run the live
//!     winit event loop.
//!   * `--shots <PATH>` — render the keyboard at a default size into a PNG at
//!     `<PATH>` and exit. No window, no event loop. Used for headless
//!     screenshot tests and CI snapshots.
//!
//! ## Integration seams
//!
//! ## The keyboard is special — the compositor owns its visibility
//!
//! In qOS a normal app can't hide itself; every window lives in the tile
//! layout and the OS owns its chrome. The on-screen keyboard is the one
//! exception: it floats over whatever the user is typing into and vanishes when
//! nothing wants text. That decision belongs to the **compositor**, not to this
//! app. The compositor recognises the keyboard by its app-id
//! ([`osk::KEYBOARD_APP_ID`]), keeps it out of the tile layout, and shows it
//! only while a text input is focused — relaying that show/hide to the keyboard
//! (and only the keyboard) over the input method. The widget never decides for
//! itself to appear or disappear.
//!
//! * **Auto-show via input-method-v2 (WIRED HERE):** when the compositor
//!   advertises `zwp_input_method_manager_v2` (the qOS compositor does), the OSK
//!   becomes an input-method client: it starts hidden, appears when the
//!   compositor activates it (a text field is focused), picks the layout from
//!   the field's content purpose, hides when focus leaves, and types via
//!   `commit_string` (the clean IM path) rather than synthetic key events. The
//!   input-method client runs on its own Wayland connection ([`osk::imclient`]);
//!   a background thread turns its events into show/hide/purpose commands
//!   delivered to the winit loop through the `EventLoopProxy`, where the widget
//!   reacts via `on_external_event`. Use `--no-input-method` to force the
//!   always-visible path; `--input-method` requires the protocol (errors if
//!   absent).
//!
//! * **Text injection (client half, fallback):** when there is no input method,
//!   the live path builds a [`TextInjector`] (real `virtual-keyboard-v1`
//!   backend, else a no-op [`NullInjector`]) and sets it as the widget's
//!   `on_key` callback, so every committed [`osk::KeyAction`] is typed into the
//!   focused surface as the user taps/swipes — the same approach `wtype` uses.
//!   See [`osk::inject`] for the keymap-generation details and the manual-test
//!   recipe.
//!
//! * **Floating overlay surface (still a seam):** the compositor already
//!   recognises the keyboard by app-id, keeps it out of the tile layout, and
//!   computes a bottom-anchored rect over the focused app (see the compositor's
//!   `keyboard_overlay` module). What is not wired yet is the *surface role*:
//!   the keyboard wants to be a `wlr-layer-shell` overlay (or an equivalent
//!   compositor-positioned surface) so the compositor maps/unmaps and places it
//!   directly. `ui::App` (winit) creates a plain xdg-toplevel and exposes no way
//!   to request a layer-shell role or even set the Wayland app-id, so today this
//!   binary opens a normal window and the keyboard simply paints nothing while
//!   the compositor's decision says hidden. Closing this seam means teaching
//!   `ui::App` to (a) set the surface app-id and (b) request the layer-shell
//!   role; the widget, the injector, and the input-method client are unchanged.
//!
//! * **FUTO swipe + prediction:** a swipe decoder will consume the
//!   pointer path during a press-drag, normalise it against
//!   [`OskWidget::key_geometry`] (the same geometry used to render), and feed
//!   candidate words into the toolbar's candidate ribbon. The decoder lives
//!   in a separate (GPL) process/crate; this binary will spawn/talk to it.
//!   No swipe code is linked here.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use osk::InputPurpose;
use osk::OskWidget;
use osk::imclient::{ImState, InputMethodClient, try_input_method};
use osk::inject::default_injector;
use osk::input_purpose_name;
use taffy::prelude::*;
use ui::app::UserEvent;
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

/// Default offscreen viewport for `--shots`. Phone-portrait-ish width with
/// just enough height for the toolbar + suggestion bar + four key rows
/// (48 + 40 + 4×56 = 312, rounded up).
const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (720, 360);

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    // A `--purpose <kind>` selects the field-specific layout (number pad,
    // dialpad, email, …) so a human (or a snapshot) can eyeball each one.
    let purpose = parse_purpose_arg(&args);
    if let Some(out) = parse_shots_arg(&args) {
        let mut widget = OskWidget::new();
        if let Some(p) = purpose {
            widget.set_purpose(p);
        }
        // `--emoji [QUERY]` stages the emoji picker open (optionally mid-search)
        // for a screenshot.
        if let Some(q) = parse_emoji_arg(&args) {
            widget.set_emoji_mode(true);
            if !q.is_empty() {
                widget.set_emoji_query(&q);
            }
        }
        render_shot_to_png(Box::new(widget), DEFAULT_SHOTS_VIEWPORT, &out)?;
        return Ok(());
    }

    // --- pick the input path. Prefer auto-show via input-method-v2 when the
    // compositor supports it: the keyboard appears on text focus, follows the
    // field's content purpose, hides on blur, and commits text via the IM. Fall
    // back to the always-visible keyboard + virtual-keyboard injection
    // otherwise. ---
    let im_pref = parse_input_method_pref(&args);
    let im = match im_pref {
        // Force the always-visible path; never try the input method.
        InputMethodPref::Off => None,
        // Try; fail loudly if explicitly requested but unavailable.
        InputMethodPref::Require => match try_input_method() {
            Some(im) => Some(im),
            None => {
                return Err(
                    "osk: --input-method requested but the compositor does not advertise \
                     zwp_input_method_manager_v2"
                        .into(),
                );
            }
        },
        // Default: use it if present, otherwise degrade silently.
        InputMethodPref::Auto => try_input_method(),
    };

    if let Some(im) = im {
        tracing::info!("osk: auto-show via input-method-v2");
        run_auto_show(im, purpose);
    } else {
        run_always_visible(purpose);
    }
    Ok(())
}

/// The always-visible keyboard: it fills its surface and types into the focused
/// app via `virtual-keyboard-v1` (or a no-op when that protocol is absent). This
/// is the path when there is no input method to drive auto-show.
fn run_always_visible(purpose: Option<InputPurpose>) {
    let mut injector = default_injector();
    tracing::info!("osk: text-injection backend = {}", injector.backend_name());
    let mut widget = OskWidget::new().on_key(move |action| {
        injector.key_action(&action);
    });
    if let Some(p) = purpose {
        widget.set_purpose(p);
    }

    // NOTE: in production this surface is a wlr-layer-shell overlay; for now it
    // is a plain ui::App window. `ui::App::run()` owns the event loop, so the
    // `on_key` callback above is the seam that needs no `ui` change — it fires
    // synchronously as each key is committed.
    // Deliberately no `--headless-drive` hook. Every key this widget commits
    // is forwarded to `injector`, which types into whatever application holds
    // focus in the live session — driving it from a test harness would inject
    // keystrokes into the user's desktop.

    let (state, autosave) = ui::persist::open("dev.qos.osk");
    let app = ui::App::new(state, Box::new(widget)).with_autosave(autosave);
    app.run();
}

/// The auto-show keyboard, driven by an `input-method-v2` client.
///
/// The keyboard starts hidden. A background thread blocks on the input-method
/// connection; each focus event is folded through an [`ImState`] reducer into a
/// minimal show/hide/purpose command, which it sends to the winit loop via the
/// `EventLoopProxy` as a [`UserEvent::External`] string the widget understands
/// (`osk:show`, `osk:hide`, `osk:purpose:<name>`). Typed/swiped text is committed
/// back to the focused app via the same input method (`commit_string`).
fn run_auto_show(im: InputMethodClient, purpose: Option<InputPurpose>) {
    // The IM client is shared: the winit thread's `on_key` callback commits
    // typed text through it, while the background thread reads focus events.
    let im = Arc::new(Mutex::new(im));

    let im_for_keys = Arc::clone(&im);
    let mut widget = OskWidget::new().hidden().on_key(move |action| {
        // Turn the committed action into the text to insert and send it via the
        // input method. State-only actions (shift/symbols/layout) and backspace
        // produce no commit string here — backspace is handled by the host's
        // surrounding-text editing, not by inserting a character.
        if let Some(text) = osk::inject::action_symbols(&action)
            .iter()
            .map(symbol_to_commit_text)
            .collect::<Option<String>>()
            && !text.is_empty()
            && let Ok(mut im) = im_for_keys.lock()
        {
            im.commit_text(&text);
        }
    });
    // A `--purpose` override still applies as the initial layout; the field's
    // content_type will replace it once focus lands.
    if let Some(p) = purpose {
        widget.set_purpose(p);
    }

    let state = ui::State::new();
    let app = ui::App::new(state, Box::new(widget));

    // Spawn the input-method reader once the event loop proxy exists, so it can
    // push show/hide/purpose commands into the loop.
    let im_for_reader = Arc::clone(&im);
    app.run_with_setup(move |proxy| {
        std::thread::spawn(move || {
            let mut model = ImState::new();
            loop {
                // Poll without blocking, then release the lock immediately so the
                // winit thread's `on_key` callback can take it to commit typed
                // text. (Blocking on the socket while holding the lock would
                // deadlock against that callback.) Focus changes are rare and
                // not latency-critical, so a short sleep between polls is fine.
                let events = {
                    let Ok(mut im) = im_for_reader.lock() else {
                        break;
                    };
                    im.poll()
                };
                for ev in events {
                    let t = model.reduce(ev);
                    if let Some(visible) = t.visibility_changed {
                        let cmd = if visible { "osk:show" } else { "osk:hide" };
                        if proxy.send_event(UserEvent::External(cmd.to_string())).is_err() {
                            return; // event loop gone
                        }
                    }
                    if let Some(p) = t.purpose_changed {
                        let cmd = format!("osk:purpose:{}", input_purpose_name(p));
                        if proxy.send_event(UserEvent::External(cmd)).is_err() {
                            return;
                        }
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(16));
            }
        });
    });
}

/// The text a single [`osk::Symbol`] inserts when committed through the input
/// method, or `None` for symbols that aren't a plain inserted character (so the
/// whole commit is skipped — e.g. backspace, handled by surrounding-text edits).
fn symbol_to_commit_text(sym: &osk::Symbol) -> Option<char> {
    match sym {
        osk::Symbol::Unicode(c) => Some(*c),
        osk::Symbol::Space => Some(' '),
        osk::Symbol::Return => Some('\n'),
        osk::Symbol::Tab => Some('\t'),
        osk::Symbol::BackSpace => None,
    }
}

/// How the binary should treat the input method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputMethodPref {
    /// Use the input method if the compositor offers it, else fall back.
    Auto,
    /// Require it — error out if the compositor doesn't offer it.
    Require,
    /// Never use it — always-visible keyboard.
    Off,
}

/// Read the input-method preference from the args. `--no-input-method` forces it
/// off; `--input-method` requires it; absence means auto-detect.
fn parse_input_method_pref(args: &[String]) -> InputMethodPref {
    if args.iter().any(|a| a == "--no-input-method") {
        InputMethodPref::Off
    } else if args.iter().any(|a| a == "--input-method") {
        InputMethodPref::Require
    } else {
        InputMethodPref::Auto
    }
}

/// Returns `Some(path)` if `--shots <path>` (or `--shots=<path>`) appears
/// in `args`, otherwise `None`.
fn parse_shots_arg(args: &[String]) -> Option<std::path::PathBuf> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--shots" {
            return iter.next().map(std::path::PathBuf::from);
        }
        if let Some(rest) = arg.strip_prefix("--shots=") {
            return Some(std::path::PathBuf::from(rest));
        }
    }
    None
}

/// `--emoji [QUERY]` (or `--emoji=QUERY`): stage the emoji picker open for a
/// shot, optionally with a preset search query. `Some("")` = open, no query.
fn parse_emoji_arg(args: &[String]) -> Option<String> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--emoji" {
            return Some(
                iter.next()
                    .filter(|s| !s.starts_with("--"))
                    .cloned()
                    .unwrap_or_default(),
            );
        }
        if let Some(rest) = arg.strip_prefix("--emoji=") {
            return Some(rest.to_string());
        }
    }
    None
}

/// Returns the [`InputPurpose`] requested by `--purpose <kind>` (or
/// `--purpose=<kind>`), or `None` when absent. An unrecognised kind is ignored
/// (the keyboard falls back to plain text) so a typo doesn't abort launch.
fn parse_purpose_arg(args: &[String]) -> Option<InputPurpose> {
    let mut iter = args.iter().skip(1);
    let raw = loop {
        let arg = iter.next()?;
        if arg == "--purpose" {
            break iter.next()?.clone();
        }
        if let Some(rest) = arg.strip_prefix("--purpose=") {
            break rest.to_string();
        }
    };
    purpose_from_str(&raw)
}

/// Map a `--purpose` value to an [`InputPurpose`]. Accepts both the short
/// spellings a user is likely to type and the enum-ish names.
fn purpose_from_str(s: &str) -> Option<InputPurpose> {
    Some(match s.to_ascii_lowercase().as_str() {
        "text" => InputPurpose::Text,
        "number" | "numeric" => InputPurpose::Number,
        "decimal" => InputPurpose::Decimal,
        "phone" | "tel" | "telephone" => InputPurpose::Telephone,
        "url" => InputPurpose::Url,
        "email" => InputPurpose::Email,
        "pin" => InputPurpose::Pin,
        "password" | "pass" => InputPurpose::Password,
        "date" => InputPurpose::Date,
        "time" => InputPurpose::Time,
        _ => return None,
    })
}

/// Run one offscreen frame against `widget` at `viewport` and write the
/// resulting RGBA to `out` as a PNG. Mirrors `ui::App`'s
/// `sync → build_layout → compute_layout → apply_layout → draw` cycle.
fn render_shot_to_png(
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    // `QOS_SHOTS_THEME=light|high-contrast` renders the shot under that theme
    // so the same frame can be graded in both light and dark (default dark).
    let theme = match std::env::var("QOS_SHOTS_THEME").ok().as_deref() {
        Some("light") => Theme::light(),
        Some("high-contrast") => Theme::high_contrast(),
        _ => Theme::dark(),
    };
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    let mut state = ui::State::new();
    widget.sync(&mut state.doc, &state.root);

    let child = widget.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy.new_with_children(
        Style {
            size: Size {
                width: Dimension::length(w as f32),
                height: Dimension::length(h as f32),
            },
            ..Default::default()
        },
        &[child],
    )?;

    {
        let measure_map = node_map.clone();
        let tr = &mut text_renderer;
        taffy.compute_layout_with_measure(
            root,
            taffy::Size {
                width: AvailableSpace::Definite(w as f32),
                height: AvailableSpace::Definite(h as f32),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )?;
    }
    widget.apply_layout(&taffy, child, Point::ORIGIN);

    let mut scene = Scene::new();
    widget.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        &theme,
        &presence,
        &permission,
    );

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut renderer = OffscreenRenderer::new(w, h);
    renderer.render_png(&scene, Color::TRANSPARENT, out);
    println!("osk: wrote shot to {}", out.display());
    Ok(())
}
