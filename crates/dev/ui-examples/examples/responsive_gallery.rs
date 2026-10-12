//! Responsive gallery showing flex/wrap behavior and CRDT session persistence.
//! `SearchField`, `Chip`, `StatTile`, and `Banner` reflow as the window narrows.
//!
//! Run:  cargo run -p ui-examples --example ui-lab -- `responsive_gallery`

use std::fs;

use tilo_ui::TextRole;
use tilo_ui::i18n::I18nManager;
use tilo_ui::theme::Theme;
use tilo_ui::widget::{
    Banner, Button, Chip, Column, Container, Row, SearchField, Spacer, StatGroup, StatTile, Text,
    TextInput, Trend, Widget,
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

    let state = if let Ok(data) = fs::read(state_file) {
        State::load(&data).unwrap_or_else(|_| State::new())
    } else {
        State::new()
    };

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
                    .push(Box::new(Text::new("title").with_role(TextRole::LargeTitle)))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(Button::new("btn_save", "save")))
                    .push(Box::new(Button::new("btn_load", "load"))),
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
                            TextInput::local()
                                .with_initial_value("Edit me")
                                .with_id("inp_1"),
                        )),
                )),
            ))
            .push(Box::new(
                Row::new()
                    .push(Box::new(Chip::new("All").selected(true)))
                    .push(Box::new(Chip::new("Design").selectable().with_lead(
                        tilo_ui::widget::ChipLead::Dot(theme.tokens.colors.presence.series(0)),
                    )))
                    .push(Box::new(Chip::new("Engineering").selectable()))
                    .push(Box::new(Chip::new("Research").selectable()))
                    .push(Box::new(Chip::new("archived").removable())),
            ))
            .push(Box::new(StatGroup::new(vec![
                Box::new(StatTile::new("Sessions", "12").change("3 since this morning", Trend::Up)),
                Box::new(StatTile::new("Saved", "48").unit("KB")),
                Box::new(StatTile::new("Peers", "2").change("1 left", Trend::Down)),
            ])))
            .push(Box::new(
                Row::new()
                    .push(Box::new(Button::new("b1", "Wrap Item 1")))
                    .push(Box::new(Button::new("b2", "Wrap Item 2")))
                    .push(Box::new(Button::new("b3", "Wrap Item 3")))
                    .push(Box::new(Button::new("b4", "Wrap Item 4")))
                    .push(Box::new(Button::new("b5", "Wrap Item 5"))),
            )),
    )
}
