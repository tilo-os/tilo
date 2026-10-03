//! Pure manual and mixed composition, using the same account content as Forms.
//! `cargo run -p ui-examples --example manual-composition-shots -- /tmp/manual-composition`
use std::path::PathBuf;
use taffy::Size;
use tilo_ui::headless::FrameHarness;
use tilo_ui::tokens::TextRole;
use tilo_ui::widget::{
    Actions, Button, Checkbox, Container, Element, Field, Form, FormPage, FormSection, Manual,
    ManualContext, MeasureConstraint as M, Scrollable, Text, TextInput, WidgetLayoutError,
};
use tilo_ui::{State, Theme};
use vello::kurbo::Rect;
const NAME: &str = "Full name";
const EMAIL: &str = "Email address for account recovery";
const NAME_HELP: &str = "This name appears on your shared documents and team workspace.";
const EMAIL_HELP: &str =
    "We use this address to help you sign in and send important account notifications.";
const EMAIL_ERROR: &str = "Enter a valid email address, including the domain after the at sign.";
const UPDATES: &str = "Send me occasional product updates and tips for working with my team.";
const DESCRIPTION: &str = "Manage your personal details and notification preferences.";
fn input(id: &str, text: &str, label: &str) -> TextInput {
    let mut input = TextInput::local().with_id(id).with_accessible_name(label);
    input.set_text(text);
    input
}
fn measured(
    ctx: &mut ManualContext<'_>,
    index: usize,
    width: f32,
) -> Result<Size<f32>, WidgetLayoutError> {
    Ok(ctx
        .measure(
            index,
            Size {
                width: M::Exact(width),
                height: M::Intrinsic,
            },
        )?
        .size)
}
fn place(
    ctx: &mut ManualContext<'_>,
    index: usize,
    x: f32,
    y: f32,
    size: Size<f32>,
) -> Result<(), WidgetLayoutError> {
    ctx.place(
        index,
        Rect::from_origin_size(
            (f64::from(x), f64::from(y)),
            (f64::from(size.width), f64::from(size.height)),
        ),
    )
}
fn line(
    ctx: &mut ManualContext<'_>,
    index: usize,
    x: f32,
    y: &mut f32,
    width: f32,
    gap: f32,
) -> Result<(), WidgetLayoutError> {
    let size = measured(ctx, index, width)?;
    place(ctx, index, x, *y, size)?;
    *y += size.height + gap;
    Ok(())
}
fn manual_geometry(
    ctx: &mut ManualContext<'_>,
    scale: f32,
) -> Result<Size<f32>, WidgetLayoutError> {
    let width = (ctx.available().width - 48.0).clamp(0.0, 720.0);
    let x = (ctx.available().width - width) / 2.0;
    let mut y = 24.0;
    line(ctx, 0, x, &mut y, width, 8.0)?;
    line(ctx, 1, x, &mut y, width, 32.0)?;
    line(ctx, 2, x, &mut y, width, 24.0)?;
    let stacked = width < 632.0 || scale > 1.0;
    for (label, control, help, error) in [(3, 4, 5, None), (6, 7, 8, Some(9))] {
        let control_x = if stacked { x } else { x + 252.0 };
        let control_width = if stacked { width } else { width - 252.0 };
        let label_size = measured(ctx, label, if stacked { width } else { 240.0 })?;
        let control_size = measured(ctx, control, control_width)?;
        if stacked {
            place(ctx, label, x, y, label_size)?;
            y += label_size.height + 8.0;
        } else {
            place(
                ctx,
                label,
                x,
                y + ((control_size.height - label_size.height) / 2.0).max(0.0),
                label_size,
            )?;
        }
        place(ctx, control, control_x, y, control_size)?;
        y += control_size.height + 4.0;
        line(ctx, help, control_x, &mut y, control_width, 4.0)?;
        if let Some(error) = error {
            line(ctx, error, control_x, &mut y, control_width, 4.0)?;
        }
        y += 20.0;
    }
    y += 8.0;
    line(ctx, 10, x, &mut y, width, 24.0)?;
    line(ctx, 11, x, &mut y, width, 32.0)?;
    let cancel = ctx
        .measure(
            12,
            Size {
                width: M::AtMost(width),
                height: M::Intrinsic,
            },
        )?
        .size;
    let save = ctx
        .measure(
            13,
            Size {
                width: M::AtMost(width),
                height: M::Intrinsic,
            },
        )?
        .size;
    if cancel.width + 12.0 + save.width <= width {
        place(
            ctx,
            12,
            x + width - save.width - 12.0 - cancel.width,
            y,
            cancel,
        )?;
        place(ctx, 13, x + width - save.width, y, save)?;
        y += cancel.height.max(save.height);
    } else {
        place(ctx, 12, x + width - cancel.width, y, cancel)?;
        y += cancel.height + 12.0;
        place(ctx, 13, x + width - save.width, y, save)?;
        y += save.height;
    }
    Ok(Size {
        width: ctx.available().width,
        height: y + 24.0,
    })
}
/// Entirely manual: individual labels, controls, support copy and both buttons.
/// Responsive rules and geometry are authored here; no Form/Actions layout.
fn manual_account(scale: f32) -> Element {
    let region = Manual::new(move |ctx| manual_geometry(ctx, scale))
        .child(Box::new(
            Text::new("Account")
                .with_role(TextRole::Heading)
                .with_heading_level(1),
        ))
        .child(Box::new(Text::new(DESCRIPTION)))
        .child(Box::new(
            Text::new("Profile")
                .with_role(TextRole::Title)
                .with_heading_level(2),
        ))
        .child(Box::new(Text::new(NAME).with_role(TextRole::Label)))
        .child(Box::new(input("name", "Alex Morgan", NAME)))
        .child(Box::new(
            Text::new(NAME_HELP).with_role(TextRole::Caption).dimmed(),
        ))
        .child(Box::new(Text::new(EMAIL).with_role(TextRole::Label)))
        .child(Box::new(input("email", "alex@", EMAIL)))
        .child(Box::new(
            Text::new(EMAIL_HELP).with_role(TextRole::Caption).dimmed(),
        ))
        .child(Box::new(
            Text::new(EMAIL_ERROR)
                .with_role(TextRole::Caption)
                .with_live(tilo_ui::a11y::Politeness::Polite),
        ))
        .child(Box::new(
            Text::new("Preferences")
                .with_role(TextRole::Title)
                .with_heading_level(2),
        ))
        .child(Box::new(
            Checkbox::new("updates", "updates").with_label(UPDATES),
        ))
        .child(Box::new(
            Button::new("cancel", "Cancel", None).content_sized(),
        ))
        .child(Box::new(
            Button::new("save", "Save account changes", Some("saved"))
                .primary()
                .content_sized(),
        ));
    surface(Box::new(region))
}
fn surface(child: Element) -> Element {
    Box::new(
        Container::new()
            .with_no_padding()
            .filling()
            .with_child(Box::new(Scrollable::new_vertical().with_child(child))),
    )
}
fn account() -> Form {
    Form::new()
        .section(
            FormSection::new("Profile")
                .field(Field::new(NAME, input("name", "Alex Morgan", NAME)).with_help(NAME_HELP))
                .field(
                    Field::new(EMAIL, input("email", "alex@", EMAIL))
                        .with_help(EMAIL_HELP)
                        .with_error(EMAIL_ERROR),
                ),
        )
        .section(FormSection::new("Preferences").content(Box::new(
            Checkbox::new("updates", "updates").with_label(UPDATES),
        )))
        .with_actions(
            Actions::new(Button::new("save", "Save account changes", Some("saved")).primary())
                .secondary(Button::new("cancel", "Cancel", None)),
        )
}
fn automatic_in_manual() -> Element {
    surface(Box::new(
        Manual::new(|ctx| {
            let width = (ctx.available().width - 48.0).clamp(0.0, 720.0);
            let x = (ctx.available().width - width) / 2.0;
            let mut y = 24.0;
            line(ctx, 0, x, &mut y, width, 8.0)?;
            line(ctx, 1, x, &mut y, width, 32.0)?;
            line(ctx, 2, x, &mut y, width, 24.0)?;
            Ok(Size {
                width: ctx.available().width,
                height: y,
            })
        })
        .child(Box::new(
            Text::new("Account")
                .with_role(TextRole::Heading)
                .with_heading_level(1),
        ))
        .child(Box::new(Text::new(DESCRIPTION)))
        .child(Box::new(account())),
    ))
}
fn manual_in_automatic() -> Element {
    let region = Manual::new(|ctx| {
        let width = ctx.available().width;
        let mut y = 0.0;
        line(ctx, 0, 0.0, &mut y, width, 12.0)?;
        line(ctx, 1, 0.0, &mut y, width, 0.0)?;
        Ok(Size { width, height: y })
    })
    .child(Box::new(Text::new(
        "A manually positioned standard-control region.",
    )))
    .child(Box::new(
        Checkbox::new("manual-updates", "updates").with_label(UPDATES),
    ));
    Box::new(
        FormPage::new(
            "Account",
            account().section(FormSection::new("Manual region").content(Box::new(region))),
        )
        .with_description(DESCRIPTION),
    )
}
/// `TILO_SHOTS_THEME=<preset>` (`light`, `high-contrast`, any `Theme::named`)
/// renders every capture on that preset; PNGs are composited on the theme's
/// own `bg_app`.
fn shot_theme() -> Theme {
    std::env::var("TILO_SHOTS_THEME")
        .ok()
        .and_then(|name| Theme::named(&name))
        .unwrap_or_else(Theme::dark)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::args_os()
        .nth(1)
        .map_or_else(|| PathBuf::from("/tmp/manual-composition"), PathBuf::from);
    std::fs::create_dir_all(&output)?;
    for (name, viewport, scale) in [
        ("wide", (1080, 800), 1.0),
        ("narrow", (340, 800), 1.0),
        ("large-text", (1080, 1100), 2.0),
        ("narrow-large-text", (340, 900), 2.0),
    ] {
        for mode in ["manual", "automatic-in-manual", "manual-in-automatic"] {
            let root = match mode {
                "automatic-in-manual" => automatic_in_manual(),
                "manual-in-automatic" => manual_in_automatic(),
                _ => manual_account(scale),
            };
            let mut harness = FrameHarness::new_with_theme(
                State::new(),
                root,
                viewport,
                shot_theme().with_text_scale(scale),
            );
            harness.render_window_png(&output.join(format!("{mode}-{name}.png")));
            std::fs::write(
                output.join(format!("{mode}-{name}.json")),
                serde_json::to_vec_pretty(&harness.inspection())?,
            )?;
            harness.click_at(5.0, 5.0);
            harness.scroll(0.0, -10000.0);
            harness.render_window_png(&output.join(format!("{mode}-{name}-end.png")));
            std::fs::write(
                output.join(format!("{mode}-{name}-end.json")),
                serde_json::to_vec_pretty(&harness.inspection())?,
            )?;
        }
    }
    for mode in ["manual", "automatic-in-manual", "manual-in-automatic"] {
        let root = match mode {
            "automatic-in-manual" => automatic_in_manual(),
            "manual-in-automatic" => manual_in_automatic(),
            _ => manual_account(1.0),
        };
        let mut resized =
            FrameHarness::new_with_theme(State::new(), root, (1080, 800), shot_theme());
        resized.resize((340, 800))?;
        resized.render_window_png(&output.join(format!("{mode}-resized-narrow.png")));
        std::fs::write(
            output.join(format!("{mode}-resized-narrow.json")),
            resized.inspection_json()?,
        )?;
    }
    Ok(())
}
