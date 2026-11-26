//! The bar's mark of a window an agent opened.

use super::super::tests::lay_out_at_width;
use super::super::{TabBar, TabDescriptor};
use automerge::AutoCommit;
use tilo_desktop_model::ownership::OpenedBy;
use tilo_ui_base::presence::PresenceCtx;
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::Widget;
use vello::Scene;
use vello::kurbo::Affine;

fn opened(title: &str) -> OpenedBy {
    OpenedBy {
        run: "agent-session:7".into(),
        agent: tilo_action_policy::MemberId::agent("scribe", "ada"),
        label: "Jane Doe\u{2019}s agent".into(),
        title: title.into(),
    }
}

fn draw(bar: &mut TabBar, tr: &mut TextRenderer, theme: &Theme) {
    bar.draw(&mut tilo_ui_base::draw::DrawCtx::new(
        &mut Scene::new(),
        Affine::IDENTITY,
        tr,
        theme,
        PresenceCtx::default().draw_env(),
    ));
}

tilo_test_env::scenario! {
    /// The agent's mark in a window bar.
    /// Each phase was a test of its own and builds its own fixture.
    fn the_agents_mark_names_and_follows_its_window;

    phase a_window_an_agent_opened_names_the_agent_and_hands_its_run_to_the_host;
    {
        let theme = Theme::light();
        let mut tr = TextRenderer::new();
        let mut bar = TabBar::new(
            "opened",
            vec![
                TabDescriptor::new(1, "Application.pdf"),
                TabDescriptor::new(2, "Notes"),
            ],
            1,
        );
        lay_out_at_width(&mut bar, 1200.0);
        assert!(bar.opened_by_chip(&mut tr, &theme).is_none());
        let mark = opened("Fill Jane Doe\u{2019}s visa application");
        assert!(bar.set_opened_by(1, Some(mark.clone())));
        assert!(!bar.set_opened_by(1, Some(mark.clone())), "unchanged");
        let (rect, label) = bar
            .opened_by_chip(&mut tr, &theme)
            .expect("the mark stands after the pills");
        assert_eq!(label, "Jane Doe\u{2019}s agent");
        assert!(rect.x0 >= bar.tab_rects[1].x1, "after the pills");

        draw(&mut bar, &mut tr, &theme);
        let (stood, node) = bar.opened_by_stop().expect("the keyboard can stand on it");
        assert_eq!(stood, rect);
        assert_eq!(node.name.as_deref(), Some("Jane Doe\u{2019}s agent"));
        assert_eq!(
            node.description.as_deref(),
            Some("Jane Doe\u{2019}s agent opened this window\nFill Jane Doe\u{2019}s visa application"),
            "who opened it, then its step on a line of its own"
        );
        // Once the page knows whom the agent works for, the mark says so.
        assert!(bar.set_opened_for(1, Some("Quinten".into())));
        assert!(!bar.set_opened_for(1, Some("Quinten".into())), "unchanged");
        draw(&mut bar, &mut tr, &theme);
        let (_, node) = bar.opened_by_stop().expect("still marked");
        assert_eq!(
            node.description.as_deref(),
            Some("Jane Doe\u{2019}s agent opened this window for Quinten\nFill Jane Doe\u{2019}s visa application"),
        );
        assert!(bar.set_opened_for(1, None));

        assert!(bar.press_opened_by(rect.center()));
        assert_eq!(bar.take_opened_by_press(), Some((1, mark.run.clone())));
        assert_eq!(bar.take_opened_by_press(), None, "once");
        assert!(Widget::perform_semantic_action(
            &mut bar,
            super::OPENED_BY_TARGET,
            &tilo_ui_base::a11y::Action::Activate,
            &mut tilo_ui_base::widget::Edits::lend(&mut AutoCommit::new()),
            &automerge::ROOT,
        ));
        assert_eq!(bar.take_opened_by_press(), Some((1, mark.run.clone())));

        assert!(bar.set_opened_by(1, Some(opened("Jane Doe\u{2019}s agent"))));
        draw(&mut bar, &mut tr, &theme);
        let (_, node) = bar.opened_by_stop().expect("still marked");
        assert_eq!(
            node.description.as_deref(),
            Some("Jane Doe\u{2019}s agent opened this window")
        );

        bar.set_active(2);
        assert!(
            bar.opened_by_chip(&mut tr, &theme).is_none(),
            "only its own window"
        );
        bar.set_active(1);
        assert!(bar.set_opened_by(1, None));
        assert!(bar.opened_by_chip(&mut tr, &theme).is_none());
    }


    phase the_agents_mark_follows_an_apps_location_in_its_bar;
    {
        use crate::window_toolbar::{ToolbarAction, ToolbarLocation, WindowToolbar};
        let _settled = tilo_ui_base::motion::testing::settled();
        let theme = Theme::light().with_reduce_motion(true);
        let mut tr = TextRenderer::new();
        let mut bar = TabBar::new("slots", vec![TabDescriptor::new(1, "Agenda")], 1);
        bar.set_window_controls(true);
        assert!(bar.set_toolbar(
            1,
            Some(WindowToolbar {
                location: Some(ToolbarLocation {
                    label: "Agenda".into(),
                    ..ToolbarLocation::default()
                }),
                actions: vec![ToolbarAction::new("new", "plus", "New note")],
                ..WindowToolbar::default()
            })
        ));
        assert!(bar.set_opened_by(1, Some(opened("File the receipts"))));
        for _ in 0..2 {
            super::super::tests::lay_out_at_width_with(&mut bar, 1200.0, &theme);
            draw(&mut bar, &mut tr, &theme);
        }
        let (rect, _) = bar
            .opened_by_chip(&mut tr, &theme)
            .expect("the mark shows beside the app's slots");
        let (_, _, location, actions) = bar.toolbar_rects();
        assert!(location.area() > 0.0, "the app's location stands");
        assert!(
            (location.x0..location.x1).contains(&rect.x0),
            "right after what the location shows: {rect:?} in {location:?}"
        );
        assert!(
            (rect.center().y - location.center().y).abs() < 0.5,
            "on the location's line"
        );
        assert!(
            actions.iter().all(|action| rect.x1 <= action.x0),
            "before the app's actions: {rect:?} {actions:?}"
        );
    }

    phase the_agents_mark_follows_the_edited_word;
    // The mark follows the word after an edited document's location, not just its label: "Edited" and
    // the agent's name never stand on each other.
    {
        use crate::window_toolbar::{ToolbarAction, ToolbarLocation, WindowToolbar};
        let _settled = tilo_ui_base::motion::testing::settled();
        let theme = Theme::light().with_reduce_motion(true);
        let mut tr = TextRenderer::new();
        let mut bar = TabBar::new("edited", vec![TabDescriptor::new(1, "Letter")], 1);
        bar.set_window_controls(true);
        assert!(bar.set_toolbar(
            1,
            Some(WindowToolbar {
                location: Some(ToolbarLocation {
                    label: "Hello from the agent".into(),
                    edited: true,
                    ..ToolbarLocation::default()
                }),
                actions: vec![ToolbarAction::new("new", "plus", "New page")],
                ..WindowToolbar::default()
            })
        ));
        assert!(bar.set_opened_by(1, Some(opened("Write a paragraph"))));
        for _ in 0..2 {
            super::super::tests::lay_out_at_width_with(&mut bar, 1200.0, &theme);
            draw(&mut bar, &mut tr, &theme);
        }
        let (rect, _) = bar
            .opened_by_chip(&mut tr, &theme)
            .expect("the mark shows beside the app's slots");
        let location = bar
            .painted_labels()
            .into_iter()
            .find(|label| label.widget == "BarLocation")
            .expect("the location is painted");
        let edited = tilo_ui_base::i18n::I18nManager::lookup_plain("window-toolbar-edited");
        assert!(
            location.text.ends_with(&edited),
            "the edited word is shown: {location:?}"
        );
        let [x, _, w, _] = location.rect;
        assert!(
            rect.x0 >= x + w,
            "the mark {rect:?} stands after the location and its edited word, which end at {}",
            x + w
        );
    }

}
