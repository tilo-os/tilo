//! The styles this device's installed packages offer, published in its person's own workspace
//! ([`STYLES_APP`]): read from the device's package store when the daemon starts, and again each
//! time the store's registry moves, so a style a person or their agent installs (or takes away)
//! reaches every reader following the resource without a restart (`shell.md` §6).

use std::path::PathBuf;
use std::sync::Arc;

use tilo_workspace_service::styles::{STYLES_APP, Styles};

use super::workspaces::ServiceWorkspaces;

/// The styles this daemon publishes, read again while it keeps this: dropping it stops the watch
/// of the store.
pub(crate) struct StylesShown {
    _watch: Option<tilo_files::native::ChangeWatch>,
}

/// Publish the styles of the store the person's apps are installed in (`$TILO_APPS_DATA_DIR`,
/// else the person's own store) in their own workspace, from now on.
pub(crate) fn publish(workspaces: &Arc<ServiceWorkspaces>) -> StylesShown {
    let styles = Styles::default();
    workspaces.copies().publish_own(
        STYLES_APP,
        tilo_workspace_service::workspaces::OwnReaders::Workspace,
        Arc::new(styles.clone()),
    );
    let catalog = match package_store::Catalog::from_env() {
        Ok(catalog) => catalog,
        Err(error) => {
            // No store: the styles of the shell this build carries are what the device offers.
            tracing::warn!(%error, "this device keeps no app store; its shell's own styles stand");
            read_again(None, &styles);
            return StylesShown { _watch: None };
        }
    };
    let registry = catalog.user().registry();
    read_again(Some(catalog.clone()), &styles);
    let watch = registry.parent().map(PathBuf::from).and_then(|root| {
        // A store nothing was installed in yet has no directory: it is watched from the start,
        // so the first install is read like every other.
        if let Err(error) = std::fs::create_dir_all(&root) {
            tracing::warn!(%error, root = %root.display(), "the app store is not watched");
            return None;
        }
        let styles = styles.clone();
        let named = registry.clone();
        tilo_files::native::watch_changes(
            &root,
            move |path| path == named,
            move || read_again(Some(catalog.clone()), &styles),
        )
        .map_err(|error| {
            tracing::warn!(%error, root = %root.display(), "the app store is not watched");
        })
        .ok()
    });
    StylesShown { _watch: watch }
}

/// Read the styles `catalog` offers on a blocking thread, and offer them in `styles`. One read at
/// a time: the last to start reads the store as the last change left it, and offers last.
fn read_again(catalog: Option<package_store::Catalog>, styles: &Styles) {
    static READING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let styles = styles.clone();
    tilo_rt::app().spawn_blocking(move || {
        let _one = READING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let apps: Vec<(String, PathBuf)> = match catalog.as_ref().map(app_catalogue::apps::read) {
            Some(Ok(apps)) => apps
                .into_iter()
                .map(|app| (app.app_id.to_string(), app.payload_root))
                .collect(),
            Some(Err(error)) => {
                // A registry that does not read keeps the styles it last offered: a person never
                // sees their switch emptied by a store halfway through a write.
                tracing::warn!(%error, "the app store's registry does not read; its styles wait");
                return;
            }
            None => Vec::new(),
        };
        styles.offer(tilo_workspace_service::styles::installed(&apps));
    });
}
