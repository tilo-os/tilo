//! An agent's reply: what it is doing while it works, and what it did once it is done.

use super::*;

#[test]
fn agent_work_thread_survives_pointer_navigation_and_projects_updates() {
    use crate::model::{AgentStepOutcome, AgentWork, AgentWorkState};
    use ui::Widget;

    for (width, height) in [(1600.0, 900.0), (800.0, 600.0)] {
        let (mut app, mut doc) = app_with_doc();
        stage_agent_work(&mut app, &mut doc);
        app.land(&mut doc);
        app.sync(&doc.chat, &automerge::ROOT);
        app.rect = vello::kurbo::Rect::new(0.0, 0.0, width, height);
        app.reconcile_flaps();
        app.right_flap.snap(true);
        render_frame(&mut app, width, height);

        let description = |app: &ChatApp| {
            app.accessibility()
                .unwrap()
                .children
                .into_iter()
                .find(|node| node.name.as_deref() == Some("Messages"))
                .unwrap()
                .children
                .into_iter()
                .find(|node| node.name.as_deref() == Some("Tide: "))
                .expect("the open thread exposes its agent reply")
                .description
                .unwrap()
        };
        let said = description(&app);
        says_its_step_and_opens_its_list(&mut app, &mut doc, &said, (width, height));

        if app.thread_is_page() {
            // A thread that fills the window is the window bar's place: its
            // Back closes it.
            ui::window_toolbar::deliver(ui::window_toolbar::ToolbarEvent::Back);
            app.apply_window_toolbar(&mut ui::widget::Edits::lend(&mut doc.chat));
        } else {
            let close = app
                .hits
                .iter()
                .find(|(_, hit)| matches!(hit, Hit::CloseThread))
                .expect("painted thread has a close control")
                .0
                .center();
            click_point(&mut app, &mut doc, close);
        }
        assert!(app.thread_open.is_none());

        // Render the settled closed flap, not the outgoing overlay frame
        // that deliberately continues intercepting input during its exit.
        app.right_flap.snap(false);

        render_frame(&mut app, width, height);
        let open = app
            .hits
            .iter()
            .find(|(_, hit)| matches!(hit, Hit::OpenThread(id) if id == "m_agent_request"))
            .expect("the original request retains its reply affordance")
            .0
            .center();
        click_point(&mut app, &mut doc, open);
        assert_eq!(app.thread_open.as_deref(), Some("m_agent_request"));

        let mut applied_step = app
            .messages
            .iter()
            .find(|message| message.id == crate::testing::agent_reply())
            .unwrap()
            .work
            .as_ref()
            .unwrap()
            .trace[0]
            .clone();
        applied_step.outcome = AgentStepOutcome::Applied;
        doc.apply_part_as(
            &crate::resource::demo_agent(),
            crate::testing::record_work(
                crate::resource::DEMO_SUMMONS,
                &AgentWork::new(
                    AgentWorkState::Done {
                        did: "Revenue curve added".into(),
                    },
                    crate::demo::NOW,
                )
                .with_trace(vec![applied_step]),
            ),
        )
        .unwrap();
        app.land(&mut doc);
        app.sync(&doc.chat, &automerge::ROOT);
        render_frame(&mut app, width, height);
        assert_eq!(
            description(&app),
            "Did one thing; Applied: Add function: calculator.workspace",
            "the state update retains the previously recorded dispatch evidence"
        );
        let reply = app
            .messages
            .iter()
            .find(|message| message.id == crate::testing::agent_reply())
            .unwrap();
        assert_eq!(reply.parent.as_deref(), Some("m_agent_request"));
        assert!(
            reply.text.is_empty(),
            "work updates do not overwrite authored text"
        );
        opens_what_the_agent_did(&mut app, &mut doc, width, height);
    }
}

/// At work, the reply says the step its agent is at and how many it has taken, and that one
/// line opens the list of what the agent did so far and closes it again.
fn says_its_step_and_opens_its_list(
    app: &mut ChatApp,
    doc: &mut crate::testing::Workspace,
    said: &str,
    (width, height): (f64, f64),
) {
    assert!(said.contains("Add function: Revenue curve"), "{said}");
    assert!(said.contains("1 step"), "{said}");
    assert!(said.contains("Requested: Add function"), "{said}");
    let reply = crate::testing::agent_reply();
    opens_what_the_agent_did(app, doc, width, height);
    render_frame(app, width, height);
    let line = app
        .hits
        .iter()
        .find(|(_, hit)| matches!(hit, Hit::AgentWork(id) if *id == reply))
        .expect("the open list keeps the line that closes it")
        .0
        .center();
    click_point(app, doc, line);
    assert!(
        !app.expanded_agent.contains(&reply),
        "a second press closes the list"
    );
    render_frame(app, width, height);
}

/// The folded line of what an agent did opens its list.
fn opens_what_the_agent_did(
    app: &mut ChatApp,
    doc: &mut crate::testing::Workspace,
    width: f64,
    height: f64,
) {
    // The folded line opens the list of what the agent did.
    app.right_flap.snap(true);
    render_frame(app, width, height);
    let reply = crate::testing::agent_reply();
    let folded = app
        .hits
        .iter()
        .find(|(_, hit)| matches!(hit, Hit::AgentWork(id) if *id == reply))
        .expect("what the agent did folds to a line that opens")
        .0
        .center();
    click_point(app, doc, folded);
    assert!(app.expanded_agent.contains(&reply));
}

#[test]
fn agent_work_compact_thread_scrolls_all_evidence_without_moving_timeline() {
    use crate::model::{AgentStep, AgentStepOutcome, AgentWork, AgentWorkState};
    use ui::Widget;
    let (mut app, mut doc) = app_with_doc();
    stage_agent_work(&mut app, &mut doc);
    app.sync(&doc.chat, &automerge::ROOT);
    let work = AgentWork::new(AgentWorkState::Done { did: "Reviewed all operations".into() }, crate::demo::NOW)
        .with_trace((0..60).map(|index| AgentStep::authored("calculator.workspace",
            format!("Operation {index}: preserve graph bounds and add the requested revenue curve"),
            AgentStepOutcome::Applied, index)).collect());
    doc.apply_part_as(
        &crate::resource::demo_agent(),
        crate::testing::record_work(crate::resource::DEMO_SUMMONS, &work),
    )
    .unwrap();
    app.sync(&doc.chat, &automerge::ROOT);
    // The person opened the list of what the agent did.
    app.expanded_agent.insert(crate::testing::agent_reply());
    render_frame(&mut app, 800.0, 600.0);
    let region = app.primary_vertical_scroll().unwrap();
    assert_eq!(region.key, "thread");
    assert!(
        region.metrics.max_scroll() > 600.0,
        "production wrapping creates real overflow"
    );
    let timeline_offset = app.scroll;
    assert!(matches!(
        app.apply_primary_vertical_scroll(ui::widget::PrimaryScrollAction::End),
        ui::widget::PrimaryScrollResult::Changed(_)
    ));
    render_frame(&mut app, 800.0, 600.0);
    let region = app.primary_vertical_scroll().unwrap();
    assert!((region.metrics.offset - region.metrics.max_scroll()).abs() < 0.01);
    assert!((app.scroll - timeline_offset).abs() < 0.01);
    let tree = app.accessibility().unwrap();
    let messages = tree
        .children
        .iter()
        .find(|node| node.name.as_deref() == Some("Messages"))
        .unwrap();
    assert_eq!(
        messages.children.len(),
        2,
        "pushed detail exposes only parent and reply"
    );
    let description = messages
        .children
        .iter()
        .find_map(|node| node.description.as_ref())
        .unwrap();
    assert!(
        description.contains("Operation 59:"),
        "the final recorded dispatch is accessible"
    );
    let point = app.thread_body.center();
    feed(&mut app, &mut doc, &cursor_moved(point.x, point.y));
    feed(&mut app, &mut doc, &wheel(2.0));
    assert!(
        f64::from(app.thread_scroll.offset_y()) < region.metrics.max_scroll(),
        "pointer wheel reaches earlier evidence"
    );
    assert!((app.scroll - timeline_offset).abs() < 0.01);
}
