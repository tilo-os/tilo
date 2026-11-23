//! A text released for one site, on its way into that site's own page, and what a window that
//! sent one strikes out of whatever the page says afterwards.
//!
//! An agent that signs in to a site names an item of its person's vault and never holds what
//! the item keeps. Whoever keeps the item releases its text for one site, and the engine writes
//! it only into that site's own page ([`PageRequest::FillReleased`](super::PageRequest)). On the
//! way there the text is held once ([`ReleasedText`]): a request that carries it is copied and
//! printed like any other, and neither a copy nor a print holds the text.
//!
//! A page may say what it was given: in a line it logs, or in the address a form sends. So the
//! window keeps what it sent ([`StruckTexts`]) and strikes it out of every answer the engine
//! gives, as the page wrote it or percent-encoded. What a page writes some other way (in
//! base64, split over two lines) is not found: the page that receives a text is its site's own,
//! and reads its own fields.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use zeroize::{Zeroize as _, Zeroizing};

use super::{PageChange, PageFailure, PageReply, PageResourceNote};

/// What stands where a released text was struck out.
pub const STRUCK: &str = "[withheld]";
/// The most released texts a window keeps to strike: the newest of them.
pub const MAX_STRUCK: usize = 8;

/// The kind of field a released text is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillInto {
    /// A password: only a password field takes it.
    Password,
    /// A code a sign-in asks for as its second step.
    Code,
    /// Any other value kept for the site.
    Value,
}

impl FillInto {
    /// The word the engine knows it by.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::Code => "code",
            Self::Value => "value",
        }
    }
}

/// A text its keeper released for one site.
///
/// It is held once. A copy shares that one text and holds no second, whoever sends it to the
/// engine takes it out, and from then on no copy holds any. It prints nothing, and the memory
/// that held it is wiped when the taken text is dropped.
#[derive(Clone)]
pub struct ReleasedText(Arc<Mutex<Option<Zeroizing<String>>>>);

impl ReleasedText {
    /// `text`, held from now on by this alone.
    #[must_use]
    pub fn new(text: String) -> Self {
        Self(Arc::new(Mutex::new(Some(Zeroizing::new(text)))))
    }

    fn held(&self) -> MutexGuard<'_, Option<Zeroizing<String>>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Take the text out, for the one write to the engine. `None` once it was taken: no copy
    /// of the request that carried it holds it any more.
    #[must_use]
    pub fn take(&self) -> Option<Zeroizing<String>> {
        self.held().take()
    }

    /// What `read` makes of the text while it is still held, without copying it.
    #[must_use]
    pub fn read<R>(&self, read: impl FnOnce(&str) -> R) -> Option<R> {
        self.held().as_ref().map(|text| read(text.as_str()))
    }

    /// How long the text is in bytes, while it is still held.
    #[must_use]
    pub fn held_bytes(&self) -> Option<usize> {
        self.read(str::len)
    }
}

impl std::fmt::Debug for ReleasedText {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ReleasedText(..)")
    }
}

/// Two are equal when they are one text. What each holds is never compared.
impl PartialEq for ReleasedText {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// The released texts a window sent, kept to strike them out of whatever its page says of
/// them. Each is wiped when the window lets go of it.
#[derive(Default)]
pub struct StruckTexts {
    held: Vec<Zeroizing<String>>,
}

impl std::fmt::Debug for StruckTexts {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StruckTexts")
            .field("held", &self.held.len())
            .finish()
    }
}

impl StruckTexts {
    /// Keep what `text` holds: from now on it is struck out of every answer. Nothing is kept
    /// of a text that was taken already or holds nothing, and beyond [`MAX_STRUCK`] the oldest
    /// goes.
    pub fn hold(&mut self, text: &ReleasedText) {
        let Some(copy) = text.read(|text| Zeroizing::new(text.to_owned())) else {
            return;
        };
        if copy.is_empty() || self.held.iter().any(|held| held.as_str() == copy.as_str()) {
            return;
        }
        if self.held.len() >= MAX_STRUCK {
            self.held.remove(0);
        }
        self.held.push(copy);
    }

    /// Whether nothing is kept to strike.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    /// Strike every kept text out of `text`. Whether it held any.
    pub fn strike(&self, text: &mut String) -> bool {
        let mut struck = false;
        for held in &self.held {
            if let Some(cleared) = without(text, held.as_bytes()) {
                // What is replaced still holds the text: it is wiped, not left to the allocator.
                text.zeroize();
                *text = cleared;
                struck = true;
            }
        }
        struck
    }

    /// Strike every kept text out of `value`: out of each text in it, each name a page chose,
    /// and a number that is one. Whether it held any.
    pub fn strike_json(&self, value: &mut serde_json::Value) -> bool {
        if self.held.is_empty() {
            return false;
        }
        match value {
            serde_json::Value::String(text) => self.strike(text),
            serde_json::Value::Number(number) => {
                let mut written = number.to_string();
                let struck = self.strike(&mut written);
                if struck {
                    *value = serde_json::Value::String(written);
                }
                struck
            }
            serde_json::Value::Array(items) => {
                let mut struck = false;
                for item in items {
                    struck |= self.strike_json(item);
                }
                struck
            }
            serde_json::Value::Object(fields) => {
                let mut struck = false;
                for field in fields.values_mut() {
                    struck |= self.strike_json(field);
                }
                let named: Vec<String> = fields
                    .keys()
                    .filter(|name| self.holds(name))
                    .cloned()
                    .collect();
                for name in named {
                    if let Some(field) = fields.remove(&name) {
                        let mut cleared = name;
                        self.strike(&mut cleared);
                        fields.insert(cleared, field);
                        struck = true;
                    }
                }
                struck
            }
            serde_json::Value::Null | serde_json::Value::Bool(_) => false,
        }
    }

    /// Strike every kept text out of what the engine answered. The picture is not read: the
    /// engine draws a field it filled this way as dots.
    pub fn strike_reply(&self, reply: &mut PageReply) {
        if self.held.is_empty() {
            return;
        }
        self.strike(&mut reply.url);
        self.strike(&mut reply.message);
        if let Some(outline) = &mut reply.outline {
            self.strike(outline);
        }
        if let Some(data) = &mut reply.data {
            self.strike_json(data);
        }
        self.strike_note(&mut reply.record);
    }

    /// Strike every kept text out of a refusal of the engine's.
    pub fn strike_failure(&self, failure: &mut PageFailure) {
        if self.held.is_empty() {
            return;
        }
        self.strike(&mut failure.message);
        self.strike_note(&mut failure.record);
    }

    /// Strike every kept text out of what the engine says changed on the page.
    pub fn strike_change(&self, change: &mut PageChange) {
        if self.held.is_empty() {
            return;
        }
        for form in &mut change.forms {
            if let Some(signature) = &mut form.signature {
                self.strike(signature);
            }
        }
        for field in &mut change.fields {
            self.strike(&mut field.role);
            self.strike(&mut field.name);
            if let Some(question) = &mut field.question {
                self.strike(question);
            }
        }
    }

    fn strike_note(&self, note: &mut PageResourceNote) {
        for other in &mut note.others {
            self.strike_json(other);
        }
    }

    fn holds(&self, text: &str) -> bool {
        self.held
            .iter()
            .any(|held| without(text, held.as_bytes()).is_some())
    }
}

/// `text` with [`STRUCK`] wherever it holds `needle`; `None` when it holds it nowhere.
fn without(text: &str, needle: &[u8]) -> Option<String> {
    if needle.is_empty() {
        return None;
    }
    let hay = text.as_bytes();
    let mut cleared: Option<String> = None;
    let mut kept_from = 0_usize;
    let mut at = 0_usize;
    while at < hay.len() {
        match match_end(hay, at, needle) {
            Some(end) if text.is_char_boundary(at) && text.is_char_boundary(end) => {
                let out = cleared.get_or_insert_with(|| String::with_capacity(text.len()));
                out.push_str(text.get(kept_from..at).unwrap_or_default());
                out.push_str(STRUCK);
                kept_from = end;
                at = end;
            }
            _ => at = at.saturating_add(1),
        }
    }
    cleared.map(|mut out| {
        out.push_str(text.get(kept_from..).unwrap_or_default());
        out
    })
}

/// Where `needle` ends in `hay` when it stands there from `start` on: as it is, with any of
/// its bytes written as a percent escape, or with a blank written as a plus sign, which is how
/// an address and a sent form write a text.
fn match_end(hay: &[u8], start: usize, needle: &[u8]) -> Option<usize> {
    let mut at = start;
    for &byte in needle {
        let here = *hay.get(at)?;
        if here == b'%' && escaped_byte(hay, at) == Some(byte) {
            at = at.saturating_add(3);
        } else if here == byte || (byte == b' ' && here == b'+') {
            at = at.saturating_add(1);
        } else {
            return None;
        }
    }
    Some(at)
}

/// The byte the percent escape at `at` stands for, when one stands there.
fn escaped_byte(hay: &[u8], at: usize) -> Option<u8> {
    let high = hex(*hay.get(at.saturating_add(1))?)?;
    let low = hex(*hay.get(at.saturating_add(2))?)?;
    Some(high.wrapping_shl(4) | low)
}

fn hex(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit.wrapping_sub(b'0')),
        b'a'..=b'f' => Some(digit.wrapping_sub(b'a').wrapping_add(10)),
        b'A'..=b'F' => Some(digit.wrapping_sub(b'A').wrapping_add(10)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::super::{PageFieldChange, PageNavigation};
    use super::*;

    /// A text with what an address escapes: a blank, a slash, a plus and a percent sign.
    const TEXT: &str = "correct horse/battery staple+1%";
    const CODE: &str = "481516";

    #[test]
    fn a_released_text_is_held_once_and_prints_nothing() {
        let released = ReleasedText::new(TEXT.to_owned());
        let copy = released.clone();
        assert_eq!(released, copy, "a copy is the same text");
        assert_ne!(
            released,
            ReleasedText::new(TEXT.to_owned()),
            "what two hold is never compared"
        );
        assert_eq!(released.held_bytes(), Some(TEXT.len()));
        assert_eq!(format!("{released:?}"), "ReleasedText(..)");
        assert_eq!(copy.read(|text| text == TEXT), Some(true));

        // The one write takes it, and then no copy holds it.
        let taken = copy.take().unwrap();
        assert_eq!(taken.as_str(), TEXT);
        assert!(released.take().is_none());
        assert_eq!(released.held_bytes(), None);
        assert_eq!(released.read(str::len), None);

        for (into, word) in [
            (FillInto::Password, "password"),
            (FillInto::Code, "code"),
            (FillInto::Value, "value"),
        ] {
            assert_eq!(into.as_str(), word);
        }
    }

    #[test]
    fn what_a_window_sent_is_struck_out_however_the_page_writes_it() {
        let mut struck = StruckTexts::default();
        assert!(struck.is_empty());
        struck.hold(&ReleasedText::new(TEXT.to_owned()));
        struck.hold(&ReleasedText::new(CODE.to_owned()));
        assert!(!struck.is_empty());
        let printed = format!("{struck:?}");
        assert!(
            !printed.contains(CODE) && !printed.contains("horse"),
            "{printed}"
        );

        for said in [
            format!("signed in with {TEXT}"),
            // As a script escapes it for an address, in either case of the digits.
            "q=correct%20horse%2Fbattery%20staple%2B1%25&x=1".to_owned(),
            "q=correct%20horse%2fbattery%20staple%2b1%25&x=1".to_owned(),
            // As a sent form writes it: a blank is a plus sign.
            "https://example.test/in?p=correct+horse%2Fbattery+staple%2B1%25".to_owned(),
            // Some bytes escaped that need not be, and the last percent sign as it is.
            "%63%6f%72%72%65%63%74 horse/battery%20staple+1%".to_owned(),
            format!("code {CODE}, twice: {CODE}"),
        ] {
            let mut text = said.clone();
            assert!(struck.strike(&mut text), "{said}");
            assert!(text.contains(STRUCK), "{text}");
            for held in ["correct", "horse", "staple", CODE] {
                assert!(!text.contains(held), "{text}");
            }
        }
        let mut whole = "q=correct%20horse%2Fbattery%20staple%2B1%25&x=1".to_owned();
        struck.strike(&mut whole);
        assert_eq!(whole, format!("q={STRUCK}&x=1"));

        // A part of it is no use of it, and is left as the page wrote it.
        let mut apart = "nothing of it: correct horse, 48151".to_owned();
        assert!(!struck.strike(&mut apart));
        assert_eq!(apart, "nothing of it: correct horse, 48151");

        let mut data = serde_json::json!({
            "entries": [
                {"level": "log", "text": format!("pw={TEXT}")},
                {"url": format!("https://example.test/in?code={CODE}"), "status": 200},
                {"args": [481_516, "x", null, true]},
            ],
            CODE: true,
        });
        assert!(struck.strike_json(&mut data));
        let written = data.to_string();
        assert!(
            !written.contains(CODE) && !written.contains("horse"),
            "{written}"
        );
        assert_eq!(data["entries"][2]["args"][0], STRUCK);
        assert_eq!(data["entries"][2]["args"][1], "x");
        assert_eq!(data["entries"][1]["status"], 200);
        assert_eq!(data[STRUCK], true);
        let mut clean = serde_json::json!({"entries": [{"text": "nothing"}], "n": 7});
        assert!(!struck.strike_json(&mut clean));
        assert_eq!(
            clean,
            serde_json::json!({"entries": [{"text": "nothing"}], "n": 7})
        );
    }

    #[test]
    fn an_answer_a_refusal_and_a_change_are_struck_whole() {
        let mut struck = StruckTexts::default();
        struck.hold(&ReleasedText::new(TEXT.to_owned()));
        let mut reply = PageReply {
            url: "https://example.test/in?p=correct+horse%2Fbattery+staple%2B1%25".into(),
            message: format!("Filled with {TEXT}"),
            outline: Some(format!("- textbox \"{TEXT}\"")),
            data: Some(serde_json::json!({"entries": [{"text": TEXT}]})),
            image: None,
            navigation: PageNavigation::None,
            loading: false,
            record: PageResourceNote {
                seq: Some(3),
                others: vec![serde_json::json!({"text": TEXT})],
                others_earlier: 0,
            },
        };
        struck.strike_reply(&mut reply);
        let printed = format!("{reply:?}");
        assert!(
            !printed.contains("horse") && printed.contains(STRUCK),
            "{printed}"
        );
        assert_eq!(reply.url, format!("https://example.test/in?p={STRUCK}"));

        let mut failure = PageFailure::new("refused", format!("no field takes {TEXT}"));
        struck.strike_failure(&mut failure);
        let printed = format!("{failure:?}");
        assert!(!printed.contains("horse"), "{printed}");
        assert_eq!(failure.code, "refused");

        let mut change = PageChange::default();
        change.fields.push(PageFieldChange {
            name: TEXT.into(),
            question: Some(format!("Is it {TEXT}?")),
            ..PageFieldChange::default()
        });
        struck.strike_change(&mut change);
        let printed = format!("{change:?}");
        assert!(!printed.contains("horse"), "{printed}");

        // A window that sent nothing changes nothing.
        let nothing = StruckTexts::default();
        let mut said = format!("pw={TEXT}");
        assert!(!nothing.strike(&mut said));
        assert_eq!(said, format!("pw={TEXT}"));
    }

    #[test]
    fn a_window_keeps_the_newest_texts_and_nothing_of_one_taken() {
        let mut struck = StruckTexts::default();
        let taken = ReleasedText::new("taken-before-it-was-kept".to_owned());
        drop(taken.take());
        struck.hold(&taken);
        struck.hold(&ReleasedText::new(String::new()));
        assert!(struck.is_empty());

        for number in 0..=MAX_STRUCK {
            struck.hold(&ReleasedText::new(format!("released-{number}-text")));
        }
        let mut said = format!("released-0-text and released-{MAX_STRUCK}-text");
        assert!(struck.strike(&mut said));
        assert_eq!(said, format!("released-0-text and {STRUCK}"));
    }
}
