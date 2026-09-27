//! Responsive gallery showing flex/wrap behavior and CRDT session persistence.
//! `SearchField`, `Chip`, `StatTile`, and `Banner` reflow as the window narrows.
//!
//! Run:  cargo run -p ui-examples --example ui-lab -- `responsive_gallery`

use std::fs;

use tilo_ui::i18n::I18nManager;
use tilo_ui::icons::Icon;
use tilo_ui::theme::Theme;
use tilo_ui::widget::{
    Banner, Button, Chip, Column, Container, Row, SearchField, Spacer, StatTile, Text, TextInput,
    Trend, Widget,
};
use tilo_ui::{App, State};

pub fn run(_args: &[String]) {
    let wake_scope = tilo_ui::wake::WakeScope::new();
    let _wake_entry = wake_scope.enter();
    I18nManager::init("en-US");
    I18nManager::add_resource(
        "en-US",
        "title = Responsive Gallery\nsave = Save Session\nload = Load Session\nwelcome = Welcome to CRDT-Native UI!".to_string(),
    );

    let theme = Theme::dark();
    let state_file = "session.crdt";

    let mut state = if let Ok(data) = fs::read(state_file) {
        State::load(&data).unwrap_or_else(|_| State::new())
    } else {
        State::new()
    };

    {
        use automerge::ReadDoc;
        use automerge::transaction::Transactable;
        let _ = state.doc.put(&state.root, "request_save", false);
        let _ = state.doc.put(&state.root, "request_load", false);
        let has_text = state
            .doc
            .get(&state.root, "text_val")
            .ok()
            .flatten()
            .is_some();
        if !has_text {
            match state
                .doc
                .put_object(&state.root, "text_val", automerge::ObjType::Text)
            {
                Ok(text_id) => {
                    let _ = state
                        .doc
                        .splice_text(&text_id, 0, 0, "Edit me and restart!");
                }
                Err(err) => {
                    eprintln!("responsive_gallery: failed to seed text_val: {err}");
                }
            }
        }
    }

    let root_widget = build_root_widget(&theme);

    let app = App::from_scoped_parts(wake_scope, state, root_widget);
    app.run();
}

/// Build the reflowing gallery layout: header, banner, search + persisted
/// text card, wrapping filter chips, wrapping stat tiles, and the original
/// wrapping action-button row.
fn build_root_widget(theme: &Theme) -> Box<dyn Widget> {
    Box::new(
        Column::new()
            .with_gap(14.0)
            .push(Box::new(
                Row::new()
                    .push(Box::new(Text::new("title").with_size(24.0)))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(Button::new(
                        "btn_save",
                        "save",
                        Some("request_save"),
                    )))
                    .push(Box::new(Button::new(
                        "btn_load",
                        "load",
                        Some("request_load"),
                    ))),
            ))
            .push(Box::new(Banner::info("Resize the window").with_message(
                "The chips and stat tiles below reflow as the width changes.",
            )))
            .push(Box::new(
                Container::new().with_background().with_child(Box::new(
                    Column::new()
                        .with_no_padding()
                        .with_intrinsic_height()
                        .with_gap(10.0)
                        .push(Box::new(Text::new("welcome")))
                        .push(Box::new(SearchField::new().with_placeholder("Search")))
                        .push(Box::new(
                            TextInput::new(vec![])
                                .with_id("inp_1")
                                .with_on_change_path("text_val"),
                        )),
                )),
            ))
            .push(Box::new(
                Row::new()
                    .push(Box::new(Chip::new("All").selected(true)))
                    .push(Box::new(
                        Chip::new("Design")
                            .selectable()
                            .with_dot(theme.tokens.colors.semantic.info),
                    ))
                    .push(Box::new(Chip::new("Engineering").selectable()))
                    .push(Box::new(Chip::new("Research").selectable()))
                    .push(Box::new(Chip::new("archived").removable())),
            ))
            .push(Box::new(
                Row::new()
                    .push(Box::new(
                        StatTile::new("Sessions", "12")
                            .with_icon(Icon::List)
                            .with_delta("3", Trend::Up),
                    ))
                    .push(Box::new(
                        StatTile::new("Saved", "48 KB").with_icon(Icon::Check),
                    ))
                    .push(Box::new(
                        StatTile::new("Peers", "2")
                            .with_icon(Icon::Bell)
                            .with_delta("1", Trend::Down),
                    )),
            ))
            .push(Box::new(
                Row::new()
                    .push(Box::new(Button::new("b1", "Wrap Item 1", None)))
                    .push(Box::new(Button::new("b2", "Wrap Item 2", None)))
                    .push(Box::new(Button::new("b3", "Wrap Item 3", None)))
                    .push(Box::new(Button::new("b4", "Wrap Item 4", None)))
                    .push(Box::new(Button::new("b5", "Wrap Item 5", None))),
            )),
    )
}
