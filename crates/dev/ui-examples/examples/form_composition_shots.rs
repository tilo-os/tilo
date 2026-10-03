//! Automatic, guided and equivalent primitive account-form source fixtures.
//! `cargo run -p ui-examples --example form-composition-shots -- /tmp/form-composition`
use std::path::PathBuf;
use tilo_ui::headless::FrameHarness;
use tilo_ui::theme::Theme;
use tilo_ui::tokens::TextRole;
use tilo_ui::widget::{
    Actions, Button, Checkbox, Column, Container, Density, Element, Field, Form, FormPage,
    FormSection, LabelPlacement, Row, Scrollable, Text, TextInput,
};

const NAME: &str = "Full name";
const EMAIL: &str = "Email address for account recovery";
const NAME_HELP: &str = "This name appears on your shared documents and team workspace.";
const EMAIL_HELP: &str =
    "We use this address to help you sign in and send important account notifications.";
const EMAIL_ERROR: &str = "Enter a valid email address, including the domain after the at sign.";
const UPDATES: &str = "Send me occasional product updates and tips for working with my team.";
const DESCRIPTION: &str = "Manage your personal details and notification preferences.";

fn name_input() -> TextInput {
    let mut input = TextInput::local().with_id("name");
    input.set_text("Alex Morgan");
    input
}
fn email_input() -> TextInput {
    let mut input = TextInput::local().with_id("email");
    input.set_text("alex@");
    input
}
fn actions() -> Actions {
    Actions::new(Button::new("save", "Save account changes", None).primary())
        .secondary(Button::new("cancel", "Cancel", None))
}
// Automatic authoring: content, grouping and order; no layout pixels or breakpoints.
fn account() -> Form {
    Form::new()
        .section(
            FormSection::new("Profile")
                .field(Field::new(NAME, name_input()).with_help(NAME_HELP))
                .field(
                    Field::new(EMAIL, email_input())
                        .with_help(EMAIL_HELP)
                        .with_error(EMAIL_ERROR),
                ),
        )
        .section(FormSection::new("Preferences").content(Box::new(
            Checkbox::new("updates", "updates").with_label(UPDATES),
        )))
        .with_actions(actions())
}
fn automatic() -> Element {
    Box::new(FormPage::new("Account", account()).with_description(DESCRIPTION))
}
// Two independent choices; remaining page/field/action behavior stays automatic.
fn guided() -> Element {
    Box::new(
        FormPage::new(
            "Account",
            account()
                .with_density(Some(Density::Compact))
                .with_label_placement(Some(LabelPlacement::Above)),
        )
        .with_description(DESCRIPTION),
    )
}

// Frozen primitive comparison. Same content, controls, order, actions, scrolling,
// readable width, hierarchy and wide/narrow intent; exact pixel parity is not claimed.
fn primitive_field(
    label: &str,
    input: TextInput,
    help: &str,
    error: Option<&str>,
    stacked: bool,
) -> Element {
    let label_text = Text::new(label).with_role(TextRole::Label);
    let mut body = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(4.0)
        .push(Box::new(input.with_accessible_name(label)))
        .push(Box::new(
            Text::new(help).with_role(TextRole::Caption).dimmed(),
        ));
    if let Some(error) = error {
        body = body.push(Box::new(Text::new(error).with_role(TextRole::Caption)));
    }
    if stacked {
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(8.0)
                .push(Box::new(label_text))
                .push(Box::new(body)),
        )
    } else {
        Box::new(
            Row::new()
                .no_wrap()
                .push(Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_width(240.0)
                        .with_child(Box::new(label_text)),
                ))
                .push(Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_flex_grow()
                        .with_child(Box::new(body)),
                )),
        )
    }
}
fn primitive(width: u32, scale: f32) -> Element {
    let stacked = width < 680 || scale > 1.0;
    let profile = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(24.0)
        .push(Box::new(
            Text::new("Profile")
                .with_role(TextRole::Title)
                .with_heading_level(2),
        ))
        .push(primitive_field(
            NAME,
            name_input(),
            NAME_HELP,
            None,
            stacked,
        ))
        .push(primitive_field(
            EMAIL,
            email_input(),
            EMAIL_HELP,
            Some(EMAIL_ERROR),
            stacked,
        ));
    let preferences = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(24.0)
        .push(Box::new(
            Text::new("Preferences")
                .with_role(TextRole::Title)
                .with_heading_level(2),
        ))
        .push(Box::new(
            Checkbox::new("updates", "updates").with_label(UPDATES),
        ));
    let sections = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(32.0)
        .push(Box::new(profile))
        .push(Box::new(preferences))
        .push(Box::new(actions()));
    let heading = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(8.0)
        .push(Box::new(
            Text::new("Account")
                .with_role(TextRole::Heading)
                .with_heading_level(1),
        ))
        .push(Box::new(Text::new(DESCRIPTION).with_role(TextRole::Body)));
    let content = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(32.0)
        .push(Box::new(heading))
        .push(Box::new(sections));
    let scroll = Box::new(
        Scrollable::new_vertical().with_child(Box::new(
            Column::new()
                .with_intrinsic_height()
                .with_vertical_padding(24.0)
                .with_horizontal_padding(24.0, 24.0)
                .push(Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_centered_max_width(720.0)
                        .with_child(Box::new(content)),
                )),
        )),
    );
    Box::new(
        Container::new()
            .with_no_padding()
            .filling()
            .with_child(scroll),
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
        .map_or_else(|| PathBuf::from("/tmp/form-composition"), PathBuf::from);
    std::fs::create_dir_all(&output)?;
    for (name, viewport, scale) in [
        ("wide", (1080, 800), 1.0),
        ("narrow", (340, 800), 1.0),
        ("large-text", (1080, 1100), 2.0),
        ("narrow-large-text", (340, 900), 2.0),
    ] {
        for mode in ["automatic", "guided", "primitive"] {
            let root = match mode {
                "guided" => guided(),
                "primitive" => primitive(viewport.0, scale),
                _ => automatic(),
            };
            let mut harness = FrameHarness::new_with_theme(
                tilo_ui::State::new(),
                root,
                viewport,
                shot_theme().with_text_scale(scale),
            );
            let stem = format!("{mode}-{name}");
            harness.render_window_png(&output.join(format!("{stem}.png")));
            std::fs::write(
                output.join(format!("{stem}.json")),
                harness.inspection_json()?,
            )?;
            if name == "narrow-large-text" {
                harness.click_at(5.0, 5.0);
                harness.scroll(0.0, -10000.0);
                harness.render_window_png(&output.join(format!("{stem}-end.png")));
                std::fs::write(
                    output.join(format!("{stem}-end.json")),
                    harness.inspection_json()?,
                )?;
            }
        }
    }
    let mut resized = FrameHarness::new_with_theme(
        tilo_ui::State::new(),
        automatic(),
        (1080, 800),
        shot_theme(),
    );
    resized.resize((340, 800))?;
    resized.render_window_png(&output.join("automatic-resized-narrow.png"));
    std::fs::write(
        output.join("automatic-resized-narrow.json"),
        resized.inspection_json()?,
    )?;
    println!("Form composition captures: {}", output.display());
    Ok(())
}
