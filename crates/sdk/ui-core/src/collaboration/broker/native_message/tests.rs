//! SDK memory/correlation tests, not a replacement for native owner acceptance.
use super::*;
use crate::invocation_scope::{
    NativeMessageStreamLifetime,
    native_message::tests::{RecordFixture, delivered_source, source_channel},
};

#[tokio::test(flavor = "current_thread")]
async fn staged_actual_executor_contribution_is_charged_and_duplicate_is_refused() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let lifetime = NativeMessageStreamLifetime::new();
            let fixture = RecordFixture::new(delivered_source(&lifetime).await);
            fixture.apply(false).unwrap();
            let contribution = fixture.capture.finish().unwrap();
            let budget = NativeDocumentBudget::default();
            let route = super::super::SdkRoute {
                spec: contribution.source().route().clone(),
                channel: source_channel().await,
                presence: None,
                workspace_controls: Default::default(),
                committed: true,
                encryption: Arc::new(Mutex::new(
                    crate::collaboration::route_keys::RouteKeys::plain(),
                )),
                native_documents: NativeDocumentQueue::new(budget.clone()),
            };
            let mut queue = NativeDocumentQueue::new(budget.clone());
            queue.stage(&route, contribution.clone()).unwrap();
            let charged = *budget.0.lock().unwrap();
            assert!(charged >= contribution.retained_bytes().unwrap());
            assert!(queue.stage(&route, contribution.clone()).is_err());
            assert_eq!(*budget.0.lock().unwrap(), charged);
            queue.observed_terminal(7, Some(contribution.source().request().invocation));
            assert!(
                queue.claimed(),
                "unsaved input cannot be released by terminal acknowledgment"
            );
            assert_eq!(*budget.0.lock().unwrap(), charged);
            drop(queue);
            assert_eq!(*budget.0.lock().unwrap(), 0);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn full_shared_budget_refuses_before_staging_and_retirement_stops_fresh_stage() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let lifetime = NativeMessageStreamLifetime::new();
            let fixture = RecordFixture::new(delivered_source(&lifetime).await);
            fixture.apply(false).unwrap();
            let contribution = fixture.capture.finish().unwrap();
            let budget = NativeDocumentBudget::default();
            let route = super::super::SdkRoute {
                spec: contribution.source().route().clone(),
                channel: source_channel().await,
                presence: None,
                workspace_controls: Default::default(),
                committed: true,
                encryption: Arc::new(Mutex::new(
                    crate::collaboration::route_keys::RouteKeys::plain(),
                )),
                native_documents: NativeDocumentQueue::new(budget.clone()),
            };
            let mut other = NativeDocumentQueue::new(budget.clone());
            other.resize_charge(MAX_UPLOAD_BYTES).unwrap();
            let mut queue = NativeDocumentQueue::new(budget.clone());
            assert!(queue.stage(&route, contribution.clone()).is_err());
            assert!(!queue.claimed());
            assert_eq!(*budget.0.lock().unwrap(), MAX_UPLOAD_BYTES);
            drop(other);
            drop(lifetime);
            assert!(queue.stage(&route, contribution).is_err());
            assert!(!queue.claimed());
            assert_eq!(*budget.0.lock().unwrap(), 0);
        })
        .await;
}
