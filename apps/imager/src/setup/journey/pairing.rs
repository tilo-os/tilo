//! Existing-device pairing presentation; custody and helper execution stay in the backend.
use super::super::{prepared_identity::PreparedIdentity, strings::t};
use std::sync::Arc;
use ui::{
    TextRole,
    widget::{Button, Text, Widget},
};

#[derive(Default)]
pub(super) struct Pairing {
    actor: imager_backend::pairing::Pairing,
}
impl Pairing {
    pub(super) fn resume(prepared: Arc<PreparedIdentity>) -> std::io::Result<Self> {
        Ok(Self {
            actor: imager_backend::pairing::Pairing::resume(prepared)?,
        })
    }
    pub fn busy(&self) -> bool {
        self.actor.busy()
    }
    pub fn ready(&self) -> bool {
        self.actor.ready()
    }
    pub fn locked_label(&self) -> bool {
        self.actor.locked_label()
    }
    pub fn started(&self) -> bool {
        self.actor.started()
    }
    pub fn prepared(&self) -> Option<Arc<PreparedIdentity>> {
        self.actor.prepared()
    }
    pub fn start(&mut self, label: &str) -> Result<(), &'static str> {
        if !imager_backend::pairing::Pairing::supported() {
            return Err("installer-pairing-helper-unavailable");
        }
        let lease = tilo_relay_protocol::ProcessLease::capture(std::process::id())
            .map_err(|_| "installer-pairing-failed")?;
        self.actor.start(label, lease.pid(), lease.start_time())
    }
    pub fn cancel(&mut self) -> Result<(), &'static str> {
        self.actor.cancel()
    }
    pub fn poll(&mut self) -> bool {
        self.actor.poll()
    }
    pub fn projection(&self) -> serde_json::Value {
        self.actor.projection()
    }
    #[cfg(all(test, target_os = "linux"))]
    pub(super) fn fixture_operation_directory(&self) -> std::path::PathBuf {
        self.actor.fixture_operation_directory()
    }
    pub fn intro(&self) -> String {
        t(if self.actor.code().is_some() && self.busy() {
            "installer-pairing-code-intro"
        } else if self.busy() {
            "installer-pairing-code-pending"
        } else if self.ready() {
            "installer-pairing-body"
        } else {
            "installer-pairing-name-body"
        })
    }
    pub fn view(
        &self,
        start: std::rc::Rc<dyn Fn()>,
        cancel: std::rc::Rc<dyn Fn()>,
    ) -> Box<dyn Widget> {
        let mut body = crate::page::stack();
        if self.ready() {
            return Box::new(body.push(Box::new(Text::new(&t("installer-existing-connected")))));
        }
        if let Some(code) = self.actor.code() {
            body = body.push(Box::new(
                Text::new(code)
                    .with_role(TextRole::LargeTitle)
                    .with_id("installer.pairing.code"),
            ));
        }
        if self.busy() {
            body = body.push(Box::new(
                Button::new("installer.pairing.cancel", &t("installer-pairing-cancel"))
                    .on_click(move || cancel()),
            ));
        } else {
            if self.actor.state() == "failed" || self.actor.state() == "cancelled" {
                let notice = Text::new(&t(if self.actor.state() == "cancelled" {
                    "installer-pairing-cancelled"
                } else {
                    "installer-pairing-failed"
                }));
                body = body.push(Box::new(if self.actor.state() == "failed" {
                    notice
                        .with_id("installer.pairing.failure")
                        .with_live(ui::a11y::Politeness::Assertive)
                } else {
                    notice
                }));
            }
            body = body.push(Box::new(
                Button::new("installer.pairing.start", &t("installer-pairing-start"))
                    .on_click(move || start()),
            ));
        }
        Box::new(body)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[test]
    fn forged_complete_status_never_becomes_enrolled_custody() {
        let (_temporary, actor) = imager_backend::pairing::failed_fixture();
        let mut pairing = Pairing { actor };
        let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
        while pairing.busy() {
            assert!(std::time::Instant::now() < deadline);
            pairing.poll();
            std::thread::yield_now();
        }
        assert_eq!(pairing.projection()["state"], "failed");
        assert!(!pairing.ready());
        let view = pairing.view(std::rc::Rc::new(|| {}), std::rc::Rc::new(|| {}));
        let notice =
            super::super::visual_tests::control(view.as_ref(), "installer.pairing.failure")
                .unwrap()
                .accessibility()
                .unwrap();
        assert_eq!(
            notice.name.as_deref(),
            Some(t("installer-pairing-failed").as_str())
        );
        assert_eq!(notice.live, Some(ui::a11y::Politeness::Assertive));
    }
}

#[cfg(all(test, target_os = "linux"))]
pub(super) fn certified_fixture() -> (tempfile::TempDir, Pairing) {
    let (directory, actor) = imager_backend::pairing::certified_fixture();
    (directory, Pairing { actor })
}
#[cfg(all(test, target_os = "linux"))]
pub(super) fn waiting_fixture() -> (tempfile::TempDir, Pairing) {
    let (directory, actor) = imager_backend::pairing::waiting_fixture();
    (directory, Pairing { actor })
}
