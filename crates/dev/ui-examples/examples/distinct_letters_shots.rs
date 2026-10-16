//! Distinct similar letters: where I, l, 1 and 0, O are set apart. Prose, a
//! chat line with a mention, a link and an email address found in it, a
//! TOTP code, a revealed password and a permission prompt, with "Always
//! distinguish similar letters" off and on, in the default face,
//! Hyperlegible and a probe face that has every distinct form as a feature.
//!
//! ```bash
//! cargo run -p ui-examples \
//!     --example distinct-letters-shots -- \
//!     --shots target/look/distinct-letters/specimen.png --viewport 1280x420 --scale 2
//! ```
//!
//! Writes `<stem>-<face>-<off|on>.png` next to the `--shots` path for the
//! faces `default`, `hyperlegible` and `probe`.

use std::path::{Path, PathBuf};

use vello::Scene;
use vello::kurbo::Affine;

use tilo_ui::font_catalog::FontCatalog;
use tilo_ui::headless::FrameHarness;
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::permissions::UserId;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::tokens::{TextRole, UiFace};
use tilo_ui::widget::marks::{Mark, MarkSpan};
use tilo_ui::widget::{
    Column, Container, Element, PermissionPrompt, RichTextLine, Row, Text, TextInput, TextInputKind,
};
use tilo_ui::{State, TextKind};

/// A face in the shipped catalog whose distinct I, l and slashed zero are
/// OpenType features rather than its default shapes: proof that resolution
/// follows what a face can do, whichever face it is.
const PROBE_FAMILY: &str = "Adwaita Sans";

const PROSE: &str =
    "Illinois holds 10 Ill-lit halls; all 0 of 11 lilies fell. Words disambiguate themselves.";
const CHAT: &str = "@lena_l1 the invite to #general:tilo.so is at https://paypa1.com/I0l-join — or mail Il0@example.org";

fn label(text: &str) -> Element {
    Box::new(
        Container::new()
            .with_no_padding()
            .with_width(170.0)
            .with_child(Box::new(
                Text::new(text).with_role(TextRole::Caption).dimmed(),
            )),
    )
}

fn line(name: &str, content: Element) -> Element {
    Box::new(
        Row::new()
            .no_wrap()
            .with_gap(16.0)
            .with_align_items(taffy::AlignItems::Center)
            .push(label(name))
            .push(content),
    )
}

fn sized(width: f32, child: Element) -> Element {
    Box::new(
        Container::new()
            .with_no_padding()
            .with_width(width)
            .with_child(child),
    )
}

fn chat_marked() -> Element {
    let text = "Thanks Ilya Lebedev, see the I0l docs";
    let mut line = RichTextLine::new().with_text(text);
    line.set_inline_formatting(true);
    line.set_marks(vec![
        MarkSpan::new(7..19, Mark::Mention("ilya".into())),
        MarkSpan::new(
            29..32,
            Mark::ExternalLink("https://docs.example.org/I0l".into()),
        ),
    ]);
    Box::new(line)
}

fn specimen(heading: &str) -> Element {
    let column = Column::new()
        .with_gap(14.0)
        .with_vertical_padding(28.0)
        .with_horizontal_padding(40.0, 40.0)
        .push(Box::new(Text::new(heading).with_role(TextRole::Subheading)))
        .push(line(
            "Prose",
            sized(1000.0, Box::new(Text::new(PROSE).with_role(TextRole::Body))),
        ))
        .push(line(
            "Chat, found in prose",
            sized(1000.0, Box::new(Text::new(CHAT).with_role(TextRole::Body))),
        ))
        .push(line("Chat, marked", sized(1000.0, chat_marked())))
        .push(line(
            "TOTP code",
            Box::new(
                Text::new("401 710")
                    .with_role(TextRole::Display)
                    .with_kind(TextKind::Secret),
            ),
        ))
        .push(line(
            "Revealed password",
            sized(
                320.0,
                Box::new(
                    TextInput::local()
                        .with_kind(TextInputKind::Password)
                        .with_secret(true)
                        .with_reveal(true)
                        .with_initial_value("Il1|O0o-lI0")
                        .with_accessible_name("Password"),
                ),
            ),
        ))
        .push(line(
            "Permission prompt",
            sized(
                1000.0,
                Box::new(PermissionPrompt::new(
                    "so.tilo.Il1-files0".into(),
                    "Read all 10 files in ~/Documents/Ill0".into(),
                    true,
                )),
            ),
        ));
    Box::new(
        Container::new()
            .with_no_padding()
            .filling()
            .with_child(Box::new(column)),
    )
}

fn arg(name: &str) -> Option<String> {
    let mut args = std::env::args().skip_while(|a| a != name);
    args.next()?;
    args.next()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let shots = PathBuf::from(
        arg("--shots").unwrap_or_else(|| "target/look/distinct-letters/specimen.png".into()),
    );
    let viewport = arg("--viewport")
        .and_then(|v| {
            let (w, h) = v.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or((1280, 720));
    // Rendered at twice the size by default so single letters can be told
    // apart in the PNG; `--scale 1` for the size people see.
    let scale: u32 = arg("--scale")
        .and_then(|v| v.parse().ok())
        .unwrap_or(2)
        .clamp(1, 4);
    let dir = shots.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let stem = shots
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("specimen")
        .to_owned();
    let catalog = FontCatalog::controlled_fixture(tilo_ui::font_catalog::checkout_root(), "en-US");
    let faces = [
        ("default", UiFace::Default.name()),
        ("hyperlegible", UiFace::Hyperlegible.name()),
        ("probe", PROBE_FAMILY),
    ];
    for (face_name, face) in faces {
        for on in [false, true] {
            let mut theme = Theme::light();
            theme.tokens.typography.set_face(face);
            theme.tokens.typography.distinct_letters = on;
            let heading = format!(
                "{face}: always distinguish similar letters {}",
                if on { "on" } else { "off" }
            );
            let theme_ground = theme.tokens.colors.surfaces.content;
            let harness = FrameHarness::new_with_controlled_inputs(
                State::new(),
                specimen(&heading),
                viewport,
                theme,
                TextRenderer::with_font_catalog(&catalog)?,
                UserId("specimen".into()),
            );
            let state = if on { "on" } else { "off" };
            let out = dir.join(format!("{stem}-{face_name}-{state}.png"));
            let mut scene = Scene::new();
            scene.append(harness.scene(), Some(Affine::scale(f64::from(scale))));
            OffscreenRenderer::new(
                viewport.0.saturating_mul(scale),
                viewport.1.saturating_mul(scale),
            )
            .render_png(&scene, theme_ground, &out);
            println!("{}", out.display());
        }
    }
    Ok(())
}
