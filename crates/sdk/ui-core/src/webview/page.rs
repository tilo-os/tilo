//! Typed requests against a live page's content.
//!
//! Browser chrome turns its page actions into a [`PageRequest`]; the engine
//! answers with exactly one [`WebViewEvent::PageResult`](super::WebViewEvent)
//! carrying the same request id. Elements are addressed by the refs a prior
//! [`PageRequest::Outline`] printed. A ref names one element of one document,
//! so a ref from a page the tab has since left is refused rather than
//! resolved against whatever now sits at the same position.
//!
//! A page inside a frame is addressed by the number the outline printed for
//! the frame (`[frame=3]`). The number is the tab's: a frame keeps it while
//! frames around it come and go, and a frame inside a frame has a number of
//! its own. A request with that number runs in the frame's own document, and
//! its refs and outline describe that document alone.
//!
//! Several actors may work in one tab: two agents, agents on other devices,
//! an agent and the person reviewing what it did. A request then travels with
//! the [`PageActor`] who asks, the engine keeps a record of what each did
//! ([`PageRequest::Ledger`], [`PageRequest::Fields`]), and an answer says
//! where its action stands in that record and what the others did meanwhile
//! ([`PageRecordNote`]).

use super::KeyModifiers;

/// Upper bound on an outline as it crosses the engine boundary.
pub const MAX_OUTLINE_BYTES: usize = 256 * 1024;
/// Upper bound on structured reply data (`PageReply::data`), serialised.
pub const MAX_DATA_BYTES: usize = 256 * 1024;
/// The widest picture a capture paints; a wider request is scaled down to
/// it, so a picture is never larger than a person could look at.
pub const MAX_CAPTURE_WIDTH: u16 = 1600;
/// Upper bound on the pixels one capture may carry, as a PNG. What the engine
/// paints is bounded the same way before it ever reaches this boundary.
pub const MAX_CAPTURE_BYTES: usize = 8 << 20;
/// Bounds of a settle wait, milliseconds.
pub const MIN_SETTLE_QUIET_MS: u32 = 50;
pub const MAX_SETTLE_MS: u32 = 20_000;
/// Largest viewport coordinate a point may name, CSS pixels.
const MAX_COORDINATE: f64 = 100_000.0;
/// Upper bound on the points of one pointer path, and the steps of one
/// drag, so a gesture is always a bounded amount of work for the engine.
pub const MAX_PATH_POINTS: usize = 1024;
pub const MAX_DRAG_STEPS: usize = 64;
/// Upper bound on the text one fill may insert.
pub const MAX_FILL_BYTES: usize = 64 * 1024;
/// Upper bound on the options one select may choose.
pub const MAX_SELECT_VALUES: usize = 64;
/// Upper bound on one option value or label.
pub const MAX_SELECT_VALUE_BYTES: usize = 1024;
/// Upper bound on the files one fill of a file input may choose.
pub const MAX_FILE_PATHS: usize = 16;
/// Upper bound on one chosen file's path.
pub const MAX_FILE_PATH_BYTES: usize = 4096;
/// Upper bound on the steps of one pointer gesture.
pub const MAX_POINTER_STEPS: usize = 256;
/// Upper bound on the button names a gesture may name.
const POINTER_BUTTONS: &[&str] = &["left", "right"];
/// How far one scroll may ask to travel in one go, CSS pixels: a wheel notch
/// moves about 100, and a bounded distance keeps one request a bounded amount
/// of work for the page.
pub const MAX_SCROLL_BY: u32 = 10_000;
const MAX_REF_BYTES: usize = 40;
/// A frame id is a small decimal number, such as `3`. The dotted path an
/// older outline printed for a frame inside a frame (`1.2`) still fits.
const MAX_FRAME_BYTES: usize = 16;
const MAX_FRAME_DEPTH: usize = 8;
/// Upper bound on the accessible name a
/// [`PageRequest::Find`] matches, as the outline prints it.
pub const MAX_FIND_NAME_BYTES: usize = 512;
/// Upper bound on the role words a [`PageRequest::Find`] matches: a few
/// words of the outline's own vocabulary, never a selector.
pub const MAX_FIND_ROLE_BYTES: usize = 40;
const MAX_MESSAGE_BYTES: usize = 1024;
const MAX_CODE_BYTES: usize = 64;
/// The longest name an engine takes for who asks, in UTF-16 units, which is
/// how the engine counts it.
pub const MAX_ACTOR_UNITS: usize = 64;
/// How much of a longer principal a shortened name keeps before its digest.
const ACTOR_HEAD_UNITS: usize = 31;
/// The most entries of other actors' work one answer carries.
pub const MAX_OTHERS: usize = 8;
/// The furthest line a read of the outline may start from.
pub const MAX_OUTLINE_FROM: u32 = 1_000_000;
/// The longest words a [`PageRequest::Search`] looks for. This and the form
/// bounds below are counted in UTF-16 units, which is how an engine counts
/// them, so a name in any script is bounded as the engine bounds it.
pub const MAX_SEARCH_UNITS: usize = 200;
/// The longest id or signature that names a form.
pub const MAX_FORM_UNITS: usize = 64;
/// The most answers one [`PageRequest::FillForm`] carries.
pub const MAX_FORM_ANSWERS: usize = 200;
/// The longest name a form asks an answer under, and the longest label of
/// the button that sends it.
pub const MAX_FORM_NAME_UNITS: usize = 200;
/// The longest text one answer places.
pub const MAX_FORM_TEXT_UNITS: usize = 65_536;
/// The most choices one answer makes of several, and the longest of them.
pub const MAX_FORM_CHOICES: usize = 64;
pub const MAX_FORM_CHOICE_UNITS: usize = 1024;

/// How long an engine counts `text` to be.
fn units(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Whether a [`PageRequest::FillForm`] sends the form it answered, and how.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum FormSubmit {
    /// The answers are placed and the form is left for its caller to send.
    #[default]
    No,
    /// With the form's first submit button.
    First,
    /// With the submit button of this label.
    Button(String),
}

/// Who asks a page request: the name the engine keeps an actor's reads and
/// actions under, so each actor in a tab is told what changed since its own
/// last read and what the others did.
///
/// It is the principal the caller admitted, unchanged, whenever the engine
/// can take it. A principal longer than [`MAX_ACTOR_UNITS`] is shortened the
/// same way every time: its first 31 UTF-16 units, a `~`, and the first 128
/// bits of its BLAKE3 digest in hex. Two long principals that begin alike
/// still get two names, and one principal always gets the same name, so the
/// record never merges two actors or splits one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PageActor(String);

impl PageActor {
    /// The name `principal` acts under, or `None` for no principal at all:
    /// a caller nobody named is the tab's one unnamed actor.
    pub fn named(principal: &str) -> Option<Self> {
        if principal.is_empty() {
            return None;
        }
        if principal.encode_utf16().count() <= MAX_ACTOR_UNITS {
            return Some(Self(principal.to_owned()));
        }
        let mut name = String::with_capacity(MAX_ACTOR_UNITS);
        let mut units = 0;
        for character in principal.chars() {
            units += character.len_utf16();
            if units > ACTOR_HEAD_UNITS {
                break;
            }
            name.push(character);
        }
        name.push('~');
        let digest = blake3::hash(principal.as_bytes());
        for byte in &digest.as_bytes()[..16] {
            use std::fmt::Write as _;
            let _ = write!(name, "{byte:02x}");
        }
        Some(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PageActor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PageRequest {
    /// A compact accessibility outline of the document. Interactive elements
    /// carry the refs every other request addresses, and a frame carries the
    /// id that addresses its own document. A long page is read in parts: a
    /// read that stops short says the line to read on from, and `from` is
    /// that line (the first is 1). Lines keep their numbers while the page
    /// does not change. Reading on does not move what
    /// [`PageRequest::Changes`] compares with; only a read from the top does.
    Outline {
        from: Option<u32>,
        frame: Option<FrameId>,
    },
    /// A trusted pointer click at the element's visible centre.
    Click {
        element: String,
        frame: Option<FrameId>,
    },
    /// A trusted pointer move to the element's visible centre, or to the
    /// viewport point (`x`, `y`) when no element is named: moving the pointer
    /// alone, with no button, so the page's hover styles apply.
    Hover {
        element: Option<String>,
        point: Option<(f64, f64)>,
    },
    /// A trusted press, move and release on the element's visible centre, or
    /// on the viewport point when no element is named: the drag and drop
    /// gesture of a person holding the pointer down and moving it.
    Drag {
        from: Option<String>,
        from_point: Option<(f64, f64)>,
        to: Option<String>,
        to_point: Option<(f64, f64)>,
        steps: u8,
    },
    /// A trusted contact path: press at the first point, move through the
    /// rest and release at the last, for drawing on canvases and other
    /// surfaces that read pointer contact. Points are viewport CSS pixels
    /// when the element is named and are measured from its padding box,
    /// otherwise from the viewport's top left.
    Path {
        element: Option<String>,
        points: Vec<(f64, f64)>,
    },
    /// Replace an editable element's contents with `text`. A password or
    /// card field is refused unless `secret` says the caller means it, and
    /// then the text is a secret the caller keeps out of logs.
    Fill {
        element: String,
        text: String,
        secret: bool,
        frame: Option<FrameId>,
    },
    /// Choose `<select>` options by value or visible label.
    Select {
        element: String,
        values: Vec<String>,
        frame: Option<FrameId>,
    },
    /// Choose the files of an `<input type=file>`, by absolute path.
    ///
    /// The caller has already checked each path is a regular file it may
    /// read; the engine hands those files to the page as a person's own
    /// picker would, dispatching `input` and `change`. A path that names no
    /// readable regular file, or one the element's `accept` refuses, is
    /// refused rather than silently dropped.
    FillFiles {
        element: String,
        paths: Vec<String>,
        frame: Option<FrameId>,
    },
    /// A trusted key press, after focusing `element` when one is named.
    Press {
        element: Option<String>,
        key: PageKey,
        frame: Option<FrameId>,
    },
    /// Submit the element's form through the page's own validation. A form
    /// that sends its data to another site than the page's is refused unless
    /// `cross_origin` says the caller means it.
    Submit {
        element: String,
        cross_origin: bool,
        frame: Option<FrameId>,
    },
    /// What changed in the outline since this tab's last read or changes:
    /// lines added (`+`) and removed (`-`), or the whole outline when the
    /// document is new.
    Changes { frame: Option<FrameId> },
    /// The element a click at viewport point (`x`, `y`) in CSS pixels would
    /// hit, and the nearest interactive element holding it.
    At {
        x: f64,
        y: f64,
        frame: Option<FrameId>,
    },
    /// Wait until the document has stopped changing for `quiet_ms` and has
    /// loaded, or `timeout_ms` passed.
    Settle {
        quiet_ms: u32,
        timeout_ms: u32,
        frame: Option<FrameId>,
    },
    /// Console messages and uncaught errors after sequence number `since`.
    Console { since: u64 },
    /// Network requests after sequence number `since`: method, URL, type,
    /// status and failure, never headers, bodies or cookies.
    Network { since: u64 },
    /// The tab's record of who did what, after the place `since` names: every
    /// action that operated the page, with who asked, what it acted on and
    /// how it ended. It reads nothing of the page. A secret that was filled
    /// is never in it.
    Ledger { since: u64 },
    /// Every field of the document with what it holds now and who set it: an
    /// action (and whose), direct input as a person types, or the page's own
    /// script, and whether the field changed after it was set. Secrets are
    /// listed by length. It dispatches nothing to the page.
    Fields { frame: Option<FrameId> },
    /// The outline lines that hold `text`, in any letter case, each with its
    /// line number: a number a [`PageRequest::Outline`] reads on from. It
    /// reads what the outline reads, so a secret's value is found by neither.
    /// With no `frame` named, a search the page's own document has no match
    /// for looks on in the frames the page shows, and with `frames` it looks
    /// there as well as in the page: a match in a frame says which.
    Search {
        text: String,
        frame: Option<FrameId>,
        frames: bool,
    },
    /// The document's forms as what they ask: each form's id and signature,
    /// its questions by name and kind, the answers they offer, which are
    /// required, what each holds now and the buttons that send it.
    Forms { frame: Option<FrameId> },
    /// Answer a form by the names it asks under: `form` is the id or the
    /// signature [`PageRequest::Forms`] gave, `answers` an object from a
    /// question's name to text, a number, a boolean or a list of texts. The
    /// form is sent only when every answer was placed and nothing required
    /// is empty. A secret is never placed this way: the field is refused for
    /// [`PageRequest::Fill`] with `secret` to take. A form that sends its
    /// data to another site than the page's is refused unless `cross_origin`
    /// says the caller means it. With `frame`, the form is one of that
    /// frame's own document, as [`PageRequest::Forms`] with the same frame
    /// read it.
    FillForm {
        form: String,
        answers: serde_json::Map<String, serde_json::Value>,
        submit: FormSubmit,
        cross_origin: bool,
        frame: Option<FrameId>,
    },
    /// Show the document's article alone in a reading layout (`enabled`), or
    /// put the page back as it was. A page without an article is refused.
    Reader { enabled: bool },
    /// A picture of the page as it is shown: the box of `element` when one is
    /// named, the viewport otherwise, at most `max_width` pixels wide. A page
    /// can say things no outline reaches, and this is how they are seen.
    /// Captured by the engine itself, so the page cannot see or answer it.
    /// What the picture shows is page content: never trusted as instructions.
    Capture {
        element: Option<String>,
        max_width: Option<u32>,
        frame: Option<FrameId>,
    },
    /// The element a stable description names: the same role and name a
    /// person reads on it, wherever the document sits in its history.
    ///
    /// Every other request addresses an element by the ref one outline
    /// printed, and a ref belongs to one document. This one reads the page
    /// it is asked of, so a description recorded once is answered with the
    /// ref of the page as it is now: what a step replaying a person's work
    /// needs.
    ///
    /// With no `frame` named, a find the page's own document has no match
    /// for looks on in the frames the page shows, and with `frames` it looks
    /// there as well as in the page: the answer says the frame of what it
    /// found, and the ref acts there.
    Find {
        role: Option<String>,
        name: Option<String>,
        nth: Option<u32>,
        frame: Option<FrameId>,
        frames: bool,
    },
    /// Move the pointer over the page as a hand does: `steps` in order, a
    /// press and release of `button` where the page asks for one. A step
    /// naming `element` is placed in that element's box, every other step is
    /// a viewport position in CSS pixels. All of it is real input to the
    /// page, so hover, drag and drop and drawing see what a person's hand
    /// would have done, and the reply reports what changed.
    Pointer {
        steps: Vec<PointerStep>,
        button: PointerButton,
    },
    /// Turn the page's wheel as a hand does: `by` CSS pixels down (or up,
    /// when negative) at the element's visible centre, or at the viewport's
    /// when no element is named. `to` names an end of the page instead, and
    /// `into_view` an element to bring into view. The wheel is real input,
    /// so the page's scroll listeners, lazy loading and intersection
    /// observers run as they do for a person, and the reply reports what
    /// changed.
    Scroll {
        element: Option<String>,
        to: Option<ScrollTarget>,
        by: Option<i32>,
        into_view: Option<String>,
        frame: Option<FrameId>,
    },
}

/// Where a [`PageRequest::Scroll`] goes instead of a distance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollTarget {
    Top,
    Bottom,
}

impl ScrollTarget {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::Bottom => "bottom",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "top" => Some(Self::Top),
            "bottom" => Some(Self::Bottom),
            _ => None,
        }
    }
}

/// One step of a [`PageRequest::Pointer`] gesture.
#[derive(Debug, Clone, PartialEq)]
pub struct PointerStep {
    pub kind: PointerStepKind,
    /// Where the step happens, relative to `element`'s box when it names one.
    pub at: PointerPoint,
    /// The element the step is placed against. A gesture that keeps naming
    /// the same element keeps stepping inside the same box.
    pub element: Option<String>,
    /// The frame the step names, when its element lives in one: the step's
    /// point is then moved by the frame's place, as a click is.
    pub frame: Option<FrameId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerStepKind {
    Move,
    Down,
    Up,
}

impl PointerStepKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Move => "move",
            Self::Down => "down",
            Self::Up => "up",
        }
    }
}

/// Where a step happens: inside an element's box or at a viewport point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PointerPoint {
    InElement { x: f64, y: f64 },
    Viewport { x: f64, y: f64 },
}

impl PointerPoint {
    pub const fn coordinates(self) -> (f64, f64) {
        match self {
            Self::InElement { x, y } | Self::Viewport { x, y } => (x, y),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerButton {
    Left,
    Right,
}

impl PointerButton {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
        }
    }

    /// The button a gesture names, left when the name is not one we know.
    pub const fn parse(name: &str) -> Self {
        match name.as_bytes() {
            b"right" => Self::Right,
            _ => Self::Left,
        }
    }
}

impl PageRequest {
    pub const fn action(&self) -> &'static str {
        match self {
            Self::Outline { .. } => "outline",
            Self::Click { .. } => "click",
            Self::Hover { .. } => "hover",
            Self::Drag { .. } => "drag",
            Self::Path { .. } => "path",
            Self::Fill { .. } => "fill",
            Self::Select { .. } => "select",
            Self::FillFiles { .. } => "fill_files",
            Self::Press { .. } => "press",
            Self::Submit { .. } => "submit",
            Self::Changes { .. } => "changes",
            Self::At { .. } => "at",
            Self::Settle { .. } => "settle",
            Self::Console { .. } => "console",
            Self::Network { .. } => "network",
            Self::Ledger { .. } => "ledger",
            Self::Fields { .. } => "fields",
            Self::Search { .. } => "search",
            Self::Forms { .. } => "forms",
            Self::FillForm { .. } => "fill_form",
            Self::Reader { .. } => "reader",
            Self::Capture { .. } => "capture",
            Self::Find { .. } => "find",
            Self::Pointer { .. } => "pointer",
            Self::Scroll { .. } => "scroll",
        }
    }

    /// Whether the request writes text into the page's focused field: a
    /// fill, or keys pressed. Which field that is, and so whether what it
    /// wrote is a credential, is the field's type, read as it settles.
    pub const fn enters_text(&self) -> bool {
        matches!(self, Self::Fill { .. } | Self::Press { .. })
    }

    /// Whether the request only looks at the page, as any visitor can: it
    /// reads the document, its picture, its messages or its requests, or
    /// waits for it, and the page sees none of it. Everything else acts on
    /// the page: it moves the pointer, types, scrolls, submits or changes
    /// how the page is shown.
    pub const fn only_observes(&self) -> bool {
        match self {
            Self::Outline { .. }
            | Self::Changes { .. }
            | Self::At { .. }
            | Self::Settle { .. }
            | Self::Console { .. }
            | Self::Network { .. }
            | Self::Ledger { .. }
            | Self::Fields { .. }
            | Self::Search { .. }
            | Self::Forms { .. }
            | Self::Capture { .. }
            | Self::Find { .. } => true,
            Self::Click { .. }
            | Self::Hover { .. }
            | Self::Drag { .. }
            | Self::Path { .. }
            | Self::Fill { .. }
            | Self::Select { .. }
            | Self::FillFiles { .. }
            | Self::Press { .. }
            | Self::Submit { .. }
            | Self::Reader { .. }
            | Self::FillForm { .. }
            | Self::Pointer { .. }
            | Self::Scroll { .. } => false,
        }
    }

    /// What the request does, in the words an answer that a dialog it opened
    /// cut short repeats: `Clicked element d1e5`, `Submitted form d1e7`.
    pub fn described(&self) -> String {
        match self {
            Self::Outline { .. } => "Read the page".to_string(),
            Self::Click { element, .. } => format!("Clicked element {element}"),
            Self::Hover {
                element: Some(element),
                ..
            } => format!("Moved the pointer over element {element}"),
            Self::Hover { element: None, .. } => "Moved the pointer".to_string(),
            Self::Drag { .. } => "Dragged on the page".to_string(),
            Self::Path { .. } => "Drew on the page".to_string(),
            Self::Fill { element, .. } => format!("Filled element {element}"),
            Self::Select {
                element, values, ..
            } => {
                format!("Chose {} on element {element}", values.join(", "))
            }
            Self::FillFiles { element, paths, .. } => {
                format!("Chose {} file(s) for element {element}", paths.len())
            }
            Self::Press {
                element: None, key, ..
            } => format!("Pressed {}", key.chord()),
            Self::Press {
                element: Some(element),
                key,
                ..
            } => format!("Pressed {} on element {element}", key.chord()),
            Self::Submit { element, .. } => format!("Submitted form {element}"),
            Self::Changes { .. } => "Read what changed".to_string(),
            Self::At { x, y, .. } => format!("Read the element at {x}, {y}"),
            Self::Settle {
                quiet_ms,
                timeout_ms,
                ..
            } => format!("Waited for the page to settle ({quiet_ms} ms, up to {timeout_ms} ms)"),
            Self::Console { .. } => "Read the console".to_string(),
            Self::Network { .. } => "Read the network".to_string(),
            Self::Ledger { .. } => "Read who did what in the tab".to_string(),
            Self::Fields { .. } => "Read the page's fields".to_string(),
            Self::Search { text, .. } => format!("Searched the page for {text:?}"),
            Self::Forms { .. } => "Read the page's forms".to_string(),
            Self::FillForm { form, .. } => format!("Answered form {form}"),
            Self::Capture { element, .. } => match element {
                Some(element) => format!("Pictured element {element}"),
                None => "Pictured the page".to_string(),
            },
            Self::Reader { enabled: true } => "Showed the article alone".to_string(),
            Self::Reader { enabled: false } => "Put the page back as it was".to_string(),
            Self::Find {
                role, name, nth, ..
            } => {
                let what = match (role.as_deref(), name.as_deref()) {
                    (Some(role), Some(name)) => format!("{role} {name:?}"),
                    (Some(role), None) => role.to_string(),
                    (None, Some(name)) => format!("{name:?}"),
                    (None, None) => "the page's own element".to_string(),
                };
                match nth {
                    Some(nth) => format!("Found {what} ({nth})"),
                    None => format!("Found {what}"),
                }
            }
            Self::Pointer { steps, .. } => format!("Moved the pointer ({} steps)", steps.len()),
            Self::Scroll { to: Some(to), .. } => format!("Scrolled to the {}", to.as_str()),
            Self::Scroll {
                into_view: Some(element),
                ..
            } => format!("Scrolled element {element} into view"),
            Self::Scroll { by: Some(by), .. } => format!("Scrolled by {by} pixels"),
            Self::Scroll { .. } => "Scrolled the page".to_string(),
        }
    }

    pub fn element(&self) -> Option<&str> {
        match self {
            Self::Outline { .. }
            | Self::Find { .. }
            | Self::Press { element: None, .. }
            | Self::Changes { .. }
            | Self::At { .. }
            | Self::Settle { .. }
            | Self::Console { .. }
            | Self::Network { .. }
            | Self::Ledger { .. }
            | Self::Fields { .. }
            | Self::Search { .. }
            | Self::Forms { .. }
            | Self::FillForm { .. }
            | Self::Reader { .. }
            | Self::Pointer { .. }
            | Self::Scroll {
                element: None,
                into_view: None,
                ..
            }
            | Self::Capture { element: None, .. }
            | Self::Hover { element: None, .. }
            | Self::Drag {
                from: None,
                to: None,
                ..
            }
            | Self::Path { element: None, .. } => None,
            Self::Click { element, .. }
            | Self::Fill { element, .. }
            | Self::Select { element, .. }
            | Self::FillFiles { element, .. }
            | Self::Press {
                element: Some(element),
                ..
            }
            | Self::Submit { element, .. }
            | Self::Hover {
                element: Some(element),
                ..
            }
            | Self::Drag {
                from: Some(element),
                ..
            }
            | Self::Drag {
                from: None,
                to: Some(element),
                ..
            }
            | Self::Path {
                element: Some(element),
                ..
            }
            | Self::Scroll {
                element: Some(element),
                ..
            }
            | Self::Scroll {
                into_view: Some(element),
                ..
            }
            | Self::Capture {
                element: Some(element),
                ..
            } => Some(element),
        }
    }

    /// The frame's document this request runs in, or the main document's.
    pub fn frame(&self) -> Option<&FrameId> {
        match self {
            Self::Outline { frame, .. }
            | Self::Click { frame, .. }
            | Self::Fill { frame, .. }
            | Self::Select { frame, .. }
            | Self::FillFiles { frame, .. }
            | Self::Press { frame, .. }
            | Self::Submit { frame, .. }
            | Self::Changes { frame }
            | Self::At { frame, .. }
            | Self::Settle { frame, .. }
            | Self::Scroll { frame, .. }
            | Self::Capture { frame, .. }
            | Self::Fields { frame }
            | Self::Search { frame, .. }
            | Self::Forms { frame }
            | Self::FillForm { frame, .. }
            | Self::Find { frame, .. } => frame.as_ref(),
            Self::Console { .. }
            | Self::Network { .. }
            | Self::Ledger { .. }
            | Self::Reader { .. }
            | Self::Hover { .. }
            | Self::Drag { .. }
            | Self::Path { .. }
            | Self::Pointer { .. } => None,
        }
    }

    /// The same request run in the main document again.
    #[must_use]
    pub fn main_frame(self) -> Self {
        match self {
            Self::Outline { from, .. } => Self::Outline { from, frame: None },
            Self::Click { element, .. } => Self::Click {
                element,
                frame: None,
            },
            Self::Fill {
                element,
                text,
                secret,
                ..
            } => Self::Fill {
                element,
                text,
                secret,
                frame: None,
            },
            Self::Select {
                element, values, ..
            } => Self::Select {
                element,
                values,
                frame: None,
            },
            Self::FillFiles { element, paths, .. } => Self::FillFiles {
                element,
                paths,
                frame: None,
            },
            Self::Press { element, key, .. } => Self::Press {
                element,
                key,
                frame: None,
            },
            Self::Submit {
                element,
                cross_origin,
                ..
            } => Self::Submit {
                element,
                cross_origin,
                frame: None,
            },
            Self::Find {
                role,
                name,
                nth,
                frames,
                ..
            } => Self::Find {
                role,
                name,
                nth,
                frame: None,
                frames,
            },
            Self::FillForm {
                form,
                answers,
                submit,
                cross_origin,
                ..
            } => Self::FillForm {
                form,
                answers,
                submit,
                cross_origin,
                frame: None,
            },
            Self::Changes { .. } => Self::Changes { frame: None },
            Self::Fields { .. } => Self::Fields { frame: None },
            Self::Search { text, frames, .. } => Self::Search {
                text,
                frame: None,
                frames,
            },
            Self::Forms { .. } => Self::Forms { frame: None },
            Self::At { x, y, .. } => Self::At { x, y, frame: None },
            Self::Scroll {
                element,
                to,
                by,
                into_view,
                ..
            } => Self::Scroll {
                element,
                to,
                by,
                into_view,
                frame: None,
            },
            Self::Settle {
                quiet_ms,
                timeout_ms,
                ..
            } => Self::Settle {
                quiet_ms,
                timeout_ms,
                frame: None,
            },
            other => other,
        }
    }

    /// Whether this request names a frame the page outline could have
    /// printed. A caller that makes one up is refused, never silently run
    /// against the main document instead.
    pub fn frame_valid(&self) -> bool {
        self.frame()
            .is_none_or(|frame| FrameId::parse(frame.as_str()) == Some(frame.clone()))
    }

    /// Whether every field is within the bounds an engine accepts.
    #[allow(
        clippy::too_many_lines,
        reason = "one match names every request's bounds, so none can be forgotten"
    )]
    pub fn valid(&self) -> bool {
        let refs: Vec<&str> = match self {
            Self::Pointer { steps, .. } => {
                steps.iter().filter_map(|s| s.element.as_deref()).collect()
            }
            _ => self.element().into_iter().collect(),
        };
        if refs.iter().any(|element| !is_element_ref(element)) {
            return false;
        }
        // Structured so that `FrameId::malformed` fails here rather than
        // running the request in the main document instead.
        if self
            .frame()
            .is_some_and(|frame| FrameId::parse(frame.as_str()) != Some(frame.clone()))
        {
            return false;
        }
        match self {
            Self::Hover { element, point } => {
                matches!((element, point), (Some(_), None) | (None, Some(_)))
            }
            Self::Drag {
                from,
                from_point,
                to,
                to_point,
                steps,
            } => {
                let known = |element: &Option<String>, point: &Option<(f64, f64)>| {
                    matches!((element, point), (Some(_), None) | (None, Some(_)))
                };
                known(from, from_point)
                    && known(to, to_point)
                    && from_point
                        .iter()
                        .chain(to_point)
                        .all(|point| point_is_viewport(*point))
                    && (1..=MAX_DRAG_STEPS).contains(&usize::from(*steps))
            }
            Self::Path { element, points } => {
                let relative = element.is_some();
                (2..=MAX_PATH_POINTS).contains(&points.len())
                    && points.iter().all(|&point| {
                        if relative {
                            point.0.is_finite() && point.1.is_finite()
                        } else {
                            point_is_viewport(point)
                        }
                    })
            }
            Self::Fill { text, .. } => text.len() <= MAX_FILL_BYTES,
            Self::Select { values, .. } => {
                !values.is_empty()
                    && values.len() <= MAX_SELECT_VALUES
                    && values
                        .iter()
                        .all(|value| value.len() <= MAX_SELECT_VALUE_BYTES)
            }
            Self::FillFiles { paths, .. } => {
                !paths.is_empty()
                    && paths.len() <= MAX_FILE_PATHS
                    && paths
                        .iter()
                        .all(|path| path.len() <= MAX_FILE_PATH_BYTES && path.starts_with('/'))
            }
            Self::Press { key, .. } => PageKey::parse(&key.chord()).as_ref() == Some(key),
            Self::At { x, y, .. } => [x, y]
                .iter()
                .all(|v| v.is_finite() && (0.0..=MAX_COORDINATE).contains(*v)),
            Self::Settle {
                quiet_ms,
                timeout_ms,
                ..
            } => {
                (MIN_SETTLE_QUIET_MS..=MAX_SETTLE_MS).contains(quiet_ms)
                    && (*quiet_ms..=MAX_SETTLE_MS).contains(timeout_ms)
            }
            Self::Pointer { steps, button } => {
                !steps.is_empty()
                    && steps.len() <= MAX_POINTER_STEPS
                    && POINTER_BUTTONS.contains(&button.as_str())
                    && steps.iter().all(|step| {
                        let (x, y) = step.at.coordinates();
                        [x, y]
                            .iter()
                            .all(|v| v.is_finite() && (0.0..=MAX_COORDINATE).contains(v))
                            && step.frame.as_ref().is_none_or(|frame| {
                                FrameId::parse(frame.as_str()) == Some(frame.clone())
                            })
                    })
            }
            Self::Capture { max_width, .. } => {
                max_width.is_none_or(|width| (1..=u32::from(MAX_CAPTURE_WIDTH)).contains(&width))
            }
            Self::Find {
                role, name, nth, ..
            } => {
                role.as_ref()
                    .is_none_or(|role| !role.is_empty() && role.len() <= MAX_FIND_ROLE_BYTES)
                    && name
                        .as_ref()
                        .is_none_or(|name| !name.is_empty() && name.len() <= MAX_FIND_NAME_BYTES)
                    && (role.is_some() || name.is_some())
                    && nth.is_none_or(|nth| nth >= 1)
            }
            Self::Outline { from, .. } => {
                from.is_none_or(|from| (1..=MAX_OUTLINE_FROM).contains(&from))
            }
            Self::Search { text, .. } => (1..=MAX_SEARCH_UNITS).contains(&units(text)),
            Self::FillForm {
                form,
                answers,
                submit,
                ..
            } => {
                (1..=MAX_FORM_UNITS).contains(&units(form))
                    && answers.len() <= MAX_FORM_ANSWERS
                    && answers.iter().all(|(name, answer)| {
                        (1..=MAX_FORM_NAME_UNITS).contains(&units(name))
                            && form_answer_valid(answer)
                    })
                    && match submit {
                        FormSubmit::No | FormSubmit::First => true,
                        FormSubmit::Button(label) => {
                            (1..=MAX_FORM_NAME_UNITS).contains(&units(label))
                        }
                    }
            }
            Self::Click { .. }
            | Self::Submit { .. }
            | Self::Changes { .. }
            | Self::Console { .. }
            | Self::Network { .. }
            | Self::Ledger { .. }
            | Self::Fields { .. }
            | Self::Forms { .. }
            | Self::Reader { .. } => true,
            Self::Scroll {
                element,
                to,
                by,
                into_view,
                ..
            } => {
                // The wheel turns over the viewport's centre unless the
                // caller names what it turns over.
                // Bringing an element into view is the place itself, and may
                // name the box the wheel turns over; otherwise the wheel goes
                // to an end of the page or by a distance, never both.
                let aimed = usize::from(to.is_some()) + usize::from(by.is_some());
                (to.is_some() || by.is_some() || into_view.is_some())
                    && if into_view.is_some() {
                        aimed == 0
                    } else {
                        aimed == 1
                    }
                    && by.is_none_or(|by| by.unsigned_abs() <= MAX_SCROLL_BY)
                    && into_view.as_ref().is_none_or(|r| r.len() <= MAX_REF_BYTES)
                    && element.as_ref().is_none_or(|r| r.len() <= MAX_REF_BYTES)
            }
        }
    }
}

/// Whether `answer` is one a form takes: text, a number, yes or no, or the
/// chosen ones of several, each within what an engine accepts.
fn form_answer_valid(answer: &serde_json::Value) -> bool {
    match answer {
        serde_json::Value::String(text) => units(text) <= MAX_FORM_TEXT_UNITS,
        serde_json::Value::Bool(_) | serde_json::Value::Number(_) => true,
        serde_json::Value::Array(chosen) => {
            chosen.len() <= MAX_FORM_CHOICES
                && chosen.iter().all(|choice| {
                    choice
                        .as_str()
                        .is_some_and(|choice| units(choice) <= MAX_FORM_CHOICE_UNITS)
                })
        }
        serde_json::Value::Null | serde_json::Value::Object(_) => false,
    }
}

/// A finite point inside the viewport coordinate bounds.
fn point_is_viewport((x, y): (f64, f64)) -> bool {
    [x, y]
        .iter()
        .all(|v| v.is_finite() && (0.0..=MAX_COORDINATE).contains(v))
}

/// A frame of the page, by the number the outline printed for it (`3`). The
/// number is the tab's: a frame keeps it while its own document moves on and
/// while frames around it come and go, and a frame inside a frame has a
/// number of its own. The dotted path an older outline printed for a nested
/// frame (`2.1`) is still read as an id; it names no frame an engine has, and
/// the engine says so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameId(String);

impl FrameId {
    /// Reads a frame id the outline printed, refusing anything else so an id
    /// can never smuggle selector or script syntax to the engine. A frame is a
    /// number, so the number alone, the number as text and the outline's own
    /// `[frame=1]` all name the same frame: a caller that copies what it read
    /// is understood.
    pub fn parse(value: &str) -> Option<Self> {
        let written = value.trim();
        // What the outline brackets is the id, and what marks it is not part
        // of it: `frame=3`, `[frame=3]` and `3` are one id.
        let written = written
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
            .unwrap_or(written);
        let written = written
            .strip_prefix("frame=")
            .or_else(|| written.strip_prefix("Frame="))
            .unwrap_or(written)
            .trim();
        let parts: Vec<&str> = written.split('.').collect();
        let digit = |part: &str| {
            !part.is_empty()
                && part.len() <= 3
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && !part.starts_with('0')
        };
        (written.len() <= MAX_FRAME_BYTES
            && !parts.is_empty()
            && parts.len() <= MAX_FRAME_DEPTH
            && parts.iter().all(|part| digit(part)))
        .then(|| Self(written.to_string()))
    }

    /// A frame id no outline printed, standing in for one the caller wrote
    /// wrongly: it fails the request rather than being read as the main
    /// document.
    pub fn malformed() -> Self {
        Self(String::new())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for FrameId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Element refs are `d<document>e<element>`: both parts are decimal, so a ref
/// can never smuggle selector or script syntax to the engine.
pub fn is_element_ref(value: &str) -> bool {
    let Some((document, element)) = value
        .strip_prefix('d')
        .and_then(|rest| rest.split_once('e'))
    else {
        return false;
    };
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    value.len() <= MAX_REF_BYTES && digits(document) && digits(element)
}

/// One key plus modifiers, written as a chord such as `Enter`, `Shift+Tab`
/// or `Control+a`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageKey {
    /// A named key (`Enter`, `ArrowDown`, …) or a single character.
    pub key: String,
    pub modifiers: KeyModifiers,
}

/// Keys the engines map to a physical key. `Space` is spelled out because a
/// literal space is invisible in a chord.
const NAMED_KEYS: &[&str] = &[
    "Enter",
    "Tab",
    "Escape",
    "Backspace",
    "Delete",
    "ArrowUp",
    "ArrowDown",
    "ArrowLeft",
    "ArrowRight",
    "Home",
    "End",
    "PageUp",
    "PageDown",
];

/// How a key's name is read: without regard to case, so `END`, `End` and
/// `end` name the same key, as do `CTRL` and `Ctrl` as a modifier.
fn named_key(name: &str) -> Option<&'static str> {
    NAMED_KEYS
        .iter()
        .copied()
        .find(|named| named.eq_ignore_ascii_case(name))
}

/// The named keys `name` could have been misspelled for, as the closest ones
/// first: the keys sharing its first letter, so an error can offer them.
fn near_keys(name: &str) -> Vec<&'static str> {
    let wanted = name.trim().to_ascii_lowercase();
    let Some(first) = wanted.chars().next() else {
        return Vec::new();
    };
    NAMED_KEYS
        .iter()
        .copied()
        .filter(|named| named.to_ascii_lowercase().starts_with(first))
        .collect()
}

impl PageKey {
    pub fn parse(chord: &str) -> Option<Self> {
        Self::parse_detailed(chord).ok()
    }

    /// [`Self::parse`], saying why the chord names no key: a caller needs to
    /// know what to write instead, and that is never the same for a missing
    /// modifier as for a key that does not exist.
    ///
    /// # Errors
    /// The chord's own problem, phrased for the person who wrote it.
    pub fn parse_detailed(chord: &str) -> Result<Self, String> {
        let written = chord.trim();
        if written.is_empty() {
            return Err(
                "press-page needs a key: name one, such as Enter, Tab, Escape, ArrowDown, End or Control+a"
                    .into(),
            );
        }
        let mut parts: Vec<&str> = written.split('+').collect();
        // `Control++` names the plus key itself.
        if written.ends_with("++") {
            parts.truncate(parts.len().saturating_sub(2));
            parts.push("+");
        }
        let Some(key) = parts.pop().filter(|key| !key.is_empty()) else {
            return Err(format!(
                "press-page was given {written:?}, which ends before it names a key; write a key after the last +, such as Control+a"
            ));
        };
        let mut modifiers = KeyModifiers::default();
        for part in parts {
            let flag = if part.eq_ignore_ascii_case("control") || part.eq_ignore_ascii_case("ctrl")
            {
                &mut modifiers.ctrl
            } else if part.eq_ignore_ascii_case("shift") {
                &mut modifiers.shift
            } else if part.eq_ignore_ascii_case("alt") {
                &mut modifiers.alt
            } else if part.eq_ignore_ascii_case("meta") || part.eq_ignore_ascii_case("cmd") {
                &mut modifiers.meta
            } else {
                return Err(format!(
                    "{part:?} is not a key modifier; hold one of Control (or Ctrl), Shift, Alt or Meta before the key, as in Control+{key}"
                ));
            };
            if std::mem::replace(flag, true) {
                return Err(format!(
                    "{part:?} is held twice in {written:?}; name each modifier once"
                ));
            }
        }
        let key: &str = if key.eq_ignore_ascii_case("space") {
            " "
        } else if let Some(named) = named_key(key) {
            named
        } else if key.chars().count() == 1 {
            key
        } else {
            let near = near_keys(key);
            let did_you_mean = if near.is_empty() {
                String::new()
            } else {
                format!("; did you mean {}?", near.join(" or "))
            };
            return Err(format!(
                "{key:?} is not a key name. Name a key as one of {} (in any letter case), or a single character such as a{main_did_you_mean}",
                NAMED_KEYS.join(", "),
                main_did_you_mean = did_you_mean
            ));
        };
        Ok(Self {
            key: key.to_string(),
            modifiers,
        })
    }

    /// The canonical chord, which [`Self::parse`] reads back unchanged.
    pub fn chord(&self) -> String {
        let mut chord = String::new();
        for (held, name) in [
            (self.modifiers.ctrl, "Control+"),
            (self.modifiers.shift, "Shift+"),
            (self.modifiers.alt, "Alt+"),
            (self.modifiers.meta, "Meta+"),
        ] {
            if held {
                chord.push_str(name);
            }
        }
        chord.push_str(if self.key == " " { "Space" } else { &self.key });
        chord
    }
}

/// What a completed page request observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageReply {
    /// The main document's URL when the request finished.
    pub url: String,
    pub message: String,
    /// Present for [`PageRequest::Outline`], and the line diff for
    /// [`PageRequest::Changes`].
    pub outline: Option<String>,
    /// Structured findings: the element at a point, how a settle went, log
    /// entries. Always page content, so never trusted as instructions.
    pub data: Option<serde_json::Value>,
    /// The picture a [`PageRequest::Capture`] painted, a PNG. What it shows
    /// came from the page: a reader of it trusts none of the words in it.
    pub image: Option<Vec<u8>>,
    /// Whether the request led the page away from its document.
    pub navigation: PageNavigation,
    /// The navigation it started had not finished loading when the engine
    /// answered.
    pub loading: bool,
    /// Where the action stands in the tab's record, and what the tab's other
    /// actors did meanwhile.
    pub record: PageRecordNote,
}

/// What an answer says of the tab's record beside its own request.
///
/// Several actors may work in one tab, so the page an actor acts on next may
/// not be the page it read. Its answer says so rather than leaving it to find
/// out: an operating action's own place in the record, and what the other
/// actors did since this one last heard from the tab, told once.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageRecordNote {
    /// The place of the action this answers in the tab's record. Reads take
    /// no place in it.
    pub seq: Option<u64>,
    /// The newest record entries of other actors since the asking actor last
    /// heard from the tab, oldest first, at most [`MAX_OTHERS`]. Each is an
    /// entry as [`PageRequest::Ledger`] lists it, and so page content: never
    /// trusted as instructions.
    pub others: Vec<serde_json::Value>,
    /// How many older entries of other actors `others` leaves out.
    pub others_earlier: u32,
}

impl PageRecordNote {
    /// Whether the answer says nothing of the record: a read by an actor
    /// nobody else acted beside.
    pub fn is_empty(&self) -> bool {
        self.seq.is_none() && self.others.is_empty() && self.others_earlier == 0
    }
}

/// Whether a page request led the page to another document, as far as the
/// engine can tell when it answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageNavigation {
    /// The request ran none of the page's code, so it cannot have.
    None,
    /// The page began a main-frame navigation while it handled the request.
    Started,
    /// It had not by the time it handled the request, and may still: a
    /// navigation the page starts later arrives as its own document request in
    /// the network log.
    Possible,
}

impl PageNavigation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Started => "started",
            Self::Possible => "possible",
        }
    }
}

/// Why a page request did nothing. `code` is stable, such as
/// `stale_element`, `obscured` or `unavailable`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageFailure {
    pub code: String,
    pub message: String,
    /// Where the refused action stands in the tab's record, and what the
    /// tab's other actors did meanwhile: often why it was refused.
    pub record: PageRecordNote,
}

impl PageFailure {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            record: PageRecordNote::default(),
        }
    }

    /// The answer of an engine without page automation.
    pub fn unavailable() -> Self {
        Self::new(
            "unavailable",
            "This browser engine cannot read or operate page content.",
        )
    }
}

pub(crate) fn encode_request(
    request_id: u32,
    actor: Option<&PageActor>,
    request: &PageRequest,
) -> serde_json::Value {
    let mut command = serde_json::json!({
        "op": "page",
        "request_id": request_id,
        "action": request.action(),
    });
    if let Some(actor) = actor {
        command["actor"] = actor.as_str().into();
    }
    if let Some(element) = request.element() {
        command["element"] = element.into();
    }
    if let PageRequest::Drag {
        to: Some(target), ..
    } = request
    {
        command["to"] = target.as_str().into();
    }
    if let Some(frame) = request.frame() {
        command["frame"] = frame.as_str().into();
    }
    match request {
        PageRequest::Fill { text, secret, .. } => {
            command["text"] = text.as_str().into();
            command["secret"] = (*secret).into();
        }
        PageRequest::Select { values, .. } => command["values"] = values.clone().into(),
        PageRequest::FillFiles { paths, .. } => command["paths"] = paths.clone().into(),
        PageRequest::Press { key, .. } => {
            command["key"] = key.key.as_str().into();
            command["ctrl"] = key.modifiers.ctrl.into();
            command["shift"] = key.modifiers.shift.into();
            command["alt"] = key.modifiers.alt.into();
            command["meta"] = key.modifiers.meta.into();
        }
        PageRequest::Submit { cross_origin, .. } => {
            command["cross_origin"] = (*cross_origin).into()
        }
        PageRequest::At { x, y, .. } => {
            command["x"] = (*x).into();
            command["y"] = (*y).into();
        }
        PageRequest::Settle {
            quiet_ms,
            timeout_ms,
            ..
        } => {
            command["quiet_ms"] = (*quiet_ms).into();
            command["timeout_ms"] = (*timeout_ms).into();
        }
        PageRequest::Console { since }
        | PageRequest::Network { since }
        | PageRequest::Ledger { since } => {
            command["since"] = (*since).into();
        }
        PageRequest::Reader { enabled } => command["enabled"] = (*enabled).into(),
        PageRequest::Find {
            role,
            name,
            nth,
            frames,
            ..
        } => {
            for (key, value) in [("role", role), ("name", name)] {
                if let Some(value) = value {
                    command[key] = value.as_str().into();
                }
            }
            if let Some(nth) = nth {
                command["nth"] = (*nth).into();
            }
            // Said only when asked for: an engine from before it takes no
            // such field.
            if *frames {
                command["frames"] = true.into();
            }
        }
        PageRequest::Capture { max_width, .. } => {
            command["max_width"] =
                u32::from(max_width.unwrap_or(u32::from(MAX_CAPTURE_WIDTH))).into();
        }
        PageRequest::Pointer { steps, button } => {
            command["button"] = button.as_str().into();
            command["steps"] = steps
                .iter()
                .map(|step| {
                    let (x, y) = step.at.coordinates();
                    let mut encoded = serde_json::json!({
                        "kind": step.kind.as_str(),
                        "x": x,
                        "y": y,
                    });
                    if let Some(element) = &step.element {
                        encoded["element"] = element.as_str().into();
                    }
                    if let Some(frame) = &step.frame {
                        encoded["frame"] = frame.as_str().into();
                    }
                    encoded
                })
                .collect();
        }
        PageRequest::Hover { element, point } => {
            // An unnamed element hovers a viewport point instead.
            if element.is_none()
                && let Some((x, y)) = point
            {
                command["action"] = "at".into();
                command["hover"] = true.into();
                command["x"] = (*x).into();
                command["y"] = (*y).into();
            }
        }
        PageRequest::Drag {
            from_point,
            to_point,
            steps,
            ..
        } => {
            // A named element travels as its ref under `element`; the
            // destination element under `to`; unnamed ends as points.
            if let Some((x, y)) = from_point {
                command["x"] = (*x).into();
                command["y"] = (*y).into();
            }
            if let Some((x, y)) = to_point {
                command["to_x"] = (*x).into();
                command["to_y"] = (*y).into();
            }
            command["steps"] = (*steps).into();
        }
        PageRequest::Path { points, .. } => {
            command["points"] = serde_json::Value::Array(
                points
                    .iter()
                    .map(|&(x, y)| serde_json::json!([x, y]))
                    .collect(),
            );
        }
        PageRequest::Outline { from, .. } => {
            if let Some(from) = from {
                command["from"] = (*from).into();
            }
        }
        PageRequest::Search { text, frames, .. } => {
            command["text"] = text.as_str().into();
            if *frames {
                command["frames"] = true.into();
            }
        }
        PageRequest::FillForm {
            form,
            answers,
            submit,
            cross_origin,
            ..
        } => {
            command["form"] = form.as_str().into();
            command["answers"] = serde_json::Value::Object(answers.clone());
            // Sending is said as the engine reads it: `true` for the form's
            // first submit button, a label for the button of that name, and
            // nothing at all for a form that is only answered.
            match submit {
                FormSubmit::No => {}
                FormSubmit::First => command["submit"] = true.into(),
                FormSubmit::Button(label) => command["submit"] = label.as_str().into(),
            }
            command["cross_origin"] = (*cross_origin).into();
        }
        PageRequest::Click { .. }
        | PageRequest::Changes { .. }
        | PageRequest::Fields { .. }
        | PageRequest::Forms { .. } => {}
        PageRequest::Scroll {
            to, by, into_view, ..
        } => {
            // A named element travels as its ref under `element`, as every
            // other request's does; the end of the page, the distance and
            // the element to bring into view have keys of their own.
            if let Some(to) = to {
                command["to"] = to.as_str().into();
            }
            if let Some(by) = by {
                command["by"] = (*by).into();
            }
            if let Some(into_view) = into_view {
                command["into_view"] = into_view.as_str().into();
            }
        }
    }
    command
}

/// Decode a `page_result` event, rejecting anything outside the bounds above.
pub(crate) fn decode_result(
    value: &serde_json::Value,
) -> Option<(u32, Result<PageReply, PageFailure>)> {
    let request_id = u32::try_from(value["request_id"].as_u64()?)
        .ok()
        .filter(|id| *id > 0 && i32::try_from(*id).is_ok())?;
    let bounded = |key: &str, limit: usize| {
        value[key]
            .as_str()
            .filter(|text| text.len() <= limit)
            .map(str::to_string)
    };
    let message = bounded("message", MAX_MESSAGE_BYTES)?;
    let record = decode_record_note(value)?;
    let result = if value["ok"].as_bool()? {
        let outline = match &value["outline"] {
            serde_json::Value::Null => None,
            _ => Some(bounded("outline", MAX_OUTLINE_BYTES)?),
        };
        let navigated = value["navigated"].as_bool()?;
        // An engine from before `navigation` only knew whether one began soon after.
        let navigation = match value["navigation"].as_str() {
            Some("none") => PageNavigation::None,
            Some("started") => PageNavigation::Started,
            Some("possible") => PageNavigation::Possible,
            Some(_) => return None,
            None if navigated => PageNavigation::Started,
            None => PageNavigation::Possible,
        };
        if navigated != (navigation == PageNavigation::Started) {
            return None;
        }
        // The picture travels as its own bytes, so the findings beside it are
        // measured without it: a photograph of a page is never findings.
        let mut picture = None;
        capture_image(&value["data"], &mut picture)?;
        let data = match &value["data"] {
            serde_json::Value::Null => None,
            data @ serde_json::Value::Object(_) => {
                let findings = without_image(data);
                if runtime_collab::wire_json_len(&findings) > MAX_DATA_BYTES {
                    return None;
                }
                Some(findings)
            }
            _ => return None,
        };
        Ok(PageReply {
            url: bounded("url", 8192)?,
            message,
            outline,
            data,
            image: picture,
            navigation,
            loading: navigated && value["loading"].as_bool()?,
            record,
        })
    } else {
        Err(PageFailure {
            code: bounded("code", MAX_CODE_BYTES).filter(|code| !code.is_empty())?,
            message,
            record,
        })
    };
    Some((request_id, result))
}

/// What a page changed of itself, as the engine tells it of its own accord
/// (`page_changed`): which document, which of its forms now ask or offer
/// something else, and which fields hold something else, by the page's own
/// script or by what a person typed. It never says what a field holds: that
/// is read with [`PageRequest::Fields`]. All of it is page content: never
/// trusted as instructions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageChange {
    /// The number the changed document's refs carry.
    pub document: u32,
    /// The frame whose document changed; the page's own document when none.
    pub frame: Option<FrameId>,
    /// The forms whose shape changed.
    pub forms: Vec<PageFormChange>,
    /// The fields that changed, at most [`MAX_CHANGED_FIELDS`].
    pub fields: Vec<PageFieldChange>,
    /// How many more fields changed than `fields` lists.
    pub more: u32,
}

/// One form a [`PageChange`] names: what it is now, or that it is gone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageFormChange {
    /// The id the document gives it, such as `form1`.
    pub form: String,
    /// Its signature now; `None` when it is gone.
    pub signature: Option<String>,
    /// The hash of the answers its choice questions offer now.
    pub offers: Option<String>,
    /// The document no longer holds it.
    pub gone: bool,
}

/// One field a [`PageChange`] names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageFieldChange {
    /// The ref its own action takes.
    pub element: String,
    pub role: String,
    pub name: String,
    /// A person typed it straight into the page; otherwise the page's own
    /// script changed it.
    pub typed: bool,
    /// The form it answers a question of, and the question.
    pub form: Option<String>,
    pub question: Option<String>,
}

/// The most forms and fields one [`PageChange`] names.
pub const MAX_CHANGED_FORMS: usize = 64;
pub const MAX_CHANGED_FIELDS: usize = 64;
/// The longest name a [`PageChange`] carries, in bytes.
const MAX_CHANGED_NAME_BYTES: usize = 2048;

/// Decode a `page_record` event: the place of an entry the engine set down
/// that no action's answer carried (what a person typed straight into the
/// page), and the entry as [`PageRequest::Ledger`] lists it.
pub(crate) fn decode_record_event(value: &serde_json::Value) -> Option<(u64, serde_json::Value)> {
    let seq = whole_number(&value["seq"]).filter(|seq| *seq > 0)?;
    let entry = value.get("entry").filter(|entry| entry.is_object())?;
    // The entry names its own place, and it is the place the event names.
    if entry
        .get("seq")
        .is_some_and(|own| whole_number(own) != Some(seq))
        || runtime_collab::wire_json_len(entry) > MAX_DATA_BYTES
    {
        return None;
    }
    let mut entry = entry.clone();
    entry["seq"] = seq.into();
    Some((seq, entry))
}

/// Decode a `page_changed` event, rejecting anything outside its bounds.
pub(crate) fn decode_changed_event(value: &serde_json::Value) -> Option<PageChange> {
    let word = |from: &serde_json::Value, key: &str| match from.get(key) {
        None | Some(serde_json::Value::Null) => Some(None),
        Some(serde_json::Value::String(text)) if text.len() <= MAX_CHANGED_NAME_BYTES => {
            Some(Some(text.clone()))
        }
        Some(_) => None,
    };
    let document = u32::try_from(whole_number(&value["document"])?)
        .ok()
        .filter(|document| *document > 0)?;
    let frame = match word(value, "frame")? {
        None => None,
        Some(frame) => Some(FrameId::parse(&frame)?),
    };
    // A frame's name and address say which frame it was: bounded text.
    word(value, "frame_name")?;
    word(value, "frame_url")?;
    let list = |key: &str, most: usize| match value.get(key) {
        None | Some(serde_json::Value::Null) => Some(&[][..]),
        Some(serde_json::Value::Array(listed)) if listed.len() <= most => Some(listed.as_slice()),
        Some(_) => None,
    };
    let forms = list("forms", MAX_CHANGED_FORMS)?
        .iter()
        .map(|form| {
            let gone = match form.get("gone") {
                None | Some(serde_json::Value::Null) => false,
                Some(gone) => gone.as_bool()?,
            };
            Some(PageFormChange {
                form: word(form, "form")?.filter(|id| !id.is_empty())?,
                signature: word(form, "signature")?,
                offers: word(form, "offers")?,
                gone,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let fields = list("fields", MAX_CHANGED_FIELDS)?
        .iter()
        .map(|field| {
            Some(PageFieldChange {
                element: word(field, "element")?.filter(|element| is_element_ref(element))?,
                role: word(field, "role")?.unwrap_or_default(),
                name: word(field, "name")?.unwrap_or_default(),
                typed: match field.get("how")?.as_str()? {
                    "input" => true,
                    "page" => false,
                    _ => return None,
                },
                form: word(field, "form")?,
                question: word(field, "question")?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let more = match value.get("more") {
        None | Some(serde_json::Value::Null) => 0,
        Some(more) => u32::try_from(whole_number(more)?).ok()?,
    };
    Some(PageChange {
        document,
        frame,
        forms,
        fields,
        more,
    })
}

/// A whole number as an engine writes one: the record's places are counted
/// in doubles on the engine's side, so `3` may arrive as `3.0`.
fn whole_number(value: &serde_json::Value) -> Option<u64> {
    const LARGEST_EXACT: f64 = 9_007_199_254_740_992.0;
    if let Some(number) = value.as_u64() {
        return Some(number);
    }
    let number = value.as_f64()?;
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked whole, not negative and exactly representable"
    )]
    (number.fract() == 0.0 && (0.0..=LARGEST_EXACT).contains(&number)).then_some(number as u64)
}

/// What an answer says of the tab's record, within its bounds: an answer
/// that says more than an engine may is refused whole, like any other.
fn decode_record_note(value: &serde_json::Value) -> Option<PageRecordNote> {
    let seq = match &value["seq"] {
        serde_json::Value::Null => None,
        seq => Some(whole_number(seq).filter(|seq| *seq > 0)?),
    };
    let others = match &value["others"] {
        serde_json::Value::Null => Vec::new(),
        serde_json::Value::Array(entries) => {
            if entries.is_empty()
                || entries.len() > MAX_OTHERS
                || !entries.iter().all(serde_json::Value::is_object)
                || runtime_collab::wire_json_len(&value["others"]) > MAX_DATA_BYTES
            {
                return None;
            }
            entries.clone()
        }
        _ => return None,
    };
    let others_earlier = match &value["others_earlier"] {
        serde_json::Value::Null => 0,
        earlier => u32::try_from(whole_number(earlier)?).ok()?,
    };
    // Earlier ones are only ever counted beside the ones that are told.
    if others.is_empty() && others_earlier > 0 {
        return None;
    }
    Some(PageRecordNote {
        seq,
        others,
        others_earlier,
    })
}

/// The findings an answer carries, with the picture taken out of them: the
/// picture's own bytes are the reply's image, never part of its findings.
fn without_image(data: &serde_json::Value) -> serde_json::Value {
    let mut findings = data.clone();
    if let Some(object) = findings.as_object_mut() {
        object.remove("capture");
    }
    findings
}

/// The picture an answer carries, decoded from its base64 PNG, bounded in
/// both its encoding and its pixels. `None` refuses the whole answer: a
/// picture it cannot take is not one it may quietly drop.
fn capture_image(data: &serde_json::Value, image: &mut Option<Vec<u8>>) -> Option<()> {
    let Some(capture) = data.get("capture") else {
        return Some(());
    };
    let bytes = decode_png(capture.as_str()?)?;
    *image = Some(bytes);
    Some(())
}

/// The first bytes of every PNG file.
const PNG_SIGNATURE: [u8; 4] = [0x89, b'P', b'N', b'G'];

/// The PNG `encoded` stands for, refused if it is longer than a picture may
/// be or is not a PNG at all.
fn decode_png(encoded: &str) -> Option<Vec<u8>> {
    if encoded.len() > MAX_CAPTURE_BYTES.div_ceil(3) * 4 {
        return None;
    }
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    // A PNG, and no other kind of file, whatever the page may have wanted.
    if !bytes.starts_with(&PNG_SIGNATURE) || bytes.len() > MAX_CAPTURE_BYTES {
        return None;
    }
    Some(bytes)
}

#[cfg(test)]
mod tests;
