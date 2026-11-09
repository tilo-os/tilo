//! Pictures of the card a person gets when they open someone else's profile.
//!
//! The card with what the person can do with them, Message first: the way
//! into the conversation they have in Chat. Captured in both themes
//! through the production frame path ([`ui::headless::Framed`]).
//!
//! ```text
//! cargo run -p tilo-desktop --example profile-card-shots -- ~/.cache/profile-card-shots
//! ```

#![allow(
    clippy::expect_used,
    clippy::print_stdout,
    reason = "a capture binary reports the files it wrote"
)]

use std::path::PathBuf;

use tilo_desktop::collaborators::{LineSource, RosterStatus};
use tilo_desktop::profile_card::{OtherPerson, ProfileCard, ProfileModel};
use ui::headless::Framed;
use ui::state::State;
use ui::theme::Theme;
use vello::kurbo::Rect;

/// The person whose card this is: someone else in the workspace, with the
/// key that lets the card open the conversation they have in Chat.
fn ada() -> ProfileModel {
    ProfileModel {
        name: "Ada Lovelace".to_owned(),
        identity: "ada".to_owned(),
        key: Some("a6d40c19e7b2f84c3d5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a7c2".to_owned()),
        devices: vec![
            tilo_desktop::here::PersonDevice {
                id: "ada-device".to_owned(),
                label: "Framework 16".to_owned(),
                this: false,
                availability: ui::Availability::Available,
            },
            tilo_desktop::here::PersonDevice {
                id: "ada-laptop".to_owned(),
                label: "ThinkPad".to_owned(),
                this: false,
                availability: ui::Availability::Focusing,
            },
        ],
        other: Some(OtherPerson {
            id: "ada-device".to_owned(),
            status: Some(RosterStatus {
                text: "Reviewing the checklist".to_owned(),
                source: LineSource::Generated,
            }),
            place: Some("In Notes".to_owned()),
            presence: Some(ui::Availability::Available),
            following: false,
            can_follow: true,
            can_message: true,
            linked: Vec::new(),
            agent: None,
        }),
        ..ProfileModel::default()
    }
}

/// An agent in the workspace: Claude Code, connected over MCP, editing a note. Its card is square
/// in its client's hue, says what it does now, and offers its card in Console and what can be done
/// with it.
fn claude() -> ProfileModel {
    ProfileModel {
        name: "Claude Code".to_owned(),
        identity: "claude-device".to_owned(),
        colour: Some(vello::peniko::Color::from_rgb8(
            ui::presence::identity_rgb(ui::tokens::PresenceHue::Orange).0,
            ui::presence::identity_rgb(ui::tokens::PresenceHue::Orange).1,
            ui::presence::identity_rgb(ui::tokens::PresenceHue::Orange).2,
        )),
        other: Some(OtherPerson {
            id: "claude-device".to_owned(),
            status: Some(RosterStatus {
                text: "Editing Storefront/checkout.rs".to_owned(),
                source: LineSource::Written,
            }),
            place: Some("In Code".to_owned()),
            presence: None,
            following: true,
            can_follow: true,
            can_message: false,
            linked: Vec::new(),
            agent: Some(tilo_desktop::profile_card::AgentFacts {
                client: "Claude Code".to_owned(),
            }),
        }),
        ..ProfileModel::default()
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::args_os()
        .nth(1)
        .map_or_else(|| PathBuf::from("profile-card-shots"), PathBuf::from);
    std::fs::create_dir_all(&output)?;
    ui::i18n::I18nManager::init("en-US");
    for ((theme_name, theme), (who, model)) in [("dark", Theme::dark()), ("light", Theme::light())]
        .into_iter()
        .flat_map(|theme| {
            [
                (theme.clone(), ("someone-elses", ada())),
                (theme, ("agents", claude())),
            ]
        })
    {
        let mut card = ProfileCard::new();
        card.toggle(
            &model,
            Rect::new(1000.0, 2.0, 1020.0, 22.0),
            Rect::new(0.0, 0.0, 1280.0, 720.0),
        );
        let mut frame = Framed::new(State::new(), card, (1280, 720), theme);
        let harness = Framed::harness_mut(&mut frame);
        harness.relayout();
        // The card's entrance runs: draw frames until it rests, so the shot
        // is the card a person sees, not one caught mid-fade.
        for _ in 0..180 {
            harness.relayout();
            if !harness.poll_frame().expect("a frame draws the card") {
                break;
            }
        }
        let path = output.join(format!("{who}-card-{theme_name}.png"));
        Framed::harness(&frame).render_png(&path);
        println!("{theme_name}: {}", path.display());
    }
    Ok(())
}
