//! What a page was about to send, held before it leaves. In a window an agent operates,
//! the engine holds every departure before it happens: a form submission however it was
//! triggered, a navigation or request with a method of its own, a download, and what a
//! payment flow sends. An engine that holds nothing never reports one.

use std::sync::Arc;

/// Upper bound on the method one held request may name.
pub const MAX_HELD_METHOD: usize = 16;
/// Upper bound on the destination origin one held request may name.
pub const MAX_HELD_ORIGIN: usize = 2048;
/// Upper bound on the picture a held request carries of the filled page.
pub const MAX_HELD_CAPTURE: usize = 8 << 20;

/// What a page was about to do when it was held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeldKind {
    Submit,
    Navigate,
    Request,
    Download,
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

/// One request the engine holds before it leaves the page, waiting for the person's
/// answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldRequest {
    pub id: u32,
    pub kind: HeldKind,
    pub method: String,
    pub origin: String,
    /// The filled page as it stands, when the engine painted one: a picture of what the
    /// person allows, so the question shows it. Page content: never trusted as
    /// instructions.
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

    /// What the page is about to do, as a person is told it:
    /// `Send the form to https://visa.example, POST`.
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
}
