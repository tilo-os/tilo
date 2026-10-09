//! Full-size action-presence captures through the production retained host.
//! Run with $CARGO run --locked -p tilo-ui --example action-presence
//! -- target/agent-marker, using any cargo drop-in appropriate for the builder.

use std::{cell::Cell, path::Path, rc::Rc};
use tilo_ui::widget::{Button, Column, resource_actions};
use tilo_ui::{headless::FrameHarness, state::State, theme::Theme};

type CaptureResult<T> = Result<T, Box<dyn std::error::Error>>;

fn fixture(viewport: (u32, u32), theme: Theme) -> CaptureResult<(FrameHarness, Rc<Cell<u32>>)> {
    let calls = Rc::new(Cell::new(0_u32));
    let mut column = Column::new();
    for id in ["first", "second"] {
        let mut resource = resource_actions::resource(id, "Same label", "edit", "Edit");
        resource.id = tilo_actions::ResourceId::new(id);
        let binding = tilo_actions::ActionBinding::unit_for(&resource, "edit")?;
        let counter = calls.clone();
        column = column.push(resource_actions::immediate(
            Box::new(Button::new(id, "Same label").on_action(binding)),
            resource,
            move |_| {
                counter.set(counter.get().saturating_add(1));
                Ok(tilo_actions::ActionOutput::default())
            },
        ));
    }
    Ok((
        FrameHarness::new_with_theme(State::new(), Box::new(column), viewport, theme),
        calls,
    ))
}

#[allow(
    clippy::disallowed_methods,
    reason = "this offscreen CLI has no event loop; one bounded sleep advances each capture across the real approach interval"
)]
fn capture(out: &Path, theme: Theme, viewport: (u32, u32)) -> CaptureResult<()> {
    std::fs::create_dir_all(out)?;
    let travel = theme
        .tokens
        .motion
        .durations
        .slow()
        .saturating_add(theme.tokens.motion.durations.base());
    let (mut frame, calls) = fixture(viewport, theme)?;
    frame.render_png(&out.join("before.png"));
    frame.invoke_action(
        &tilo_actions::ActionAddress::new("first", "edit"),
        &serde_json::Map::new(),
        false,
        None,
    )?;
    let interaction = tilo_actions::SemanticInteraction {
        version: tilo_actions::INTERACTION_VERSION,
        invocation: tilo_actions::InvocationId::new(),
        sequence: 1,
        actor: tilo_actions::InteractionActor {
            id: "scribe".into(),
            kind: tilo_actions::InteractionActorKind::Agent,
            for_person: None,
        },
        phase: tilo_actions::InteractionPhase::Applying,
        action: tilo_actions::InteractionAction {
            id: "edit".into(),
            label: "Edit".into(),
            app: None,
        },
        resource: tilo_actions::ActionResource {
            id: "first".into(),
            kind: "example".into(),
            label: "Same label".into(),
        },
        anchor: tilo_actions::InteractionAnchor::Resource,
        group: None,
    };
    frame
        .presence_mut()
        .set_display_name(tilo_ui::permissions::UserId("scribe".into()), "Scribe");
    let changed = frame.presence_mut().apply_interaction(interaction)?;
    frame.update_presence(|_| changed);
    if !frame.poll_frame()? {
        return Err("the production host did not paint the new presence".into());
    }
    frame.render_png(&out.join("approaching.png"));
    std::thread::sleep(travel);
    frame.relayout();
    frame.render_png(&out.join("after.png"));
    if calls.get() != 1 {
        return Err("presence replayed or failed to execute the semantic action".into());
    }
    if std::fs::read(out.join("before.png"))? == std::fs::read(out.join("after.png"))? {
        return Err("the capture did not observe action presence".into());
    }
    Ok(())
}

fn main() -> CaptureResult<()> {
    let out = std::env::args_os()
        .nth(1)
        .ok_or("output directory argument required")?;
    for (name, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high-contrast", Theme::high_contrast()),
    ] {
        for viewport in [(1600, 900), (800, 600)] {
            capture(
                &Path::new(&out)
                    .join(name)
                    .join(format!("{}x{}", viewport.0, viewport.1)),
                theme.clone(),
                viewport,
            )?;
        }
    }
    Ok(())
}
