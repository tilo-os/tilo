//! The hardware compatibility report's sections, as the setup window shows them.

use super::*;
use crate::setup::support_report_view::{self, Placement};

impl SetupApp {
    pub(super) fn hardware_report_sections(&self) -> Vec<Box<dyn Widget>> {
        match &self.support_report {
            Some(report) => support_report_view::sections(report, Placement::ThisComputer),
            None => vec![support_report_view::checking()],
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::setup::support_report_view::tests::fixture_support_report;

    fn has_id(widget: &dyn Widget, id: &str) -> bool {
        widget.test_id() == Some(id) || widget.children().into_iter().any(|child| has_id(child, id))
    }

    tilo_test_env::scenario! {
        fn the_support_report_shows_limitations_and_evidence;

        phase checking_report_stays_visible_until_the_probe_reports;
        {
            let app = SetupApp::for_full_os_check(tilo_ui_base::LocalState::new(None));
            assert!(app.support_report.is_none());
            assert!(
                app.hardware_report_sections()
                    .iter()
                    .any(|widget| has_id(widget.as_ref(), "setup.hardware.checking"))
            );
        }

        phase the_window_draws_the_probe_report;
        {
            let mut app = SetupApp::for_full_os_check(tilo_ui_base::LocalState::new(None));
            app.support_report = Some(fixture_support_report());
            let sections = app.hardware_report_sections();
            for id in [
                "setup.hardware.model",
                "setup.hardware.summary",
                "setup.hardware.fingerprint",
                "setup.hardware.wifi",
                "setup.hardware.camera",
            ] {
                assert!(
                    sections
                        .iter()
                        .any(|widget| has_id(widget.as_ref(), id)),
                    "{id} is drawn"
                );
            }
        }

        phase semantic_projection_carries_the_identity_statuses_and_evidence;
        {
            let mut app = SetupApp::for_full_os_check(tilo_ui_base::LocalState::new(None));
            app.support_report = Some(fixture_support_report());
            let hardware = app.setup_projection().hardware.unwrap();
            assert_eq!(hardware.source, "local-scan");
            assert_eq!(hardware.model.as_deref(), Some("Test Laptop 15"));
            assert_eq!(hardware.variant.as_deref(), Some("TESTBOARD"));
            assert!(hardware.supported >= 4);
            assert_eq!(hardware.limited, 1);
            assert!(!hardware.blocking);
            let graphics = hardware
                .components
                .iter()
                .find(|component| component.kind == "graphics")
                .unwrap();
            assert_eq!(graphics.status, "supported");
            let evidence = serde_json::to_value(&graphics.evidence).unwrap();
            assert!(evidence.to_string().contains("amdgpu"));
            let fingerprint = hardware
                .components
                .iter()
                .find(|component| component.kind == "fingerprint")
                .unwrap();
            assert_eq!(fingerprint.status, "limited");
            assert_eq!(
                serde_json::to_value(fingerprint.remedy).unwrap(),
                serde_json::json!("password-sign-in")
            );
        }
    }
}
