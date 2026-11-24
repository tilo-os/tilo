//! The styles a person picks from (`docs/designs/app-language/shell.md` §6), as the session shows
//! them: the device's daemon lists them, from its package store, as a resource of the person's own
//! workspace ([`tilo_workspace_service::styles`]), and the session follows that resource. What the
//! session adds is its own: whether a style failed in this session's shell, and the module of a
//! style's package, which the shell's runtime loads to draw it.

/// The compiled module of the installed package `app`, from the person's store: what a view of
/// its that the shell composes runs as, when the session's own services keep no records of it.
///
/// A style is a view of another installed package, and no session service runs that
/// package's records — a style is code and nothing else — so the module is read from the
/// store's row the way `installed_shell` reads the shell's (`Bundle::read`). Without it, a
/// style the person chose would wait for a package nothing would ever publish.
#[must_use]
pub fn module_from_store(app: &str) -> Option<std::sync::Arc<[u8]>> {
    // The shell's own package is what the part runs, never one it loads beside itself: the
    // store holds it, and sending it here would load a second copy.
    if app == tilo_session_core::shell::SHELL_APP {
        return None;
    }
    let catalog = package_store::Catalog::from_env().ok()?;
    let installed = app_catalogue::apps::read_one(&catalog, app)
        .ok()
        .flatten()?;
    let bundle = tilolang_tilo::bundle::Bundle::read(&installed.payload_root).ok()?;
    Some(std::sync::Arc::from(bundle.wasm))
}

/// The styles that failed in this session's shell, each with why, as the runtime reported it
/// (`shell.md` §6, "Nothing can lock a person out"): what the session marks on the styles it
/// shows, the device's list otherwise as the daemon publishes it. A store that moved clears them:
/// the shell draws a reinstalled style again to learn whether it is fixed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Failed {
    pub(crate) revision: u64,
    pub(crate) reasons: std::collections::BTreeMap<String, String>,
}

impl Failed {
    /// The styles `offered` with this session's failures marked on them.
    pub(crate) fn marked(
        &self,
        mut offered: Vec<tilo_workspace_service::styles::Style>,
    ) -> Vec<tilo_workspace_service::styles::Style> {
        for style in &mut offered {
            if let Some(reason) = self.reasons.get(&style.style) {
                style.loads = false;
                style.reason.clone_from(reason);
            }
        }
        offered
    }

    /// Record that `style` failed for `reason`. Whether that is news.
    pub(crate) fn fail(&mut self, style: &str, reason: &str) -> bool {
        if self.reasons.get(style).is_some_and(|was| was == reason) {
            return false;
        }
        self.reasons.insert(style.to_owned(), reason.to_owned());
        self.revision = self.revision.saturating_add(1);
        true
    }

    /// The styles `ended` names load again.
    pub(crate) fn recover(&mut self, ended: impl Fn(&str) -> bool) {
        let before = self.reasons.len();
        self.reasons.retain(|style, _| !ended(style));
        if self.reasons.len() != before {
            self.revision = self.revision.saturating_add(1);
        }
    }
}
