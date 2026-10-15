//! Export real hosted Typst frames for a syntax/live-preview comparison.
//!
//! Run with an output directory; link a verified font catalog beside the binary.
//! Frame pacing and the illustrative remote compile delay belong to the compositor
//! of the comparison, not to the editor. Timings here include host settling.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Instant;

use typst_editor::TypstEditorShell;
use typst_editor::actions::TypstEffect;
use typst_editor::app::TypstApp;
use typst_editor::samples::{DEFAULT_INDEX, SAMPLES};
use ui::framework::ResourceApp as _;
use ui::framework::testing::TestApp;
use ui::offscreen::OffscreenRenderer;
use ui::presence::capability::presence_key;
use ui::theme::Theme;

const TAIL: &str = " - 15 tau^4 + 6 tau^5";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = PathBuf::from(std::env::args().nth(1).ok_or("expected output directory")?);
    std::fs::create_dir_all(&output)?;
    let mut app = TestApp::<TypstEditorShell>::new()?;
    for invoke in TypstEditorShell::scenario(None) {
        app.invoke(invoke)?;
    }
    app.harness().resize((800, 794))?;
    let result = capture(&mut app, &output);
    app.harness().close_components();
    result
}

fn prepare_document(
    app: &mut TestApp<TypstEditorShell>,
) -> Result<(ui::tilo_resource::Id, String, usize), Box<dyn std::error::Error>> {
    let sample = SAMPLES.get(DEFAULT_INDEX).ok_or("missing note")?;
    let file = app
        .state()
        .files
        .iter()
        .find(|(_, file)| file.name.0 == sample.name)
        .map(|(id, _)| id.clone())
        .ok_or("missing note file")?;
    app.effect_on(
        &format!("typst.project-file:{file}"),
        &TypstEffect::OpenProjectFile {
            id: file.to_string(),
        },
    )?;
    let source = sample
        .source
        .replace(TAIL, "")
        .replace("$ s(tau) = 10 tau^3 $", "$ s(tau) = 10 tau^3\n  $");
    let path = typst_editor::Workspace::paths().files.item(&file).source;
    let edit = path.splice(0, sample.source.chars().count(), &source);
    app.invoke(ui::tilo_resource::Invoke::edit(&edit)?)?;
    let insertion = source.find("\n  $").ok_or("missing equation")?;
    let insertion = source
        .get(..insertion)
        .ok_or("invalid offset")?
        .chars()
        .count();
    Ok((file, source, insertion))
}

fn capture(
    app: &mut TestApp<TypstEditorShell>,
    output: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut theme = Theme::dark();
    // A comparison has only half a landscape canvas. This theme keeps both
    // production panes visible rather than switching to the narrow view picker.
    theme.tokens.metrics.compact_breakpoint = 640.0;
    app.harness().set_theme(theme.clone());
    let (file, source, insertion) = prepare_document(app)?;
    let key = presence_key(app.surface_mut::<TypstApp>().source_widget())
        .ok_or("missing source presence scope")?;
    let caret = source.find("A good transition").ok_or("missing abstract")?;
    app.harness().update_presence(|presence| {
        presence.set_display_name(ui::UserId("ada".into()), "Ada");
        presence.set_caret(&key, ui::UserId("ada".into()), caret);
        true
    });
    app.harness().relayout();
    app.harness().drag_at((376.0, 400.0), (300.0, 400.0));
    let node = app
        .harness()
        .a11y_node_named("Typst source")
        .ok_or("missing source node")?;
    app.harness()
        .perform_a11y_action(node, &ui::a11y::Action::Focus)?;
    app.harness().perform_a11y_action(
        node,
        &ui::a11y::Action::SetSelection {
            anchor: insertion,
            focus: insertion,
        },
    )?;
    app.settle();
    app.harness()
        .wait_for_visuals(ui::time::Duration::from_secs(10))?;
    let size = app.harness().frame_output().device_size;
    let mut renderer = OffscreenRenderer::try_new(size.0, size.1)?;
    let background = theme.tokens.colors.surfaces.content;
    let _ = app.harness().render_rgba_with(&mut renderer, background);
    renderer.render_png(
        app.harness().scene(),
        background,
        output.join("right-000.png"),
    );
    let mut timings = String::from("step,character,settle_ms\n");
    for (index, ch) in TAIL.chars().enumerate() {
        let started = Instant::now();
        let step = index.checked_add(1).ok_or("frame index overflow")?;
        app.harness().type_text(&ch.to_string());
        app.settle();
        // A superscript is incomplete until its next character. Keep the real
        // last-good preview rather than manufacturing a render for invalid math.
        if ch != '^' {
            app.harness()
                .wait_for_visuals(ui::time::Duration::from_secs(10))?;
        }
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        // A transient incomplete exponent can have opened diagnostics; once
        // resolved, close that panel through its normal published action.
        if app.harness().a11y_node_named("Close problems").is_some()
            && app.harness().a11y_node_named("Problems (1)").is_none()
        {
            app.effect(&TypstEffect::Problems)?;
        }
        app.harness().relayout();
        let _ = app.harness().render_rgba_with(&mut renderer, background);
        renderer.render_png(
            app.harness().scene(),
            background,
            output.join(format!("right-{step:03}.png")),
        );
        writeln!(timings, "{step},{ch} ,{elapsed:.3}")?;
        if app
            .harness()
            .presence()
            .caret_for(&key, &ui::UserId("ada".into()))
            != Some(caret)
        {
            return Err("typing moved Ada's caret".into());
        }
    }
    std::fs::write(output.join("timings.csv"), timings)?;
    let state = app.state();
    let final_source = &state.file(&file).ok_or("file disappeared")?.source;
    if !final_source.contains(TAIL) {
        return Err("equation edit did not persist".into());
    }
    Ok(())
}
