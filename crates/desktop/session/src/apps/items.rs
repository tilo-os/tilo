//! The apps each of whose windows shows one item of the workspace's shared library
//! (`document: library.documents`): PDF Viewer, Canvas, Video Editor and the others whose package
//! names a collection of a root as its windows' document. The library stays the workspace's one
//! root, which search, agents and other apps list as a whole; a window is that root opened on one
//! of its items, and another item opens in a window of its own beside it.

use tilo_actions::ApiError;
use tilo_actions::remote::{RemoteActionTarget, application_provider};
use tilo_app_launch::Launch;

use super::Apps;

/// Whether each window of the installed package `app` shows one item of a collection of its
/// workspace's root, as its package declares (`PackageMeta::document_items`). An app that is no
/// installed tilolang package declares nothing of the sort.
pub(crate) fn windows_show_items(app: &str) -> bool {
    let Ok(Some(installed)) = app_catalogue::apps::read_one_from_env(app) else {
        return false;
    };
    let module = installed
        .payload_root
        .join(tilolang_tilo::bundle::WASM_FILE);
    let Ok(wasm) = std::fs::read(&module) else {
        return false;
    };
    tilolang_tilo::tilolang::PackageMeta::from_wasm(&wasm)
        .is_some_and(|meta| !meta.document_items.is_empty())
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
    (!root.is_empty() && !item.is_empty())
        .then(|| tilolang_tilo::publish::resource_id(app, id))
}

impl Apps {
    /// Carry out `target`, an app's `open` with `arguments` in `workspace`, when it opens an item
    /// of an app whose windows each show one ([`windows_show_items`]): in the window on that
    /// item, never in place of what another window shows ([`Self::open_item`]). `None` for every
    /// other action, which goes to the app as it is.
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
        let item = item_of(app, anchor)?;
        windows_show_items(app).then(|| self.open_item(workspace, app, &item))
    }

    /// Show `item` of `app` in `workspace` in the window that shows it, or in a window of its
    /// own: the window already on the item comes forward, and otherwise one opens on it beside the
    /// app's other windows, its tab keeping the item so the layout and the next restart open it
    /// there again. The window's instance runs on the workspace's one root of the app and shows
    /// the item by its id.
    ///
    /// # Errors
    /// The window could not be started.
    pub(crate) fn open_item(&self, workspace: &str, app: &str, item: &str) -> Result<(), ApiError> {
        let desktop = self.desktop.borrow().clone();
        if let Some(desktop) = &desktop {
            let raised = desktop.with_policy(|policy| {
                policy
                    .tab_showing(app, item)
                    .is_some_and(|tab| policy.focus_tab(tab))
            });
            if raised {
                desktop.ask();
                return Ok(());
            }
        }
        let beside = desktop.as_ref().is_some_and(|desktop| {
            desktop.with_policy(|policy| policy.anchor_for(app).is_some())
        });
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
