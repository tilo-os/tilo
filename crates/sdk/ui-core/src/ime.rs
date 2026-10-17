//! Focused widget IME intent shared by native windows and browser input bridges.
/// Keyed, ephemeral equality check for a field's text and selection. This is
/// invalidation metadata, never a persisted text hash or insertion authority.
pub fn edit_fingerprint(text: &str, anchor: usize, focus: usize, key: &[u8; 32]) -> [u8; 32] {
    let mut hash = blake3::Hasher::new_keyed(key);
    hash.update(&anchor.to_le_bytes());
    hash.update(&focus.to_le_bytes());
    hash.update(text.as_bytes());
    *hash.finalize().as_bytes()
}

/// Scope a private field stamp to its editable subtarget inside one widget.
pub fn scoped_fingerprint(scope: &[u8], field: [u8; 32], key: &[u8; 32]) -> [u8; 32] {
    let mut hash = blake3::Hasher::new_keyed(key);
    hash.update(b"tilo.ime.subtarget.v1");
    hash.update(&field);
    hash.update(scope);
    *hash.finalize().as_bytes()
}

/// What a field holds, for an input method to pick its layout: an email
/// field gets `@`, a phone field the dial pad, a PIN the digit pad.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum InputPurpose {
    /// Free text.
    #[default]
    Text,
    /// A person's name.
    Name,
    Search,
    Email,
    Url,
    /// A number, with sign and decimal separator.
    Number,
    /// A number with a fractional part.
    Decimal,
    Phone,
    /// Digits only, such as a pairing passkey.
    Pin,
    /// A code sent to the person to confirm a sign-in.
    OneTimeCode,
    Password,
    Date,
    Time,
    /// A terminal, whose keyboard wants the extra keys.
    Terminal,
}

/// Which letters an input method capitalises on its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Capitalization {
    /// Its own default.
    #[default]
    None,
    /// The first letter of each sentence.
    Sentences,
    /// The first letter of each word.
    Words,
}

/// How an input method may help in a field. Each is a request on top of the
/// input method's own default for the purpose; none set leaves that default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct InputHints {
    /// Suggest completions of the word being typed.
    pub completion: bool,
    /// Suggest corrections of misspelt words.
    pub spellcheck: bool,
    pub capitalization: Capitalization,
    /// Prefer lower-case letters.
    pub lowercase: bool,
    /// Line breaks are text.
    pub multiline: bool,
    /// Do not show what is typed, in the keyboard either.
    pub hidden: bool,
    /// Learn nothing from what is typed and suggest nothing from it.
    pub sensitive: bool,
}

/// A field's purpose and hints, as the toolkit tells an input method.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct InputContent {
    pub purpose: InputPurpose,
    pub hints: InputHints,
}

/// Wire values of `zwp_text_input_v3`'s `content_purpose` and
/// `content_hint`.
mod wire {
    pub const NORMAL: u32 = 0;
    pub const DIGITS: u32 = 2;
    pub const NUMBER: u32 = 3;
    pub const PHONE: u32 = 4;
    pub const URL: u32 = 5;
    pub const EMAIL: u32 = 6;
    pub const NAME: u32 = 7;
    pub const PASSWORD: u32 = 8;
    pub const PIN: u32 = 9;
    pub const DATE: u32 = 10;
    pub const TIME: u32 = 11;
    pub const TERMINAL: u32 = 13;

    pub const COMPLETION: u32 = 0x1;
    pub const SPELLCHECK: u32 = 0x2;
    pub const AUTO_CAPITALIZATION: u32 = 0x4;
    pub const LOWERCASE: u32 = 0x8;
    pub const TITLECASE: u32 = 0x20;
    pub const HIDDEN_TEXT: u32 = 0x40;
    pub const SENSITIVE_DATA: u32 = 0x80;
    pub const MULTILINE: u32 = 0x200;
}

impl InputContent {
    /// Free text, with the input method's defaults.
    pub const TEXT: Self = Self::of(InputPurpose::Text);
    pub const TERMINAL: Self = Self::of(InputPurpose::Terminal);
    /// A password: hidden, and nothing learned from it.
    pub const PASSWORD: Self = Self::of(InputPurpose::Password).secret();

    #[must_use]
    pub const fn of(purpose: InputPurpose) -> Self {
        Self {
            purpose,
            hints: InputHints {
                completion: false,
                spellcheck: false,
                capitalization: Capitalization::None,
                lowercase: false,
                multiline: false,
                hidden: false,
                sensitive: false,
            },
        }
    }

    /// The same content, hidden and sensitive: nothing typed is shown,
    /// learned or suggested.
    #[must_use]
    pub const fn secret(mut self) -> Self {
        self.hints.hidden = true;
        self.hints.sensitive = true;
        self
    }

    /// Whether the input method must neither show nor learn what is typed.
    #[must_use]
    pub const fn is_secret(self) -> bool {
        self.hints.hidden || self.hints.sensitive || matches!(self.purpose, InputPurpose::Password)
    }

    /// The `zwp_text_input_v3` content hint bits and purpose for this
    /// content. The protocol has no search, decimal or one-time-code
    /// purpose; those ask for the nearest layout (free text, number,
    /// digits).
    #[must_use]
    pub const fn wayland(self) -> (u32, u32) {
        let purpose = match self.purpose {
            InputPurpose::Text | InputPurpose::Search => wire::NORMAL,
            InputPurpose::Name => wire::NAME,
            InputPurpose::Email => wire::EMAIL,
            InputPurpose::Url => wire::URL,
            InputPurpose::Number | InputPurpose::Decimal => wire::NUMBER,
            InputPurpose::Phone => wire::PHONE,
            InputPurpose::Pin => wire::PIN,
            InputPurpose::OneTimeCode => wire::DIGITS,
            InputPurpose::Password => wire::PASSWORD,
            InputPurpose::Date => wire::DATE,
            InputPurpose::Time => wire::TIME,
            InputPurpose::Terminal => wire::TERMINAL,
        };
        let h = self.hints;
        let mut hint = 0;
        if h.completion {
            hint |= wire::COMPLETION;
        }
        if h.spellcheck {
            hint |= wire::SPELLCHECK;
        }
        hint |= match h.capitalization {
            Capitalization::None => 0,
            Capitalization::Sentences => wire::AUTO_CAPITALIZATION,
            Capitalization::Words => wire::TITLECASE,
        };
        if h.lowercase {
            hint |= wire::LOWERCASE;
        }
        if h.multiline {
            hint |= wire::MULTILINE;
        }
        if h.hidden {
            hint |= wire::HIDDEN_TEXT;
        }
        if h.sensitive {
            hint |= wire::SENSITIVE_DATA;
        }
        (hint, purpose)
    }

    /// The nearest of winit's own purposes, for a platform that takes no
    /// more than that.
    #[must_use]
    pub const fn winit_purpose(self) -> winit::window::ImePurpose {
        if self.is_secret() {
            winit::window::ImePurpose::Password
        } else if matches!(self.purpose, InputPurpose::Terminal) {
            winit::window::ImePurpose::Terminal
        } else {
            winit::window::ImePurpose::Normal
        }
    }
}

/// What a browser's text control is told about a field: the attributes a
/// web runner sets on the editor it focuses for the field. An empty
/// `enterkeyhint` leaves the browser's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct HtmlInputHints {
    pub inputmode: &'static str,
    pub enterkeyhint: &'static str,
    pub autocapitalize: &'static str,
    pub autocomplete: &'static str,
    pub spellcheck: bool,
}

impl InputContent {
    /// The browser attributes for this content. Free text keeps what a web
    /// runner's editor always had: no capitalisation, autofill or
    /// spellchecking of the browser's own, which the toolkit does itself.
    #[must_use]
    pub const fn html(self) -> HtmlInputHints {
        let inputmode = match self.purpose {
            InputPurpose::Search => "search",
            InputPurpose::Email => "email",
            InputPurpose::Url => "url",
            InputPurpose::Number | InputPurpose::Decimal => "decimal",
            InputPurpose::Phone => "tel",
            InputPurpose::Pin | InputPurpose::OneTimeCode => "numeric",
            InputPurpose::Text
            | InputPurpose::Name
            | InputPurpose::Password
            | InputPurpose::Date
            | InputPurpose::Time
            | InputPurpose::Terminal => "text",
        };
        let secret = self.is_secret();
        let autocapitalize = match self.hints.capitalization {
            _ if secret || self.hints.lowercase => "off",
            Capitalization::None => "off",
            Capitalization::Sentences => "sentences",
            Capitalization::Words => "words",
        };
        // Autofill offers only what a field of this purpose holds, and
        // never anything for a secret it could leak into.
        let autocomplete = match self.purpose {
            _ if secret => "off",
            InputPurpose::Name => "name",
            InputPurpose::Email => "email",
            InputPurpose::Url => "url",
            InputPurpose::Phone => "tel",
            InputPurpose::OneTimeCode => "one-time-code",
            _ => "off",
        };
        HtmlInputHints {
            inputmode,
            enterkeyhint: if matches!(self.purpose, InputPurpose::Search) {
                "search"
            } else {
                ""
            },
            autocapitalize,
            autocomplete,
            spellcheck: self.hints.spellcheck && !secret,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImeWindowState {
    /// What the field holds and how the input method may help in it,
    /// including whether it takes line feeds.
    pub content: InputContent,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl ImeWindowState {
    pub fn from_widget(widget: &dyn crate::Widget) -> Option<Self> {
        let mut content = widget.ime_content()?;
        let rect = widget.ime_cursor_rect()?;
        // Line feeds are text only where text is: never in a secret or a
        // terminal.
        content.hints.multiline |= !content.is_secret()
            && content.purpose != InputPurpose::Terminal
            && widget.ime_multiline();
        Some(Self::from_rect(content, rect))
    }

    #[must_use]
    pub fn from_rect(content: InputContent, rect: vello::kurbo::Rect) -> Self {
        let bounded = |v: f64| {
            if v.is_finite() {
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "value is clamped to [0.0, u32::MAX] on the line above, so the cast is exact"
                )]
                {
                    v.max(0.0).min(f64::from(u32::MAX)).round() as u32
                }
            } else {
                0
            }
        };
        ImeWindowState {
            content,
            x: bounded(rect.x0),
            y: bounded(rect.y0),
            width: bounded(rect.width()).max(1),
            height: bounded(rect.height()).max(1),
        }
    }
}

/// What an input method may read of the focused field: its text around the
/// selection, for suggestions, autocorrection and word deletion.
///
/// Read from the field's accessibility node, so a field that hides its value
/// from assistive technology (a password) hides it from the keyboard too.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImeSurroundingText {
    pub text: String,
    /// Byte offsets into `text`.
    pub anchor: usize,
    pub focus: usize,
}

impl ImeSurroundingText {
    /// Bytes of context kept on each side of the selection.
    pub const CONTEXT: usize = 2048;

    pub fn from_widget(widget: &dyn crate::Widget) -> Option<Self> {
        let node = widget.accessibility()?;
        if node.state.protected {
            return None;
        }
        let selection = node.text_selection?;
        Self::around(&node.value?, selection.anchor, selection.focus)
    }

    /// The selection within `text`, with up to [`Self::CONTEXT`] bytes on
    /// each side. `None` when an offset is not a character boundary.
    pub fn around(text: &str, anchor: usize, focus: usize) -> Option<Self> {
        if !text.is_char_boundary(anchor) || !text.is_char_boundary(focus) {
            return None;
        }
        let start = text.ceil_char_boundary(anchor.min(focus).saturating_sub(Self::CONTEXT));
        let end = text.floor_char_boundary(anchor.max(focus).saturating_add(Self::CONTEXT));
        Some(Self {
            text: text.get(start..end)?.to_owned(),
            anchor: anchor - start,
            focus: focus - start,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ImeSurroundingText;
    use crate::Widget;
    use crate::widget::TextInput;

    #[test]
    fn surrounding_text_is_the_fields_text_and_selection() {
        let mut field = TextInput::local().with_initial_value("hello world");
        field.set_focus(true);
        assert!(field.perform_action(
            &crate::a11y::Action::SetSelection {
                anchor: 6,
                focus: 11
            },
            &[],
            &mut crate::widget::Edits::lend(&mut automerge::AutoCommit::new()),
            &automerge::ROOT,
        ));
        assert_eq!(
            ImeSurroundingText::from_widget(&field),
            Some(ImeSurroundingText {
                text: "hello world".into(),
                anchor: 6,
                focus: 11,
            })
        );
    }

    #[test]
    fn deferred_input_fingerprint_changes_with_selection_at_the_same_caret() {
        let key = [7; 32];
        let mut field = TextInput::local().with_initial_value("hello");
        field.set_focus(true);
        field.set_cursor_pos(5);
        let before = field.ime_edit_fingerprint(&key).unwrap();
        let caret = field.cursor_pos();
        assert!(field.perform_action(
            &crate::a11y::Action::SetSelection {
                anchor: 0,
                focus: 5
            },
            &[],
            &mut crate::widget::Edits::lend(&mut automerge::AutoCommit::new()),
            &automerge::ROOT,
        ));
        assert_eq!(field.cursor_pos(), caret);
        assert_ne!(field.ime_edit_fingerprint(&key).unwrap(), before);
        assert!(
            TextInput::local()
                .with_secret(true)
                .ime_edit_fingerprint(&key)
                .is_none()
        );
    }

    #[test]
    fn a_browser_editor_is_told_each_purpose_and_keeps_free_texts_defaults() {
        use super::{Capitalization, HtmlInputHints, InputContent, InputPurpose};
        let html = |content: InputContent| content.html();
        let attrs =
            |inputmode, enterkeyhint, autocapitalize, autocomplete, spellcheck| HtmlInputHints {
                inputmode,
                enterkeyhint,
                autocapitalize,
                autocomplete,
                spellcheck,
            };
        assert_eq!(
            html(InputContent::TEXT),
            attrs("text", "", "off", "off", false)
        );
        let of = InputContent::of;
        assert_eq!(
            html(of(InputPurpose::Email)),
            attrs("email", "", "off", "email", false)
        );
        assert_eq!(
            html(of(InputPurpose::Phone)),
            attrs("tel", "", "off", "tel", false)
        );
        assert_eq!(
            html(of(InputPurpose::Search)),
            attrs("search", "search", "off", "off", false)
        );
        assert_eq!(
            html(of(InputPurpose::OneTimeCode)),
            attrs("numeric", "", "off", "one-time-code", false)
        );
        let mut name = of(InputPurpose::Name);
        name.hints.capitalization = Capitalization::Words;
        assert_eq!(html(name), attrs("text", "", "words", "name", false));
        let mut checked = InputContent::TEXT;
        checked.hints.spellcheck = true;
        checked.hints.capitalization = Capitalization::Sentences;
        assert_eq!(html(checked), attrs("text", "", "sentences", "off", true));
        // A secret offers no autofill, capitalisation or spellchecking.
        let mut secret = of(InputPurpose::Email).secret();
        secret.hints.spellcheck = true;
        secret.hints.capitalization = Capitalization::Words;
        assert_eq!(html(secret), attrs("email", "", "off", "off", false));
        assert_eq!(
            html(InputContent::PASSWORD),
            attrs("text", "", "off", "off", false)
        );
    }

    #[test]
    fn a_password_field_gives_the_keyboard_nothing() {
        let mut field = TextInput::local()
            .with_initial_value("hunter2")
            .with_secret(true);
        field.set_focus(true);
        assert_eq!(ImeSurroundingText::from_widget(&field), None);
    }

    #[test]
    fn long_text_is_cut_to_whole_characters_around_the_selection() {
        let before = "é".repeat(ImeSurroundingText::CONTEXT);
        let after = "ü".repeat(ImeSurroundingText::CONTEXT);
        let text = format!("{before}|{after}");
        let caret = before.len();
        let around = ImeSurroundingText::around(&text, caret, caret + 1).unwrap();
        assert_eq!(around.text.len(), 2 * ImeSurroundingText::CONTEXT + 1);
        assert_eq!(&around.text[around.anchor..around.focus], "|");
        assert!(around.text.starts_with('é') && around.text.ends_with('ü'));
        assert_eq!(ImeSurroundingText::around("é", 1, 1), None);
    }
}
