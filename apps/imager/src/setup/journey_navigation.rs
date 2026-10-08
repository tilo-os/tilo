//! Fixed, portal-mediated completion destinations. Never imports credentials.
pub const ACCOUNTS: &str = "settings://accounts";
pub const STORE: &str = "tilo-store:catalogue";
pub struct Navigation {
    pub opener: fn(&str) -> ui::portal::LaunchChannel,
    pending: Option<ui::portal::LaunchChannel>,
}
impl Default for Navigation {
    fn default() -> Self {
        Self {
            opener: ui::portal::send_open_uri,
            pending: None,
        }
    }
}
impl Navigation {
    pub fn available(&self) -> bool {
        self.pending.is_none()
    }
    pub fn request(&mut self, uri: &'static str) -> Result<(), String> {
        if !self.available() {
            return Err(super::strings::t("imager-journey-navigation-pending"));
        }
        self.pending = Some((self.opener)(uri));
        Ok(())
    }
    pub fn poll(&mut self) -> Option<Result<(), String>> {
        use std::sync::mpsc::TryRecvError;
        let outcome = match self.pending.as_ref()?.try_recv() {
            Ok(ui::portal::LaunchOutcome::Opened) => Ok(()),
            Ok(ui::portal::LaunchOutcome::Failed(error)) => Err(error),
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                Err(super::strings::t("imager-journey-navigation-lost"))
            }
        };
        self.pending = None;
        Some(outcome)
    }
}
