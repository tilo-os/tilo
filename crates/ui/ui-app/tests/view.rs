//! Freezing an app carries its document and its view: scroll, caret, open resource and view
//! mode all round-trip, and bytes that are truncated, garbage or a future version are refused
//! without a panic.
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, transaction::Transactable};
use std::collections::HashMap;
use taffy::TaffyTree;
use tilo_ui::App;
use tilo_ui::resource::{DocumentCheckout, DocumentId, ResourceType};
use tilo_ui::state::State;
use tilo_ui::view::{FROZEN_VERSION, FreezeThaw, FrozenApp, ThawError, View, ViewContribution};
use tilo_ui_base::Theme;
use tilo_ui_base::widget::TaffyTreeExt;
use tilo_ui_base::widget::Widget;
use vello::kurbo::Rect as KurboRect;

struct DocRoot {
    caret: String,
    scroll: Vec<f64>,
    mode: Option<String>,
    resource: String,
    focused: bool,
    rect: KurboRect,
}

impl DocRoot {
    fn new() -> Self {
        Self {
            caret: String::new(),
            scroll: Vec::new(),
            mode: None,
            resource: "file:/notes.md".into(),
            focused: false,
            rect: KurboRect::ZERO,
        }
    }
}

impl Widget for DocRoot {
    tilo_ui_base::provides!(
        dyn tilo_ui::resource::capability::ResourceScoped,
        dyn tilo_ui::view::capability::ViewState,
        dyn tilo_ui_base::widget::undo_points::UndoPoints
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
        "doc-root"
    }
    fn get_rect(&self) -> KurboRect {
        self.rect
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }
    fn is_focused(&self) -> bool {
        self.focused
    }
}

impl tilo_ui_base::widget::undo_points::UndoPoints for DocRoot {
    fn get_state(&self) -> Option<String> {
        Some(self.caret.clone())
    }
    fn set_state(&mut self, state: &str) {
        self.caret = state.to_string();
    }
}

impl tilo_ui::view::capability::ViewState for DocRoot {
    fn view_state(&self) -> Option<ViewContribution> {
        Some(ViewContribution {
            scroll: self.scroll.clone(),
            view_mode: self.mode.clone(),
        })
    }
    fn restore_view_state(&mut self, view: &ViewContribution) {
        self.scroll.clone_from(&view.scroll);
        self.mode.clone_from(&view.view_mode);
    }
}

impl tilo_ui::resource::capability::ResourceScoped for DocRoot {
    fn record_intent(&self) -> Option<DocumentCheckout> {
        DocumentId::parse(&self.resource)
            .map(|id| DocumentCheckout::new(id, ResourceType::new("document")))
    }
}

fn text_at(doc: &AutoCommit, key: &str) -> Option<String> {
    let (_, obj) = doc.get(&ROOT, key).ok()??;
    doc.text(&obj).ok()
}

#[test]
fn freeze_and_thaw_round_trip_doc_and_view_and_reject_bad_bytes() {
    // Case `freeze_thaw_round_trips_doc_and_view`: A populated doc + a non-default
    // View (scrolled, a caret, an open resource, a view mode) freezes and thaws into
    // a *fresh* app with BOTH the document content AND every view field intact —
    // asserted on the real freeze/thaw path, and on the bytes in between so no field
    // is silently dropped by capture.
    {
        #[cfg(windows)]
        let file_resource = r"file:C:\report.md";
        #[cfg(not(windows))]
        let file_resource = "file:/report.md";
        // App 1: a document with real text, and a root carrying a non-default
        // presentation (scroll / caret / mode / open resource).
        let mut root = DocRoot::new();
        root.caret = "caret=42".into();
        root.scroll = vec![10.0, 20.0];
        root.mode = Some("grid".into());
        root.resource = file_resource.into();

        let mut state = State::new();
        let body = state.doc.put_object(&ROOT, "body", ObjType::Text).unwrap();
        state.doc.splice_text(&body, 0, 0, "Hello, world").unwrap();
        tilo_ui::resource::init(
            &mut state.doc,
            &DocumentId::parse(file_resource).unwrap(),
            &ResourceType::new("document"),
            "home-workspace",
        );

        let mut app1 = App::new_with_controlled_inputs(state, Box::new(root));
        app1.autofocus_first = true; // focus the single field so a caret exists
        app1.init_layout();

        let v1 = app1.capture_view();
        assert_eq!(v1.scroll, vec![10.0, 20.0]);
        assert_eq!(v1.view_mode.as_deref(), Some("grid"));
        assert_eq!(v1.open_resource.as_deref(), Some(file_resource));
        assert_eq!(v1.focus.as_ref().unwrap().ordinal, 0);
        assert_eq!(v1.focus.as_ref().unwrap().ephemeral, "caret=42");

        let frozen = app1.freeze();

        // The bytes in between carry every field — capture didn't drop any.
        let env: FrozenApp = tilo_ui::postcard::from_bytes(&frozen).unwrap();
        assert_eq!(env.version, FROZEN_VERSION);
        assert_eq!(env.view, v1);

        // App 2: a fresh app with an EMPTY doc and a default root.
        let mut app2 = App::new_with_controlled_inputs(State::new(), Box::new(DocRoot::new()));
        app2.init_layout();
        assert_eq!(
            text_at(&app2.state_mut_for_test().doc.clone(), "body"),
            None
        );

        app2.thaw(&frozen).expect("thaw a well-formed envelope");

        let doc2 = app2.state_mut_for_test().doc.clone();
        assert_eq!(
            text_at(&doc2, "body").as_deref(),
            Some("Hello, world"),
            "the CRDT document round-tripped through freeze/thaw"
        );
        assert_eq!(
            tilo_ui::resource::identity(&doc2).map(|c| c.id.as_str().to_string()),
            Some(file_resource.to_string()),
            "thaw reopened the resource the View named — its document is live"
        );

        let v2 = app2.capture_view();
        assert_eq!(v2.scroll, vec![10.0, 20.0], "scroll restored to the widget");
        assert_eq!(v2.view_mode.as_deref(), Some("grid"), "view mode restored");
        let f2 = v2.focus.as_ref().expect("focus restored");
        assert_eq!(f2.ordinal, 0, "focus ordinal restored");
        assert_eq!(f2.ephemeral, "caret=42", "caret restored via set_state");
    }

    // Case `thaw_rejects_bad_bytes_without_panicking`: Thawing truncated or garbage
    // bytes returns an error, and an envelope from an unknown version is rejected
    // rather than mis-read — never a panic.
    {
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(DocRoot::new()));
        app.init_layout();

        assert!(matches!(
            app.thaw(&[0xff, 0x00, 0x7e, 0x13]),
            Err(ThawError::Envelope(_))
        ));
        assert!(matches!(app.thaw(&[]), Err(ThawError::Envelope(_))));

        // A valid envelope truncated mid-way still fails cleanly.
        let good = app.freeze();
        let truncated = &good[..good.len() / 2];
        assert!(
            app.thaw(truncated).is_err(),
            "truncated bytes error, not panic"
        );

        let future = FrozenApp {
            version: FROZEN_VERSION + 99,
            state: AutoCommit::new().save(),
            view: View::default(),
        };
        let bytes = tilo_ui::postcard::to_stdvec(&future).unwrap();
        assert!(matches!(
            app.thaw(&bytes),
            Err(ThawError::UnsupportedVersion { .. })
        ));
    }

    // Case `custom_serialization_app_round_trips_via_override`: A stub whose state is
    // NOT a CRDT overrides only `serialize_state` / `restore_state` and round-trips
    // through the *same* `freeze`/`thaw` default path — proving that view
    // capture/restore default cleanly for it.
    {
        struct BrowserStub {
            session: String,
        }

        impl FreezeThaw for BrowserStub {
            fn serialize_state(&self) -> Vec<u8> {
                self.session.clone().into_bytes()
            }
            fn restore_state(&mut self, bytes: &[u8]) -> Result<(), ThawError> {
                self.session = String::from_utf8(bytes.to_vec())
                    .map_err(|e| ThawError::State(e.to_string()))?;
                Ok(())
            }
        }

        let src = BrowserStub {
            session: "tabs=[example.com, tilo.so]; active=1".into(),
        };
        let frozen = src.freeze();

        let mut dst = BrowserStub {
            session: String::new(),
        };
        dst.thaw(&frozen).expect("thaw the browser stub");
        assert_eq!(
            dst.session, "tabs=[example.com, tilo.so]; active=1",
            "the non-CRDT state round-tripped through the serialize_state hook"
        );

        let env: FrozenApp = tilo_ui::postcard::from_bytes(&frozen).unwrap();
        assert_eq!(env.view, View::default());

        // A malformed state payload surfaces as a State error, not a panic.
        let bad = FrozenApp {
            version: FROZEN_VERSION,
            state: vec![0xff, 0xfe, 0xfd],
            view: View::default(),
        };
        let bad_bytes = tilo_ui::postcard::to_stdvec(&bad).unwrap();
        assert!(matches!(dst.thaw(&bad_bytes), Err(ThawError::State(_))));
    }
}
