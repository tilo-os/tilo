//! The cards apps draw themselves, over the desk.
//! A call or a route is not a picture the session can draw: it is live content its app keeps
//! drawing while the person works elsewhere. The app presents such an activity as a card of its own
//! ([`Present`]), and the floating layer decides where it stands: while the app's own window is on
//! screen the card stands down and the app draws the activity there, and it comes back the moment
//! that window goes out of view.
//! This is the session's side of that: what each app presented, the place the layer gave it this
//! pass, and the geometry the desk places its window at. The app's own geometry is a request; the
//! layer's place is the answer.
//!
//! [`Present`]: ui::floating_surface::FloatingSurfaceClientMessage::Present

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use tilo_desktop_policy::cards::Placements;
use ui::floating_surface::FloatingSurfaceProtocolRequest;
use ui::vello::kurbo::{Point, Rect, Size};

/// An activity: the app that runs it, and the activity's own id.
type Key = (String, String);

/// One card an app presented.
#[derive(Clone, Debug)]
pub(crate) struct Presented {
    /// The app that runs the activity and draws the card.
    pub(crate) app: String,
    /// The activity's own id, as the app's summary names it.
    pub(crate) activity: String,
    /// The capability the app's card window announces.
    pub(crate) token: String,
    /// The connection that vouched for the app when it presented the card.
    pub(crate) connection: u64,
    /// The card body's size, as the app asked for it.
    pub(crate) content: Size,
    /// The soft frame the app draws around the body: left, top, right and bottom, as its own
    /// presentation asked for them.
    pub(crate) frame: [f64; 4],
    /// Where the body stood in the app's own window: its pointer coordinates reach the desk through
    /// this.
    pub(crate) local: Point,
    /// The body's place on the desk the app was last told to draw at;
    /// `None` while it draws the activity inline.
    pub(crate) told: Option<Rect>,
    /// The app itself stood the card down: it draws the activity in its own window until it
    /// presents the card again.
    pub(crate) stood_down: bool,
    /// Whether the app was told its card's window is up: it stops painting the copy it keeps for
    /// the handoff, and the card alone draws it.
    pub(crate) adopted: bool,
}

impl Presented {
    /// The body inside the whole window the floating layer places: the soft frame the app draws
    /// around it, taken off.
    #[must_use]
    pub(crate) fn body(&self, window: Rect) -> Rect {
        let [left, top, right, bottom] = self.frame;
        Rect::new(
            window.x0 + left,
            window.y0 + top,
            (window.x1 - right).max(window.x0 + left + 1.0),
            (window.y1 - bottom).max(window.y0 + top + 1.0),
        )
    }

    /// The whole window's size: the body and the soft frame around it.
    #[must_use]
    pub(crate) fn window_size(&self) -> Size {
        let [left, top, right, bottom] = self.frame;
        Size::new(
            self.content.width + left + right,
            self.content.height + top + bottom,
        )
    }
}

#[derive(Default)]
struct Shared {
    /// What each app presented, by the activity it belongs to.
    cards: BTreeMap<Key, Presented>,
    /// The place the floating layer gave each card this pass, in desk pixels.
    slots: BTreeMap<Key, Rect>,
    /// The cards whose app has its own window on the desk: the app draws them there, and they stand
    /// down.
    in_view: BTreeSet<Key>,
}

/// The cards the apps draw themselves, shared by the session that authorises them and the layer
/// that places them.
#[derive(Clone, Default)]
pub(crate) struct Cards {
    shared: Rc<RefCell<Shared>>,
    /// Where each card's window stands, as the desk lays it out.
    placements: Placements,
}

impl Cards {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Where the desk places the cards' windows.
    pub(crate) fn placements(&self) -> Placements {
        self.placements.clone()
    }

    /// The whole window size the app asked for the activity's card, when it presented one: what the
    /// layer reserves for it, and what the session lays its window out by.
    pub(crate) fn size(&self, app: &str, activity: &str) -> Option<Size> {
        self.shared
            .borrow()
            .cards
            .get(&(app.to_owned(), activity.to_owned()))
            .map(Presented::window_size)
    }

    /// Whether the app's own window is on the desk now: the activity is drawn there, and no card
    /// shows.
    pub(crate) fn in_view(&self, app: &str, activity: &str) -> bool {
        self.shared
            .borrow()
            .in_view
            .contains(&(app.to_owned(), activity.to_owned()))
    }

    /// Whether the app's card is drawn now: it was told where to stand, and its window is up there.
    pub(crate) fn live(&self, app: &str, activity: &str) -> bool {
        let shared = self.shared.borrow();
        shared
            .cards
            .get(&(app.to_owned(), activity.to_owned()))
            .is_some_and(|card| self.placements.drawn(&card.token))
    }

    /// The place the layer gave the activity's card this pass.
    pub(crate) fn slot(&self, app: &str, activity: &str) -> Option<Rect> {
        self.shared
            .borrow()
            .slots
            .get(&(app.to_owned(), activity.to_owned()))
            .copied()
    }

    /// The card `token` draws, while the app still presents it.
    pub(crate) fn by_token(&self, token: &str) -> Option<Presented> {
        self.shared
            .borrow()
            .cards
            .values()
            .find(|card| card.token == token)
            .cloned()
    }

    /// The cards the apps presented, as they stand now.
    pub(crate) fn presented(&self) -> Vec<Presented> {
        self.shared.borrow().cards.values().cloned().collect()
    }

    /// Whether `at` stands inside a card the layer placed last pass.
    pub(crate) fn holds(&self, at: Rect) -> bool {
        self.shared
            .borrow()
            .slots
            .values()
            .any(|slot| slot.contains(at.center()))
    }

    /// A new pass of the layer's layout: the places it gave last are gone until it gives them
    /// again.
    pub(crate) fn begin_slots(&self) {
        self.shared.borrow_mut().slots.clear();
    }

    /// The layer gave the activity's card this place on the desk.
    pub(crate) fn slot_at(&self, app: &str, activity: &str, at: Rect) {
        self.shared
            .borrow_mut()
            .slots
            .insert((app.to_owned(), activity.to_owned()), at);
    }

    /// Note whether the app's own window is on the desk.
    pub(crate) fn see(&self, app: &str, activity: &str, in_view: bool) {
        let key = (app.to_owned(), activity.to_owned());
        let mut shared = self.shared.borrow_mut();
        if in_view {
            shared.in_view.insert(key);
        } else {
            shared.in_view.remove(&key);
        }
    }

    /// Record the place the app was told to draw its card at (`None`: it draws the activity
    /// inline). Returns whether that is news.
    pub(crate) fn told(&self, app: &str, activity: &str, at: Option<Rect>) -> bool {
        let mut shared = self.shared.borrow_mut();
        let Some(card) = shared.cards.get_mut(&(app.to_owned(), activity.to_owned())) else {
            return false;
        };
        if card.told == at {
            return false;
        }
        card.told = at;
        // A card that moves, or comes back from standing down, is a window whose first frame the
        // app has not seen yet.
        card.adopted = false;
        true
    }

    /// The app's card's window is up: it is told once per place, so the copy it painted for the
    /// handoff goes.
    pub(crate) fn adopt(&self, app: &str, activity: &str) -> bool {
        let mut shared = self.shared.borrow_mut();
        let Some(card) = shared.cards.get_mut(&(app.to_owned(), activity.to_owned())) else {
            return false;
        };
        if card.adopted {
            return false;
        }
        card.adopted = true;
        true
    }

    /// The cards whose windows went away while their apps still presented them, each once: the apps
    /// to return them home.
    pub(crate) fn lost(&self) -> Vec<String> {
        self.placements.take_lost()
    }

    /// An app presents a card: its geometry is the app's request, and the
    /// token it is given is the session's answer, good on any window the app
    /// draws for this activity on that connection.
    ///
    /// # Errors
    /// The activity cannot be a card this session draws, or another window of
    /// the app already draws it: a card belongs to the window that presented
    /// it, and only that window may move, stand down or end it.
    pub(crate) fn present(
        &self,
        app: &str,
        connection: u64,
        request: &FloatingSurfaceProtocolRequest,
    ) -> Result<(), String> {
        if !request.role.uses_app_owned_host() {
            return Err("this session draws no captured surface as an app's card".into());
        }
        if request.id.is_empty() || request.id.len() > 128 {
            return Err("a card names the activity it shows".into());
        }
        let body = request.initial_rect.unwrap_or([
            0.0,
            0.0,
            request.preferred_size[0],
            request.preferred_size[1],
        ]);
        let content = Size::new((body[2] - body[0]).max(1.0), (body[3] - body[1]).max(1.0));
        let frame = request.presentation_rect.map_or([0.0; 4], |window| {
            [
                (body[0] - window[0]).max(0.0),
                (body[1] - window[1]).max(0.0),
                (window[2] - body[2]).max(0.0),
                (window[3] - body[3]).max(0.0),
            ]
        });
        let local = Point::new(body[0], body[1]);
        let key = (app.to_owned(), request.id.clone());
        if self
            .shared
            .borrow()
            .cards
            .get(&key)
            .is_some_and(|card| card.connection != connection)
        {
            return Err("another window of this app already draws this activity".into());
        }
        let token = self.shared.borrow().cards.get(&key).map_or_else(
            || uuid::Uuid::new_v4().simple().to_string(),
            |card| card.token.clone(),
        );
        let shown = Presented {
            app: app.to_owned(),
            activity: request.id.clone(),
            token: token.clone(),
            connection,
            content,
            frame,
            local,
            told: None,
            stood_down: false,
            adopted: false,
        };
        let size = shown.window_size();
        self.shared.borrow_mut().cards.insert(key, shown);
        // A card with no place yet is hidden until the layer gives it one.
        self.placements
            .place(&token, (round(size.width), round(size.height)), None);
        Ok(())
    }

    /// The app stands its card down: it draws the activity in its own window until it presents the
    /// card again. An app echoing the stand-down the session asked for is no stand-down at all — it
    /// already draws it there.
    pub(crate) fn stood_down(
        &self,
        app: &str,
        connection: u64,
        activity: &str,
    ) -> Option<Presented> {
        let mut shared = self.shared.borrow_mut();
        let card = shared
            .cards
            .get_mut(&(app.to_owned(), activity.to_owned()))?;
        if card.connection != connection || card.told.is_none() {
            return None;
        }
        card.stood_down = true;
        card.adopted = false;
        Some(card.clone())
    }

    /// The app presents the activity in the card again after standing it down: while the layer
    /// shows the card it draws the activity there once more. Returns whether that is news.
    pub(crate) fn presented_again(&self, app: &str, connection: u64, activity: &str) -> bool {
        let mut shared = self.shared.borrow_mut();
        let Some(card) = shared.cards.get_mut(&(app.to_owned(), activity.to_owned())) else {
            return false;
        };
        if card.connection != connection || !card.stood_down {
            return false;
        }
        card.stood_down = false;
        true
    }

    /// The app ended the card: nothing of it is placed again.
    pub(crate) fn dismissed(&self, connection: u64, activity: &str) -> Option<Presented> {
        let mut shared = self.shared.borrow_mut();
        let key = shared
            .cards
            .iter()
            .find(|(_, card)| card.connection == connection && card.activity == activity)
            .map(|(key, _)| key.clone())?;
        let card = shared.cards.remove(&key)?;
        shared.in_view.remove(&key);
        shared.slots.remove(&key);
        Some(card)
    }

    /// The connection ended: the cards it vouched for go with it.
    pub(crate) fn connection_closed(&self, connection: u64) -> Vec<Presented> {
        let mut shared = self.shared.borrow_mut();
        let mut gone = Vec::new();
        shared.cards.retain(|key, card| {
            if card.connection != connection {
                return true;
            }
            gone.push((key.clone(), card.clone()));
            false
        });
        gone.into_iter()
            .map(|(key, card)| {
                shared.slots.remove(&key);
                shared.in_view.remove(&key);
                card
            })
            .collect()
    }
}

/// `value` as a whole logical pixel.
#[allow(clippy::cast_possible_truncation, reason = "a card's size on a screen")]
fn round(value: f64) -> i32 {
    value.round() as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::floating_surface::{
        FloatingSurfaceProtocolRequest, FloatingSurfaceProtocolRole, FloatingSurfaceProtocolSource,
    };

    fn call(id: &str) -> FloatingSurfaceProtocolRequest {
        FloatingSurfaceProtocolRequest {
            id: id.to_owned(),
            title: "Ada".into(),
            role: FloatingSurfaceProtocolRole::ActiveCall,
            source: FloatingSurfaceProtocolSource::AppRegion {
                app_id: "so.tilo.chat".into(),
                region: [0.0, 0.0, 360.0, 240.0],
            },
            preferred_size: [360.0, 240.0],
            aspect_ratio: None,
            initial_rect: Some([40.0, 60.0, 400.0, 300.0]),
            presentation_rect: Some([32.0, 52.0, 408.0, 308.0]),
        }
    }

    #[test]
    fn a_card_keeps_the_token_its_activity_was_given() {
        let cards = Cards::new();
        cards
            .present("so.tilo.chat", 7, &call("chat-call:voice"))
            .unwrap();
        let first = cards.presented().remove(0);
        cards
            .present("so.tilo.chat", 7, &call("chat-call:voice"))
            .unwrap();
        let again = cards.presented().remove(0);
        assert_eq!(first.token, again.token, "the same call keeps its card");
        assert_eq!(again.content, Size::new(360.0, 240.0));
        assert_eq!(again.window_size(), Size::new(376.0, 256.0));
        assert_eq!(
            again.body(Rect::new(8.0, 16.0, 384.0, 272.0)),
            Rect::new(16.0, 24.0, 376.0, 264.0),
            "the layer's place inside the frame is the app's body"
        );
    }

    #[test]
    fn only_the_connection_that_presented_a_card_ends_it() {
        let cards = Cards::new();
        cards
            .present("so.tilo.chat", 7, &call("chat-call:voice"))
            .unwrap();
        cards.told(
            "so.tilo.chat",
            "chat-call:voice",
            Some(Rect::new(0.0, 0.0, 360.0, 240.0)),
        );
        assert!(
            cards
                .stood_down("so.tilo.chat", 8, "chat-call:voice")
                .is_none()
        );
        assert!(cards.dismissed(8, "chat-call:voice").is_none());
        assert!(
            cards
                .stood_down("so.tilo.chat", 7, "chat-call:voice")
                .is_some()
        );
        assert_eq!(cards.connection_closed(8).len(), 0);
        assert_eq!(cards.connection_closed(7).len(), 1);
        assert!(cards.size("so.tilo.chat", "chat-call:voice").is_none());
    }

    /// The card its app stood down is the connection that presented it to bring back: another one
    /// of the app's windows cannot take it over by presenting a frame for it.
    #[test]
    fn another_connection_cannot_resume_a_card_that_stood_down() {
        let cards = Cards::new();
        cards
            .present("so.tilo.chat", 7, &call("chat-call:voice"))
            .unwrap();
        cards.told(
            "so.tilo.chat",
            "chat-call:voice",
            Some(Rect::new(0.0, 0.0, 360.0, 240.0)),
        );
        assert!(
            cards
                .stood_down("so.tilo.chat", 7, "chat-call:voice")
                .is_some()
        );
        assert!(
            !cards.presented_again("so.tilo.chat", 8, "chat-call:voice"),
            "a window that never presented the card leaves it stood down"
        );
        assert!(cards.presented_again("so.tilo.chat", 7, "chat-call:voice"));
    }

    #[test]
    fn another_window_of_the_app_cannot_take_over_a_card() {
        let cards = Cards::new();
        cards
            .present("so.tilo.chat", 7, &call("chat-call:voice"))
            .unwrap();
        let card = cards.presented().remove(0);
        assert!(
            cards
                .present("so.tilo.chat", 8, &call("chat-call:voice"))
                .is_err()
        );
        let kept = cards.presented().remove(0);
        assert_eq!(kept.connection, 7, "the window that presented it keeps it");
        assert_eq!(kept.token, card.token);
    }

    #[test]
    fn a_card_with_no_place_from_the_layer_is_drawn_nowhere() {
        let cards = Cards::new();
        cards
            .present("so.tilo.chat", 7, &call("chat-call:voice"))
            .unwrap();
        let card = cards.presented().remove(0);
        cards.told(
            "so.tilo.chat",
            "chat-call:voice",
            Some(Rect::new(100.0, 100.0, 460.0, 340.0)),
        );
        assert!(
            !cards.live("so.tilo.chat", "chat-call:voice"),
            "no window is up yet"
        );
        cards.placements().place(
            &card.token,
            (368, 248),
            Some(Rect::new(100.0, 100.0, 468.0, 348.0)),
        );
        cards.placements().mapped(&card.token, 3);
        assert!(cards.live("so.tilo.chat", "chat-call:voice"));
        cards.placements().suspend(&card.token);
        assert!(!cards.live("so.tilo.chat", "chat-call:voice"), "stood down");
    }

    #[test]
    fn a_card_is_adopted_once_its_window_is_up_and_again_after_it_stands_down() {
        let cards = Cards::new();
        cards
            .present("so.tilo.chat", 7, &call("chat-call:voice"))
            .unwrap();
        let card = cards.presented().remove(0);
        cards.placements().place(
            &card.token,
            (376, 256),
            Some(Rect::new(0.0, 0.0, 376.0, 256.0)),
        );
        cards.placements().mapped(&card.token, 4);
        cards.told(
            "so.tilo.chat",
            "chat-call:voice",
            Some(Rect::new(8.0, 8.0, 368.0, 248.0)),
        );
        assert!(cards.adopt("so.tilo.chat", "chat-call:voice"), "told once");
        assert!(!cards.adopt("so.tilo.chat", "chat-call:voice"), "not again");
        cards
            .stood_down("so.tilo.chat", 7, "chat-call:voice")
            .expect("stood down");
        assert!(
            cards.adopt("so.tilo.chat", "chat-call:voice"),
            "fresh after a return"
        );
    }

    #[test]
    fn a_card_its_app_presents_again_after_standing_it_down_is_drawn_again() {
        let cards = Cards::new();
        cards
            .present("so.tilo.chat", 7, &call("chat-call:voice"))
            .unwrap();
        cards.told(
            "so.tilo.chat",
            "chat-call:voice",
            Some(Rect::new(0.0, 0.0, 360.0, 240.0)),
        );
        let stood_down = cards
            .stood_down("so.tilo.chat", 7, "chat-call:voice")
            .expect("stood down");
        cards.told("so.tilo.chat", "chat-call:voice", None);
        assert!(!cards.presented_again("so.tilo.chat", 8, "chat-call:voice"));
        assert!(
            cards.presented()[0].stood_down,
            "another window changes nothing"
        );
        assert!(cards.presented_again("so.tilo.chat", 7, &stood_down.activity));
        assert!(!cards.presented()[0].stood_down);
        assert!(
            !cards.presented_again("so.tilo.chat", 7, "chat-call:voice"),
            "told once"
        );
    }

    #[test]
    fn echoing_the_stand_down_the_session_asked_for_is_no_stand_down() {
        let cards = Cards::new();
        cards
            .present("so.tilo.chat", 7, &call("chat-call:voice"))
            .unwrap();
        // The session has already told the app it draws the call itself.
        cards.told("so.tilo.chat", "chat-call:voice", None);
        assert!(
            cards
                .stood_down("so.tilo.chat", 7, "chat-call:voice")
                .is_none()
        );
        assert!(
            !cards.presented()[0].stood_down,
            "the call's card comes back when its window goes"
        );
    }

    #[test]
    fn a_cards_window_that_went_away_is_one_to_ask_its_app_for_again() {
        let cards = Cards::new();
        cards
            .present("so.tilo.chat", 7, &call("chat-call:voice"))
            .unwrap();
        let card = cards.presented().remove(0);
        cards.placements().place(
            &card.token,
            (376, 256),
            Some(Rect::new(0.0, 0.0, 376.0, 256.0)),
        );
        cards.placements().mapped(&card.token, 4);
        assert!(cards.lost().is_empty());
        cards.placements().gone(4);
        assert_eq!(cards.lost(), vec![card.token.clone()]);
        assert!(cards.lost().is_empty(), "told once");
        assert_eq!(
            cards.by_token(&card.token).map(|card| card.activity),
            Some("chat-call:voice".to_owned())
        );
    }

    #[test]
    fn a_picture_in_picture_mirror_is_not_a_card() {
        let cards = Cards::new();
        let mut request = call("pip:1");
        request.role = FloatingSurfaceProtocolRole::PictureInPicture;
        assert!(cards.present("so.tilo.chat", 7, &request).is_err());
        assert!(cards.presented().is_empty());
    }
}
