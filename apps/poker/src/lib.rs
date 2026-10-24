//! Poker: Texas hold'em for everyone in the room.
//!
//! The table is the room's default record, the one every device of the room
//! opens when nothing names another, so everyone who joins the session is at
//! the same table. Opening it seats nobody: whoever opens the table watches
//! until they take a seat, with the "Take a seat" button or a seat's own
//! "Sit here", the table's `sit` action. Only the table's creator writes what
//! it starts with, so someone who joins never writes into a copy the room's
//! table is about to replace, and the buttons wait until the table has
//! arrived. Each move is an action on the table ([`game`]), and each
//! participant's window draws the same table with only their own cards face
//! up ([`view`]).

/// The application id.
pub const APP_ID: &str = "so.tilo.poker";
/// The window and launcher title.
pub const APP_TITLE: &str = "Poker";

pub mod card;
pub mod cards;
pub mod game;
pub mod locales;
pub mod view;

use ui::prelude::*;

pub use game::Table;

pub struct App;

impl ResourceApp for App {
    type Resource = Table;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    fn label(table: &Table) -> String {
        view::location(table)
    }

    fn view(cx: &mut ViewCx<Table>) -> Element {
        // Names arrive from the room after the table: the view follows both.
        let names = cx.people().revision();
        cx.switch(&names, |cx, _| {
            let table = cx.value(Table::clone);
            cx.switch(&table, view::table)
        })
    }
}

ui::dev_reload_entry!(App);
