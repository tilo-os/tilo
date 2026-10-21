//! Poker: Texas hold'em for everyone in the room.
//!
//! The table is the room's default record, the one every device of the room
//! opens when nothing names another, so everyone who joins the session sits
//! at the same table. Sitting down is the table's `sit` action, from a seat's
//! button or from whoever brings people to the table (the web launcher seats
//! each player who joins); each move is an action on the table ([`game`]),
//! and each participant's window draws the same table with only their own
//! cards face up ([`view`]).

/// The application id.
pub const APP_ID: &str = "so.tilo.poker";
/// The window and launcher title.
pub const APP_TITLE: &str = "Poker";

pub mod card;
pub mod cards;
pub mod game;
pub mod locales;
pub mod view;

use ui::i18n::I18nManager;
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
        if table.hand.number == 0 {
            I18nManager::lookup_plain("poker-title")
        } else {
            I18nManager::format("poker-hand-number", &[("number", table.hand.number.into())])
        }
    }

    fn view(cx: &mut ViewCx<Table>) -> Element {
        take_a_seat(cx);
        // Names arrive from the room after the table: the view follows both.
        let names = cx.people().revision();
        cx.switch(&names, |cx, _| {
            let table = cx.value(Table::clone);
            cx.switch(&table, view::table)
        })
    }
}

/// Seat whoever opens the table, once, while there is a free seat: joining
/// the session is sitting down. They can leave the seat again.
fn take_a_seat(cx: &ViewCx<Table>) {
    if !cx.mode().is_live() || cx.read_only() {
        return;
    }
    let Some(me) = cx.me() else {
        return;
    };
    let seat = cx.read(|table| {
        let table = table?;
        if table.players.contains_key(me) {
            return None;
        }
        table.free_seat()
    });
    if let Some(seat) = seat {
        let seat = u32::try_from(seat.saturating_add(1)).unwrap_or(1);
        cx.dispatcher().submit(Table::SIT.with(seat));
    }
}

ui::dev_reload_entry!(App);
