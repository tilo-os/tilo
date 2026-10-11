use super::*;
use crate::Theme;
use crate::record::{RecordCheckout, RecordId, RecordKind};
use crate::state::State;
use crate::view::{FreezeThaw, ViewContribution, ViewSubset};
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use taffy::TaffyTree;
use vello::kurbo::Rect as KurboRect;

/// No focus field — [`ViewSubset`] excludes the caret.
struct ViewRoot {
    scroll: Vec<f64>,
    mode: Option<String>,
    resource: Option<String>,
    rect: KurboRect,
}

impl ViewRoot {
    fn new() -> Self {
        Self {
            scroll: Vec::new(),
            mode: None,
            resource: None,
            rect: KurboRect::ZERO,
        }
    }
}

impl Widget for ViewRoot {
    crate::testing::inert_widget_methods!(apply_layout, draw);
    crate::provides!(
        dyn crate::record::capability::RecordScoped,
        dyn crate::view::capability::ViewState
    );

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        taffy.widget_leaf(taffy::Style::default(), node_map, self)
    }
    fn name(&self) -> &'static str {
        "view-root"
    }
    fn get_rect(&self) -> KurboRect {
        self.rect
    }
}

impl crate::view::capability::ViewState for ViewRoot {
    fn view_state(&self) -> Option<ViewContribution> {
        Some(ViewContribution {
            scroll: self.scroll.clone(),
            view_mode: self.mode.clone(),
        })
    }
    fn restore_view_state(&mut self, view: &ViewContribution) {
        self.scroll = view.scroll.clone();
        self.mode = view.view_mode.clone();
    }
}

impl crate::record::capability::RecordScoped for ViewRoot {
    fn record_intent(&self) -> Option<RecordCheckout> {
        let r = self.resource.as_ref()?;
        RecordId::parse(r).map(|id| RecordCheckout::new(id, RecordKind::new("document")))
    }
}

fn app_with_root(root: ViewRoot) -> App {
    let mut app = App::new(State::new(), Box::new(root));
    app.init_layout();
    app
}

#[test]
fn application_launch_receipts_are_bounded_terminal_and_admitted() {
    // Case `application_launch_receipts_are_terminal_once_and_late_replies_are_inert`.
    {
        use tilo_actions::remote::{InvocationReceipt, ReceiptStatus, RemoteActionMessage};
        let mut app = app_with_root(ViewRoot::new());
        for (request_id, status) in [
            (
                1,
                ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput::default(),
                },
            ),
            (
                2,
                ReceiptStatus::Failed {
                    error: tilo_actions::ActionFailure {
                        code: "missing_hash".into(),
                        message: "launch content is unavailable on this device".into(),
                        details: None,
                        retryable: false,
                    },
                },
            ),
            (
                3,
                ReceiptStatus::Cancelled {
                    message: Some("launcher refused".into()),
                },
            ),
        ] {
            let invocation = tilo_actions::InvocationId::new();
            let launch = tilo_actions::InvocationId::new();
            app.actions
                .pending_application_launches
                .insert(request_id, invocation);
            app.actions
                .broker_requests
                .insert(BrokerReply::Receipt(launch), request_id);
            app.actions.directed_invocations.insert(
                invocation,
                DirectedInvocation {
                    wire_id: request_id + 100,
                    cancellation_requested: false,
                    authorization: None,
                },
            );
            app.apply_collab_event_for_test(CollabEvent::RemoteAction {
                message: RemoteActionMessage::Receipt(InvocationReceipt {
                    invocation: launch,
                    status,
                }),
            });
            assert!(
                !app.actions
                    .pending_application_launches
                    .contains_key(&request_id)
            );
            assert!(!app.actions.directed_invocations.contains_key(&invocation));
            // A duplicate broker receipt cannot emit a second terminal update.
            app.apply_collab_event_for_test(CollabEvent::RemoteAction {
                message: RemoteActionMessage::Receipt(InvocationReceipt {
                    invocation: launch,
                    status: ReceiptStatus::Rejected {
                        error: tilo_actions::remote::ActionHostError::UnknownAction,
                    },
                }),
            });
            assert!(!app.actions.directed_invocations.contains_key(&invocation));
        }
    }

    // Case `disconnected_application_launches_are_bounded_and_failed_once`.
    {
        let mut app = app_with_root(ViewRoot::new());
        let invocation = tilo_actions::InvocationId::new();
        app.actions
            .pending_application_launches
            .insert(1, invocation);
        app.actions.directed_invocations.insert(
            invocation,
            DirectedInvocation {
                wire_id: 9,
                cancellation_requested: false,
                authorization: None,
            },
        );
        assert!(app.poll_directed_invocations());
        assert!(app.actions.pending_application_launches.is_empty());
        assert!(!app.actions.directed_invocations.contains_key(&invocation));
    }

    // Case `application_launch_admission_refuses_a_full_receipt_queue`.
    {
        let mut app = app_with_root(ViewRoot::new());
        for request_id in 0..MAX_PENDING_APPLICATION_LAUNCHES as u64 {
            app.actions
                .pending_application_launches
                .insert(request_id, tilo_actions::InvocationId::new());
        }

        assert!(matches!(
            app.admit_application_launch(
                MAX_PENDING_APPLICATION_LAUNCHES as u64,
                tilo_actions::InvocationId::new(),
            ),
            Err(tilo_actions::DoorError::Unavailable { code, .. })
                if code == "application_launch_queue_full"
        ));
        assert_eq!(
            app.actions.pending_application_launches.len(),
            MAX_PENDING_APPLICATION_LAUNCHES
        );
    }
}

#[test]
fn continuation_restores_framework_view_fidelity_before_custom_semantics() {
    // Case `generic_continuation_restores_view_for_apps_without_custom_metadata`.
    {
        let mut sender = app_with_root(ViewRoot {
            scroll: vec![48.0, 12.0],
            mode: Some("grid".into()),
            resource: Some("record:stable-photo".into()),
            rect: KurboRect::ZERO,
        });
        sender.layout.last_frame_size = Some(winit::dpi::PhysicalSize::new(900, 600));
        let view = <App as FreezeThaw>::capture_view(&sender);
        let metadata = crate::collaboration::ContinuationMetadata {
            schema_version: GENERIC_VIEW_CONTINUATION_SCHEMA,
            bytes: postcard::to_stdvec(&PortableContinuationView::capture(view)).unwrap(),
        };

        // Model launcher delivery before first layout, which is the real
        // continuation order. The view waits until focus/layout nodes exist.
        let mut receiver = App::new(State::new(), Box::new(ViewRoot::new()));
        receiver.continuation.pending_restore = Some(metadata);
        receiver.apply_pending_continuation_restore();
        assert!(receiver.continuation.pending_view.is_some());
        receiver.init_layout();

        let restored = <App as FreezeThaw>::capture_view(&receiver);
        assert_eq!(restored.scroll, vec![48.0, 12.0]);
        assert_eq!(restored.view_mode.as_deref(), Some("grid"));
        assert!(receiver.continuation.pending_view.is_none());
    }

    // Case `generic_continuation_never_serializes_path_or_widget_ephemeral_state`.
    {
        let portable = PortableContinuationView::capture(crate::view::View {
            scroll: vec![1.0],
            focus: Some(crate::view::FocusView {
                ordinal: 2,
                ephemeral: "/home/alice/private-token".into(),
            }),
            open_resource: Some("file:/home/alice/private.txt".into()),
            viewport: crate::view::ViewRect::default(),
            view_mode: Some("grid".into()),
        });
        let bytes = postcard::to_stdvec(&portable).unwrap();
        assert!(!bytes.windows(5).any(|window| window == b"/home"));
        let restored = portable.into_view();
        assert_eq!(restored.open_resource, None);
        assert_eq!(restored.focus.unwrap().ephemeral, "");
    }

    // Case `custom_semantics_cannot_replace_framework_view_fidelity`.
    {
        let view = crate::view::View {
            scroll: vec![81.0, 13.0],
            focus: Some(crate::view::FocusView {
                ordinal: 3,
                ephemeral: "private-editor-state".into(),
            }),
            open_resource: Some("file:/private/document".into()),
            viewport: crate::view::ViewRect {
                x: 1.0,
                y: 2.0,
                width: 1280.0,
                height: 720.0,
            },
            view_mode: Some("reader".into()),
        };
        let metadata = compose_structural_continuation_metadata(
            view,
            Some(crate::collaboration::ContinuationMetadata {
                schema_version: 7,
                bytes: b"custom-safe-projection".to_vec(),
            }),
        )
        .expect("framework envelope");

        assert_eq!(metadata.schema_version, STRUCTURAL_CONTINUATION_SCHEMA);
        let facets = structural_continuation_facets(&metadata.bytes).expect("valid facets");
        assert!(facets.presentation());
        assert!(facets.semantic());
        let envelope = postcard::from_bytes::<StructuralContinuationEnvelope>(&metadata.bytes)
            .expect("structural envelope");
        assert_eq!(envelope.presentation.scroll, vec![81.0, 13.0]);
        assert_eq!(envelope.presentation.focus_ordinal, Some(3));
        assert_eq!(envelope.presentation.view_mode.as_deref(), Some("reader"));
        assert!(!metadata.bytes.windows(7).any(|window| window == b"private"));
    }

    // Case `unsafe_custom_semantics_are_stripped_without_losing_the_view`.
    {
        let metadata = compose_structural_continuation_metadata(
            crate::view::View {
                scroll: vec![42.0],
                focus: None,
                open_resource: None,
                viewport: crate::view::ViewRect::default(),
                view_mode: Some("grid".into()),
            },
            Some(crate::collaboration::ContinuationMetadata {
                schema_version: 1,
                bytes: b"opaque-unknown-payload".to_vec(),
            }),
        )
        .unwrap();
        let sanitized = sanitize_structural_continuation_metadata(&metadata.bytes, |_, _| false)
            .expect("valid presentation survives");
        let facets = structural_continuation_facets(&sanitized).unwrap();
        assert!(facets.presentation());
        assert!(!facets.semantic());
        let envelope = postcard::from_bytes::<StructuralContinuationEnvelope>(&sanitized).unwrap();
        assert_eq!(envelope.presentation.scroll, vec![42.0]);
        assert_eq!(envelope.presentation.view_mode.as_deref(), Some("grid"));
        assert!(!sanitized.windows(6).any(|window| window == b"opaque"));
    }

    // Case `oversized_custom_semantics_cannot_suppress_framework_view_fidelity`.
    {
        let metadata = compose_structural_continuation_metadata(
            crate::view::View {
                scroll: vec![7.0],
                focus: None,
                open_resource: None,
                viewport: crate::view::ViewRect::default(),
                view_mode: Some("list".into()),
            },
            Some(crate::collaboration::ContinuationMetadata {
                schema_version: 1,
                bytes: vec![b'x'; 32 * 1024],
            }),
        )
        .expect("presentation-only fallback");
        let facets = structural_continuation_facets(&metadata.bytes).unwrap();
        assert!(facets.presentation());
        assert!(!facets.semantic());
    }

    // Case `structural_continuation_restores_framework_view_before_layout`.
    {
        let metadata = compose_structural_continuation_metadata(
            crate::view::View {
                scroll: vec![64.0],
                focus: None,
                open_resource: None,
                viewport: crate::view::ViewRect::default(),
                view_mode: Some("list".into()),
            },
            None,
        )
        .unwrap();
        let mut receiver = App::new(State::new(), Box::new(ViewRoot::new()));
        receiver.continuation.pending_restore = Some(metadata);
        receiver.apply_pending_continuation_restore();
        assert!(receiver.continuation.pending_view.is_some());
        receiver.init_layout();
        let restored = <App as FreezeThaw>::capture_view(&receiver);
        assert_eq!(restored.scroll, vec![64.0]);
        assert_eq!(restored.view_mode.as_deref(), Some("list"));
    }
}

#[test]
fn view_presence_broadcasts_and_follow_binds_the_local_viewport() {
    // Case `view_subset_broadcasts_through_real_presence_path`:
    // Asserted on both the frame that leaves the sender (via the broadcast
    // sink) and the frame the receiver applies.
    {
        #[cfg(windows)]
        let file_record = r"file:C:\report.md";
        #[cfg(not(windows))]
        let file_record = "file:/report.md";
        let mut sender = app_with_root(ViewRoot {
            scroll: vec![10.0, 20.0],
            mode: Some("grid".into()),
            resource: Some(file_record.into()),
            rect: KurboRect::ZERO,
        });
        sender.layout.last_frame_size = Some(winit::dpi::PhysicalSize::new(1024, 768));

        let captured: Arc<Mutex<Vec<(PresenceKey, PresencePayload)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&captured);
        sender.set_presence_broadcast_sink(move |k, p| {
            sink.lock().unwrap().push((k.clone(), p.clone()));
        });

        sender.broadcast_view_presence();

        let frames = captured.lock().unwrap().clone();
        assert_eq!(frames.len(), 1, "exactly one View frame is broadcast");
        let (key, _payload) = &frames[0];
        let sent = key
            .as_view()
            .expect("the broadcast key carries a decodable View subset");
        assert_eq!(sent.scroll, vec![10.0, 20.0], "scroll rode the frame");
        assert_eq!(
            sent.open_resource.as_deref(),
            Some(file_record),
            "the open resource rode the frame"
        );
        assert_eq!(
            sent.view_mode.as_deref(),
            Some("grid"),
            "view mode rode the frame"
        );
        assert_eq!(sent.viewport.width, 1024.0, "viewport width rode the frame");
        assert_eq!(
            sent.viewport.height, 768.0,
            "viewport height rode the frame"
        );

        let mut receiver = app_with_root(ViewRoot::new());
        assert!(
            receiver
                .presence_ctx()
                .peer_view(&crate::permissions::UserId("alice".into()))
                .is_none(),
            "no View known for the peer before their broadcast arrives"
        );
        receiver.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
            user_id: "alice".into(),
            key: key.clone(),
            payload: PresencePayload::Arrived,
        });

        let peer_view = receiver
            .presence_ctx()
            .peer_view(&crate::permissions::UserId("alice".into()))
            .expect("the peer's View is stored after their broadcast");
        assert_eq!(
            peer_view, &sent,
            "the receiver stores the peer's exact View subset"
        );

        // The inert carrier payload must not be processed as a real Arrived.
        assert!(
            receiver.presence_ctx().remote_cursors.is_empty(),
            "a View frame must not register a phantom cursor"
        );
    }

    // Case `follow_binds_local_viewport_and_unfollow_releases`.
    {
        fn deliver(app: &mut App, peer: &str, scroll: Vec<f64>, mode: &str) {
            let subset = ViewSubset {
                scroll,
                open_resource: None,
                viewport: crate::view::ViewRect::default(),
                view_mode: Some(mode.into()),
            };
            app.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
                user_id: peer.into(),
                key: PresenceKey::view(&subset),
                payload: PresencePayload::Arrived,
            });
        }

        let mut app = app_with_root(ViewRoot::new());

        deliver(&mut app, "bob", vec![100.0], "list");
        assert_eq!(
            <App as FreezeThaw>::capture_view(&app).scroll,
            Vec::<f64>::new(),
            "receiving a peer's View must not move the local view on its own"
        );

        app.follow("bob");
        let v = <App as FreezeThaw>::capture_view(&app);
        assert_eq!(
            v.scroll,
            vec![100.0],
            "follow moved the local view to the peer's scroll"
        );
        assert_eq!(
            v.view_mode.as_deref(),
            Some("list"),
            "follow adopted the peer's view mode"
        );

        deliver(&mut app, "bob", vec![250.0], "board");
        assert_eq!(
            <App as FreezeThaw>::capture_view(&app).scroll,
            vec![250.0],
            "a followed peer's later View keeps moving the local view"
        );

        app.unfollow();
        deliver(&mut app, "bob", vec![999.0], "grid");
        assert_eq!(
            <App as FreezeThaw>::capture_view(&app).scroll,
            vec![250.0],
            "after unfollow the local view is released and stops tracking"
        );
    }

    // Case `following_absent_peer_is_a_noop`.
    {
        let mut app = app_with_root(ViewRoot {
            scroll: vec![7.0],
            mode: Some("grid".into()),
            resource: None,
            rect: KurboRect::ZERO,
        });

        app.follow("ghost");

        let v = <App as FreezeThaw>::capture_view(&app);
        assert_eq!(
            v.scroll,
            vec![7.0],
            "following an absent peer leaves the local view untouched"
        );
        assert_eq!(
            app.followed_peer(),
            Some("ghost"),
            "the follow intent is still latched"
        );
    }
}
