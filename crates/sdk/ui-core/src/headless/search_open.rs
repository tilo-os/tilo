//! Opening the platform search from inside the app, as the window host does
//! (`App::open_search`): a search launcher's invocation, `--search` on a
//! capture, or a test opens this window's action palette narrowed to the
//! request's scope with its query typed, over the app's own search
//! providers and open resources.

use super::FrameHarness;
use crate::search::SearchRequest;

impl FrameHarness {
    /// Open search the way `request` asks: this app or one part of it, with
    /// the query already typed.
    pub fn open_search(&mut self, request: &SearchRequest) -> Result<(), String> {
        self.invalidate_local();
        if self.action_palette.is_some()
            || self.caller_palette.palette.is_some()
            || self.root.modal_capture().is_some()
            || self.focus_trap_active
            || self.history.as_ref().is_some_and(|h| h.captures_input())
        {
            return Err("Finish the current dialog or command first".into());
        }
        let mut resources = Vec::new();
        crate::bound_dispatch::collect_unbound_resources(
            self.root.as_ref(),
            &self.state.doc,
            &mut resources,
        );
        let mut providers = crate::bound_dispatch::search_providers(self.root.as_ref());
        providers.push(Box::new(crate::search::OpenResourceSearch::new(&resources)));
        let mut palette = Box::new(crate::action_palette::ActionPalette::with_host_text(
            self.text.font_system.locale().to_owned(),
        ));
        palette.load_open_resources(&resources);
        // The app's own actions, as the window host's palette lists them
        // (`App::open_action_palette`): search leads with what the app does.
        palette.load_bound_resources(
            crate::bound_dispatch::discover_bound_resources(
                self.root.as_ref(),
                &self.state.doc,
                None,
            )
            .unwrap_or_default(),
        );
        palette.set_providers(providers);
        palette.open();
        palette.set_scope(request.resource_scope().cloned());
        if let Some(query) = &request.query {
            palette.set_query(query);
            palette.select_query();
        }
        self.discard_layout_tree();
        self.focused = None;
        self.action_palette = Some(palette);
        self.relayout();
        Ok(())
    }

    /// A control invocation addressed to the host search door opens search
    /// here instead of reaching the app. Returns whether it was one.
    pub(super) fn take_search_invocation(
        &mut self,
        intent: &tilo_actions::ControlInvocation,
    ) -> bool {
        let Some(request) = SearchRequest::from_invocation(intent) else {
            return false;
        };
        if let Err(error) = self.open_search(&request) {
            tracing::warn!(%error, "headless: search not opened");
        }
        true
    }

    /// A search launcher of a component this frame hosts. The window's own
    /// component opens search here; one hosted inside the root (an app in a
    /// desktop's window) asks its host, as in the native window host
    /// (`App::take_search_invocation`): the desktop around it opens search
    /// over that window ([`crate::widget::AppHostIntent::OpenSearch`]),
    /// where this frame's palette would search the whole desktop. Returns
    /// whether it was one.
    pub(super) fn take_component_search_invocation(
        &mut self,
        target: &crate::bound_dispatch::BoundActionTarget,
        intent: &tilo_actions::ControlInvocation,
    ) -> bool {
        if self.component_instance_id() == Some(target.binding.instance_id) {
            return self.take_search_invocation(intent);
        }
        let Some(request) = SearchRequest::from_invocation(intent) else {
            return false;
        };
        if target.binding.enter().is_ok() {
            target.binding.request_search(request);
        }
        true
    }

    /// Continue one bounded provider page per frame, as the window host's
    /// redraw does. Returns whether more pages remain.
    pub(super) fn advance_search(&mut self) -> bool {
        self.action_palette
            .as_deref_mut()
            .filter(|palette| palette.is_open())
            .is_some_and(|palette| palette.advance_search_page().pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::ScopeDecl;
    use crate::state::State;
    use crate::theme::Theme;

    #[test]
    fn a_search_launcher_opens_this_windows_palette_narrowed_and_prefilled() {
        let request =
            SearchRequest::within(ScopeDecl::new("folder:inbox", "Inbox")).with_query("invoice");
        let mut frame = FrameHarness::new_with_controlled_inputs(
            State::new(),
            Box::new(crate::search::search_button(request)),
            (640, 480),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("fixture".into()),
        );
        frame.relayout();
        assert!(frame.action_palette.is_none());
        frame
            .click_named("search-open-folder:inbox")
            .expect("the launcher is pressed");
        let palette = frame
            .action_palette
            .as_deref()
            .expect("the launcher opens search here, not in the app");
        assert_eq!(
            palette.scope().map(|scope| scope.label.as_str()),
            Some("Inbox")
        );
        assert_eq!(palette.query(), "invoice");
        assert!(
            frame.open_search(&SearchRequest::this_app()).is_err(),
            "one search at a time"
        );
    }

    #[test]
    fn semantic_search_uses_the_published_host_door_and_refuses_invalid_requests() {
        let mut frame = FrameHarness::new_with_controlled_inputs(
            State::new(),
            Box::new(crate::widget::Text::new("search-fixture")),
            (640, 480),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("fixture".into()),
        );
        let address = crate::search::search_address();
        assert!(tilo_actions::door::resolve(&frame.resources(), &address).is_some());
        let bad = serde_json::json!({"query": 42})
            .as_object()
            .unwrap()
            .clone();
        assert!(matches!(
            frame.invoke_action(&address, &bad, false, None),
            Err(tilo_actions::DoorError::InvalidArguments(_))
        ));
        assert!(frame.action_palette.is_none());
        let before = frame.dump_json();
        let request =
            SearchRequest::within(ScopeDecl::new("folder:inbox", "Inbox")).with_query("invoice");
        frame
            .invoke_action(&address, &request.arguments(), false, None)
            .unwrap();
        let palette = frame
            .action_palette
            .as_deref()
            .expect("public search opens");
        assert_eq!(palette.query(), "invoice");
        assert_eq!(
            palette.scope().map(|scope| scope.label.as_str()),
            Some("Inbox")
        );
        assert_eq!(
            frame.dump_json(),
            before,
            "opening search does not edit app state"
        );
        assert!(
            matches!(
                frame.invoke_action(&address, &request.arguments(), false, None),
                Err(tilo_actions::DoorError::Failed(_))
            ),
            "a busy host must refuse instead of silently reporting success"
        );
    }

    #[test]
    fn a_component_owned_search_launcher_opens_the_host_palette() {
        use crate::application::{LaunchTarget, RestoreInput};
        use crate::application_test_support as probe;
        let owner = probe::ProbeHost::new();
        let mut component =
            probe::instantiate_probe(&owner, 71, LaunchTarget::New, RestoreInput::None).unwrap();
        struct Launcher(crate::widget::Button);
        impl crate::widget::Decorator for Launcher {
            type Inner = crate::widget::Button;
            fn inner(&self) -> &Self::Inner {
                &self.0
            }
            fn inner_mut(&mut self) -> &mut Self::Inner {
                &mut self.0
            }
        }
        impl crate::widget::AppWidget for Launcher {
            fn app_kind(&self) -> crate::widget::AppKind {
                crate::widget::AppKind::Editor
            }
            fn app_title(&self) -> &'static str {
                "Search fixture"
            }
            fn app_icon(&self) -> crate::widget::IconDescriptor {
                crate::widget::IconDescriptor::Named("search".into())
            }
        }
        component.root = Some(Box::new(Launcher(crate::search::search_button(
            SearchRequest::within(ScopeDecl::new("folder:inbox", "Inbox")).with_query("invoice"),
        ))));
        let mut frame =
            FrameHarness::from_component(component, (640, 480), probe::runner_inputs()).unwrap();
        assert!(frame.action_palette.is_none());
        frame.click_named("search-open-folder:inbox").unwrap();
        let palette = frame.action_palette.as_deref().expect("host search opens");
        assert_eq!(
            palette.scope().map(|scope| scope.label.as_str()),
            Some("Inbox")
        );
        assert_eq!(palette.query(), "invoice");
    }
}
