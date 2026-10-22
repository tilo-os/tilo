use super::super::agent_access::{
    AccessFocus, AgentAccessView, draw_agent_access, set_agent_answers,
};
use super::*;
use tilo_action_policy::agent_answers::AnswerScope;
use tilo_action_policy::{ActionKind, Answer};

const OWNER: &str = "owner-user";
const AGENT: &str = "scribe";

fn ws(apps: usize) -> tilo_desktop_model::Workspace {
    let mut ws =
        tilo_desktop_model::Workspace::new(tilo_desktop_model::WorkspaceId::from(32), "access");
    ws.set_owner(OWNER);
    super::super::admit_agent(
        &mut ws,
        &OWNER.to_string(),
        AGENT,
        super::super::AgentPreset::Operator,
    );
    let answers: Vec<_> = (0..apps)
        .map(|index| {
            (
                AnswerScope::app(format!("so.tilo.app{index:02}")),
                ActionKind::Read,
                Some(Answer::Deny),
            )
        })
        .collect();
    set_agent_answers(&mut ws, &OWNER.to_string(), AGENT, &answers);
    ws
}

/// Paint the view as the overlay would, with its scroll and focus.
fn paint(
    overlay: &mut WorkspaceSettingsOverlay,
    ws: &tilo_desktop_model::Workspace,
    open: Option<&AnswerScope>,
) {
    let mut scene = Scene::new();
    let mut text = TextRenderer::new();
    let theme = Theme::dark();
    overlay.agent_access = draw_agent_access(
        &mut ui::paint::PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
        Rect::new(0.0, 0.0, 640.0, 520.0),
        (ws, &super::super::NoAppDirectory),
        &AgentAccessView {
            agent: AGENT,
            open,
            editable: true,
            scroll: overlay.agent_access_scroll,
            focus: overlay.agent_access_focus.as_ref(),
            models: overlay.agent_models.as_ref(),
            models_open: overlay.agent_models_open,
        },
    );
}

#[test]
fn tab_reaches_every_control_and_scrolls_it_into_view() {
    let ws = ws(24);
    let mut overlay = WorkspaceSettingsOverlay::default();
    overlay.open_agent_review(AGENT.into());
    paint(&mut overlay, &ws, None);
    assert_eq!(overlay.handle_agent_access_key(AgentAccessKey::Next), None);
    assert_eq!(overlay.agent_access_focus, Some(AccessFocus::Back));
    // Shift+Tab from the first control wraps to the last app, which is
    // below the fold until focus brings it up.
    overlay.handle_agent_access_key(AgentAccessKey::Previous);
    let last = AccessFocus::Expand(AnswerScope::app("so.tilo.app23"));
    assert_eq!(overlay.agent_access_focus, Some(last.clone()));
    assert!(overlay.agent_access_scroll > 0.0);
    paint(&mut overlay, &ws, None);
    let shown = overlay.agent_access.stop_rect(&last).unwrap();
    assert!(overlay.agent_access.viewport.contains(shown.center()));
    assert_eq!(
        overlay.handle_agent_access_key(AgentAccessKey::Activate),
        Some(EventOutcome::ToggleAgentApp(AnswerScope::app(
            "so.tilo.app23"
        )))
    );
    assert_eq!(
        overlay.handle_agent_access_key(AgentAccessKey::Back),
        Some(EventOutcome::CloseAgentReview)
    );
}

#[test]
fn arrows_change_the_focused_answer_one_step_at_a_time() {
    let ws = ws(1);
    let app = AnswerScope::app("so.tilo.app00");
    let mut overlay = WorkspaceSettingsOverlay::default();
    overlay.open_agent_review(AGENT.into());
    paint(&mut overlay, &ws, Some(&app));
    overlay.agent_access_focus = Some(AccessFocus::Answers(app.clone(), ActionKind::Read));
    // Reading is Deny: there is nothing further right.
    assert_eq!(overlay.handle_agent_access_key(AgentAccessKey::Right), None);
    assert_eq!(
        overlay.handle_agent_access_key(AgentAccessKey::Left),
        Some(EventOutcome::SetAgentAnswers {
            agent: AGENT.into(),
            answers: vec![(app.clone(), ActionKind::Read, Some(Answer::Ask))],
        })
    );
    assert_eq!(
        overlay.handle_agent_access_key(AgentAccessKey::Activate),
        None,
        "Enter on an answer control changes nothing by itself"
    );
}

#[test]
fn the_agent_plans_with_the_model_chosen_in_its_view() {
    use super::super::agent_access::{AgentModelOption, AgentModels};
    let option = |model: &str| AgentModelOption {
        profile: "provider".into(),
        model: model.into(),
        label: format!("{model} from Provider"),
    };
    let ws = ws(1);
    let mut overlay = WorkspaceSettingsOverlay::default();
    overlay.open_agent_review(AGENT.into());
    overlay.set_agent_models(Some(AgentModels {
        default: Some("large from Provider".into()),
        chosen: Some(option("small")),
        offered: vec![option("large"), option("small")],
    }));
    paint(&mut overlay, &ws, None);
    let head = overlay
        .agent_access
        .stop_rect(&AccessFocus::ExpandModels)
        .expect("the model is shown above the apps");
    assert_eq!(
        overlay.agent_access_outcome(head.center()),
        Some(EventOutcome::ToggleAgentModels)
    );
    overlay.toggle_agent_models();
    paint(&mut overlay, &ws, None);
    let stops: Vec<_> = overlay
        .agent_access
        .stops
        .iter()
        .map(|(stop, _)| stop.clone())
        .take(4)
        .collect();
    // The model in use has no Use of its own.
    assert_eq!(
        stops,
        [
            AccessFocus::Back,
            AccessFocus::ExpandModels,
            AccessFocus::Model(None),
            AccessFocus::Model(Some(option("large"))),
        ]
    );
    let large = overlay
        .agent_access
        .stop_rect(&AccessFocus::Model(Some(option("large"))))
        .unwrap();
    assert_eq!(
        overlay.agent_access_outcome(large.center()),
        Some(EventOutcome::SetAgentModel {
            agent: AGENT.into(),
            model: Some(option("large")),
        })
    );
    overlay.agent_access_focus = Some(AccessFocus::Model(None));
    assert_eq!(
        overlay.handle_agent_access_key(AgentAccessKey::Activate),
        Some(EventOutcome::SetAgentModel {
            agent: AGENT.into(),
            model: None,
        })
    );
    overlay.close_agent_review();
    overlay.open_agent_review(AGENT.into());
    assert!(!overlay.agent_models_open);
}
