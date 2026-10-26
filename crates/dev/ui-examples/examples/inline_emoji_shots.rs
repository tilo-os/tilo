//! A workspace's own emoji inside text: chat-like message bodies and a
//! composer ([`tilo_ui::widget::RichTextLine`], wrapped, with marks), a
//! label drawn through [`TextRenderer::draw_style`] and a cursor reaction,
//! each writing `:name:` of emoji a registered workspace has, in dark and
//! light, at one and two device pixels per point. A `:name:` the workspace
//! lacks, and inline code, stay text.
//!
//! Run:  `cargo run -p ui-examples --example ui-shots -- inline_emoji_shots`
//! Out:  `$INLINE_EMOJI_SHOTS_OUT` (default `/tmp/inline_emoji_shots`)

use std::cell::Cell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::presence::{PresenceCtx, ReactionFloat, paint_reaction};
use tilo_ui::scale::composite_transform;
use tilo_ui::text_render::{TextRenderer, TextStyle, TextWeight};
use tilo_ui::theme::Theme;
use tilo_ui::tokens::TextRole;
use tilo_ui::wake::WakeScope;
use tilo_ui::widget::{Mark, MarkSpan, RichTextLine, Widget};
use tilo_ui::workspace_emoji::{CustomEmoji, EmojiSource, WorkspaceEmoji};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};

/// The emoji of the pictured workspace, as its record would hold them.
struct Workspace {
    emoji: Rc<WorkspaceEmoji>,
    revision: Cell<u64>,
}

impl EmojiSource for Workspace {
    fn emoji(&self) -> Rc<WorkspaceEmoji> {
        Rc::clone(&self.emoji)
    }
    fn revision(&self) -> u64 {
        self.revision.get()
    }
    fn can_manage(&self) -> bool {
        false
    }
    fn add(&self, _: &str, _: &[u8]) -> Result<(), String> {
        Err("a picture of a workspace".into())
    }
    fn remove(&self, _: &str) -> Result<(), String> {
        Err("a picture of a workspace".into())
    }
    fn choose_and_add(&self) {}
}

/// A 64-pixel emoji picture: `shade` gives each point of the unit square
/// (0..1 both ways) a colour, sampled four times per pixel each way.
fn picture(shade: impl Fn(f32, f32) -> [f32; 4]) -> Arc<[u8]> {
    const SIDE: u16 = 64;
    const SAMPLES: u16 = 4;
    let at = |pixel: u16, sample: u16| {
        (f32::from(pixel) + (f32::from(sample) + 0.5) / f32::from(SAMPLES)) / f32::from(SIDE)
    };
    let samples = f32::from(SAMPLES * SAMPLES);
    let mut image = image::RgbaImage::new(u32::from(SIDE), u32::from(SIDE));
    for row in 0..SIDE {
        for column in 0..SIDE {
            let mut sum = [0.0f32; 4];
            for down in 0..SAMPLES {
                for across in 0..SAMPLES {
                    let [red, green, blue, cover] = shade(at(column, across), at(row, down));
                    sum = [
                        sum[0] + red * cover,
                        sum[1] + green * cover,
                        sum[2] + blue * cover,
                        sum[3] + cover,
                    ];
                }
            }
            let alpha = sum[3] / samples;
            let channel = |premultiplied: f32| {
                if alpha > 0.0 {
                    byte(premultiplied / samples / alpha)
                } else {
                    0
                }
            };
            image.put_pixel(
                u32::from(column),
                u32::from(row),
                image::Rgba([
                    channel(sum[0]),
                    channel(sum[1]),
                    channel(sum[2]),
                    byte(alpha),
                ]),
            );
        }
    }
    let mut png = std::io::Cursor::new(Vec::new());
    if let Err(error) = image.write_to(&mut png, image::ImageFormat::Png) {
        eprintln!("encode emoji picture: {error}");
    }
    Arc::from(png.into_inner())
}

/// A colour channel `channel` (0..1) as a byte.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the channel is clamped to 0..=255 first"
)]
fn byte(channel: f32) -> u8 {
    (channel.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// `len` logical pixels at `scale` device pixels each.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a screen's side in device pixels fits a u32"
)]
fn physical(len: u32, scale: f64) -> u32 {
    (f64::from(len) * scale).round().max(0.0) as u32
}

fn within(u: f32, v: f32, cx: f32, cy: f32, r: f32) -> bool {
    (u - cx).powi(2) + (v - cy).powi(2) <= r * r
}

fn workspace_emoji() -> WorkspaceEmoji {
    // A violet-to-coral disc with a white ring: the workspace's mark.
    let tilo = picture(|u, v| {
        if !within(u, v, 0.5, 0.5, 0.48) {
            return [0.0; 4];
        }
        if within(u, v, 0.5, 0.5, 0.24) && !within(u, v, 0.5, 0.5, 0.14) {
            return [1.0, 1.0, 1.0, 1.0];
        }
        let t = f32::midpoint(u, v);
        [0.45 + 0.5 * t, 0.30, 0.95 - 0.45 * t, 1.0]
    });
    // A green bird's head: an eye and an orange beak.
    let parrot = picture(|u, v| {
        if within(u, v, 0.40, 0.36, 0.08) {
            return [1.0, 1.0, 1.0, 1.0];
        }
        if within(u, v, 0.42, 0.37, 0.04) {
            return [0.1, 0.1, 0.1, 1.0];
        }
        let beak = u > 0.62 && u < 0.92 && (v - 0.5).abs() < 0.16 * (0.92 - u) / 0.3;
        if beak {
            return [1.0, 0.62, 0.12, 1.0];
        }
        if within(u, v, 0.45, 0.52, 0.42) {
            return [0.18, 0.78 - 0.3 * v, 0.40, 1.0];
        }
        [0.0; 4]
    });
    // A rocket-red rounded square with a white arrow: "ship it".
    let shipit = picture(|u, v| {
        let (dx, dy) = ((u - 0.5).abs() - 0.32, (v - 0.5).abs() - 0.32);
        let outside = dx.max(0.0).hypot(dy.max(0.0)) > 0.14;
        if outside {
            return [0.0; 4];
        }
        let shaft = (u - 0.5).abs() < 0.07 && v > 0.36 && v < 0.78;
        let head = v > 0.18 && v < 0.42 && (u - 0.5).abs() < (v - 0.18) * 0.95;
        if shaft || head {
            return [1.0, 1.0, 1.0, 1.0];
        }
        [0.92, 0.26, 0.24, 1.0]
    });
    WorkspaceEmoji::new(vec![
        CustomEmoji {
            name: "tilo".into(),
            image: tilo,
        },
        CustomEmoji {
            name: "party_parrot".into(),
            image: parrot,
        },
        CustomEmoji {
            name: "shipit".into(),
            image: shipit,
        },
    ])
}

/// Lay `line` out `width` wide at `origin` through the real taffy pass;
/// returns its height.
fn place(line: &mut RichTextLine, theme: &Theme, origin: Point, width: f32) -> f64 {
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut nodes: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let mut text = TextRenderer::new();
    let node = line.build_layout(&mut taffy, theme, &mut nodes);
    let laid_out = taffy.compute_layout_with_measure(
        node,
        Size {
            width: AvailableSpace::Definite(width),
            height: AvailableSpace::MaxContent,
        },
        |known, available, node_id, _, _| {
            nodes.get(&node_id).map_or(Size::ZERO, |&p| unsafe {
                (*p).measure(known, available, &mut text)
            })
        },
    );
    if let Err(error) = laid_out {
        eprintln!("line layout: {error}");
        return 0.0;
    }
    line.apply_layout(&taffy, node, origin);
    line.get_rect().height()
}

/// A wrapped message line writing `text`, each part of `marks` marked where
/// it is last written: inline code comes after the emoji it shows.
fn message(text: &str, marks: Vec<(&str, Mark)>) -> RichTextLine {
    let marks = marks
        .into_iter()
        .filter_map(|(part, mark)| {
            let start = text.rfind(part)?;
            Some(MarkSpan {
                range: start..start.saturating_add(part.len()),
                mark,
            })
        })
        .collect();
    let mut line = RichTextLine::new().with_text(text);
    line.set_wrap(true);
    line.set_marks(marks);
    line
}

/// Where the conversation's text starts, and how wide it runs.
const LEFT: f64 = 96.0;
const COLUMN: f32 = 640.0;

/// One picture being painted.
struct Frame<'a> {
    theme: &'a Theme,
    transform: Affine,
    text: TextRenderer,
    scene: Scene,
    presence: PresenceCtx,
}

impl Frame<'_> {
    fn label(&mut self, at: Point, words: &str, style: &TextStyle, ink: Color) {
        self.text.draw_style(
            &mut self.scene,
            words,
            ink,
            self.transform.pre_translate(at.to_vec2()),
            None,
            style,
        );
    }

    /// Lay `line` out at `origin`, `width` wide, and paint it; returns its
    /// height.
    fn line(&mut self, line: &mut RichTextLine, origin: Point, width: f32) -> f64 {
        let height = place(line, self.theme, origin, width);
        line.draw(
            &mut self.scene,
            self.transform,
            &mut self.text,
            self.theme,
            &self.presence,
        );
        height
    }
}

/// The channel's topic and its messages; returns where they end.
fn conversation(frame: &mut Frame<'_>) -> f64 {
    let theme = frame.theme;
    let colors = &theme.tokens.colors;
    let name_style = theme.text(TextRole::Body).with_weight(TextWeight::Bold);
    let meta_style = theme.text(TextRole::Caption);
    frame.label(
        Point::new(40.0, 28.0),
        "# launch \u{2014} Topic: ship week :shipit: :tilo:",
        &theme.text(TextRole::Label),
        colors.text_tertiary,
    );

    let said = [
        (
            "Ada",
            "Shipped the new build :party_parrot: thanks everyone!",
            vec![],
        ),
        (
            "Grace",
            "Release is green :tilo: :tilo: \u{2014} @Ada can you tag it? :nope: stays text, and so does code: :tilo:",
            vec![
                ("Release is green", Mark::Bold),
                ("@Ada", Mark::Mention("ada".into())),
                (":tilo:", Mark::Code),
            ],
        ),
        (
            "Linus",
            "A longer note so the emoji meet the wrap: we moved the sync worker onto the shared runtime :shipit: and the cold start dropped by a third, which means the first window paints before the splash would have :party_parrot: and nobody waits on fonts anymore :tilo:",
            vec![],
        ),
        ("Ada", ":party_parrot:", vec![]),
    ];
    let mut y = 76.0;
    for (who, body, marks) in said {
        let hue = match who {
            "Ada" => Color::from_rgb8(0xE0, 0x6C, 0x4F),
            "Grace" => Color::from_rgb8(0x4F, 0x8C, 0xE0),
            _ => Color::from_rgb8(0x5B, 0xB0, 0x7A),
        };
        frame.scene.fill(
            Fill::NonZero,
            frame.transform,
            hue,
            None,
            &Circle::new((LEFT - 30.0, y + 18.0), 16.0),
        );
        frame.label(Point::new(LEFT, y), who, &name_style, colors.text_primary);
        frame.label(
            Point::new(LEFT + 64.0, y + 2.0),
            "10:42",
            &meta_style,
            colors.text_tertiary,
        );
        let mut line = message(body, marks);
        let height = frame.line(&mut line, Point::new(LEFT, y + 22.0), COLUMN);
        y += 22.0 + height + 18.0;
    }
    y
}

/// The composer below the conversation at `y`: the draft writes emoji, one
/// of them selected.
fn composer(frame: &mut Frame<'_>, y: f64) {
    let colors = &frame.theme.tokens.colors;
    let draft = "Thanks all :tilo: \u{2014} next up is the web build :shipit:";
    let field = Rect::new(40.0, y + 8.0, 40.0 + f64::from(COLUMN) + 56.0, y + 64.0);
    frame.scene.fill(
        Fill::NonZero,
        frame.transform,
        colors.surfaces.raised,
        None,
        &RoundedRect::from_rect(field, 10.0),
    );
    let mut composer = message(draft, vec![]);
    composer.set_focus(true);
    place(
        &mut composer,
        frame.theme,
        Point::new(56.0, y + 22.0),
        COLUMN,
    );
    // A first paint shapes the draft, so the selection lands on its layout.
    composer.draw(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut frame.text,
        frame.theme,
        &frame.presence,
    );
    let at = draft.find(":tilo:").unwrap_or_default();
    composer
        .inner_mut()
        .set_selection(at.saturating_sub(4), at.saturating_add(":tilo: ".len()));
    composer.draw(
        &mut frame.scene,
        frame.transform,
        &mut frame.text,
        frame.theme,
        &frame.presence,
    );
}

/// A cursor reaction rising over the conversation.
fn reaction(frame: &mut Frame<'_>) {
    let cursor = Point::new(820.0, 300.0);
    paint_reaction(
        &mut frame.scene,
        frame.transform,
        &mut frame.text,
        frame.theme,
        cursor,
        &ReactionFloat {
            emoji: ":party_parrot:".into(),
            rise: 18.0,
            opacity: 1.0,
        },
    );
    frame.scene.fill(
        Fill::NonZero,
        frame.transform,
        frame.theme.tokens.colors.text_primary,
        None,
        &Circle::new(cursor, 3.0),
    );
}

fn shoot(theme: &Theme, logical: (u32, u32), device_scale: f64, out: &Path) {
    let mut frame = Frame {
        theme,
        transform: composite_transform(device_scale),
        text: TextRenderer::new(),
        scene: Scene::new(),
        presence: PresenceCtx::default(),
    };
    let y = conversation(&mut frame);
    composer(&mut frame, y);
    reaction(&mut frame);

    let mut renderer = OffscreenRenderer::new(
        physical(logical.0, device_scale),
        physical(logical.1, device_scale),
    );
    renderer.render_png(&frame.scene, theme.tokens.colors.surfaces.content, out);
    println!("wrote {}", out.display());
}

pub fn run(_args: &[String]) {
    let dir = std::env::var("INLINE_EMOJI_SHOTS_OUT")
        .unwrap_or_else(|_| "/tmp/inline_emoji_shots".to_string());
    if let Err(error) = std::fs::create_dir_all(&dir) {
        eprintln!("output directory {dir}: {error}");
        return;
    }
    let dir = Path::new(&dir);

    // The window: its wake scope, with the workspace's emoji registered for
    // it, as a window's host registers its workspace's emoji record.
    let window = WakeScope::new();
    let _entry = window.enter();
    tilo_ui::workspace_emoji::register(
        &window.handle(),
        Rc::new(Workspace {
            emoji: Rc::new(workspace_emoji()),
            revision: Cell::new(1),
        }),
    );

    for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
        shoot(
            &theme,
            (1440, 900),
            1.0,
            &dir.join(format!("inline-1440x900-{name}.png")),
        );
        shoot(
            &theme,
            (1280, 720),
            2.0,
            &dir.join(format!("inline-2560x1440-dpr2-{name}.png")),
        );
    }
}
