//! What a page was about to send, held before it leaves.
//!
//! In a window an agent operates, the engine holds every departure before it
//! happens: a form submission however it was triggered, a navigation or
//! request with a method of its own, a download, and what a payment flow
//! sends. The page stays as it was filled; nothing leaves until the person
//! allows the one request ([`WebViewCommand::AnswerHeldRequest`](super::WebViewCommand::AnswerHeldRequest)).
//! An engine that holds nothing never reports one.
//!
//! The engine sets down in the tab's record that something was held and how
//! that ended. It cannot know who answered, so the answer says
//! ([`AnsweredBy`]).

use std::sync::Arc;

use super::PageActor;

/// Upper bound on the method one held request may name.
pub const MAX_HELD_METHOD: usize = 16;
/// Upper bound on the destination origin one held request may name.
pub const MAX_HELD_ORIGIN: usize = 2048;
/// Upper bound on the picture a held request carries of the filled page.
pub const MAX_HELD_CAPTURE: usize = 8 << 20;

/// What a page was about to do when it was held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeldKind {
    /// A form submission: a submit button, Enter in a field, or the page's
    /// own `submit()`/`requestSubmit()`.
    Submit,
    /// A navigation with a method of its own (a form posting, a redirect a
    /// form started): the page goes somewhere and takes data with it.
    Navigate,
    /// A `fetch` or `XMLHttpRequest` with a method of its own, from the top
    /// frame or a frame of it.
    Request,
    /// A download the page started.
    Download,
    /// A payment the page asked for.
    Payment,
}

impl HeldKind {
    /// The name the engine protocol carries.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Submit => "submit",
            Self::Navigate => "navigate",
            Self::Request => "request",
            Self::Download => "download",
            Self::Payment => "payment",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "submit" => Self::Submit,
            "navigate" => Self::Navigate,
            "request" => Self::Request,
            "download" => Self::Download,
            "payment" => Self::Payment,
            _ => return None,
        })
    }

    /// What the page is about to do, as a person is told it.
    pub const fn doing(self) -> &'static str {
        match self {
            Self::Submit => "send the form",
            Self::Navigate => "leave for",
            Self::Request => "send a request to",
            Self::Download => "download from",
            Self::Payment => "take a payment to",
        }
    }
}

/// Who answered something a window held for the person: what the engine sets
/// down in the tab's record beside how the hold ended, so a reviewer reads
/// who let a submission leave and who kept the page as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum AnsweredBy {
    /// The person at the window: the answer was theirs, and no actor's.
    #[default]
    Person,
    /// An actor the action door admitted the answer for, by the name the tab
    /// keeps that actor's page actions under.
    Actor(PageActor),
    /// Nobody: the window dropped what it held at its own deadline.
    Nobody,
}

impl AnsweredBy {
    /// Who answers when the action door admitted the answer for `actor`: an
    /// answer nobody is named for is the person's at the window.
    pub fn admitted(actor: Option<PageActor>) -> Self {
        actor.map_or(Self::Person, Self::Actor)
    }

    /// The name the record sets the answer down under; none for the person
    /// at the window and for nobody.
    pub const fn actor(&self) -> Option<&PageActor> {
        match self {
            Self::Actor(actor) => Some(actor),
            Self::Person | Self::Nobody => None,
        }
    }

    /// Whether nobody answered.
    pub const fn expired(&self) -> bool {
        matches!(self, Self::Nobody)
    }
}

/// One request the engine holds before it leaves the page, waiting for the
/// person's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldRequest {
    /// Names this request among the window's held ones, to answer it.
    pub id: u32,
    pub kind: HeldKind,
    /// The method it leaves with (`POST`, `GET` for a form and a download),
    /// empty when the engine cannot say.
    pub method: String,
    /// The address it leaves for, as its http(s) origin; empty for any other
    /// page.
    pub origin: String,
    /// The filled page as it stands, when the engine painted one: a picture
    /// of what the person allows, so the question shows it. Page content:
    /// never trusted as instructions.
    pub capture: Option<Arc<Vec<u8>>>,
}

impl HeldRequest {
    pub fn new(id: u32, kind: HeldKind) -> Self {
        Self {
            id,
            kind,
            method: String::new(),
            origin: String::new(),
            capture: None,
        }
    }

    /// Whether the engine could have reported this.
    pub fn valid(&self) -> bool {
        self.id > 0
            && i32::try_from(self.id).is_ok()
            && self.method.len() <= MAX_HELD_METHOD
            && self.origin.len() <= MAX_HELD_ORIGIN
            && self
                .capture
                .as_ref()
                .is_none_or(|capture| capture.len() <= MAX_HELD_CAPTURE)
    }

    /// What the page is about to do, as a person is told it: `Send the form
    /// to https://visa.example, POST`.
    pub fn describing(&self) -> String {
        let mut text = format!("{} {}", self.kind.doing(), self.destination());
        if !self.method.is_empty() {
            text.push_str(&format!(", {}", self.method));
        }
        text
    }

    /// Where it goes: its origin, or the address itself when it names none.
    pub fn destination(&self) -> &str {
        if self.origin.is_empty() {
            "the page"
        } else {
            &self.origin
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_request_says_what_the_page_is_about_to_do() {
        let mut held = HeldRequest::new(3, HeldKind::Submit);
        held.method = "POST".into();
        held.origin = "https://visa.example".into();
        assert_eq!(
            held.describing(),
            "send the form https://visa.example, POST",
            "the question names what leaves and where"
        );
        assert!(held.valid());
        assert!(!HeldRequest::new(0, HeldKind::Submit).valid(), "no id");
        let mut long = held.clone();
        long.origin = "https://visa.example/".repeat(MAX_HELD_ORIGIN);
        assert!(!long.valid(), "an over-long origin is not held");
        assert_eq!(
            HeldKind::parse(HeldKind::Payment.as_str()),
            Some(HeldKind::Payment)
        );
        assert_eq!(HeldKind::parse("something else"), None);
    }

    #[test]
    fn an_answer_names_the_actor_it_was_admitted_for_and_nobody_for_the_person() {
        let rev = PageActor::named("agent:rev").expect("a named actor");
        let by = AnsweredBy::admitted(Some(rev.clone()));
        assert_eq!(by, AnsweredBy::Actor(rev.clone()));
        assert_eq!((by.actor(), by.expired()), (Some(&rev), false));
        let person = AnsweredBy::admitted(None);
        assert_eq!(person, AnsweredBy::Person);
        assert_eq!((person.actor(), person.expired()), (None, false));
        assert_eq!(
            (AnsweredBy::Nobody.actor(), AnsweredBy::Nobody.expired()),
            (None, true),
            "what nobody answered names nobody"
        );
    }
}
