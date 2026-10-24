//! Pictures a planning model is shown.
//!
//! A plan that edits a picture or a drawing is planned better when the
//! model sees it: what a canvas `look` rendered, a PDF page, a scan someone
//! put in the room. Pictures reach the model three ways, each through the
//! same `Eyes` that budgets them and keeps the conversation's text honest
//! about what was shown:
//!
//! - a `read_view` whose read rendered a picture (Canvas `look`) carries it
//!   with its answer;
//! - [`LOOK_CONTENT`] shows a picture kept as content by its address, one
//!   this conversation was shown;
//! - a re-plan's first turn carries the newest picture the task's steps
//!   looked at.
//!
//! Every picture is at most [`MOST_PICTURE_SIDE`] pixels a side and
//! [`MOST_PICTURE_BYTES`] bytes, a PNG or a JPEG. A picture an app rendered
//! within those passes through as it is; anything else (one too large, a
//! room's picture that may carry where and when it was taken) is decoded,
//! shrunk and written out again in a confined worker
//! (`worker::shrink_picture`), never in the host: a picture someone put in
//! the room is untrusted input. Pictures always follow the request's stable
//! prefix (instructions, tools, the apps' reference), so a provider's prompt
//! cache keeps working, and no picture's bytes are ever logged.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::sync::Arc;

use base64::Engine as _;
use tilo_model_gateway::host_backend::protocol::{Content, ModelRequest};

/// The longest side of a picture a model is shown, in pixels.
pub const MOST_PICTURE_SIDE: u32 = 1024;

/// The most bytes of one picture a model is shown.
pub const MOST_PICTURE_BYTES: usize = 1024 * 1024;

/// The most pictures one planning turn's answers carry.
pub const MOST_PICTURES_PER_TURN: usize = 2;

/// The most pictures one plan is shown, its first turn's included: enough to
/// look through a handful of scans to pick the right ones (about 1k tokens each).
pub const MOST_PICTURES_PER_PLAN: usize = 6;

/// The most bytes of a picture handed to the confined worker to shrink: a
/// larger one is refused before it is read further.
pub const MOST_SOURCE_BYTES: usize = 32 * 1024 * 1024;

/// The planning tool that shows a picture kept as content.
pub const LOOK_CONTENT: &str = "look_content";

/// Turns pictures on (`1`, `on`) or off (`0`, `off`) for a planning model,
/// whatever its provider's default ([`crate::PlanModel::sees`]).
pub const VISION_ENV: &str = "TILO_AGENT_VISION";

/// Lets agents show the room's own pictures (scans, photos people put in
/// the room) to their model's provider: `1`/`on`/`all` for every agent, or
/// a comma-separated list of agent ids or labels. Off by default; pictures
/// an app rendered (a canvas look, a PDF page) are always shown.
pub const ROOM_PICTURES_ENV: &str = "TILO_AGENT_ROOM_PICTURES";

/// The prefix of a content address.
const CONTENT_PREFIX: &str = "tilo.content:";

/// A picture as a model is shown it: a PNG or a JPEG within the limits.
#[derive(Clone, PartialEq, Eq)]
pub struct Picture {
    /// The content address it is kept under (`tilo.content:<hash>`): what
    /// the model names it by. A shrunk picture keeps its source's address.
    pub content: String,
    /// `image/png` or `image/jpeg`.
    pub media_type: &'static str,
    /// Its width in pixels.
    pub width: u32,
    /// Its height in pixels.
    pub height: u32,
    /// The encoded picture.
    pub bytes: Arc<[u8]>,
}

/// Never the bytes: a picture in a log is its address and size.
impl std::fmt::Debug for Picture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Picture")
            .field("content", &self.content)
            .field("media_type", &self.media_type)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

impl Picture {
    /// Its format as the text beside it names it.
    #[must_use]
    pub fn format(&self) -> &'static str {
        if self.media_type == "image/png" {
            "PNG"
        } else {
            "JPEG"
        }
    }

    /// `768×512 JPEG`.
    #[must_use]
    pub fn size_note(&self) -> String {
        format!("{}×{} {}", self.width, self.height, self.format())
    }

    /// As the model gateway's neutral image content: inline bytes, never a
    /// path or a URL the provider would fetch.
    #[must_use]
    pub fn content(&self) -> Content {
        Content::Image {
            media_type: self.media_type.to_owned(),
            base64: base64::engine::general_purpose::STANDARD.encode(&self.bytes),
        }
    }
}

/// What a read or a look answered: the text the model reads, and the
/// pictures it showed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    /// The answer as text; it names each picture by its address.
    pub text: String,
    /// The pictures, each within the limits.
    pub pictures: Vec<Picture>,
}

impl From<String> for Seen {
    fn from(text: String) -> Self {
        Self {
            text,
            pictures: Vec::new(),
        }
    }
}

/// What may be done with bytes offered as a picture, from their header
/// alone: nothing here decodes a picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Admission {
    /// A PNG or JPEG within the limits, shown as it is when an app rendered
    /// it.
    PassThrough {
        /// `image/png` or `image/jpeg`.
        media_type: &'static str,
        /// Its width in pixels.
        width: u32,
        /// Its height in pixels.
        height: u32,
    },
    /// A picture to shrink or write out again in the confined worker: too
    /// large, or in a format a model is not shown (GIF, WebP, BMP).
    Shrink,
    /// Not shown, and why.
    Refuse(String),
}

/// What may be done with `bytes`, offered as a picture.
#[must_use]
pub fn admit(bytes: &[u8]) -> Admission {
    if bytes.len() > MOST_SOURCE_BYTES {
        return Admission::Refuse(format!(
            "it is {} bytes, more than a picture shown to the model may be read from",
            bytes.len()
        ));
    }
    match picture_size(bytes) {
        Some((_, width, height)) if width == 0 || height == 0 => {
            Admission::Refuse("it is a picture with no pixels".into())
        }
        Some((format, width, height))
            if width <= MOST_PICTURE_SIDE
                && height <= MOST_PICTURE_SIDE
                && bytes.len() <= MOST_PICTURE_BYTES =>
        {
            Admission::PassThrough {
                media_type: if format == "PNG" {
                    "image/png"
                } else {
                    "image/jpeg"
                },
                width,
                height,
            }
        }
        Some(_) => Admission::Shrink,
        None if other_picture(bytes) => Admission::Shrink,
        None if is_pdf(bytes) => Admission::Refuse("it is a PDF, not a picture".into()),
        None => Admission::Refuse("it is not a PNG, JPEG, GIF or WebP picture".into()),
    }
}

/// Whether `bytes` start as a picture format the worker can write out as a
/// PNG or JPEG.
fn other_picture(bytes: &[u8]) -> bool {
    bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
        || (bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP".as_slice()))
        || bytes.starts_with(b"BM")
}

/// Whether `bytes` are a PDF.
#[must_use]
pub fn is_pdf(bytes: &[u8]) -> bool {
    bytes.starts_with(b"%PDF-")
}

/// A PNG's or JPEG's format and size, from its header.
#[must_use]
pub fn picture_size(bytes: &[u8]) -> Option<(&'static str, u32, u32)> {
    let be16 = |at: usize| -> Option<u32> {
        Some(u32::from(u16::from_be_bytes([
            *bytes.get(at)?,
            *bytes.get(at.checked_add(1)?)?,
        ])))
    };
    let be32 = |at: usize| -> Option<u32> {
        Some(u32::from_be_bytes(
            bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
        ))
    };
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(("PNG", be32(16)?, be32(20)?));
    }
    if !bytes.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut at = 2_usize;
    while *bytes.get(at)? == 0xFF {
        let marker = *bytes.get(at.checked_add(1)?)?;
        // A start-of-frame segment holds the size; the others are skipped.
        if (0xC0..=0xCF).contains(&marker) && ![0xC4, 0xC8, 0xCC].contains(&marker) {
            return Some(("JPEG", be16(at.checked_add(7)?)?, be16(at.checked_add(5)?)?));
        }
        let length = usize::try_from(be16(at.checked_add(2)?)?).ok()?;
        at = at.checked_add(2)?.checked_add(length)?;
    }
    None
}

/// The content hash `address` names (`tilo.content:<hash>`, or the bare
/// hash), lowercase; `None` for anything else.
#[must_use]
pub fn content_hash(address: &str) -> Option<String> {
    let address = address.trim();
    let hash = address.strip_prefix(CONTENT_PREFIX).unwrap_or(address);
    (hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| hash.to_ascii_lowercase())
}

/// The content address of `hash`.
#[must_use]
pub fn content_address(hash: &str) -> String {
    format!("{CONTENT_PREFIX}{hash}")
}

/// The content hashes `text` names by address, in order.
fn addresses(text: &str) -> impl Iterator<Item = String> + '_ {
    text.match_indices(CONTENT_PREFIX).filter_map(|(at, _)| {
        let hash: String = text
            .get(at.saturating_add(CONTENT_PREFIX.len())..)?
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .take(65)
            .collect();
        (hash.len() == 64).then(|| hash.to_ascii_lowercase())
    })
}

/// `bytes`, offered as the picture at `content`, as a model is shown it.
/// A PNG or JPEG an app rendered (`rendered`) within the limits passes
/// through; anything else is shrunk and written out again in the confined
/// worker, which also drops what a photo says about itself (where and when
/// it was taken).
///
/// # Errors
///
/// Why it is not shown: not a picture, too large to read, or the worker
/// could not write it out within the limits.
pub fn prepare(content: &str, bytes: &[u8], rendered: bool) -> Result<Picture, String> {
    let content = content_hash(content)
        .map(|hash| content_address(&hash))
        .ok_or_else(|| format!("{content} is not a content address"))?;
    match admit(bytes) {
        Admission::PassThrough {
            media_type,
            width,
            height,
        } if rendered => Ok(Picture {
            content,
            media_type,
            width,
            height,
            bytes: Arc::from(bytes),
        }),
        Admission::PassThrough { .. } | Admission::Shrink => {
            let shrunk =
                crate::worker::shrink_picture(bytes, MOST_PICTURE_SIDE, MOST_PICTURE_BYTES)?;
            // What the worker wrote is checked as any untrusted picture is.
            match admit(&shrunk) {
                Admission::PassThrough {
                    media_type,
                    width,
                    height,
                } => Ok(Picture {
                    content,
                    media_type,
                    width,
                    height,
                    bytes: Arc::from(shrunk),
                }),
                _ => Err("the picture could not be made small enough to show".into()),
            }
        }
        Admission::Refuse(why) => Err(why),
    }
}

/// One planning conversation's view of pictures: whether its model sees
/// them, the content addresses it was shown, and how many pictures its
/// turn and plan have left.
pub(crate) struct Eyes {
    sees: bool,
    /// The content hashes the conversation's text named.
    shown: BTreeSet<String>,
    plan_left: usize,
    turn_left: usize,
}

impl Eyes {
    /// A conversation with a model that `sees` pictures, or not.
    pub(crate) fn new(sees: bool) -> Self {
        Self {
            sees,
            shown: BTreeSet::new(),
            plan_left: MOST_PICTURES_PER_PLAN,
            turn_left: MOST_PICTURES_PER_TURN,
        }
    }

    /// Whether its model is shown pictures.
    pub(crate) const fn sees(&self) -> bool {
        self.sees
    }

    /// Note the content addresses `text`, shown to the model, names.
    pub(crate) fn show(&mut self, text: &str) {
        self.shown.extend(addresses(text));
    }

    /// Whether the conversation was shown the content at `address`.
    pub(crate) fn was_shown(&self, address: &str) -> bool {
        content_hash(address).is_some_and(|hash| self.shown.contains(&hash))
    }

    /// A new turn, with its own pictures.
    pub(crate) fn next_turn(&mut self) {
        self.turn_left = MOST_PICTURES_PER_TURN;
    }

    /// What the model is sent for `seen`: its text, saying of each picture
    /// whether it is shown below or why not, and the pictures shown.
    pub(crate) fn attach(&mut self, seen: Seen) -> (String, Vec<Picture>) {
        let Seen { mut text, pictures } = seen;
        self.show(&text);
        let mut kept = Vec::new();
        for picture in pictures {
            self.show(&picture.content);
            let withheld = if !self.sees {
                Some("this model is not shown pictures".to_owned())
            } else if self.plan_left == 0 {
                Some(format!(
                    "this plan has been shown its {MOST_PICTURES_PER_PLAN} pictures"
                ))
            } else if self.turn_left == 0 {
                Some(format!(
                    "this turn has shown its {MOST_PICTURES_PER_TURN} pictures; ask again in \
                     another turn"
                ))
            } else {
                None
            };
            match withheld {
                None => {
                    self.plan_left = self.plan_left.saturating_sub(1);
                    self.turn_left = self.turn_left.saturating_sub(1);
                    let _ = write!(
                        text,
                        "\n({} picture shown below, {})",
                        picture.content,
                        picture.size_note()
                    );
                    kept.push(picture);
                }
                Some(why) => {
                    let _ = write!(text, "\n({} picture not shown: {why})", picture.content);
                }
            }
        }
        (text, kept)
    }

    /// The note the first turn's text ends with for `pictures` (what the
    /// task's steps looked at), and those shown.
    pub(crate) fn attach_first(&mut self, pictures: &[Picture]) -> (String, Vec<Picture>) {
        if pictures.is_empty() {
            return (String::new(), Vec::new());
        }
        let (text, kept) = self.attach(Seen {
            text: "\n\nWhat the steps so far looked at:".into(),
            pictures: pictures.to_vec(),
        });
        (text, kept)
    }
}

/// How many pictures `request` carries, and their bytes as sent (encoded):
/// for the usage log, which never holds the pictures.
#[must_use]
pub fn pictures_in(request: &ModelRequest) -> (usize, usize) {
    request
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|content| match content {
            Content::Image { base64, .. } => Some(base64.len()),
            _ => None,
        })
        .fold((0_usize, 0_usize), |(count, bytes), size| {
            (count.saturating_add(1), bytes.saturating_add(size))
        })
}

/// Whether an agent shows the room's own pictures to its model's provider.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RoomPictures {
    /// Not shown: the default.
    #[default]
    Withheld,
    /// Shown, as the person running the agent allowed.
    Shown,
}

impl From<bool> for RoomPictures {
    fn from(shown: bool) -> Self {
        if shown { Self::Shown } else { Self::Withheld }
    }
}

/// Whether the agent `agent_id`, shown as `label`, may show the room's own
/// pictures to its model, as [`ROOM_PICTURES_ENV`] reads `setting`.
#[must_use]
pub fn room_pictures_allowed(agent_id: &str, label: &str, setting: Option<&str>) -> bool {
    let Some(setting) = setting.map(str::trim).filter(|setting| !setting.is_empty()) else {
        return false;
    };
    match setting.to_ascii_lowercase().as_str() {
        "1" | "on" | "true" | "yes" | "all" => true,
        "0" | "off" | "false" | "no" | "none" => false,
        _ => setting.split(',').map(str::trim).any(|named| {
            !named.is_empty() && (named == agent_id || named.eq_ignore_ascii_case(label))
        }),
    }
}

/// Whether pictures are turned on or off for every planning model
/// ([`VISION_ENV`]), as `setting` reads.
#[must_use]
pub fn vision_setting(setting: Option<&str>) -> Option<bool> {
    match setting?.trim().to_ascii_lowercase().as_str() {
        "1" | "on" | "true" | "yes" => Some(true),
        "0" | "off" | "false" | "no" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A PNG of `width`×`height` grey pixels.
    pub(crate) fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            width,
            height,
            image::Rgb([0x80, 0x90, 0xa0]),
        ))
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
        bytes
    }

    pub(crate) fn address(fill: char) -> String {
        content_address(&fill.to_string().repeat(64))
    }

    pub(crate) fn picture(fill: char) -> Picture {
        prepare(&address(fill), &png(8, 6), true).unwrap()
    }

    #[test]
    fn a_rendered_picture_within_the_limits_passes_through_and_others_are_shrunk_or_refused() {
        assert_eq!(
            admit(&png(640, 480)),
            Admission::PassThrough {
                media_type: "image/png",
                width: 640,
                height: 480
            }
        );
        assert_eq!(admit(&png(2048, 16)), Admission::Shrink, "too wide");
        let mut gif = b"GIF89a".to_vec();
        gif.extend([0; 16]);
        assert_eq!(admit(&gif), Admission::Shrink, "a GIF is written out again");
        assert!(matches!(admit(b"%PDF-1.7\n"), Admission::Refuse(why) if why.contains("PDF")));
        assert!(matches!(admit(b"hello"), Admission::Refuse(_)));
        let mut huge = png(4, 4);
        huge.resize(MOST_SOURCE_BYTES + 1, 0);
        assert!(matches!(admit(&huge), Admission::Refuse(why) if why.contains("bytes")));

        let shown = prepare(&address('a'), &png(64, 32), true).unwrap();
        assert_eq!(
            (shown.media_type, shown.width, shown.height),
            ("image/png", 64, 32)
        );
        assert_eq!(shown.content, address('a'));
        assert!(prepare("not an address", &png(4, 4), true).is_err());
        assert!(prepare(&address('a'), b"%PDF-1.4", true).is_err());
    }

    #[test]
    fn a_picture_is_sent_as_inline_bytes_counted_and_never_logged() {
        use tilo_model_gateway::host_backend::protocol::{Message, Role, ToolChoice};
        let picture = picture('b');
        let Content::Image { media_type, base64 } = picture.content() else {
            panic!("an image");
        };
        assert_eq!(media_type, "image/png");
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(&base64)
                .unwrap(),
            picture.bytes.to_vec()
        );
        let logged = format!("{picture:?}");
        assert!(
            !logged.contains(&base64) && logged.contains("bytes"),
            "{logged}"
        );
        let request = ModelRequest {
            messages: vec![
                Message::text(Role::System, "x"),
                Message {
                    role: Role::User,
                    content: vec![
                        Content::Text { text: "x".into() },
                        picture.content(),
                        picture.content(),
                    ],
                },
            ],
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            max_output_tokens: 1,
        };
        assert_eq!(pictures_in(&request), (2, base64.len() * 2));
    }

    #[test]
    fn a_turn_and_a_plan_show_only_so_many_pictures() {
        let mut eyes = Eyes::new(true);
        let seen = |fill| Seen {
            text: "answered".into(),
            pictures: vec![picture(fill)],
        };
        let (text, shown) = eyes.attach(seen('1'));
        assert_eq!(shown.len(), 1);
        assert!(text.contains("picture shown below, 8×6 PNG"), "{text}");
        assert_eq!(eyes.attach(seen('2')).1.len(), 1);
        let (text, shown) = eyes.attach(seen('3'));
        assert!(shown.is_empty(), "two a turn");
        assert!(text.contains("not shown: this turn"), "{text}");
        eyes.next_turn();
        assert_eq!(eyes.attach(seen('4')).1.len(), 1);
        assert_eq!(eyes.attach(seen('5')).1.len(), 1);
        eyes.next_turn();
        assert_eq!(eyes.attach(seen('a')).1.len(), 1);
        assert_eq!(eyes.attach(seen('b')).1.len(), 1);
        eyes.next_turn();
        let (text, shown) = eyes.attach(seen('6'));
        assert!(shown.is_empty(), "six a plan");
        assert!(text.contains("this plan has been shown its 6"), "{text}");
        assert!(eyes.was_shown(&address('6')), "its address was still shown");

        let mut blind = Eyes::new(false);
        let (text, shown) = blind.attach(seen('7'));
        assert!(shown.is_empty());
        assert!(text.contains("picture not shown"), "{text}");
    }

    #[test]
    fn only_addresses_the_conversation_named_count_as_shown() {
        let mut eyes = Eyes::new(true);
        let hash = "c".repeat(64);
        eyes.show(&format!(
            "Content produced so far: tilo.content:{} and tilo.content:latest",
            hash.to_ascii_uppercase()
        ));
        assert!(eyes.was_shown(&content_address(&hash)));
        assert!(eyes.was_shown(&hash), "a bare hash names it too");
        assert!(!eyes.was_shown(&address('d')));
        assert!(!eyes.was_shown("tilo.content:latest"));
    }

    #[test]
    fn room_pictures_are_off_unless_turned_on_for_every_agent_or_by_name() {
        assert!(!room_pictures_allowed("a1", "Ada", None));
        assert!(!room_pictures_allowed("a1", "Ada", Some("0")));
        assert!(room_pictures_allowed("a1", "Ada", Some("1")));
        assert!(room_pictures_allowed("a1", "Ada", Some("on")));
        assert!(room_pictures_allowed("a1", "Ada", Some("b2, a1")));
        assert!(room_pictures_allowed("a1", "Ada", Some("ada")));
        assert!(!room_pictures_allowed("a1", "Ada", Some("b2,Bob")));
        assert_eq!(vision_setting(Some("off")), Some(false));
        assert_eq!(vision_setting(Some("1")), Some(true));
        assert_eq!(vision_setting(Some("maybe")), None);
        assert_eq!(vision_setting(None), None);
    }
}
