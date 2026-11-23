//! This device's own web UI, as the host operation `web.serve` switches it.
//!
//! The listener is the embedder's: the process that serves browsers registers
//! what starts and stops it ([`Workspaces::serve_web_with`]), and the host
//! operation only asks. A device that serves no web UI answers nothing, and
//! says so.

use super::Workspaces;

/// What starts and stops this device's own web UI.
pub trait WebUi: Send + Sync {
    /// Serve it (`true`) or stop serving it (`false`), now.
    ///
    /// # Errors
    ///
    /// It could not be started or stopped, named with why.
    fn serve(&self, serving: bool) -> Result<(), String>;
}

impl Workspaces {
    /// Let `web` serve this device's web UI, as the host operation
    /// `web.serve` asks for it.
    pub fn serve_web_with(&self, web: std::sync::Arc<dyn WebUi>) {
        *self
            .inner
            .web_ui
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(web);
    }

    /// Serve the device's web UI (`true`) or stop serving it (`false`): what the host
    /// operation `web.serve` asks, and what a server's entry among its person's servers
    /// applies as it changes.
    ///
    /// # Errors
    ///
    /// This device serves no web UI, or the listener would not start or stop.
    pub fn serve_web(&self, serving: bool) -> Result<(), String> {
        let web = self
            .inner
            .web_ui
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .ok_or_else(|| "this device serves no web UI".to_owned())?;
        web.serve(serving)
    }
}
