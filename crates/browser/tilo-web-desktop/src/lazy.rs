//! Framework code the first frame does not need, loaded the first time
//! something uses it: the search engine (tantivy and its codecs), the
//! spelling dictionaries (the embedded English list and Hunspell), which
//! the page fetches and builds in its idle time once its first frame is up, the hyphenation
//! patterns, which the first hyphenated paragraph asks for, the image
//! decoders, which the first web page image or decoded picture asks for,
//! the model service's clients, which the first model request or provider
//! settings change asks for, the pinned people overlay, which pinning
//! who is here asks for, the emoji catalogue, which the first picker,
//! reaction name or `:name:` completion asks for, History, which the
//! first window that registers it asks for, desktop search, which the
//! desktop asks for as it first lays out, the notification centre,
//! which opening it asks for, the HTML presenter, which a page mounted
//! with `dom: true` asks for, the directory and invite sheets, which
//! opening one asks for.
//!
//! Like an app's `definition()` (`crate::split`), each export below is the
//! only way to the code it hands over, so the splitter moves that code into
//! its own module. The crate that uses the code declares it with
//! `lazy_code!` and is built with its deferring feature; on first use it
//! asks the loader registered here, the module's export then installs it,
//! and every mount lays out and draws again so that what the code changes
//! shows.

use wasm_bindgen::prelude::{Closure, JsValue, wasm_bindgen};

#[wasm_bindgen(inline_js = r#"
export function afterIdle(delay, work) {
    const idle = globalThis.requestIdleCallback
        ?? (run => setTimeout(() => run({ timeRemaining: () => 4 }), 0));
    setTimeout(() => idle(deadline => work(performance.now() + Math.max(1, deadline.timeRemaining())),
        { timeout: 1000 }), delay);
}
export function now() { return performance.now(); }
"#)]
extern "C" {
    /// Run `work` in the page's first idle period `delay` ms from now (or
    /// within a second more, if the page is never idle), with the time it
    /// should hand the page back by.
    #[wasm_bindgen(js_name = afterIdle)]
    fn after_idle(delay: u32, work: &JsValue);
    #[wasm_bindgen(js_name = now)]
    fn now() -> f64;
}

/// Split out the code behind a toolkit surface that loads on first use
/// (`tilo_ui::lazy_code`): `$code` is where the toolkit asks for it (a
/// `LazyCode` or `Deferred`), `$builtin` the only expression that reaches
/// it, which the splitter therefore moves into module `$name`; `$then`, if
/// given, runs once it is installed. `register`
/// tells `$code` how to fetch it; the fetch installs it and lays every mount
/// out again so that what it changes shows.
macro_rules! split_code {
    ($(#[$meta:meta])* mod $module:ident: $ty:ty = $builtin:expr => $code:path, module = $name:literal, hash = $hash:literal $(, then = $then:path)?;) => {
        $(#[$meta])*
        mod $module {
            use std::ffi::c_void;

            use crate::split::Callback;

            #[link(wasm_import_module = "./__wasm_split.js")]
            #[allow(
                improper_ctypes,
                reason = "the function pointer only passes between Rust"
            )]
            unsafe extern "C" {
                #[link_name = concat!("__wasm_split_load_", $name, "_", $hash, "_code")]
                fn load_module(callback: Callback, data: *const c_void);
                #[link_name = concat!("__wasm_split_00___", $name, "___00_import_", $hash, "_code")]
                fn code() -> $ty;
            }

            #[unsafe(export_name = concat!("__wasm_split_00___", $name, "___00_export_", $hash, "_code"))]
            #[allow(
                improper_ctypes_definitions,
                reason = "the function pointer only passes between Rust"
            )]
            extern "C" fn export() -> $ty {
                $builtin
            }

            fn fetch() {
                wasm_bindgen_futures::spawn_local(async {
                    if crate::split::load(load_module).await {
                        // SAFETY: the module that defines `code` is loaded.
                        $code.install(unsafe { code() });
                        $($then();)?
                        tilo_browser_runtime::refresh_mounts();
                    } else {
                        $code.load_failed();
                    }
                });
            }

            pub(super) fn register() {
                $code.defer(fetch);
            }
        }
    };
}

split_code! {
    /// The search engine (tantivy and its codecs): the first search asks for
    /// it, and the page fetches it once its first frame is up.
    mod search: tilo_search::MakeEngine = tilo_search::builtin_engine() => tilo_search::ENGINE,
        module = "search", hash = "8c1f5e0d2b7a4936";
}

split_code! {
    /// The spelling dictionaries (the embedded English list and Hunspell):
    /// the page fetches them in its idle time once its first frame is up.
    mod spelling: spellcheck::Dictionaries = spellcheck::builtin_dictionaries() => spellcheck::DICTIONARIES,
        module = "spelling", hash = "3e9a71c4d05b28f6";
}

split_code! {
    /// The hyphenation patterns: the first hyphenated paragraph asks for them.
    mod hyphenation: tilo_ui::Hyphenate = tilo_ui::builtin_hyphenation() => tilo_ui::HYPHENATION,
        module = "hyphenation", hash = "b7d24f9a610ce583";
}

split_code! {
    /// The image decoders: the first web page image or decoded picture asks
    /// for them.
    mod page_images: tilo_ui::image_decoders::ImageDecoders = tilo_ui::image_decoders::builtin_image_decoders()
        => tilo_ui::image_decoders::IMAGE_DECODERS, module = "page_images", hash = "90894fa21934d3a4";
}

split_code! {
    /// The model service's clients: the first model request or provider
    /// settings change asks for them.
    mod model_clients: tilo_browser_runtime::ModelClients = tilo_browser_runtime::builtin_model_clients()
        => tilo_browser_runtime::MODEL_CLIENTS, module = "model_clients", hash = "5d2c8e71a94f03b6";
}

split_code! {
    /// The emoji catalogue: the first picker, reaction name or `:name:`
    /// completion asks for it.
    mod emoji: tilo_ui::emoji_catalogue::CatalogueFor = tilo_ui::emoji_catalogue::builtin_catalogue
        => tilo_ui::emoji_catalogue::CATALOGUE, module = "emoji", hash = "4b8e2d6f1a9c7035";
}

split_code! {
    /// Feedback's custody and delivery: a page configured with a destination
    /// asks for them.
    mod feedback_code: tilo_browser_runtime::FeedbackCode = tilo_browser_runtime::builtin_feedback()
        => tilo_browser_runtime::FEEDBACK, module = "feedback_service", hash = "9c3e5a17b2d4f860",
        then = tilo_browser_runtime::feedback_arrived;
}

split_code! {
    /// The code that shows a mount as HTML: a page that mounts with
    /// `dom: true` asks for it.
    mod dom_presenter: tilo_browser_runtime::DomCode = tilo_browser_runtime::builtin_dom()
        => tilo_browser_runtime::DOM_CODE, module = "dom_presenter", hash = "6c2f8a1d93e5b047";
}

split_code! {
    /// The directory and invite sheets: the first time the person opens one.
    mod sheets: tilo_browser_runtime::SheetsCode = tilo_browser_runtime::builtin_sheets()
        => tilo_browser_runtime::SHEETS, module = "page_sheets", hash = "8d3f1a6c05e2b947";
}

split_code! {
    /// What a person's agents on a host ask them: a page asked to answer for
    /// the root it holds fetches the session and the question sheet.
    mod questions: tilo_browser_runtime::QuestionsCode = tilo_browser_runtime::builtin_questions()
        => tilo_browser_runtime::QUESTIONS, module = "agent_questions", hash = "d5b3e8074c16a2f9",
        then = tilo_browser_runtime::questions_arrived;
}

split_code! {
    /// The profile card: the first card that opens asks for it.
    mod profile_card: tilo_desktop::profile_card::MakeProfileCard = tilo_desktop::profile_card::builtin_profile_card()
        => tilo_desktop::profile_card::PROFILE_CARD, module = "profile_card", hash = "5f81c2e94a3d07b6";
}

split_code! {
    /// The action palette: the first window to lay out asks for it.
    mod action_palette: tilo_ui::action_palette::MakeActionPalette = tilo_ui::action_palette::builtin_action_palette()
        => tilo_ui::action_palette::ACTION_PALETTE, module = "action_palette", hash = "a26d9f4c1e0b8573";
}

split_code! {
    /// Desktop search: the desktop asks for it as it first lays out.
    mod desktop_search: tilo_desktop_search::MakeDesktopSearch = tilo_desktop_search::builtin_desktop_search()
        => tilo_desktop_search::DESKTOP_SEARCH, module = "desktop_search", hash = "7be226dd82b8cdaa";
}

split_code! {
    /// The notification centre: the first time it opens asks for it.
    mod notification_centre: tilo_desktop_surface::notifications::MakeNotificationCentre = tilo_desktop_surface::notifications::builtin_notification_centre()
        => tilo_desktop_surface::notifications::NOTIFICATION_CENTRE, module = "notification_centre", hash = "3d94b0e6a1c7f258";
}

split_code! {
    /// A theme's surface shader: the first theme that states one asks for it.
    mod surface_shader: tilo_ui::part_paint::SurfaceShaderCode = tilo_ui::part_paint::builtin_surface_shader()
        => tilo_ui::part_paint::SURFACE_SHADER, module = "surface_shader", hash = "c40e7b19d2a6f385";
}

split_code! {
    /// History: the first window that registers it asks for it.
    mod history: tilo_ui::history_mode::MakeHistory = tilo_ui::history_mode::builtin_history()
        => tilo_ui::history_mode::HISTORY, module = "history", hash = "e1a7c5039b6d2f84";
}

/// Once the page has drawn its first frame, fetch the search engine's code.
///
/// The engine is a module of its own here, and a search that fetches it has no
/// rows until it lands — it says it is still searching the whole time, which on
/// a cold page is the wait between typing and the first row. The page fetching
/// it as soon as it has drawn something leaves that wait to the person's own
/// time before they first search, and the palette's own fetch (as it opens) is
/// then a no-op. Nothing of it is needed for the first frame, so it waits for
/// one, and a fetch that fails is tried again as the engine's loader says.
fn prepare_search_after_first_frame() {
    let work = Closure::once_into_js(move |_deadline: f64| {
        if tilo_browser_runtime::first_frame_presented() {
            tilo_search::prepare_engine();
        } else {
            prepare_search_after_first_frame();
        }
    });
    after_idle(SPELLING_POLL_MS, &work);
}

/// How often the page looks again whether its first frame is up, or the
/// dictionaries' code is in.
const SPELLING_POLL_MS: u32 = 250;

/// How long after the first frame the page begins on the spelling
/// dictionaries: the app in front, if its code is still coming, gets the
/// link first.
const SPELLING_AFTER_FIRST_FRAME_MS: u32 = 1_000;

/// Whether the first frame is up and its grace period has been granted.
static SPELLING_RELEASED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Once the page has drawn its first frame, fetch the spelling dictionaries'
/// code and build the English dictionary in the page's idle time, a slice per
/// idle period, so that neither the first frame nor any keystroke waits for
/// either. A field typed in before then flags nothing yet, and checks again
/// once the dictionary is built.
fn prepare_spelling_when_idle(delay: u32) {
    let work = Closure::once_into_js(move |deadline: f64| {
        use std::sync::atomic::Ordering;
        if !SPELLING_RELEASED.load(Ordering::Relaxed) {
            if tilo_browser_runtime::first_frame_presented() {
                SPELLING_RELEASED.store(true, Ordering::Relaxed);
                prepare_spelling_when_idle(SPELLING_AFTER_FIRST_FRAME_MS);
            } else {
                prepare_spelling_when_idle(SPELLING_POLL_MS);
            }
            return;
        }
        match spellcheck::prepare_dictionaries(&mut || now() < deadline) {
            Some(true) => tilo_browser_runtime::refresh_mounts(),
            Some(false) => prepare_spelling_when_idle(0),
            // The code is still on its way.
            None => prepare_spelling_when_idle(SPELLING_POLL_MS),
        }
    });
    after_idle(delay, &work);
}

pub(crate) fn register() {
    search::register();
    prepare_search_after_first_frame();
    spelling::register();
    spellcheck::build_dictionaries_in_steps();
    prepare_spelling_when_idle(SPELLING_POLL_MS);
    hyphenation::register();
    page_images::register();
    model_clients::register();
    emoji::register();
    history::register();
    surface_shader::register();
    action_palette::register();
    feedback_code::register();
    dom_presenter::register();
    profile_card::register();
    sheets::register();
    questions::register();
    desktop_search::register();
    notification_centre::register();
}
