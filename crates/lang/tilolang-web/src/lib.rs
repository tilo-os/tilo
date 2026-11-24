//! Tilolang in a browser page.
//!
//! - **Packages** ([`compile_package`], [`open_app`]): a tree of package files, read as the
//!   `tilolang` tool reads a directory of packages ([`files`]), compiled in dependency order
//!   against what the platform and the native apps publish; an app of it run over one of its
//!   scenarios, its window laid out by the toolkit and written as semantic HTML
//!   ([`window::AppPage`]), and what the person does on that page acted on the window.
//! - **The playground** ([`playground`]): one source text, run by two people whose workspaces sync,
//!   and its view trees as JSON.
//! - **Highlighting** ([`playground::tokenize`]): a source's spans as the toolkit's code editor
//!   colours them.
//!
//! Every export takes and answers JSON text, so a page needs no bindings of its own.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value as Json, json};
use ui::composition::page::PageAction;
use wasm_bindgen::prelude::*;

pub mod files;
mod fonts;
pub mod playground;
pub mod window;

use files::{Failure, Tree};
use tilolang_window::{OpenedFile, Packages};
use window::{AppPage, Staging};

/// The most a page opens an app with: a file it holds whole in memory before any app reads it.
const MAX_OPENED_BYTES: usize = 512 * 1024 * 1024;

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
fn parse_files(files_json: &str) -> Result<BTreeMap<String, String>, Failure> {
    serde_json::from_str(files_json).map_err(|error| Failure {
        message: format!("the files are not a JSON object of path to text: {error}"),
        diagnostics: Vec::new(),
    })
}

fn read_files(files_json: &str) -> Result<Tree, Failure> {
    Tree::read(parse_files(files_json)?)
}

/// A package with no root opens no window of its own: its views are drawn where a host shows them
/// (a player's card in the shell). The page is such a host: a package beside it, in the directory
/// this answers, holds one new resource of the type the package's first view draws and shows it
/// through that view, as the shell would. `None` when the package declares no view that draws
/// one of its own types.
fn host_of(app: &tilolang::meta::PackageMeta, root: &str) -> Option<(String, String)> {
    let (view, ty) = app
        .views
        .iter()
        .filter(|v| v.presents.is_none() && v.row_of.is_none())
        .find_map(|view| {
            let [param] = view.params.as_slice() else {
                return None;
            };
            let ty = app
                .types
                .iter()
                .find(|t| t.id == param.ty || t.name == param.ty)?;
            Some((view.name.clone(), ty.name.clone()))
        })?;
    let dir = format!("{}.host", root.trim_end_matches('/'));
    let source = format!(
        "app {id}.host \"{title}\"\n\n\
         use {id} as hosted\n\n\
         /// Where the page draws the package's `{view}` view, as a host does: one {ty}, made new.\n\
         type Host {{\n    held: Collection<hosted.{ty}>\n}}\n\n\
         root Host\n\n\
         initial action start(h: Host) {{\n    h.held.insert(hosted.{ty}())\n}}\n\n\
         default view host for Host\n\n\
         view host(h: Host) {{\n    for x in h.held {{\n        hosted.{view}(x)\n    }}\n}}\n",
        id = app.app,
        title = app.title,
    );
    Some((dir, source))
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

/// The file a page opens an app with, as `(name, media type, bytes)`: `None` when it opens none.
/// The name is the person's, so it stays a name and never a path.
fn opened_file(request: Option<OpenRequest>) -> Result<Option<(String, String, Vec<u8>)>, String> {
    let Some(request) = request else {
        return Ok(None);
    };
    let refused_name = request.name.is_empty()
        || request.name.len() > 255
        || request.name.chars().any(|c| matches!(c, '/' | '\\' | '\0'));
    if refused_name {
        return Err(format!("`{}` is no file name", request.name));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(request.bytes)
        .map_err(|error| format!("`{}`: {error}", request.name))?;
    if bytes.len() > MAX_OPENED_BYTES {
        return Err(format!("`{}` is too large to open here", request.name));
    }
    let media = request
        .media
        .unwrap_or_else(|| tilolang_tilo::runtime::blob::media_of(&request.name));
    Ok(Some((request.name, media, bytes)))
}

/// Install the files each package ships under `media/`, by the name an address (`pkg:<name>`)
/// gives it: a view or a scenario plays one from these bytes.
fn install_assets(built: &[tilolang::source::BuiltPackage]) {
    for package in built {
        tilolang_window::media_source::install_package_assets(
            package
                .source
                .assets
                .iter()
                .map(|asset| (asset.name.clone(), asset.bytes.clone())),
        );
    }
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

/// A file the page opened the app with: its name, the media type it declares, and its bytes as
/// base64, since a page's answer is JSON text.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenRequest {
    name: String,
    media: Option<String>,
    bytes: String,
}

/// How [`open_app`] stages the window: `{scenario, root, width, height, dark, open, link}`, each
/// optional.
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
    /// The file the app is opened with, as a shell opens one: its bytes are kept in the page's
    /// content store and the app's action for its kind reads them.
    #[serde(default, alias = "openFile")]
    open: Option<OpenRequest>,
    /// The link the app is opened with: its `open-link` shows what it names.
    #[serde(default)]
    link: Option<String>,
}

/// Compile the package in `root` of `files_json` and what it uses, run its app over a scenario
/// and lay its window out as a page.
///
/// Answers `{ok: true, handle, app, packages, scenario, css, body, gaps, hosted}`, where `body` is
/// the page's HTML and `css` its stylesheet, or `{ok: false, message, diagnostics}`. `hosted`
/// names the app when it has no window of its own and the page draws its first view as a host
/// does ([`host_of`]); `app` is then that host.
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
    let files = match parse_files(files_json) {
        Ok(files) => files,
        Err(failure) => return failed(&failure),
    };
    let compile = |files: BTreeMap<String, String>, root: &str| {
        Tree::read(files).and_then(|tree| tree.compile(root))
    };
    let (mut built, mut published) = match compile(files.clone(), root) {
        Ok(compiled) => compiled,
        Err(failure) => return failed(&failure),
    };
    let Some(mut app) = built.last().map(|package| package.compiled.meta.clone()) else {
        return refused(format!("{root}: no package"));
    };
    let mut hosted = None;
    if app.roots.is_empty()
        && let Some((dir, source)) = host_of(&app, root)
    {
        let mut with_host = files;
        with_host.insert(format!("{dir}/app.tl"), source);
        (built, published) = match compile(with_host, &dir) {
            Ok(compiled) => compiled,
            Err(failure) => return failed(&failure),
        };
        hosted = Some(app.app.clone());
        let Some(host) = built.last().map(|package| package.compiled.meta.clone()) else {
            return refused(format!("{dir}: no package"));
        };
        app = host;
    }
    if options.open.is_some() && options.link.is_some() {
        return refused("an app opens a file or a link, never both");
    }
    let opened = match opened_file(options.open) {
        Ok(opened) => opened,
        Err(message) => return refused(message),
    };
    // An app opened with a file or a link starts empty, as a first launch does: the scenario is
    // there for a page that shows what the app holds.
    let scenario = if opened.is_some() || options.link.is_some() {
        None
    } else {
        match options.scenario {
            None => app.scenarios.first().map(|s| s.name.clone()),
            Some(named) => Some(named).filter(|name| !name.is_empty()),
        }
    };
    // The files the packages ship under `media/`, which a view or a scenario names as `pkg:…`:
    // installed before the window is laid out, so their addresses play from their bytes.
    install_assets(&built);
    let compiled = Rc::new(built.iter().map(|b| b.compiled.clone()).collect::<Vec<_>>());
    let packages = Rc::new(Packages::new(compiled, published));
    let staging = Staging {
        scenario: scenario.as_deref(),
        root: options.root.as_deref(),
        viewport: (options.width.unwrap_or(1024), options.height.unwrap_or(720)),
        dark: options.dark,
        link_home: options.link_home,
        open: opened.as_ref().map(|(name, media, bytes)| OpenedFile {
            name: name.as_str(),
            media: media.as_str(),
            bytes: bytes.as_slice(),
        }),
        link: options.link.as_deref(),
    };
    let mut page = match AppPage::open(&app.app, &packages, staging) {
        Ok(page) => page,
        Err(error) => return refused(error),
    };
    let (css, body) = (page.css(), page.render());
    let gaps = json!(page.gaps());
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
        object.insert("gaps".into(), gaps);
        object.insert("hosted".into(), hosted.into());
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
/// `"activate"`, `"toggle"`, `"focus"`, `"increment"`, `"decrement"`, `{"value": text}` or
/// `{"select": {anchor, focus}}`, as the element's `data-a` asks, or `{"resize": {width,
/// height}}`. Answers `{ok, patches}`: in order, each element replaced (`{replace: {id, html}}`),
/// put in (`{insert: {parent, at, html}}`) or taken out (`{remove: {id}}`), by its `data-p`.
#[wasm_bindgen]
pub fn app_act(handle: u32, id: u32, target: Option<String>, action_json: &str) -> String {
    let action = match page_action(action_json) {
        Ok(action) => action,
        Err(error) => return refused(error),
    };
    with_page(handle, move |page| {
        page.act(id, target.as_deref(), action)
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

/// What a page sends for an element's `data-a` ([`PageAction`]): its JSON, or the bare word of
/// an action that carries nothing (`activate`).
fn page_action(action_json: &str) -> Result<PageAction, String> {
    serde_json::from_str(action_json)
        .or_else(|_| serde_json::from_value(Json::String(action_json.to_owned())))
        .map_err(|error| {
            format!(
                "`{action_json}` is no page action: activate, toggle, focus, increment, \
                 decrement, {{value}}, {{select}} or {{resize}} ({error})"
            )
        })
}
