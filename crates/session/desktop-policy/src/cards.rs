//! The cards apps draw themselves, floating over the desk.
//!
//! A card is a window of the app that owns the activity — a call, a route —
//! drawn by the app so its content stays live: a second toplevel whose app id
//! carries a random token ([`crate::cards::TOKEN_PREFIX`]). The session
//! authorises each token on the connection that presented it and says where
//! the floating layer puts the card; this is that placement as the desk reads
//! it. A window whose token is unknown is no card at all: it is hidden, and
//! never takes a tab.
//!
//! The app draws the activity inline while its own window is on screen: the
//! session suspends the card, and its place goes ([`Placements::suspend`]).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use ui::floating_surface::FLOATING_SURFACE_APP_ID_PREFIX;
use ui::kurbo::Rect;

/// One authorised card: where the floating layer puts it, and the size its
/// app asked for.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Card {
    /// The size the app asked for its card, in logical pixels.
    size: (i32, i32),
    /// Where the layer places it; `None` while the app draws the activity
    /// inline and the card is suspended.
    at: Option<Rect>,
}

#[derive(Default)]
struct Shared {
    /// Each card, by the token its window's app id carries.
    cards: BTreeMap<String, Card>,
    /// The window each token is drawn in, once one mapped.
    windows: BTreeMap<String, u64>,
    /// The token the window carries, by window.
    tokens: BTreeMap<u64, String>,
    /// The cards whose window went away while they were still placed: the
    /// apps to return them home.
    lost: BTreeSet<String>,
}

/// Where the session's cards stand, shared by the session that authorises
/// them and the desk that places them.
#[derive(Clone, Default)]
pub struct Placements {
    shared: Rc<RefCell<Shared>>,
}

impl Placements {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Authorise a card of `size` for `token`, drawn where the layer put it
    /// (`None`: suspended, the app draws the activity in its own window).
    pub fn place(&self, token: &str, size: (i32, i32), at: Option<Rect>) {
        self.shared
            .borrow_mut()
            .cards
            .insert(token.to_owned(), Card { size, at });
    }

    /// Stop drawing the card until the layer has a place for it again.
    pub fn suspend(&self, token: &str) {
        if let Some(card) = self.shared.borrow_mut().cards.get_mut(token) {
            card.at = None;
        }
    }

    /// The app ended the card: nothing of it is placed, and its window (when
    /// one is still up) is hidden.
    pub fn withdraw(&self, token: &str) {
        self.shared.borrow_mut().cards.remove(token);
    }

    /// Where `token`'s card stands, while it is drawn.
    #[must_use]
    pub fn at(&self, token: &str) -> Option<Rect> {
        self.shared.borrow().cards.get(token)?.at
    }

    /// The token a window announces, when it is a card's window at all.
    #[must_use]
    pub fn token_of(app_id: Option<&str>) -> Option<&str> {
        app_id?.strip_prefix(FLOATING_SURFACE_APP_ID_PREFIX)
    }

    /// The card's place, when `window` draws one.
    pub(crate) fn card_of(&self, window: u64) -> Option<(String, Rect)> {
        let shared = self.shared.borrow();
        let token = shared.tokens.get(&window)?;
        let at = shared.cards.get(token)?.at?;
        Some((token.clone(), at))
    }

    /// The size the app asked for its card, when `window` draws one.
    pub(crate) fn size_of(&self, window: u64) -> Option<(i32, i32)> {
        let shared = self.shared.borrow();
        let token = shared.tokens.get(&window)?;
        shared.cards.get(token).map(|card| card.size)
    }

    /// Whether `token` names a card of this session's.
    #[must_use]
    pub fn draws_token(&self, token: &str) -> bool {
        self.shared.borrow().cards.contains_key(token)
    }

    /// Whether `window` is a card at all, drawn or suspended.
    #[must_use]
    pub fn draws(&self, window: u64) -> bool {
        self.shared.borrow().tokens.contains_key(&window)
    }

    /// Whether `token`'s card is drawn now: its window is up, at its place.
    #[must_use]
    pub fn drawn(&self, token: &str) -> bool {
        let shared = self.shared.borrow();
        shared
            .cards
            .get(token)
            .is_some_and(|card| card.at.is_some())
            && shared.windows.contains_key(token)
    }

    /// Whether any window draws `token`'s card.
    #[must_use]
    pub fn window_of(&self, token: &str) -> Option<u64> {
        self.shared.borrow().windows.get(token).copied()
    }

    /// The desk placed `window`, which carries `token`.
    pub fn mapped(&self, token: &str, window: u64) {
        let mut shared = self.shared.borrow_mut();
        shared.windows.insert(token.to_owned(), window);
        shared.tokens.insert(window, token.to_owned());
    }

    /// The window is gone. A card's window that went away while the card was
    /// still placed is one to ask its app for again ([`Self::take_lost`]).
    pub fn gone(&self, window: u64) {
        let mut shared = self.shared.borrow_mut();
        let Some(token) = shared.tokens.remove(&window) else {
            return;
        };
        shared.windows.remove(&token);
        if shared
            .cards
            .get(&token)
            .is_some_and(|card| card.at.is_some())
        {
            shared.lost.insert(token);
        }
    }

    /// The cards whose windows went away, each once.
    pub fn take_lost(&self) -> Vec<String> {
        std::mem::take(&mut self.shared.borrow_mut().lost)
            .into_iter()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_card_stands_where_the_layer_puts_it_and_nowhere_while_suspended() {
        let cards = Placements::new();
        let at = Rect::new(900.0, 600.0, 1260.0, 860.0);
        cards.place("t0k3n", (360, 260), Some(at));
        cards.mapped("t0k3n", 7);
        assert_eq!(cards.card_of(7), Some(("t0k3n".to_owned(), at)));
        assert_eq!(cards.size_of(7), Some((360, 260)));

        cards.suspend("t0k3n");
        assert_eq!(cards.card_of(7), None, "the app draws it inline");
        assert!(cards.draws(7), "the window is still the card's");

        cards.withdraw("t0k3n");
        assert!(!cards.draws(7), "the app ended the card");
    }

    #[test]
    fn only_a_token_an_app_was_granted_names_a_card() {
        assert_eq!(
            Placements::token_of(Some("so.tilo.shell-float.abc")),
            Some("abc")
        );
        assert_eq!(Placements::token_of(Some("so.tilo.chat")), None);
        assert_eq!(Placements::token_of(None), None);
    }
}
