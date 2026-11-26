//! The apps each of whose windows shows one item of the workspace's shared library
//! (`document: library.documents`): PDF Viewer, Canvas, Video Editor and the others whose package
//! names a collection of a root as its windows' document. The library stays the workspace's one
//! root, which search, agents and other apps list as a whole; a window is that root opened on one
//! of its items, and another item opens in a window of its own beside it.
//!
//! The item a window shows is the session's to hold, with the window's tab: a file opened in such
//! an app becomes its item through the app's own published action for the file, here, before any
//! window opens, so the window is bound to the item from its first frame, a restart opens it on
//! that item again, and the same file opened twice comes back to the one window.

use std::sync::{Arc, Mutex, PoisonError};

use tilo_actions::remote::{RemoteActionTarget, application_action_target, application_provider};
use tilo_actions::{ActionOutput, ApiError};
use tilo_app_launch::Launch;
use tilolang_tilo::tilolang::PackageMeta;

use super::{Apps, Workspace};

/// The file opens and search hits under way for the apps whose windows each show one item.
#[derive(Default)]
pub(super) struct Items {
    /// What each file's action answered, from the dispatch's own thread, until the session's next
    /// pass opens the item it made: the workspace and app, the file's name, and the answer.
    answered: Arc<Mutex<Vec<FileAnswer>>>,
    /// The place inside an item a window that is still to open is to show, by the item: what a
    /// search hit on a page of a document names, the window being the document's.
    hits: std::cell::RefCell<std::collections::BTreeMap<String, String>>,
}

/// What an app's action for a file answered: the workspace and app it ran in, the file it took,
/// and what it made.
struct FileAnswer {
    workspace: String,
    app: String,
    launch: Launch,
    answer: Result<ActionOutput, String>,
}

/// The package metadata of the installed package `app`, when each of its windows shows one item
/// of a collection of its workspace's root (`PackageMeta::document_items`). An app that is no
/// installed tilolang package declares nothing of the sort.
fn items_package(app: &str) -> Option<PackageMeta> {
    let installed = app_catalogue::apps::read_one_from_env(app).ok()??;
    let module = installed
        .payload_root
        .join(tilolang_tilo::bundle::WASM_FILE);
    let wasm = std::fs::read(&module).ok()?;
    PackageMeta::from_wasm(&wasm).filter(|meta| !meta.document_items.is_empty())
}

/// Whether each window of the installed package `app` shows one item of a collection of its
/// workspace's root, as its package declares.
pub(crate) fn windows_show_items(app: &str) -> bool {
    items_package(app).is_some()
}

/// The file a launch opens, by the hash of its content and its name, when it opens one as content.
fn content_of(launch: &Launch) -> Option<(&str, &str, Option<&str>)> {
    match launch {
        Launch::Content { app, hash, name } => Some((app, hash, name.as_deref())),
        _ => None,
    }
}

/// The action of `meta` that opens the file called `name` and the resource it runs on, as a
/// target in `workspace`, with the arguments it takes the file `hash` in: the action the
/// package declares for that kind of file (`opens: ["application/pdf", .pdf]`), on its root,
/// published as every caller reaches it.
fn file_action(
    meta: &PackageMeta,
    workspace: &str,
    hash: &str,
    name: Option<&str>,
) -> Result<
    (
        RemoteActionTarget,
        serde_json::Map<String, serde_json::Value>,
    ),
    String,
> {
    let media = tilolang_tilo::runtime::blob::media_of(name.unwrap_or_default());
    let opening = name.map_or_else(|| "the file".to_owned(), |name| format!("`{name}`"));
    let action = tilolang_tilo::meta::action_opening(meta, name, &media)
        .ok_or_else(|| format!("{} does not open {opening}", meta.title))?;
    let root = tilolang_tilo::meta::opening_root(meta, action)
        .ok_or_else(|| format!("{} opens {opening} into nothing it keeps", meta.title))?;
    let file = action
        .params
        .iter()
        .find(|param| param.ty == "File")
        .ok_or_else(|| format!("{} takes no file to open {opening}", meta.title))?;
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        file.name.clone(),
        serde_json::json!({"hash": hash, "name": name.unwrap_or(hash)}),
    );
    let target = application_action_target(
        workspace,
        &meta.app,
        tilolang_tilo::naming::resource_id(&meta.app, &format!("root-{}", root.id)),
        tilolang_tilo::meta::published_name(action),
    );
    Ok((target, arguments))
}

/// The item of `app` that `anchor` names, as the app publishes it (`tl:<app>:<root>/<item>`):
/// `None` for the root itself, which is the library every window of the app opens on, and for a
/// resource of another app. A paragraph the anchor names in the item is left out: a window shows
/// the item, and the item is what names the window.
pub(crate) fn item_of(app: &str, anchor: &str) -> Option<String> {
    let (resource, _) = tilolang_tilo::search::split_anchor(anchor);
    let id = match resource.strip_prefix("tl:") {
        Some(published) => {
            let (of, id) = published.split_once(':')?;
            if of != app {
                return None;
            }
            id
        }
        None => resource,
    };
    let (root, item) = id.split_once('/')?;
    (!root.is_empty() && !item.is_empty()).then(|| tilolang_tilo::naming::resource_id(app, id))
}

impl Apps {
    /// Carry out `target`, an app's `open` with `arguments` in `workspace`, when it opens an item
    /// of an app whose windows each show one ([`windows_show_items`]): in the window on that
    /// item, never in place of what another window shows ([`Self::open_item`]). The item is the
    /// resource the `open` is of, or the one its anchor names: a search hit inside a document (a
    /// page, a mark, a line) is an `open` of the document told the place, which its window is
    /// then shown at. `None` for every other action, which goes to the app as it is.
    pub(super) fn opens_item(
        &self,
        target: &RemoteActionTarget,
        arguments: &serde_json::Map<String, serde_json::Value>,
        workspace: &str,
    ) -> Option<Result<(), ApiError>> {
        if target.address.action != tilo_desktop_search::OPEN_ACTION {
            return None;
        }
        let (_, app) = application_provider(&target.provider)?;
        let anchor = arguments
            .get(tilo_desktop_search::OPEN_ANCHOR)
            .and_then(serde_json::Value::as_str)
            .unwrap_or(target.address.resource.as_str());
        let named = item_of(app, anchor);
        let item = item_of(app, &target.address.resource).or_else(|| named.clone())?;
        // A place inside the item: an item under it, or a place in its text.
        let inside = named.is_some_and(|named| {
            named != item || tilolang_tilo::search::split_anchor(anchor).1.is_some()
        });
        windows_show_items(app)
            .then(|| self.open_item(workspace, app, &item, inside.then_some(anchor)))
    }

    /// Show `item` of `app` in `workspace` in the window that shows it, or in a window of its
    /// own: the window already on the item comes forward, and otherwise one opens on it beside the
    /// app's other windows, its tab keeping the item so the layout and the next restart open it
    /// there again. The window's instance runs on the workspace's one root of the app and shows
    /// the item by its id, at `hit` when a search hit named a place inside it.
    ///
    /// # Errors
    /// The window could not be started.
    pub(crate) fn open_item(
        &self,
        workspace: &str,
        app: &str,
        item: &str,
        hit: Option<&str>,
    ) -> Result<(), ApiError> {
        let desktop = self.desktop.borrow().clone();
        if let Some(desktop) = &desktop {
            let raised = desktop.with_policy(|policy| {
                policy
                    .tab_showing(app, item)
                    .filter(|tab| policy.focus_tab(*tab))
            });
            if let Some(tab) = raised {
                desktop.ask();
                if let Some(hit) = hit {
                    self.reveal_in(workspace, app, tab, hit);
                }
                return Ok(());
            }
        }
        if let Some(hit) = hit {
            self.items
                .hits
                .borrow_mut()
                .insert(item.to_owned(), hit.to_owned());
        }
        let beside = desktop
            .as_ref()
            .is_some_and(|desktop| desktop.with_policy(|policy| policy.anchor_for(app).is_some()));
        let launch = Launch::Package {
            app: app.to_owned(),
            open: None,
        };
        self.start_window(
            &launch,
            &self.workspace_named(workspace),
            beside,
            None,
            Some(item),
        )
        .map(|_| ())
    }

    /// What the window opening on `item` is to show first: the place inside it a search hit
    /// named, once ([`Self::open_item`]), else the item itself.
    pub(super) fn shown_first(&self, item: Option<&str>) -> Option<String> {
        let item = item?;
        Some(
            self.items
                .hits
                .borrow_mut()
                .remove(item)
                .unwrap_or_else(|| item.to_owned()),
        )
    }

    /// Show `hit` in the window `tab` of `app` in `workspace`, which already shows the item the
    /// hit is in: its instance is told, in the process that runs it.
    fn reveal_in(&self, workspace: &str, app: &str, tab: u64, hit: &str) {
        let processes = match self.device_processes() {
            Ok(processes) => processes,
            Err(why) => {
                tracing::warn!(app, tab, why, "a window was not told the place a hit names");
                return;
            }
        };
        let (workspace, app, hit) = (workspace.to_owned(), app.to_owned(), hit.to_owned());
        // The daemon answers on its connection: never on the session's loop.
        let telling = std::thread::Builder::new()
            .name("tilolang-reveal".into())
            .spawn(move || match processes.reveal(&workspace, &app, tab, &hit) {
                Ok(true) => {}
                Ok(false) => tracing::warn!(app, tab, "no process runs the window a hit is shown in"),
                Err(error) => tracing::warn!(%error, app, tab, "a window was not told the place a hit names"),
            });
        if let Err(error) = telling {
            tracing::warn!(%error, "a window was not told the place a hit names");
        }
    }

    /// Open the file `launch` opens, when its app's windows each show one item of its library:
    /// the app's own action for the file runs through the dispatch, finding the item those bytes
    /// already are or making one, and the window on that item opens or comes forward once it
    /// answers ([`Self::follow_items`]). `None` for any other launch, which starts as it is.
    ///
    /// # Errors
    /// The app opens no file of this kind, or its action could not be delivered.
    pub(super) fn opens_file_item(
        &self,
        launch: &Launch,
        workspace: &Workspace,
    ) -> Option<Result<(), ApiError>> {
        let meta = items_package(launch.package()?)?;
        // A file of the person's is first kept as content, which the app reads it by.
        #[cfg(target_os = "linux")]
        let launch = match self.hosted_launch(launch, &workspace.id) {
            Ok(launch) => launch,
            Err(error) => return Some(Err(ApiError::Failed(error.to_string()))),
        };
        #[cfg(target_os = "linux")]
        let launch = launch.as_ref();
        let (app, hash, name) = content_of(launch)?;
        let (target, arguments) = match file_action(&meta, &workspace.id, hash, name) {
            Ok(action) => action,
            Err(why) => return Some(Err(ApiError::Failed(why))),
        };
        let answered = Arc::clone(&self.items.answered);
        let (at, of, file) = (workspace.id.clone(), app.to_owned(), launch.clone());
        Some(self.deliver_answered(
            target,
            arguments,
            &workspace.id,
            Some(Box::new(move |answer| {
                answered
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(FileAnswer {
                        workspace: at,
                        app: of,
                        launch: file,
                        answer,
                    });
            })),
        ))
    }

    /// Open the item each file's action made, as its answer came: the window on it comes forward
    /// or opens. A file the action did not take opens in a window of the app as it is, which says
    /// why, rather than nowhere.
    pub(super) fn follow_items(&self) {
        let answered = std::mem::take(
            &mut *self
                .items
                .answered
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        for FileAnswer {
            workspace,
            app,
            launch,
            answer,
        } in answered
        {
            let made = answer.and_then(|output| {
                output
                    .resources
                    .iter()
                    .find_map(|made| item_of(&app, made))
                    .ok_or_else(|| format!("{app} made no item of its library: {output:?}"))
            });
            let opened = match made {
                Ok(item) => self.open_item(&workspace, &app, &item, None),
                Err(why) => {
                    tracing::warn!(
                        app,
                        why,
                        "a file's action made no item; its window says why"
                    );
                    self.start_window(&launch, &self.workspace_named(&workspace), true, None, None)
                        .map(|_| ())
                }
            };
            if let Err(error) = opened {
                tracing::error!(app, %error, "the window on a file's item did not open");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::item_of;

    const PDF: &str = "so.tilo.pdf-viewer";

    #[test]
    fn an_anchor_names_an_item_of_the_library_or_the_library_itself() {
        // An item, as the app publishes it and as its id alone: the same window's item.
        assert_eq!(
            item_of(PDF, "tl:so.tilo.pdf-viewer:lib/doc1").as_deref(),
            Some("tl:so.tilo.pdf-viewer:lib/doc1")
        );
        assert_eq!(
            item_of(PDF, "lib/doc1").as_deref(),
            Some("tl:so.tilo.pdf-viewer:lib/doc1")
        );
        // The library is what every window opens on, and is no item.
        assert_eq!(item_of(PDF, "tl:so.tilo.pdf-viewer:lib"), None);
        // Another app's resource is never one of this app's items.
        assert_eq!(item_of(PDF, "tl:so.tilo.canvas:lib/doc1"), None);
    }
}
