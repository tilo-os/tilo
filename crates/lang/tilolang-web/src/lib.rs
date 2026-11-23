//! Tilolang in a browser page.
//!
//! - **Packages** ([`compile_package`], [`open_app`]): a tree of package files, read as the
//!   `tilolang` tool reads a directory of packages ([`files`]), compiled in dependency order
//!   against what the platform and the native apps publish; an app of it run over one of its
//!   scenarios, its window laid out by the toolkit and written as semantic HTML
//!   ([`window::AppPage`]), and what the person does on that page acted on the window.
//! - **The playground** ([`playground`]): one source text, run by two people whose workspaces sync,
//!   and its view trees as JSON.
//! - **Highlighting** ([`playground::tokenize`]): the lexer's tokens as editor spans.
//!
//! Every export takes and answers JSON text, so a page needs no bindings of its own.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use serde::Deserialize;
use serde_json::{Value as Json, json};
use wasm_bindgen::prelude::*;

pub mod files;
mod fonts;
mod platform;
pub mod playground;
pub mod window;

pub use platform::Packages;

use files::{Failure, Tree};
use window::{AppPage, Staging};

thread_local! {
    static PAGES: RefCell<BTreeMap<u32, AppPage>> = const { RefCell::new(BTreeMap::new()) };
    static NEXT: RefCell<u32> = const { RefCell::new(1) };
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn error(message: &str);
}

/// A panic in the page's module says what failed on the console before the module stops, rather
/// than leaving the page an `unreachable` to puzzle over.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(start)]
fn start() {
    std::panic::set_hook(Box::new(|info| error(&format!("tilolang-web: {info}"))));
}

pub use fonts::{add_font, fonts_ready, startup_fonts};

/// The files a page hands over: `{ "apps/tl/tasks/app.tl": "…", … }`.
fn read_files(files_json: &str) -> Result<Tree, Failure> {
    let files: BTreeMap<String, String> =
        serde_json::from_str(files_json).map_err(|error| Failure {
            message: format!("the files are not a JSON object of path to text: {error}"),
            diagnostics: Vec::new(),
        })?;
    Tree::read(files)
}

fn failed(failure: &Failure) -> String {
    json!({
        "ok": false,
        "message": failure.message,
        "diagnostics": failure.diagnostics,
    })
    .to_string()
}

fn refused(message: impl Into<String>) -> String {
    json!({ "ok": false, "message": message.into(), "diagnostics": [] }).to_string()
}

/// What a compile of `root` built: each package in dependency order, the app (`root`'s) last.
fn built_json(built: &[tilolang::source::BuiltPackage]) -> Json {
    let packages: Vec<Json> = built
        .iter()
        .map(|package| {
            let meta = &package.compiled.meta;
            json!({
                "dir": package.dir.display().to_string(),
                "app": meta.app,
                "title": meta.title,
                "wasmBytes": package.compiled.wasm.len(),
                "scenarios": meta.scenarios.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
                "roots": meta.roots.iter().filter_map(|id| {
                    meta.types.iter().find(|t| &t.id == id).map(|t| t.name.clone())
                }).collect::<Vec<_>>(),
            })
        })
        .collect();
    let app = packages.last().cloned().unwrap_or(Json::Null);
    json!({ "ok": true, "app": app, "packages": packages })
}

/// Compile the package in the directory `root` of `files_json` and the packages it uses.
///
/// Answers `{ok: true, app, packages}` or `{ok: false, message, diagnostics}`.
#[wasm_bindgen]
pub fn compile_package(files_json: &str, root: &str) -> String {
    match read_files(files_json).and_then(|tree| tree.compile(root)) {
        Ok((built, _)) => built_json(&built).to_string(),
        Err(failure) => failed(&failure),
    }
}

/// How [`open_app`] stages the window: `{scenario, root, width, height, dark}`, each optional.
#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct Options {
    /// The scenario to stage; absent stages the package's first, `""` none.
    scenario: Option<String>,
    root: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    dark: bool,
    /// Where the workspace's resources open on the web (`tilolang_tilo::share::LinkHome`, as
    /// JSON), when the page stands behind a workspace: what a resource's `share-link` mints
    /// under. Absent on a page no workspace stands behind, and then the link is refused.
    #[serde(default, alias = "linkHome")]
    link_home: Option<tilolang_tilo::share::LinkHome>,
}

/// Compile the package in `root` of `files_json` and what it uses, run its app over a scenario
/// and lay its window out as a page.
///
/// Answers `{ok: true, handle, app, packages, scenario, css, body}`, where `body` is the page's
/// HTML and `css` its stylesheet, or `{ok: false, message, diagnostics}`.
#[wasm_bindgen]
pub fn open_app(files_json: &str, root: &str, options_json: &str) -> String {
    let options: Options = if options_json.trim().is_empty() {
        Options::default()
    } else {
        match serde_json::from_str(options_json) {
            Ok(options) => options,
            Err(error) => return refused(format!("the options do not read: {error}")),
        }
    };
    if let Err(why) = fonts::ready() {
        return refused(why);
    }
    let (built, published) = match read_files(files_json).and_then(|tree| tree.compile(root)) {
        Ok(compiled) => compiled,
        Err(failure) => return failed(&failure),
    };
    let Some(app) = built.last().map(|package| package.compiled.meta.clone()) else {
        return refused(format!("{root}: no package"));
    };
    let scenario = match options.scenario {
        None => app.scenarios.first().map(|s| s.name.clone()),
        Some(named) => Some(named).filter(|name| !name.is_empty()),
    };
    let compiled = Rc::new(built.iter().map(|b| b.compiled.clone()).collect::<Vec<_>>());
    let packages = Rc::new(Packages::new(compiled, published));
    let staging = Staging {
        scenario: scenario.as_deref(),
        root: options.root.as_deref(),
        viewport: (options.width.unwrap_or(1024), options.height.unwrap_or(720)),
        dark: options.dark,
        link_home: options.link_home,
    };
    let mut page = match AppPage::open(&app.app, &packages, &staging) {
        Ok(page) => page,
        Err(error) => return refused(error),
    };
    let (css, body, gaps) = (page.css(), page.render(), page.gaps());
    let handle = NEXT.with(|next| {
        let mut next = next.borrow_mut();
        let handle = *next;
        *next = next.wrapping_add(1);
        handle
    });
    PAGES.with(|pages| pages.borrow_mut().insert(handle, page));
    let mut out = built_json(&built);
    if let Some(object) = out.as_object_mut() {
        object.insert("handle".into(), handle.into());
        object.insert("scenario".into(), scenario.into());
        object.insert("css".into(), css.into());
        object.insert("body".into(), body.into());
        object.insert("gaps".into(), json!(gaps));
    }
    out.to_string()
}

fn with_page(handle: u32, f: impl FnOnce(&mut AppPage) -> Result<Json, String>) -> String {
    PAGES.with(|pages| {
        let mut pages = pages.borrow_mut();
        let Some(page) = pages.get_mut(&handle) else {
            return refused(format!("no app is open as {handle}"));
        };
        match f(page) {
            Ok(Json::Object(mut answer)) => {
                answer.insert("ok".into(), true.into());
                Json::Object(answer).to_string()
            }
            Ok(other) => json!({ "ok": true, "value": other }).to_string(),
            Err(error) => refused(error),
        }
    })
}

fn patches_json(patches: &[ui::composition::page::Patch]) -> Json {
    json!({ "patches": patches })
}

/// The open app's whole page body again, and what of it is not the browser's own elements:
/// `{ok, body, gaps}`.
#[wasm_bindgen]
pub fn app_render(handle: u32) -> String {
    with_page(handle, |page| {
        Ok(json!({ "body": page.render(), "gaps": page.gaps() }))
    })
}

/// What the person did on the element numbered `id` (its `data-p`) at `target` (its `data-t`):
/// `"activate"`, `"toggle"`, `"focus"`, `{"value": text}` or `{"select": {anchor, focus}}`, as
/// the element's `data-a` asks. Answers `{ok, patches: [{id, html}]}`: each element to replace,
/// by its `data-p`.
#[wasm_bindgen]
pub fn app_act(handle: u32, id: u32, target: Option<String>, action_json: &str) -> String {
    let action = match semantic(action_json) {
        Ok(action) => action,
        Err(error) => return refused(error),
    };
    with_page(handle, move |page| {
        page.act(id, target.as_deref(), &action)
            .map(|patches| patches_json(&patches))
    })
}

/// The system's copy link to the resource `resource` of the app open as `handle`: `{ok,
/// value: {url}}`, or `{ok: false, message}` where no page stands behind the window or the
/// resource is not the app's. A page's copy-link control asks this, and the link is minted by
/// the same code a native window's palette reaches.
#[wasm_bindgen]
pub fn app_share(handle: u32, resource: &str) -> String {
    with_page(handle, |page| page.share_link(resource))
}

/// The open app's window laid out at `width` × `height`: `{ok, patches}`.
#[wasm_bindgen]
pub fn app_resize(handle: u32, width: u32, height: u32) -> String {
    with_page(handle, |page| {
        page.resize((width, height))
            .map(|patches| patches_json(&patches))
    })
}

/// Close the open app `handle`; whether one was open.
#[wasm_bindgen]
pub fn app_close(handle: u32) -> bool {
    PAGES.with(|pages| pages.borrow_mut().remove(&handle).is_some())
}

/// What a page sends for an element's `data-a`, as the toolkit's semantic action.
fn semantic(action_json: &str) -> Result<ui::a11y::Action, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "lowercase")]
    enum Said {
        Value(String),
        Select { anchor: usize, focus: usize },
    }
    let json: Json = serde_json::from_str(action_json)
        .or_else(|_| Ok::<_, String>(Json::String(action_json.to_owned())))?;
    match json {
        Json::String(word) => match word.as_str() {
            "activate" => Ok(ui::a11y::Action::Activate),
            "toggle" => Ok(ui::a11y::Action::ToggleCheck),
            "focus" => Ok(ui::a11y::Action::Focus),
            "increment" => Ok(ui::a11y::Action::Increment),
            "decrement" => Ok(ui::a11y::Action::Decrement),
            other => Err(format!(
                "`{other}` is no page action: activate, toggle, focus, increment, decrement, \
                 {{value}} or {{select}}"
            )),
        },
        other => match serde_json::from_value::<Said>(other) {
            Ok(Said::Value(value)) => Ok(ui::a11y::Action::SetValue(value)),
            Ok(Said::Select { anchor, focus }) => {
                Ok(ui::a11y::Action::SetSelection { anchor, focus })
            }
            Err(error) => Err(format!("the action does not read: {error}")),
        },
    }
}
