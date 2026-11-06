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
    for (theme_name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
        let mut card = ProfileCard::new();
        card.toggle(
            &ada(),
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
        let path = output.join(format!("someone-elses-card-{theme_name}.png"));
        Framed::harness(&frame).render_png(&path);
        println!("{theme_name}: {}", path.display());
    }
    Ok(())
}
