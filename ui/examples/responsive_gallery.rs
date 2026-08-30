//! Responsive gallery — shows the toolkit's flex/wrap behaviour and CRDT
//! session persistence, now also exercising the newer shared components
//! (SearchField, Chip, StatTile, Banner) in a layout that reflows as the
//! window narrows. Chips and stat tiles wrap; the toolbar buttons wrap.
//!
//! Run:  cargo run -p ui --example ui-lab -- responsive_gallery

use std::fs;

use ui::i18n::I18nManager;
use ui::icons::Icon;
use ui::theme::Theme;
use ui::widget::{
    Banner, Button, Chip, Column, Container, Row, SearchField, Spacer, StatTile, Text, TextInput,
    Trend,
};
use ui::{App, State};

pub fn run(_args: &[String]) {
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
        if state.doc.get(&state.root, "text_val").unwrap().is_none() {
            let text_id = state
                .doc
                .put_object(&state.root, "text_val", automerge::ObjType::Text)
                .unwrap();
            let _ = state
                .doc
                .splice_text(&text_id, 0, 0, "Edit me and restart!");
        }
    }

    let root_widget = Box::new(
        Column::new()
            .with_gap(14.0)
            // Header: title + session buttons.
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
            // An inline banner spanning the width.
            .push(Box::new(Banner::info("Resize the window").with_message(
                "The chips and stat tiles below reflow as the width changes.",
            )))
            // A search field + a persisted text field inside a card.
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
            // Wrapping filter chips.
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
            // Wrapping stat tiles.
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
            // Wrapping action buttons (original responsive demo).
            .push(Box::new(
                Row::new()
                    .push(Box::new(Button::new("b1", "Wrap Item 1", None)))
                    .push(Box::new(Button::new("b2", "Wrap Item 2", None)))
                    .push(Box::new(Button::new("b3", "Wrap Item 3", None)))
                    .push(Box::new(Button::new("b4", "Wrap Item 4", None)))
                    .push(Box::new(Button::new("b5", "Wrap Item 5", None))),
            )),
    );

    let app = App::new(state, root_widget);
    app.run();
}
