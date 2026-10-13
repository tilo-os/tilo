//! Native runtime transaction admission; package authority stays on the worker.
use super::{Admitted, Finish, Job, StoreApp};
use crate::runtime_recovery::{Phase, Recovery, RuntimeTarget};
use crate::worker::Request;

impl StoreApp {
    pub(super) fn runtime_row(&self) -> Option<ui::framework::LiveRow> {
        let recovery = self.runtime_recovery.as_ref()?;
        let mut row = ui::framework::LiveRow::new(
            "store:runtime-repair",
            "store.runtime-repair",
            format!("{} — {}", recovery.target.app, recovery.target.runtime),
        )
        .effect(&crate::resource::StoreEffect::InspectRuntime)
        .effect(&crate::resource::StoreEffect::ShowRuntime)
        .effect(&crate::resource::StoreEffect::BackRuntime);
        match &recovery.phase {
            Phase::Review(review) => {
                if let Ok(review) = serde_json::to_string(review) {
                    row = row.effect(&crate::resource::StoreEffect::AcquireRuntime { review });
                }
            }
            Phase::Failed(_) => row = row.effect(&crate::resource::StoreEffect::RetryRuntime),
            _ => {}
        }
        Some(row)
    }

    pub(super) fn runtime_visible(&self) -> bool {
        self.runtime_recovery
            .as_ref()
            .is_some_and(|repair| repair.visible)
    }

    pub(super) fn review_runtime(&mut self, target: RuntimeTarget) -> Result<Admitted, String> {
        self.reserve_job()?;
        if let Some(recovery) = &mut self.runtime_recovery
            && recovery.target == target
            && recovery.busy()
        {
            recovery.visible = true;
            self.structure_dirty = true;
            return Ok(Admitted::Done);
        }
        if self.runtime_recovery.as_ref().is_some_and(Recovery::busy) {
            return Err(ui::i18n!("store-runtime-busy"));
        }
        self.runtime_recovery = Some(Recovery {
            target: target.clone(),
            phase: Phase::Inspecting,
            visible: true,
        });
        self.status_line = ui::i18n!("store-runtime-inspecting");
        self.operation_error = None;
        self.structure_dirty = true;
        Ok(Job::admitted(
            Request::ReviewRuntime(target),
            Finish::RuntimeReview,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::StoreEffect;
    use app_host::runtime_transaction::{RuntimeOffer, RuntimeReview};

    pub(crate) fn review_fixture() -> RuntimeReview {
        RuntimeReview {
            consumer_app: "so.tilo.browser".into(),
            consumer_root: "ab".repeat(32),
            runtime: "tilo-runtime/2026.10.9-chromium".into(),
            offer: Some(RuntimeOffer {
                registry: "Tilo".into(),
                package_id: "so.tilo.runtime.chromium".into(),
                version: "0.1.1".into(),
                root: "cd".repeat(32),
                publisher: "ef".repeat(32),
                archive_hash: "01".repeat(32),
                size_bytes: 42,
            }),
        }
    }

    #[test]
    fn runtime_actions_require_review_consent_and_retry_never_acquires() {
        let mut app = StoreApp::new();
        assert!(
            app.admit(StoreEffect::AcquireRuntime {
                review: "{}".into()
            })
            .is_err()
        );
        assert!(
            app.admit(StoreEffect::ReviewRuntime {
                app: "so.tilo.browser".into(),
                runtime: "tilo-runtime/v/extra".into(),
            })
            .is_err()
        );
        let effect = || StoreEffect::ReviewRuntime {
            app: "so.tilo.browser".into(),
            runtime: "tilo-runtime/2026.10.9-chromium".into(),
        };
        let Admitted::Job(job) = app.admit(effect()).unwrap() else {
            panic!("inspection job")
        };
        assert!(matches!(job.request, Request::ReviewRuntime(_)));
        assert_eq!(job.finish, Finish::RuntimeReview);
        let review = review_fixture();
        app.runtime_recovery
            .as_mut()
            .unwrap()
            .reviewed(Ok(review.clone()));
        let mut changed = review.clone();
        changed.offer.as_mut().unwrap().size_bytes += 1;
        assert!(
            app.admit(StoreEffect::AcquireRuntime {
                review: serde_json::to_string(&changed).unwrap(),
            })
            .is_err()
        );
        let consent = StoreEffect::AcquireRuntime {
            review: serde_json::to_string(&review).unwrap(),
        };
        let Admitted::Job(job) = app.admit(consent.clone()).unwrap() else {
            panic!("acquisition job")
        };
        let Request::AcquireRuntime(actual) = job.request else {
            panic!("exact acquisition")
        };
        assert_eq!(*actual, review);
        assert_eq!(job.finish, Finish::RuntimeAcquire);
        assert!(
            app.admit(consent).is_err(),
            "duplicate consent must not enqueue acquisition"
        );
        app.admit(StoreEffect::BackRuntime).unwrap();
        assert!(!app.runtime_visible());
        assert!(
            app.runtime_recovery.as_ref().unwrap().busy(),
            "Back is not cancellation"
        );
        app.admit(effect()).unwrap();
        assert!(
            app.runtime_visible(),
            "the handoff returns to the existing running repair"
        );
        app.runtime_recovery
            .as_mut()
            .unwrap()
            .acquired(Err("archive hash mismatch".into()));
        let Admitted::Job(retry) = app.admit(StoreEffect::RetryRuntime).unwrap() else {
            panic!("retry inspection")
        };
        assert!(
            matches!(retry.request, Request::ReviewRuntime(_)),
            "retry requires fresh review and consent"
        );
        assert_eq!(retry.finish, Finish::RuntimeReview);
        let mut present = review;
        present.offer = None;
        app.runtime_recovery.as_mut().unwrap().reviewed(Ok(present));
        assert!(matches!(
            app.runtime_recovery.as_ref().unwrap().phase,
            Phase::Complete(_)
        ));
        assert!(
            app.admit(StoreEffect::AcquireRuntime {
                review: "{}".into()
            })
            .is_err()
        );
    }
}
