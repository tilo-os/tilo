//! The paint stream: a page as a display list instead of pixels. [`PaintSource`] runs
//! where the engine runs. It asks the engine for its layout whenever the page may have
//! changed, converts it into a [`Frame`], resolves image URLs to content hashes (reading
//! the bytes from the engine's own cache, never fetching again) and emits the frame with
//! the [`Patch`] from the previous one.

pub use tilo_paint_stream as stream;

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use tilo_paint_stream::fonts::{ALIKE, family_names, generic_class};
pub use tilo_paint_stream::fonts::{FontClass, font_class};
use tilo_paint_stream::snapshot::Told;
use tilo_paint_stream::wire::Payload;
use tilo_paint_stream::{
    BlobRef, Color as PaintColor, Frame, Item, ItemKey, LayerId, Patch, Point, ROOT,
};
use tilo_webview::{MAX_FAMILY_LIST, MAX_SHOWN_FAMILIES, MAX_SHOWN_FIELDS};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill, ImageBrush, ImageData};

use tilo_ui_base::{text_render::FontChoice, text_render::TextRenderer, text_render::TextStyle, text_render::TextWeight};
use crate::time::{Duration, Instant};
use tilo_lock::MutexRecover as _;
use tilo_webview::{
    FrameSlot, KeyModifiers, SharedEngine, WebViewCommand, WebViewEngine, WebViewEvent,
};

const MIN_CAPTURE_INTERVAL: Duration = Duration::from_millis(50);
/// How long the engine is given to answer before it is looked at again.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
/// The family list whose class the engine is asked for when it is asked whether it still
/// has a capture ([`Capture`]): one it can say the class of at once.
const LOOK_FAMILY: &str = "serif";
/// How often the wait between two looks at one capture doubles, from [`REQUEST_TIMEOUT`].
const LOOK_DOUBLINGS: u32 = 5;
/// The looks at one capture whose answers are still told from any other answer.
const MAX_LOOKS: usize = 8;
const MAX_RESOURCE_REQUESTS: usize = 8;
/// Password fields remembered of one page.
const MAX_SECRET_FIELDS: usize = 4096;
/// How soon the engine is asked again what class of face it sets the page's text in, while
/// the page still loads faces of its own, and how often.
const FACES_RECHECK: Duration = Duration::from_secs(1);
const FACES_RECHECKS: u8 = 8;

/// A capture of the page the engine has not answered yet. However long the engine takes
/// over it, its answer is the page: a large page on a busy machine takes longer than
/// [`REQUEST_TIMEOUT`], and asking for it again would have the engine start over while what
/// it was at work on is thrown away. An engine that lost the request never answers, though,
/// and the two are told apart by asking it something small behind the capture
/// ([`LOOK_FAMILY`]), since what an engine is asked behind a capture it answers behind it:
/// the capture's answer comes first from an engine still at work on it, and the small
/// answer alone from one that lost it, which is then asked for the capture again. An engine
/// that predates the small ask reads it as a capture and answers it with the page.
struct Capture {
    id: u32,
    /// When the engine is next asked whether it still has the capture.
    look: Option<Instant>,
    /// How often it was asked.
    looks: u32,
    /// What it was asked behind the capture, by request.
    behind: Vec<u32>,
}

/// One ask of the engine for what a snapshot does not say.
struct ShownRequest {
    id: u32,
    at: Instant,
    /// The fields asked about, each with what it held.
    fields: Vec<(u64, String)>,
    /// The family lists asked about.
    families: Vec<String>,
}
const MAX_BLOB_BYTES: usize = 16 << 20;
/// Patches queued for a stream consumer before it must take a whole frame.
const MAX_QUEUED_PATCHES: usize = 64;
/// Largest part of a payload one message of the app → shell channel carries once base64
/// encoded (its frames are at most 1 MiB). A larger payload goes in several.
pub const MAX_SHARE_PART: usize = 700 * 1024;
/// Largest encoded payload a share hands over whole: what the paint stream's wire carries.
/// Of a larger one only its size and its start go, so the session can say that the page is
/// too large rather than wait for it.
pub const MAX_SHARE_PAYLOAD: usize = stream::wire::MAX_PAYLOAD_BYTES;
const MAX_SHARE_BLOB: usize = stream::wire::MAX_BLOB_BYTES;
/// Shared frames waiting for the app to send them before it must start over. A whole frame
/// and an image of the largest size both fit.
const MAX_SHARE_QUEUE_BYTES: usize = 2 * MAX_SHARE_PAYLOAD;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResourceSpec {
    Pending(u32),
    Ready([u8; 32]),
    Failed,
}

/// A new frame, and the patch from the frame before it (`None` for the first).
pub struct PaintUpdate {
    pub frame: Arc<Frame>,
    pub patch: Option<Patch>,
}

/// Host-side producer of frames and patches for one page.
pub struct PaintSource {
    viewport: Point,
    min_interval: Duration,
    dirty: bool,
    next_request: u32,
    capture: Option<Capture>,
    last_capture: Option<Instant>,
    /// The engine's snapshot the page was last converted from, kept while the engine is
    /// still to say what a field of it shows: the page is then converted again from this,
    /// with what the engine said, and not captured again.
    snapshot: Option<serde_json::Value>,
    /// What class of face the engine said it sets a family list like, by the list as its
    /// style computes it; `None` where it could not tell. A list is carried as the engine
    /// set it where the page is published ([`Self::publish`]), so what the engine says of
    /// a list costs no second capture of the page and no second conversion.
    classes: HashMap<String, Option<FontClass>>,
    raw: Option<Frame>,
    frame: Option<Arc<Frame>>,
    resources: HashMap<String, ResourceSpec>,
    resource_requests: HashMap<u32, (String, Instant)>,
    blobs: HashMap<[u8; 32], Arc<Vec<u8>>>,
    vectors: HashMap<[u8; 32], Option<Arc<tilo_paint_stream::svg::Drawing>>>,
    /// What the engine said of the page beside its layout: what its dates, times and
    /// numbers show, and which fields were password fields.
    told: Told,
    /// The fields the last snapshot named that the engine has not said of yet.
    asks: Vec<(u64, String)>,
    /// The family lists the last snapshot's text is set in that the engine has not said
    /// the class of. A page is not published while there are any: its text would arrive in
    /// one class of face and again in another.
    families: Vec<String>,
    /// The ask the engine has not answered.
    shown_request: Option<ShownRequest>,
    /// When the engine is asked again for the classes it said, and how often still: a page
    /// that loads a face of its own is set in another until the face is there.
    faces_recheck: Option<(Instant, u8)>,
    /// The document the engine's answers are about: its address, less the fragment.
    document: String,
}

impl PaintSource {
    #[must_use]
    pub fn new(viewport: Point) -> Self {
        Self {
            told: Told {
                asking: true,
                ..Told::default()
            },
            asks: Vec::new(),
            families: Vec::new(),
            shown_request: None,
            faces_recheck: None,
            document: String::new(),
            viewport,
            min_interval: MIN_CAPTURE_INTERVAL,
            dirty: true,
            next_request: 0,
            capture: None,
            last_capture: None,
            snapshot: None,
            classes: HashMap::new(),
            raw: None,
            frame: None,
            resources: HashMap::new(),
            resource_requests: HashMap::new(),
            blobs: HashMap::new(),
            vectors: HashMap::new(),
        }
    }

    /// Shortest time between two captures.
    pub const fn set_min_interval(&mut self, interval: Duration) {
        self.min_interval = interval;
    }

    /// The page may have changed: capture again at the next opportunity.
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    /// When [`Self::poll`] next has something to do although the engine says nothing new: a
    /// change that waits for the capture interval to pass, or a request whose answer is
    /// overdue. Whoever runs the source comes back then; the engine wakes nobody for either.
    #[must_use]
    pub fn due(&self, now: Instant) -> Option<Instant> {
        let capture = match &self.capture {
            Some(capture) => capture.look,
            None if self.dirty && self.shown_request.is_none() => Some(
                self.last_capture
                    .and_then(|at| at.checked_add(self.min_interval))
                    .unwrap_or(now),
            ),
            None => None,
        };
        let resources = self
            .resource_requests
            .values()
            .filter_map(|(_, at)| at.checked_add(REQUEST_TIMEOUT))
            .min();
        let shown = match &self.shown_request {
            Some(request) => request.at.checked_add(REQUEST_TIMEOUT),
            None if self.asking() => Some(now),
            None => self.faces_recheck.map(|(at, _)| at),
        };
        capture.into_iter().chain(resources).chain(shown).min()
    }

    pub fn set_viewport(&mut self, viewport: Point) {
        if viewport != self.viewport {
            self.viewport = viewport;
            self.dirty = true;
        }
    }

    #[must_use]
    pub fn frame(&self) -> Option<Arc<Frame>> {
        self.frame.clone()
    }

    #[must_use]
    pub fn blob(&self, hash: &[u8; 32]) -> Option<Arc<Vec<u8>>> {
        self.blobs.get(hash).cloned()
    }

    fn request_id(&mut self) -> u32 {
        self.next_request = self.next_request % 0x7fff_fffe + 1;
        self.next_request
    }

    /// Ask the engine whether it still has the capture it has not answered, once it is time
    /// to: something small behind the capture, and again after twice as long.
    fn look(&mut self, now: Instant) -> Option<WebViewCommand> {
        self.capture
            .as_ref()
            .and_then(|capture| capture.look)
            .filter(|look| *look <= now)?;
        let request_id = self.request_id();
        let capture = self.capture.as_mut()?;
        capture.looks = capture.looks.saturating_add(1);
        let wait =
            REQUEST_TIMEOUT.checked_mul(2_u32.saturating_pow(capture.looks.min(LOOK_DOUBLINGS)));
        capture.look = wait.and_then(|wait| now.checked_add(wait));
        if capture.behind.len() >= MAX_LOOKS {
            capture.behind.remove(0);
        }
        capture.behind.push(request_id);
        Some(WebViewCommand::PaintShown {
            request_id,
            nodes: Vec::new(),
            families: vec![LOOK_FAMILY.to_owned()],
        })
    }

    /// Commands the engine should run now, and the page where it is published without the
    /// engine saying anything more: an ask it did not answer in time was given up.
    pub fn poll(&mut self, now: Instant) -> (Vec<WebViewCommand>, Option<PaintUpdate>) {
        let mut out = Vec::new();
        let mut update = None;
        out.extend(self.look(now));
        let expired: Vec<u32> = self
            .resource_requests
            .iter()
            .filter(|(_, (_, at))| now.saturating_duration_since(*at) > REQUEST_TIMEOUT)
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            if let Some((url, _)) = self.resource_requests.remove(&id) {
                self.resources.insert(url, ResourceSpec::Failed);
            }
        }
        // An engine that does not answer leaves a field drawn with what it holds and text
        // set in the class of face its list asks for.
        if let Some(request) = self
            .shown_request
            .take_if(|request| now.saturating_duration_since(request.at) > REQUEST_TIMEOUT)
        {
            update = self.shown(request, None);
        }
        if self.shown_request.is_none() {
            let again = self
                .faces_recheck
                .take_if(|(at, _)| *at <= now)
                .map(|(_, left)| left);
            // What is asked stays noted until the engine said it or was given up.
            let (fields, families) = if self.asking() {
                (
                    self.asks.iter().take(MAX_SHOWN_FIELDS).cloned().collect(),
                    self.families
                        .iter()
                        .take(MAX_SHOWN_FAMILIES)
                        .cloned()
                        .collect::<Vec<_>>(),
                )
            } else if again.is_some() {
                let said = self.classes.keys().take(MAX_SHOWN_FAMILIES).cloned();
                (Vec::new(), said.collect())
            } else {
                (Vec::new(), Vec::new())
            };
            if !fields.is_empty() || !families.is_empty() {
                if let Some(left) = again.filter(|left| *left > 0) {
                    // Kept until the engine says its faces are all there.
                    self.faces_recheck = now.checked_add(FACES_RECHECK).map(|at| (at, left - 1));
                }
                let id = self.request_id();
                out.push(WebViewCommand::PaintShown {
                    request_id: id,
                    nodes: fields.iter().map(|(node, _)| *node).collect(),
                    families: families.clone(),
                });
                self.shown_request = Some(ShownRequest {
                    id,
                    at: now,
                    fields,
                    families,
                });
            }
        }
        let rested = self
            .last_capture
            .is_none_or(|at| now.saturating_duration_since(at) >= self.min_interval);
        // No capture is asked for while the engine is still to answer an ask: behind a
        // capture the answer would wait for as long as the capture takes, and a page that
        // waits for it as long again.
        if self.dirty && self.capture.is_none() && self.shown_request.is_none() && rested {
            let request_id = self.request_id();
            self.capture = Some(Capture {
                id: request_id,
                look: now.checked_add(REQUEST_TIMEOUT),
                looks: 0,
                behind: Vec::new(),
            });
            self.last_capture = Some(now);
            self.dirty = false;
            out.push(WebViewCommand::PaintCapture { request_id });
        }
        out.extend(self.request_resources(now));
        (out, update)
    }

    fn request_resources(&mut self, now: Instant) -> Vec<WebViewCommand> {
        let wanted: Vec<String> = self
            .raw
            .iter()
            .flat_map(Frame::blobs)
            .filter_map(|b| match b {
                BlobRef::Url(u) if !self.resources.contains_key(u) => Some(u.clone()),
                _ => None,
            })
            .collect();
        let mut out = Vec::new();
        for url in wanted {
            if self.resources.contains_key(&url) {
                continue;
            }
            if self.resource_requests.len() >= MAX_RESOURCE_REQUESTS {
                break;
            }
            let request_id = self.request_id();
            self.resources
                .insert(url.clone(), ResourceSpec::Pending(request_id));
            self.resource_requests
                .insert(request_id, (url.clone(), now));
            out.push(WebViewCommand::PaintResource { request_id, url });
        }
        out
    }

    /// Images inlined as `data:` URLs need no engine round trip.
    fn resolve_inline(&mut self) {
        let inline: Vec<String> = self
            .raw
            .iter()
            .flat_map(Frame::blobs)
            .filter_map(|b| match b {
                BlobRef::Url(u) if u.starts_with("data:") && !self.resources.contains_key(u) => {
                    Some(u.clone())
                }
                _ => None,
            })
            .collect();
        for url in inline {
            let state = data_url_bytes(&url)
                .and_then(|bytes| self.store(bytes))
                .map_or(ResourceSpec::Failed, ResourceSpec::Ready);
            self.resources.insert(url, state);
        }
    }

    fn store(&mut self, bytes: Vec<u8>) -> Option<[u8; 32]> {
        if bytes.is_empty() || bytes.len() > MAX_BLOB_BYTES {
            return None;
        }
        let hash = *blake3::hash(&bytes).as_bytes();
        self.blobs.entry(hash).or_insert_with(|| Arc::new(bytes));
        Some(hash)
    }

    pub fn on_event(&mut self, event: &WebViewEvent) -> (Option<PaintUpdate>, bool) {
        match event {
            WebViewEvent::PaintSnapshot {
                request_id,
                snapshot,
            } => {
                if self
                    .shown_request
                    .take_if(|request| request.id == *request_id)
                    .is_some()
                    || self
                        .capture
                        .take_if(|capture| capture.behind.contains(request_id))
                        .is_some()
                {
                    // An engine that predates the ask read it as a capture: it cannot say
                    // what a field shows or what face it sets, and what it answered with is
                    // the page. Asked behind a capture it lost, that is the capture's answer.
                    self.told.asking = false;
                    self.asks.clear();
                    self.families.clear();
                    self.faces_recheck = None;
                    self.snapshot = None;
                    return (self.captured(snapshot.as_deref()), true);
                }
                // The answer to the capture is taken however late it comes.
                if self
                    .capture
                    .take_if(|capture| capture.id == *request_id)
                    .is_none()
                {
                    return (None, true);
                }
                (self.captured(snapshot.as_deref()), true)
            }
            WebViewEvent::PaintShown {
                request_id,
                shown,
                classes,
                settled,
            } => {
                if self
                    .capture
                    .take_if(|capture| capture.behind.contains(request_id))
                    .is_some()
                {
                    // The engine said what it was asked behind a capture and nothing of the
                    // capture: it lost that, and is asked for it again at once.
                    self.dirty = true;
                    self.last_capture = None;
                    return (None, true);
                }
                let Some(request) = self
                    .shown_request
                    .take_if(|request| request.id == *request_id)
                else {
                    return (None, true);
                };
                let said = shown.iter().flatten().cloned().collect();
                if *settled {
                    self.faces_recheck = None;
                } else if self.faces_recheck.is_none() && !request.families.is_empty() {
                    self.faces_recheck = crate::time::now()
                        .checked_add(FACES_RECHECK)
                        .map(|at| (at, FACES_RECHECKS));
                }
                (self.shown(request, Some((&said, classes))), true)
            }
            WebViewEvent::PaintResource { request_id, bytes } => {
                let Some((url, _)) = self.resource_requests.remove(request_id) else {
                    return (None, true);
                };
                let state = bytes
                    .clone()
                    .and_then(|b| self.store(b))
                    .map_or(ResourceSpec::Failed, ResourceSpec::Ready);
                let ready = matches!(state, ResourceSpec::Ready(_));
                self.resources.insert(url, state);
                (if ready { self.publish() } else { None }, true)
            }
            WebViewEvent::UriChanged(uri) => {
                // Another document: what the engine said was of the one before.
                let document = uri.split('#').next().unwrap_or_default();
                if document != self.document {
                    document.clone_into(&mut self.document);
                    self.told.shown.clear();
                    self.told.secret.clear();
                    self.classes.clear();
                    self.asks.clear();
                    self.families.clear();
                    self.faces_recheck = None;
                    self.snapshot = None;
                }
                self.dirty = true;
                (None, false)
            }
            WebViewEvent::LoadFinishedAt { .. } => {
                self.dirty = true;
                (None, false)
            }
            _ => (None, false),
        }
    }

    /// Whether the engine has still to say something of the page: what a field shows, or
    /// what class of face it sets a family list in.
    fn asking(&self) -> bool {
        self.told.asking && !(self.asks.is_empty() && self.families.is_empty())
    }

    /// Take the engine's layout snapshot of the page.
    fn captured(&mut self, snapshot: Option<&str>) -> Option<PaintUpdate> {
        let snapshot = snapshot.and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())?;
        self.convert(snapshot)
    }

    /// Convert the engine's layout snapshot of the page with what the engine said its
    /// fields show so far, note what it is still to be asked, and publish the page unless
    /// its text waits for the engine to say what class of face it is set in. The snapshot
    /// is kept while the engine is still to say what a field of it shows, to be converted
    /// again with the answer. Every family list is converted as its style computes it:
    /// what class the engine sets it in is put on the page where it is published.
    fn convert(&mut self, snapshot: serde_json::Value) -> Option<PaintUpdate> {
        let converted =
            tilo_paint_stream::snapshot::convert_with(&snapshot, self.viewport, 0, &self.told)
                .ok()?;
        if self.told.secret.len() > MAX_SECRET_FIELDS {
            self.told.secret.clear();
        }
        self.told.secret.extend(converted.passwords);
        // What was said of a field the page no longer has is of no use.
        self.told
            .shown
            .retain(|node, _| converted.asks.iter().any(|(asked, _)| asked == node));
        let told = &self.told.shown;
        self.asks = converted
            .asks
            .into_iter()
            .filter(|(node, value)| told.get(node).is_none_or(|(held, _)| held != value))
            .collect();
        self.families.clear();
        for family in converted.families {
            if self.classes.contains_key(&family) {
                continue;
            }
            if self.told.asking && family.len() <= MAX_FAMILY_LIST {
                self.families.push(family);
            } else {
                self.classes.insert(family, None);
            }
        }
        self.raw = Some(converted.frame);
        self.resolve_inline();
        self.snapshot = (self.told.asking && !self.asks.is_empty()).then_some(snapshot);
        self.publish()
    }

    /// Take what the engine said to one ask (`said`): what its fields show, each for the
    /// value it held when it was asked, and the class of face it sets each family list
    /// like, in the order asked. A field it said nothing of is drawn with what it holds,
    /// and a list it said nothing of is set in the class it asks for. An engine that did
    /// not answer in time (`None`) said nothing of any, and what it said of a list before
    /// stands: text is not set in another class of face for an answer that is late.
    ///
    /// Nothing of this captures the page again. What a field shows is put into the
    /// snapshot the page was converted from, which is converted again, and what class a
    /// list is set in is put on the page as it is published. Only a page whose snapshot is
    /// no longer kept is captured again for what a field shows.
    fn shown(
        &mut self,
        request: ShownRequest,
        said: Option<(&HashMap<u64, String>, &[String])>,
    ) -> Option<PaintUpdate> {
        let fields = !request.fields.is_empty();
        let mut changed = false;
        for (node, value) in request.fields {
            let text = said.and_then(|(shown, _)| shown.get(&node).cloned());
            self.told.shown.insert(node, (value, text));
        }
        for (index, family) in request.families.into_iter().enumerate() {
            let Some((_, classes)) = said else {
                if let std::collections::hash_map::Entry::Vacant(unsaid) =
                    self.classes.entry(family)
                {
                    unsaid.insert(None);
                    changed = true;
                }
                continue;
            };
            let class = classes.get(index).and_then(|name| FontClass::named(name));
            changed |= self.classes.insert(family, class) != Some(class);
        }
        // What the engine has said, or was given up on, is not asked again.
        let shown = &self.told.shown;
        self.asks
            .retain(|(node, value)| shown.get(node).is_none_or(|(held, _)| held != value));
        let classes = &self.classes;
        self.families.retain(|family| !classes.contains_key(family));
        if fields {
            return if let Some(snapshot) = self.snapshot.take() {
                self.convert(snapshot)
            } else {
                self.dirty = true;
                self.last_capture = None;
                None
            };
        }
        if changed { self.publish() } else { None }
    }

    /// Publish the page as it was last converted, each image as the bytes it shows and
    /// each family list as the engine said it sets it ([`tilo_paint_stream::fonts::told`]).
    /// A page whose text the engine has still to say the class of is not published, by
    /// whatever brought it here: it would arrive in one class of face and again in another.
    fn publish(&mut self) -> Option<PaintUpdate> {
        if self.told.asking && !self.families.is_empty() {
            return None;
        }
        let mut next = self.raw.clone()?;
        // The lists the engine set in another class than they ask for, each as it is carried.
        let said: HashMap<String, String> = self
            .classes
            .iter()
            .filter_map(|(css, class)| {
                let carried = tilo_paint_stream::fonts::told(css, (*class)?);
                (carried != *css).then(|| (css.clone(), carried))
            })
            .collect();
        for layer in next.layers.values_mut() {
            let items = std::mem::take(&mut layer.items);
            layer.items = items
                .into_iter()
                .filter_map(|item| self.resolve(item, &said))
                .collect();
        }
        let previous = self.frame.clone();
        next.seq = previous.as_ref().map_or(1, |p| p.seq.saturating_add(1));
        let patch = previous.as_ref().map(|p| p.diff(&next));
        if patch.as_ref().is_some_and(Patch::is_empty) {
            return None;
        }
        let frame = Arc::new(next);
        self.frame = Some(Arc::clone(&frame));
        Some(PaintUpdate { frame, patch })
    }
}

impl PaintSource {
    fn resolve(&mut self, mut item: Item, said: &HashMap<String, String>) -> Option<Item> {
        if let Item::Text { font, .. } = &mut item {
            if let Some(carried) = said.get(&font.family) {
                font.family.clone_from(carried);
            }
            return Some(item);
        }
        let Item::Image { key, rect, blob } = item else {
            return Some(item);
        };
        let hash = match &blob {
            BlobRef::Hash(hash) => *hash,
            BlobRef::Url(url) => match self.resources.get(url.as_str()) {
                Some(ResourceSpec::Ready(hash)) => *hash,
                _ => return None,
            },
        };
        let blobs = &self.blobs;
        let drawing = self.vectors.entry(hash).or_insert_with(|| {
            blobs
                .get(&hash)
                .and_then(|b| tilo_paint_stream::svg::drawing(b))
                .map(Arc::new)
        });
        Some(match drawing {
            Some(drawing) => Item::Vector {
                key,
                rect,
                view_box: drawing.0,
                shapes: drawing.1.clone(),
            },
            None => Item::Image {
                key,
                rect,
                blob: BlobRef::Hash(hash),
            },
        })
    }
}

/// One encoded payload of the page a window shares, for the session.
pub struct SharedFrame {
    /// The payload, or only its start when it is larger than a share hands over
    /// ([`MAX_SHARE_PAYLOAD`]): `of` says which.
    pub payload: Vec<u8>,
    /// How many bytes the whole payload has.
    pub of: usize,
    /// Where the page sits in the window, logical pixels: x, y, w, h.
    pub page: [f64; 4],
    pub scale: f64,
}

impl SharedFrame {
    /// A payload that goes whole.
    #[must_use]
    pub const fn whole(payload: Vec<u8>, page: [f64; 4], scale: f64) -> Self {
        Self {
            of: payload.len(),
            payload,
            page,
            scale,
        }
    }

    /// Whether the payload is all here: one larger than a share hands over is not.
    #[must_use]
    pub const fn complete(&self) -> bool {
        self.payload.len() == self.of
    }
}

static PAINT_CAPABLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether this app can stream a page, for it to tell the session.
#[must_use]
pub fn share_capable() -> bool {
    PAINT_CAPABLE.load(std::sync::atomic::Ordering::Relaxed)
}

/// What one window hands its session of its page.
#[derive(Default)]
struct ShareOutbox {
    watched: bool,
    keyframe: bool,
    frames: VecDeque<SharedFrame>,
    bytes: usize,
}

/// Every window's share, by window. A process shows several windows
/// ([`crate::window_link`]), each with its own page, its own connection to the session and
/// its own watchers: what one of them is asked for, and what it hands over, is never
/// another's. The functions below act on the window whose events run
/// ([`crate::window_toolbar::current_window`]); `None` is a process that runs its one
/// window with no host around it, as a test does. A window nobody watches keeps nothing
/// here.
static SHARES: Mutex<std::collections::BTreeMap<Option<u64>, ShareOutbox>> =
    Mutex::new(std::collections::BTreeMap::new());

/// Read or change the share of `window`.
fn share_of<T>(window: Option<u64>, act: impl FnOnce(&mut ShareOutbox) -> T) -> T {
    let mut shares = SHARES.lock_recover();
    let outbox = shares.entry(window).or_default();
    let answer = act(outbox);
    if !outbox.watched {
        shares.remove(&window);
    }
    answer
}

/// The window whose events run now.
fn window() -> Option<u64> {
    crate::window_toolbar::current_window()
}

/// Whether the session shares this window's page with someone watching.
#[must_use]
pub fn share_watched() -> bool {
    share_watched_in(window())
}

/// Whether the session shares the page of `window` with someone watching.
fn share_watched_in(window: Option<u64>) -> bool {
    SHARES
        .lock_recover()
        .get(&window)
        .is_some_and(|outbox| outbox.watched)
}

/// The session started (`true`) or stopped (`false`) sharing this window's page.
pub fn set_share_watched(watched: bool) {
    share_of(window(), |outbox| {
        outbox.watched = watched;
        outbox.keyframe = watched;
        if !watched {
            outbox.frames.clear();
            outbox.bytes = 0;
        }
    });
}

/// Whether this window's next shared frame must be a whole one; asking clears it.
#[must_use]
pub fn take_share_keyframe() -> bool {
    share_of(window(), |outbox| std::mem::take(&mut outbox.keyframe))
}

/// Queue one frame of this window's page for the session.
pub fn queue_share(frame: SharedFrame) {
    share_of(window(), |outbox| {
        if !outbox.watched {
            return;
        }
        let bytes = outbox.bytes.saturating_add(frame.payload.len());
        if bytes > MAX_SHARE_QUEUE_BYTES {
            outbox.frames.clear();
            outbox.bytes = 0;
            outbox.keyframe = true;
            return;
        }
        outbox.bytes = bytes;
        outbox.frames.push_back(frame);
    });
}

/// How many more bytes of images this window's share takes now, beside a whole frame: what
/// is past it stays with the engine until the app has sent what is queued.
#[must_use]
pub fn share_room() -> usize {
    share_of(window(), |outbox| {
        MAX_SHARE_QUEUE_BYTES
            .saturating_sub(MAX_SHARE_PAYLOAD)
            .saturating_sub(outbox.bytes)
    })
}

/// This window closes: nothing of its share stays behind, watched or not. A process that runs
/// its one window with no host around it keeps its share to its end.
pub fn forget_share() {
    if let Some(window) = window() {
        SHARES.lock_recover().remove(&Some(window));
    }
}

/// The oldest frame this window has to send its session.
pub fn next_share_frame() -> Option<SharedFrame> {
    share_of(window(), |outbox| {
        let frame = outbox.frames.pop_front()?;
        outbox.bytes = outbox.bytes.saturating_sub(frame.payload.len());
        Some(frame)
    })
}

/// The frames this window has to send its session, oldest first.
pub fn take_share_frames() -> Vec<SharedFrame> {
    share_of(window(), |outbox| {
        outbox.bytes = 0;
        outbox.frames.drain(..).collect()
    })
}

fn data_url_bytes(url: &str) -> Option<Vec<u8>> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    if meta.ends_with(";base64") {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.decode(data).ok()
    } else {
        Some(data.as_bytes().to_vec())
    }
}

struct PaintState {
    source: PaintSource,
    last_pixels: usize,
    patches: VecDeque<Patch>,
    keyframe_needed: bool,
    sent_blobs: std::collections::HashSet<[u8; 32]>,
    wanted_visible: bool,
    /// The frame a view last drew (its pixels' address).
    presented_pixels: usize,
    undrawn_since: Option<Instant>,
    painting: bool,
    hide_after: Duration,
    /// The window that runs this engine: the one whose events last called it, whose share
    /// is the one this page is handed over in.
    window: Option<u64>,
    /// When to come back for the images of the page a watched share was not handed yet, for
    /// want of room; `None` while it holds them all.
    images_due: Option<Instant>,
}

/// How soon a share that had no room for all of a page's images is offered the rest: what
/// it holds goes to the session meanwhile.
const SHARE_IMAGES_RETRY: Duration = Duration::from_millis(30);

const HIDE_UNPRESENTED: Duration = Duration::from_secs(2);

/// Any engine, drawn as a display list.
pub struct PaintEngine {
    inner: SharedEngine,
    state: Mutex<PaintState>,
    /// Draw this view from the display list.
    draws: bool,
}

impl PaintEngine {
    #[must_use]
    pub fn new(inner: SharedEngine) -> Arc<Self> {
        Self::build(inner, MIN_CAPTURE_INTERVAL, true)
    }

    /// As [`Self::new`], capturing at most once per `interval`.
    #[must_use]
    pub fn with_interval(inner: SharedEngine, interval: Duration) -> Arc<Self> {
        Self::build(inner, interval, true)
    }

    /// An engine drawn from its own pixels, whose page is captured as a display list only
    /// while a session shares it with someone watching.
    #[must_use]
    pub fn streaming(inner: SharedEngine) -> Arc<Self> {
        Self::build(inner, MIN_CAPTURE_INTERVAL, false)
    }

    fn build(inner: SharedEngine, interval: Duration, draws: bool) -> Arc<Self> {
        PAINT_CAPABLE.store(true, std::sync::atomic::Ordering::Relaxed);
        let mut source = PaintSource::new(Point { x: 800.0, y: 600.0 });
        source.set_min_interval(interval);
        Arc::new(Self {
            inner,
            state: Mutex::new(PaintState {
                source,
                last_pixels: 0,
                patches: VecDeque::new(),
                keyframe_needed: true,
                sent_blobs: std::collections::HashSet::new(),
                wanted_visible: true,
                presented_pixels: 0,
                undrawn_since: None,
                painting: true,
                hide_after: HIDE_UNPRESENTED,
                window: None,
                images_due: None,
            }),
            draws,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PaintState> {
        self.state.lock_recover()
    }

    /// The patches since the last call, oldest first, or `Err` with the current frame
    /// when the consumer must start over from a whole frame.
    pub fn take_stream(&self) -> Result<Vec<Patch>, Option<Arc<Frame>>> {
        Self::take_stream_locked(&mut self.lock())
    }

    fn take_stream_locked(state: &mut PaintState) -> Result<Vec<Patch>, Option<Arc<Frame>>> {
        if state.keyframe_needed {
            state.patches.clear();
            let frame = state.source.frame();
            state.keyframe_needed = frame.is_none();
            return Err(frame);
        }
        Ok(state.patches.drain(..).collect())
    }

    /// Whether the session shares this engine's page with someone watching: the share of
    /// the window that runs the engine. A window's events run its own engine and no other
    /// window's, so the window whose events last called it is the one; a call from outside
    /// any window's events (the engine's own thread) is the same window's still.
    fn watched(state: &mut PaintState) -> bool {
        if let Some(window) = window() {
            state.window = Some(window);
        }
        share_watched_in(state.window)
    }

    /// Run the source: notice new pixels (the page changed) and issue its requests.
    fn pump(&self, state: &mut PaintState) {
        if !self.draws && !Self::watched(state) {
            return;
        }
        let pixels = self
            .inner
            .frames()
            .lock_recover()
            .as_ref()
            .map_or(0, |f| Arc::as_ptr(&f.rgba) as usize);
        if pixels != state.last_pixels {
            state.last_pixels = pixels;
            state.source.invalidate();
        }
        let (commands, update) = state.source.poll(crate::time::now());
        if let Some(update) = update {
            Self::absorb(state, update);
        }
        for command in commands {
            self.inner.send(command);
        }
    }

    /// The address of the engine's current frame's pixels; 0 for none.
    fn current_pixels(&self) -> usize {
        self.inner
            .frames()
            .lock_recover()
            .as_ref()
            .map_or(0, |frame| Arc::as_ptr(&frame.rgba) as usize)
    }

    fn update_visibility(&self, state: &mut PaintState) {
        let now = crate::time::now();
        if self.current_pixels() == state.presented_pixels {
            state.undrawn_since = None;
        } else if state.undrawn_since.is_none() {
            state.undrawn_since = Some(now);
        }
        let seen = state
            .undrawn_since
            .is_none_or(|since| now.saturating_duration_since(since) < state.hide_after);
        let paint = state.wanted_visible && (seen || Self::watched(state));
        if paint != state.painting {
            state.painting = paint;
            self.inner.set_visible(paint);
        }
    }

    fn absorb(state: &mut PaintState, update: PaintUpdate) {
        if let Some(patch) = update.patch {
            if state.patches.len() >= MAX_QUEUED_PATCHES {
                state.keyframe_needed = true;
                state.patches.clear();
            } else {
                state.patches.push_back(patch);
            }
        }
    }
}

impl WebViewEngine for PaintEngine {
    fn create_tab(&self) -> Result<SharedEngine, String> {
        let tab: SharedEngine =
            Self::build(self.inner.create_tab()?, MIN_CAPTURE_INTERVAL, self.draws);
        Ok(tab)
    }
    fn set_visible(&self, visible: bool) {
        let mut state = self.lock();
        state.wanted_visible = visible;
        self.update_visibility(&mut state);
    }
    fn presented(&self) {
        let pixels = self.current_pixels();
        let mut state = self.lock();
        state.presented_pixels = pixels;
        state.undrawn_since = None;
        self.update_visibility(&mut state);
    }
    fn set_modifiers(&self, modifiers: KeyModifiers) {
        self.inner.set_modifiers(modifiers);
    }
    fn take_new_tab_requests(&self) -> Vec<(String, bool)> {
        self.inner.take_new_tab_requests()
    }
    fn supports_native_filtering(&self) -> bool {
        self.inner.supports_native_filtering()
    }
    fn send(&self, cmd: WebViewCommand) {
        if let WebViewCommand::Resize {
            width,
            height,
            scale_factor,
        } = &cmd
        {
            let scale = if *scale_factor > 0.0 {
                *scale_factor
            } else {
                1.0
            };
            let viewport = Point {
                x: *width as f32 / scale,
                y: *height as f32 / scale,
            };
            self.lock().source.set_viewport(viewport);
        }
        self.inner.send(cmd);
    }
    fn frames(&self) -> FrameSlot {
        self.inner.frames()
    }
    fn paint_seq(&self) -> u64 {
        // A view draws this engine from its pixels unless it draws the display list: the
        // list a share captures beside them is nothing the view has to repaint for.
        if !self.draws {
            return 0;
        }
        self.lock().source.frame().map_or(0, |frame| frame.seq)
    }
    fn paint_due(&self) -> Option<Instant> {
        let mut state = self.lock();
        if self.draws || !Self::watched(&mut state) {
            return None;
        }
        state
            .source
            .due(crate::time::now())
            .into_iter()
            .chain(state.images_due)
            .min()
    }
    fn drain_events(&self) -> Vec<WebViewEvent> {
        let events = self.inner.drain_events();
        let mut state = self.lock();
        let mut passed = Vec::with_capacity(events.len());
        for event in events {
            let (update, consumed) = state.source.on_event(&event);
            if let Some(update) = update {
                Self::absorb(&mut state, update);
            }
            if !consumed {
                passed.push(event);
            }
        }
        self.pump(&mut state);
        self.update_visibility(&mut state);
        drop(state);
        passed
    }
    fn request_close(&self) -> bool {
        self.inner.request_close()
    }
    fn drain_download_events(&self) -> Vec<WebViewEvent> {
        self.inner.drain_download_events()
    }
    fn set_waker(&self, waker: Box<dyn Fn() + Send + Sync>) {
        self.inner.set_waker(waker);
    }
    fn wrapped(&self) -> Option<&SharedEngine> {
        Some(&self.inner)
    }
    fn paint_frame(&self) -> Option<Arc<Frame>> {
        let mut state = self.lock();
        self.pump(&mut state);
        if !self.draws {
            return None;
        }
        state.source.frame()
    }
    fn take_paint_stream(&self, keyframe: bool) -> Vec<Payload> {
        self.take_paint_stream_within(keyframe, usize::MAX)
    }
    fn take_paint_stream_within(&self, keyframe: bool, room: usize) -> Vec<Payload> {
        let mut state = self.lock();
        self.update_visibility(&mut state);
        self.pump(&mut state);
        if keyframe {
            state.keyframe_needed = true;
        }
        let mut out = Vec::new();
        match Self::take_stream_locked(&mut state) {
            Err(Some(frame)) => {
                state.sent_blobs.clear();
                out.push(Payload::Key((*frame).clone()));
            }
            Err(None) => return out,
            Ok(patches) if patches.is_empty() => {}
            Ok(patches) => out.push(Payload::Patches(patches)),
        }
        let Some(frame) = state.source.frame() else {
            return out;
        };
        let mut taken = 0_usize;
        state.images_due = None;
        for hash in frame.blobs().filter_map(|b| match b {
            BlobRef::Hash(h) => Some(*h),
            BlobRef::Url(_) => None,
        }) {
            if state.sent_blobs.contains(&hash) {
                continue;
            }
            let Some(bytes) = state.source.blob(&hash) else {
                continue;
            };
            if bytes.len() > MAX_SHARE_BLOB {
                // Said once: the page goes without this image.
                state.sent_blobs.insert(hash);
                tracing::warn!(
                    bytes = bytes.len(),
                    largest = MAX_SHARE_BLOB,
                    "an image of the shared page is too large to hand over: the page is shared without it"
                );
                continue;
            }
            if taken.saturating_add(bytes.len()) > room {
                // The rest waits with the engine until the app has sent what it holds.
                state.images_due = crate::time::now().checked_add(SHARE_IMAGES_RETRY);
                break;
            }
            taken = taken.saturating_add(bytes.len());
            state.sent_blobs.insert(hash);
            out.push(Payload::Blob {
                hash,
                bytes: bytes.to_vec(),
            });
        }
        out
    }
    fn paint_blob(&self, hash: &[u8; 32]) -> Option<Arc<Vec<u8>>> {
        self.lock().source.blob(hash)
    }
}

fn color(c: PaintColor, opacity: f32) -> Color {
    let [r, g, b, a] = c.0;
    Color::from_rgba8(r, g, b, a).multiply_alpha(opacity)
}

fn rect(r: tilo_paint_stream::Rect) -> Rect {
    Rect::new(
        f64::from(r.x),
        f64::from(r.y),
        f64::from(r.x + r.w),
        f64::from(r.y + r.h),
    )
}

fn weight(w: u16) -> TextWeight {
    match w {
        0..=449 => TextWeight::Regular,
        450..=549 => TextWeight::Medium,
        550..=649 => TextWeight::Semibold,
        _ => TextWeight::Bold,
    }
}

/// Draws [`Frame`]s.
#[derive(Default)]
pub struct PageRenderer {
    /// The families this device has, as it names them.
    installed: Option<Vec<String>>,
    families: HashMap<String, FontChoice>,
    images: HashMap<[u8; 32], Option<ImageData>>,
    paths: HashMap<String, Option<BezPath>>,
}

/// Where a renderer gets a blob's encoded bytes.
pub trait BlobSource {
    fn blob(&self, hash: &[u8; 32]) -> Option<Arc<Vec<u8>>>;
}

impl<F: Fn(&[u8; 32]) -> Option<Arc<Vec<u8>>>> BlobSource for F {
    fn blob(&self, hash: &[u8; 32]) -> Option<Arc<Vec<u8>>> {
        self(hash)
    }
}

/// Everything a draw needs besides the frame.
pub struct PageCanvas<'a> {
    pub scene: &'a mut Scene,
    /// Maps CSS pixels of the page's viewport to the scene.
    pub transform: Affine,
    pub text: &'a mut TextRenderer,
    pub blobs: &'a dyn BlobSource,
}

impl PageRenderer {
    /// A renderer for a page about to be shown.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Draw `frame`, clipped to its viewport.
    pub fn draw(&mut self, canvas: &mut PageCanvas<'_>, frame: &Frame) {
        let viewport = Rect::new(
            0.0,
            0.0,
            f64::from(frame.viewport.x),
            f64::from(frame.viewport.y),
        );
        canvas.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            canvas.transform,
            &viewport,
        );
        canvas.scene.fill(
            Fill::NonZero,
            canvas.transform,
            color(frame.background, 1.0),
            None,
            &viewport,
        );
        self.draw_layer(canvas, frame, ROOT, Point::default(), 0);
        canvas.scene.pop_layer();
    }

    fn draw_layer(
        &mut self,
        canvas: &mut PageCanvas<'_>,
        frame: &Frame,
        id: LayerId,
        parent_content: Point,
        depth: usize,
    ) {
        if depth > frame.layers.len() {
            return;
        }
        let Some(layer) = frame.layers.get(&id) else {
            return;
        };
        let base = match (id, layer.anchor) {
            (ROOT, _) | (_, tilo_paint_stream::Anchor::Viewport) => Point::default(),
            (_, tilo_paint_stream::Anchor::Content) => parent_content,
        };
        let origin = Point {
            x: base.x + layer.origin.x,
            y: base.y + layer.origin.y,
        };
        let content = Point {
            x: origin.x - layer.scroll.x,
            y: origin.y - layer.scroll.y,
        };
        let layered = layer.clips || layer.opacity < 1.0;
        if layered {
            let clip = if layer.clips {
                Rect::new(
                    f64::from(origin.x),
                    f64::from(origin.y),
                    f64::from(origin.x + layer.size.x),
                    f64::from(origin.y + layer.size.y),
                )
            } else {
                Rect::new(-1e7, -1e7, 1e7, 1e7)
            };
            canvas.scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                layer.opacity.clamp(0.0, 1.0),
                canvas.transform,
                &clip,
            );
        }
        let at = canvas.transform * Affine::translate((f64::from(content.x), f64::from(content.y)));
        for item in &layer.items {
            match item {
                Item::Layer(child) => {
                    self.draw_layer(canvas, frame, *child, content, depth.saturating_add(1));
                }
                other => self.draw_item(canvas, at, other),
            }
        }
        if layered {
            canvas.scene.pop_layer();
        }
    }

    fn draw_item(&mut self, canvas: &mut PageCanvas<'_>, at: Affine, item: &Item) {
        match item {
            Item::Fill {
                rect: r,
                color: c,
                radius,
                ..
            } => {
                let shape = RoundedRect::from_rect(rect(*r), f64::from(*radius));
                canvas
                    .scene
                    .fill(Fill::NonZero, at, color(*c, 1.0), None, &shape);
            }
            Item::Border {
                rect: r,
                widths,
                colors,
                radius,
                ..
            } => draw_border(canvas.scene, at, rect(*r), widths, colors, *radius),
            Item::Text {
                key,
                rect: r,
                text,
                font,
                color: c,
            } => self.draw_text(canvas, at, (*r, key.part), text, font, *c),
            Item::Image { rect: r, blob, .. } => {
                if let BlobRef::Hash(hash) = blob
                    && let Some(image) = self.image(canvas.blobs, hash)
                {
                    let target = rect(*r);
                    let sx = target.width() / f64::from(image.width.max(1));
                    let sy = target.height() / f64::from(image.height.max(1));
                    let brush_at = Affine::translate((target.x0, target.y0))
                        * Affine::scale_non_uniform(sx, sy);
                    canvas.scene.fill(
                        Fill::NonZero,
                        at,
                        &ImageBrush::new(image),
                        Some(brush_at),
                        &target,
                    );
                }
            }
            Item::Vector {
                rect: r,
                view_box,
                shapes,
                ..
            } => self.draw_vector(canvas.scene, at, *r, *view_box, shapes),
            Item::Layer(_) => {}
        }
    }

    fn draw_vector(
        &mut self,
        scene: &mut Scene,
        at: Affine,
        r: tilo_paint_stream::Rect,
        view_box: tilo_paint_stream::Rect,
        shapes: &[tilo_paint_stream::Shape],
    ) {
        if view_box.w <= 0.0 || view_box.h <= 0.0 {
            return;
        }
        let scale = f64::from((r.w / view_box.w).min(r.h / view_box.h));
        let dx = f64::from(r.x) + (f64::from(r.w) - f64::from(view_box.w) * scale) / 2.0;
        let dy = f64::from(r.y) + (f64::from(r.h) - f64::from(view_box.h) * scale) / 2.0;
        let place = at
            * Affine::translate((dx, dy))
            * Affine::scale(scale)
            * Affine::translate((-f64::from(view_box.x), -f64::from(view_box.y)));
        for shape in shapes {
            let Some(path) = self
                .paths
                .entry(shape.path.clone())
                .or_insert_with(|| BezPath::from_svg(&shape.path).ok())
                .as_ref()
            else {
                continue;
            };
            if let Some(fill) = shape.fill {
                let rule = if shape.even_odd {
                    Fill::EvenOdd
                } else {
                    Fill::NonZero
                };
                scene.fill(rule, place, color(fill, 1.0), None, path);
            }
            if let Some(stroke) = shape.stroke
                && shape.stroke_width > 0.0
            {
                let style = Stroke::new(f64::from(shape.stroke_width));
                scene.stroke(&style, place, color(stroke, 1.0), None, path);
            }
        }
    }

    /// Draw one run of text in its box. A line the engine laid out is fitted to its box's
    /// width, so it starts and ends where the engine's line does whatever face this device
    /// sets it in: drawn narrower rather than run into what follows it, and a little wider
    /// rather than end short. What a field holds ([`ItemKey::SLOT`]) has the field's room
    /// for a box: it is drawn at its own width, from the side its field aligns it to, and
    /// only ever narrower.
    fn draw_text(
        &mut self,
        canvas: &mut PageCanvas<'_>,
        at: Affine,
        (r, part): (tilo_paint_stream::Rect, u32),
        text: &str,
        font: &tilo_paint_stream::FontSpec,
        c: PaintColor,
    ) {
        let family = self.family(canvas.text, &font.family);
        let style = TextStyle::new(font.size)
            .with_weight(weight(font.weight))
            .with_italic(font.italic)
            .with_family(family);
        let (w, h) = canvas.text.measure_style(text, None, &style);
        let slot = part & ItemKey::SLOT != 0;
        let sx = if w <= 0.0 || r.w <= 0.0 {
            1.0
        } else if slot {
            (r.w / w).clamp(0.5, 1.0)
        } else {
            (r.w / w).clamp(0.5, 1.25)
        };
        let spare = sx.mul_add(-w, r.w);
        let dx = if !slot {
            0.0
        } else if part & ItemKey::SLOT_END != 0 {
            spare
        } else if part & ItemKey::SLOT_CENTRE != 0 {
            spare / 2.0
        } else {
            0.0
        };
        let dy = f64::from((r.h - h) / 2.0);
        let place = at
            * Affine::translate((f64::from(r.x + dx), f64::from(r.y) + dy))
            * Affine::scale_non_uniform(f64::from(sx), 1.0);
        canvas
            .text
            .draw_style(canvas.scene, text, color(c, 1.0), place, None, &style);
    }

    /// The face this device sets a page's family list in: the first family of the list it
    /// has, or a face it has that is drawn with the same measures (a page that asks for
    /// Times New Roman is set in Liberation Serif where that is what is installed, which is
    /// what the engine itself does), and otherwise the face of the list's class
    /// ([`font_class`]).
    fn family(&mut self, text: &TextRenderer, css: &str) -> FontChoice {
        if let Some(choice) = self.families.get(css) {
            return choice.clone();
        }
        let installed = self.installed.get_or_insert_with(|| text.system_families());
        let has = |wanted: &str| {
            installed
                .iter()
                .find(|family| family.eq_ignore_ascii_case(wanted))
                .cloned()
        };
        let mut choice = None;
        for name in family_names(css) {
            let lower = name.to_ascii_lowercase();
            if generic_class(&lower).is_some() {
                break;
            }
            let alike = ALIKE
                .iter()
                .find(|group| group.contains(&lower.as_str()))
                .into_iter()
                .flat_map(|group| group.iter().copied());
            if let Some(family) = has(name).or_else(|| alike.into_iter().find_map(has)) {
                choice = Some(FontChoice::Named(family));
                break;
            }
        }
        let choice = choice.unwrap_or_else(|| match font_class(css) {
            FontClass::Serif => FontChoice::Serif,
            FontClass::Sans => FontChoice::SansSerif,
            FontClass::Mono => FontChoice::Monospace,
        });
        self.families.insert(css.to_string(), choice.clone());
        choice
    }

    fn image(&mut self, blobs: &dyn BlobSource, hash: &[u8; 32]) -> Option<ImageData> {
        if let Some(cached) = self.images.get(hash) {
            return cached.clone();
        }
        let bytes = blobs.blob(hash)?;
        let decoded = tilo_ui_base::image_decoders::page(&bytes);
        self.images.insert(*hash, decoded.clone());
        decoded
    }
}

fn draw_border(
    scene: &mut Scene,
    at: Affine,
    b: Rect,
    widths: &[f32; 4],
    colors: &[PaintColor; 4],
    radius: f32,
) {
    let [top, right, bottom, left] = widths.map(f64::from);
    let [first, ..] = *colors;
    let uniform = [right, bottom, left].iter().all(|w| (w - top).abs() < 0.01)
        && colors.iter().all(|c| *c == first);
    if uniform && top > 0.0 {
        let inset = b.inset(-top / 2.0);
        let shape = RoundedRect::from_rect(inset, (f64::from(radius) - top / 2.0).max(0.0));
        scene.stroke(&Stroke::new(top), at, color(first, 1.0), None, &shape);
        return;
    }
    let sides = [
        Rect::new(b.x0, b.y0, b.x1, b.y0 + top),
        Rect::new(b.x1 - right, b.y0, b.x1, b.y1),
        Rect::new(b.x0, b.y1 - bottom, b.x1, b.y1),
        Rect::new(b.x0, b.y0, b.x0 + left, b.y1),
    ];
    for (side, c) in sides.iter().zip(colors) {
        if side.width() > 0.0 && side.height() > 0.0 && c.0[3] > 0 {
            scene.fill(Fill::NonZero, at, color(*c, 1.0), None, side);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_webview::test_engine::TestEngine;

    const TOP: &str =
        include_str!("../../../../engines/paint-stream/tests/fixtures/layers.top.json");
    const SCROLLED: &str =
        include_str!("../../../../engines/paint-stream/tests/fixtures/layers.scrolled.json");
    const TOP_PNG: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/layers.top.png"));

    fn paint_engine(snapshot: &str) -> (Arc<TestEngine>, Arc<PaintEngine>) {
        let test = TestEngine::new();
        test.set_paint_snapshot(snapshot);
        let engine = PaintEngine::with_interval(test.clone(), Duration::ZERO);
        engine.send(WebViewCommand::Resize {
            width: 800,
            height: 600,
            scale_factor: 1.0,
        });
        (test, engine)
    }

    fn settle(engine: &PaintEngine) -> Vec<WebViewEvent> {
        let mut passed = Vec::new();
        for _ in 0..4 {
            drop(engine.paint_frame());
            passed.extend(engine.drain_events());
        }
        passed
    }

    tilo_test_env::scenario! {
        fn a_page_is_published_from_the_snapshot_its_engine_gives;

        phase a_snapshot_becomes_a_frame_and_the_source_keeps_its_own_traffic;
        {
            let (test, engine) = paint_engine(TOP);
            let passed = settle(&engine);
            let frame = engine
                .paint_frame()
                .expect("a frame from the engine's snapshot");
            assert!(
                frame.layers.len() >= 5,
                "root plus header, sidebar, ghost, button"
            );
            assert!(
                passed.iter().all(|e| !matches!(
                    e,
                    WebViewEvent::PaintSnapshot { .. } | WebViewEvent::PaintResource { .. }
                )),
                "paint traffic never reaches the app: {passed:?}"
            );
            assert!(
                test.commands()
                    .iter()
                    .any(|c| matches!(c, WebViewCommand::PaintCapture { .. })),
                "the wrapper asked the engine for its layout"
            );
            let hash = frame
                .blobs()
                .find_map(|b| match b {
                    BlobRef::Hash(h) => Some(*h),
                    BlobRef::Url(_) => None,
                })
                .expect("image resolved");
            let bytes = engine.paint_blob(&hash).expect("blob kept");
            assert_eq!(*blake3::hash(&bytes).as_bytes(), hash);
            assert!(
                frame.blobs().all(|b| matches!(b, BlobRef::Hash(_))),
                "no URL leaves the host"
            );
        }

        phase svg_images_reach_clients_as_vectors;
        {
            let (_, engine) = paint_engine(TOP);
            settle(&engine);
            let frame = engine.paint_frame().expect("frame");
            let fills: Vec<[u8; 4]> = frame
                .layers
                .values()
                .flat_map(|l| &l.items)
                .filter_map(|i| match i {
                    Item::Vector { shapes, .. } => Some(shapes),
                    _ => None,
                })
                .flatten()
                .filter_map(|s| s.fill.map(|c| c.0))
                .collect();
            assert!(
                fills.contains(&[30, 90, 200, 255]),
                "the SVG <img> as a vector: {fills:?}"
            );
        }

        phase a_consumer_starts_from_a_whole_frame_then_follows_patches;
        {
            let (test, engine) = paint_engine(TOP);
            settle(&engine);
            let Err(Some(keyframe)) = engine.take_stream() else {
                panic!("a new consumer starts from a whole frame");
            };
            let mut client = (*keyframe).clone();
            assert_eq!(
                engine.take_stream().ok().map(|p| p.len()),
                Some(0),
                "nothing changed yet"
            );

            test.set_paint_snapshot(SCROLLED);
            test.send(WebViewCommand::Navigate("https://example.test/".into()));
            settle(&engine);
            let patches = engine.take_stream().expect("following");
            assert_eq!(patches.len(), 1, "{patches:?}");
            for patch in &patches {
                let wire: Patch =
                    tilo_paint_stream::decode(&tilo_paint_stream::encode(patch).expect("encode"))
                        .expect("decode");
                client.apply(&wire).expect("apply");
            }
            assert_eq!(&client, &*engine.paint_frame().expect("frame"));
        }

        phase an_engine_without_layout_capture_falls_back_to_pixels;
        {
            let test = TestEngine::new();
            let engine = PaintEngine::with_interval(test, Duration::ZERO);
            settle(&engine);
            assert!(engine.paint_frame().is_none());
        }
    }

    #[test]
    #[serial_test::serial(paint_share)]
    fn a_streaming_engine_captures_only_while_a_session_watches() {
        let test = TestEngine::new();
        test.set_paint_snapshot(TOP);
        let engine = PaintEngine::streaming(test.clone());
        engine.send(WebViewCommand::Resize {
            width: 800,
            height: 600,
            scale_factor: 1.0,
        });
        set_share_watched(false);
        settle(&engine);
        assert!(
            engine.take_paint_stream(true).is_empty(),
            "nobody watches: nothing captured"
        );
        assert!(
            !test
                .commands()
                .iter()
                .any(|c| matches!(c, WebViewCommand::PaintCapture { .. })),
            "an unwatched page costs the engine nothing"
        );
        assert!(share_capable(), "the app can tell the session it streams");

        set_share_watched(true);
        assert!(
            take_share_keyframe(),
            "a new watcher starts from a whole frame"
        );
        settle(&engine);
        let payloads = engine.take_paint_stream(true);
        assert!(
            matches!(payloads.first(), Some(Payload::Key(_))),
            "{} payloads",
            payloads.len()
        );
        assert!(
            payloads.iter().any(|p| matches!(p, Payload::Blob { .. })),
            "with the images the page shows"
        );
        assert!(
            engine.paint_frame().is_none(),
            "the view itself still draws pixels"
        );
        assert!(
            engine.take_paint_stream(false).is_empty(),
            "nothing changed since"
        );
        set_share_watched(false);
    }

    #[test]
    #[serial_test::serial(paint_share)]
    fn a_page_no_view_draws_stops_painting_until_one_does() {
        use crate::time::testing::with_monotonic_time;
        let test = TestEngine::new();
        let engine = PaintEngine::streaming(test.clone());
        engine.lock().hide_after = Duration::from_millis(30);
        set_share_watched(false);
        let start = crate::time::now();
        let at = |ms| start + Duration::from_millis(ms);
        with_monotonic_time(at(0), || {
            engine.presented();
            drop(engine.drain_events());
        });
        assert!(
            test.visibility_changes().is_empty(),
            "drawn just now: still painting"
        );

        with_monotonic_time(at(60), || drop(engine.drain_events()));
        assert!(
            test.visibility_changes().is_empty(),
            "a still page on screen keeps painting"
        );

        with_monotonic_time(at(60), || {
            engine.send(WebViewCommand::Navigate("https://example.test/next".into()));
            drop(engine.drain_events());
        });
        with_monotonic_time(at(120), || drop(engine.drain_events()));
        assert_eq!(
            test.visibility_changes(),
            vec![false],
            "undrawn: told it is hidden"
        );
        with_monotonic_time(at(120), || drop(engine.drain_events()));
        assert_eq!(test.visibility_changes(), vec![false], "and told once");

        with_monotonic_time(at(120), || engine.presented());
        assert_eq!(
            test.visibility_changes(),
            vec![false, true],
            "drawn again: painting"
        );

        with_monotonic_time(at(120), || {
            engine.set_visible(false);
            engine.presented();
        });
        assert_eq!(test.visibility_changes(), vec![false, true, false]);
        with_monotonic_time(at(120), || engine.set_visible(true));

        set_share_watched(true);
        with_monotonic_time(at(180), || drop(engine.take_paint_stream(false)));
        assert_eq!(test.visibility_changes(), vec![false, true, false, true]);
        set_share_watched(false);
    }

    /// How many captures of its page `test` was asked for.
    fn captures(test: &TestEngine) -> usize {
        test.commands()
            .iter()
            .filter(|command| matches!(command, WebViewCommand::PaintCapture { .. }))
            .count()
    }

    /// An engine drawn from its pixels, as a Browser window runs it, on the page `snapshot`.
    fn streaming_engine(snapshot: &str) -> (Arc<TestEngine>, Arc<PaintEngine>) {
        let test = TestEngine::new();
        test.set_paint_snapshot(snapshot);
        let engine = PaintEngine::streaming(test.clone());
        engine.send(WebViewCommand::Resize {
            width: 800,
            height: 600,
            scale_factor: 1.0,
        });
        (test, engine)
    }

    /// A process shows several windows, each with its page and its share. What the session
    /// asks of one window, that window alone answers: another window of the process is not
    /// watched, captures nothing, and holds nothing of the first window's page; told that
    /// nobody watches it, it stops nothing of the first. An engine belongs to the window
    /// whose events run it, also when its own thread calls it.
    #[test]
    #[serial_test::serial(paint_share)]
    fn each_window_of_a_process_shares_its_own_page_and_no_other() {
        use crate::hosted_window::HostedWindow;
        use crate::time::testing::with_monotonic_time;
        let (one, other) = (HostedWindow::new(), HostedWindow::new());
        let (first_test, first) = streaming_engine(TOP);
        let (second_test, second) = streaming_engine(SCROLLED);
        set_share_watched(false);
        let start = crate::time::now();
        let at = |ms| start + Duration::from_millis(ms);

        // The session asks the first window for its page.
        with_monotonic_time(at(0), || {
            let _events = crate::window_toolbar::enter(Some(&one));
            set_share_watched(true);
            assert!(share_watched());
            assert!(take_share_keyframe(), "it starts from a whole frame");
            drop(settle(&first));
            let payloads = first.take_paint_stream(true);
            assert!(
                matches!(payloads.first(), Some(Payload::Key(_))),
                "{} payloads",
                payloads.len()
            );
            for payload in payloads {
                queue_share(SharedFrame::whole(
                    payload.encode().expect("a payload"),
                    [0.0; 4],
                    1.0,
                ));
            }
        });

        // The second window is asked for nothing.
        with_monotonic_time(at(0), || {
            let _events = crate::window_toolbar::enter(Some(&other));
            assert!(!share_watched(), "nobody watches this window");
            assert!(!take_share_keyframe());
            drop(settle(&second));
            assert!(second.take_paint_stream(true).is_empty());
            assert_eq!(captures(&second_test), 0, "its page is not captured");
            queue_share(SharedFrame::whole(vec![0, 1, 2], [0.0; 4], 1.0));
            assert!(
                take_share_frames().is_empty(),
                "it holds nothing: not its own page, and not the first window's"
            );
            // The session tells it nobody watches it, as it tells every window it stops.
            set_share_watched(false);
        });
        assert!(
            !share_watched(),
            "a process that runs one window with no host around it was never asked"
        );

        // Both pages paint again a moment later, and the engines' own threads take that:
        // outside any window's events each is still its own window's.
        with_monotonic_time(at(10), || {
            first_test.paint_before_page();
            second_test.paint_before_page();
            drop(first.drain_events());
            drop(second.drain_events());
            assert_eq!(
                first.paint_due(),
                Some(at(50)),
                "the watched window's page is captured again once its interval passed"
            );
            assert_eq!(second.paint_due(), None, "the other window's never");
        });
        assert_eq!(captures(&second_test), 0);

        // The first window's share went on through the second window's stop.
        let _events = crate::window_toolbar::enter(Some(&one));
        assert!(share_watched(), "another window's stop stops nothing here");
        let frames = take_share_frames();
        assert!(
            frames
                .first()
                .is_some_and(|frame| Payload::is_key(&frame.payload) && frame.complete()),
            "the first window holds its own page: {} frames",
            frames.len()
        );
        set_share_watched(false);
    }

    /// A page nothing changes has no work while it is watched, and the view that draws it
    /// from pixels repaints for nothing a share captured. A change that comes sooner after a
    /// capture than the capture interval is not lost: the engine wakes nobody for it, so it
    /// says when it is due, and whoever takes its events then gets it captured.
    #[test]
    #[serial_test::serial(paint_share)]
    fn a_watched_page_says_when_it_has_work_and_a_still_one_has_none() {
        use crate::time::testing::with_monotonic_time;
        let (test, engine) = streaming_engine(TOP);
        set_share_watched(false);
        let start = crate::time::now();
        let at = |ms| start + Duration::from_millis(ms);
        with_monotonic_time(at(0), || {
            assert_eq!(engine.paint_due(), None, "nobody watches: nothing is due");
            set_share_watched(true);
            assert!(
                engine.paint_due().is_some_and(|due| due <= at(0)),
                "a watched page that was never captured is due now"
            );
            drop(settle(&engine));
        });
        // Whatever the page still had to settle is long done.
        with_monotonic_time(at(1000), || {
            test.paint_before_page();
            drop(settle(&engine));
            assert!(matches!(
                engine.take_paint_stream(true).first(),
                Some(Payload::Key(_))
            ));
            assert_eq!(engine.paint_due(), None, "a still page has no work");
            assert_eq!(
                engine.paint_seq(),
                0,
                "and a view that draws its pixels repaints for nothing the share captured"
            );
        });
        let captured = captures(&test);
        // The page paints again ten milliseconds after its last capture.
        with_monotonic_time(at(1010), || {
            test.set_paint_snapshot(SCROLLED);
            test.paint_before_page();
            drop(engine.drain_events());
            assert_eq!(captures(&test), captured, "too soon after the last capture");
            assert_eq!(
                engine.paint_due(),
                Some(at(1050)),
                "it is due when the capture interval has passed"
            );
        });
        with_monotonic_time(at(1050), || {
            drop(engine.drain_events());
            assert_eq!(captures(&test), captured + 1, "captured when due");
            assert_eq!(
                engine.paint_due(),
                Some(at(1050) + REQUEST_TIMEOUT),
                "and looked at again should the answer not come"
            );
            drop(settle(&engine));
            assert_eq!(engine.paint_due(), None, "the answer came: still again");
            assert!(
                matches!(
                    engine.take_paint_stream(false).first(),
                    Some(Payload::Patches(_))
                ),
                "the change is in the stream"
            );
        });
        set_share_watched(false);
    }

    /// A share is handed the images of a page as it has room for them: the rest stay with
    /// the engine, which says when to come back, and none is handed over twice or lost.
    #[test]
    #[serial_test::serial(paint_share)]
    fn images_a_share_has_no_room_for_wait_with_the_engine() {
        let (_test, engine) = streaming_engine(TOP);
        set_share_watched(true);
        drop(settle(&engine));
        let all = engine.take_paint_stream(true);
        let images = all
            .iter()
            .filter(|payload| matches!(payload, Payload::Blob { .. }))
            .count();
        assert!(images >= 1, "the page shows an image");
        assert_eq!(engine.paint_due(), None, "everything was handed over");

        // The same page again, to a share with no room for images.
        let whole = engine.take_paint_stream_within(true, 0);
        assert!(matches!(whole.first(), Some(Payload::Key(_))));
        assert_eq!(whole.len(), 1, "the whole frame comes, and no image");
        assert!(
            engine.paint_due().is_some(),
            "the engine says when to come back for the images"
        );
        let rest = engine.take_paint_stream_within(false, usize::MAX);
        assert_eq!(
            rest.iter()
                .filter(|payload| matches!(payload, Payload::Blob { .. }))
                .count(),
            images,
            "they come when there is room"
        );
        assert_eq!(engine.paint_due(), None);
        assert!(
            engine
                .take_paint_stream_within(false, usize::MAX)
                .is_empty()
        );
        set_share_watched(false);
    }

    tilo_test_env::scenario! {
        fn a_pages_text_is_set_in_its_face_and_at_its_own_width;

        phase a_family_list_is_set_in_the_face_or_at_least_the_class_its_page_asked_for;
        // A page that names no face is set by its engine in the engine's own standard one, a
        // serif, and its fields in the engine's sans: a device that has neither face keeps the
        // class of each. A face the device has is the face it sets, and one drawn with the same
        // measures stands in for the one a page named.
        {
            let text = TextRenderer::new();
            let mut renderer = PageRenderer::new();
            assert_eq!(
                renderer.family(&text, "\"Times New Roman\""),
                FontChoice::Serif,
                "the engine's standard face, where the device has none like it"
            );
            assert_eq!(renderer.family(&text, "Arial"), FontChoice::SansSerif);
            assert_eq!(
                renderer.family(&text, "\"Courier New\", monospace"),
                FontChoice::Monospace
            );
            let has = tilo_ui_base::text_render::UI_MONOSPACE_FAMILY;
            assert_eq!(
                renderer.family(&text, &format!("Harbour, \"{has}\", serif")),
                FontChoice::Named(has.to_owned()),
                "the first face of the list the device has"
            );

            // A device that has the faces the engine itself sets those pages in.
            let mut renderer = PageRenderer {
                installed: Some(vec!["Liberation Serif".into(), "Liberation Sans".into()]),
                ..PageRenderer::default()
            };
            assert_eq!(
                renderer.family(&text, "\"Times New Roman\""),
                FontChoice::Named("Liberation Serif".into())
            );
            assert_eq!(
                renderer.family(&text, "Helvetica, sans-serif"),
                FontChoice::Named("Liberation Sans".into())
            );
            assert_eq!(
                renderer.family(&text, "sans-serif, \"Times New Roman\""),
                FontChoice::SansSerif,
                "a generic family ends the list: nothing after it is looked at"
            );
        }

        phase what_a_field_holds_is_drawn_at_its_own_width_in_the_fields_room;
        // What a field holds is drawn at its own width in the room the field has, from the
        // side the field aligns it to: it is not stretched to the field as a line the engine
        // laid out is fitted to its box.
        {
            let room = tilo_paint_stream::Rect::new(20.0, 20.0, 360.0, 30.0);
            let style = TextStyle::new(20.0);
            let (natural, _) = TextRenderer::new().measure_style("Brazil", None, &style);
            let (left, right) = inked(ItemKey::SLOT, room, "Brazil");
            // Ink is a little narrower than the words' advance: the first and the last letter
            // stand in from their edges.
            let drawn = (right - left) as f32;
            assert!(
                drawn <= natural + 1.0 && drawn >= natural * 0.85,
                "{drawn} wide where the words are {natural} wide"
            );
            assert!((20..=24).contains(&left), "from the field's start: {left}");

            let (fitted_left, fitted_right) = inked(3, room, "Brazil");
            assert!(
                (fitted_right - fitted_left) as f32 > drawn * 1.15,
                "a line box of the engine's is fitted to: {fitted_left}..{fitted_right}"
            );

            let (_, end) = inked(ItemKey::SLOT | ItemKey::SLOT_END, room, "Brazil");
            assert!((372..=380).contains(&end), "to the field's end: {end}");
            let (from, to) = inked(ItemKey::SLOT | ItemKey::SLOT_CENTRE, room, "Brazil");
            assert!(
                (from + to).abs_diff(400) <= 6,
                "centred in the field: {from}..{to}"
            );
        }
    }

    /// The engine suite's form with every common control, as its engine laid it out.
    const CONTROLS: &str =
        include_str!("../../../../engines/paint-stream/tests/fixtures/controls.json");

    /// The engine's number for the element of the form whose `id` is `id`.
    fn control(id: &str) -> u64 {
        let snapshot: serde_json::Value = serde_json::from_str(CONTROLS).expect("the fixture");
        let string = |index: &serde_json::Value| {
            index
                .as_u64()
                .and_then(|at| snapshot["strings"].get(usize::try_from(at).ok()?))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        let nodes = &snapshot["documents"][0]["nodes"];
        let found = nodes["attributes"]
            .as_array()
            .expect("attributes")
            .iter()
            .position(|held| {
                held.as_array().is_some_and(|held| {
                    held.chunks_exact(2)
                        .any(|pair| string(&pair[0]) == "id" && string(&pair[1]) == id)
                })
            })
            .expect("an element of that id");
        nodes["backendNodeId"][found].as_u64().expect("a node")
    }

    /// Every run of text a frame draws, with the family list it is sent in.
    fn runs(frame: &Frame) -> Vec<(String, String)> {
        frame
            .layers
            .values()
            .flat_map(|layer| &layer.items)
            .filter_map(|item| match item {
                Item::Text { text, font, .. } => {
                    Some((text.trim().to_owned(), font.family.clone()))
                }
                _ => None,
            })
            .collect()
    }

    /// The asks a test engine was sent for what a snapshot does not say.
    fn asks(test: &TestEngine) -> Vec<(Vec<u64>, Vec<String>)> {
        test.commands()
            .into_iter()
            .filter_map(|command| match command {
                WebViewCommand::PaintShown {
                    nodes, families, ..
                } => Some((nodes, families)),
                _ => None,
            })
            .collect()
    }

    fn controls_engine() -> (Arc<TestEngine>, Arc<PaintEngine>) {
        let test = TestEngine::new();
        test.set_paint_snapshot(CONTROLS);
        let engine = PaintEngine::with_interval(test.clone(), Duration::ZERO);
        engine.send(WebViewCommand::Resize {
            width: 1280,
            height: 800,
            scale_factor: 1.0,
        });
        (test, engine)
    }

    tilo_test_env::scenario! {
        fn the_engine_says_once_what_a_capture_shows_and_a_capture_is_never_asked_for_twice;

        phase the_engine_is_asked_what_a_date_shows_and_what_class_of_face_it_sets;
        // A date shows what the engine words it as, not what its field holds, and a page's
        // text is set in the class of face the engine set it in on its machine. Only the
        // engine can say either: it is asked once, the page is not published in one face to
        // be published again in another, and what it did not say stays as the page gives it.
        {
            let (test, engine) = controls_engine();
            test.set_paint_shown([
                (control("date"), "03/01/2027".to_owned()),
                (control("time"), "02:30 PM".to_owned()),
            ]);
            // A machine without a Times sets the page's text in its sans.
            test.set_paint_classes([
                ("\"Times New Roman\"", "sans-serif"),
                ("Arial", "sans-serif"),
            ]);
            drop(engine.paint_frame());
            drop(engine.drain_events());
            assert!(
                engine.paint_frame().is_none(),
                "a page waits for the engine to say what face its text is set in"
            );
            settle(&engine);
            let frame = engine
                .paint_frame()
                .expect("the page, once the engine said");
            let drawn = runs(&frame);
            let family = |text: &str| {
                drawn
                    .iter()
                    .find(|(said, _)| said == text)
                    .map(|(_, family)| family.as_str())
            };
            assert_eq!(family("03/01/2027"), Some("monospace"), "{drawn:?}");
            assert_eq!(
                family("2027-03-01"),
                None,
                "what the field holds is not shown"
            );
            assert!(family("02:30 PM").is_some());
            assert_eq!(
                family("1234.5"),
                Some("Arial"),
                "what the engine said nothing of is drawn as its field holds it"
            );
            assert_eq!(family("Every control"), Some("sans-serif"));
            assert_eq!(family("Name"), Some("sans-serif"));

            let asked = asks(&test);
            assert_eq!(asked.len(), 1, "asked once: {asked:?}");
            let (nodes, families) = &asked[0];
            assert!(nodes.contains(&control("date")) && nodes.contains(&control("number")));
            assert!(
                !nodes.contains(&control("password")) && !nodes.contains(&control("card")),
                "the engine is never asked what a secret field shows"
            );
            assert_eq!(families, &["\"Times New Roman\"", "Arial", "monospace"]);
            settle(&engine);
            assert_eq!(asks(&test).len(), 1, "and not again while the page stays");
            assert!(
                runs(&frame)
                    .iter()
                    .all(|(text, _)| { !text.contains("hunter2") && !text.contains("4111") }),
                "no secret is in the page"
            );
        }

        phase an_engine_that_predates_the_ask_has_its_page_drawn_as_it_gives_it;
        // An engine that predates the ask reads it as a capture and answers with a snapshot:
        // the page is published from that, drawn as it always was, and the engine is not
        // asked again.
        {
            let (test, engine) = controls_engine();
            settle(&engine);
            let frame = engine.paint_frame().expect("the page");
            let drawn = runs(&frame);
            assert!(
                drawn
                    .iter()
                    .any(|(text, family)| text == "2027-03-01" && family == "monospace"),
                "{drawn:?}"
            );
            assert!(
                drawn
                    .iter()
                    .any(|(text, family)| text == "Every control" && family == "\"Times New Roman\""),
                "the list its page computed"
            );
            assert_eq!(asks(&test).len(), 1);
            test.paint_before_page();
            settle(&engine);
            assert_eq!(
                asks(&test).len(),
                1,
                "an engine that cannot say is asked once"
            );
        }

        phase a_first_publication_takes_one_capture_and_arrives_in_one_class_of_face;
        // A page's first publication costs its engine one capture: what the engine says of the
        // capture's fields and faces is put into the snapshot it gave, which is not asked for
        // again. Nothing is published before the engine said, so the page arrives once, in the
        // class of face it stays in.
        {
            let (test, engine) = controls_engine();
            test.set_paint_shown([(control("date"), "03/01/2027".to_owned())]);
            test.set_paint_classes([
                ("\"Times New Roman\"", "sans-serif"),
                ("Arial", "sans-serif"),
            ]);
            drop(engine.paint_frame());
            drop(engine.drain_events());
            assert!(
                matches!(engine.take_stream(), Err(None)),
                "nothing is published before the engine said what face the text is set in"
            );
            settle(&engine);
            let Err(Some(first)) = engine.take_stream() else {
                panic!("a consumer starts from the whole page");
            };
            assert_eq!(first.seq, 1, "the first frame published");
            assert_eq!(
                family_of(&first, "Every control").as_deref(),
                Some("sans-serif"),
                "in the class of face the engine said"
            );
            assert!(
                family_of(&first, "03/01/2027").is_some(),
                "with what its date shows"
            );
            assert_eq!(captures(&test), 1, "of one capture");
            assert_eq!(asks(&test).len(), 1, "and one ask beside it");
            settle(&engine);
            assert_eq!(
                engine.take_stream().ok().map(|patches| patches.len()),
                Some(0),
                "and nothing follows it: the page did not change"
            );
            assert_eq!(captures(&test), 1);
        }

        phase the_engine_is_asked_for_no_capture_while_it_is_still_to_say_of_the_last;
        // The engine answers what it is asked of a page before it is asked for the next
        // capture: behind a capture its answer would wait for as long as the capture takes,
        // and the page with it. The change that asked for the capture is captured as soon as
        // the engine has said.
        {
            let (test, engine) = controls_engine();
            test.set_paint_classes([("\"Times New Roman\"", "sans-serif")]);
            drop(engine.paint_frame());
            test.hold_paint();
            drop(engine.drain_events());
            assert_eq!(
                (captures(&test), asks(&test).len()),
                (1, 1),
                "the page was captured and the engine asked what face it is set in"
            );
            // The page paints anew before the engine has said.
            test.paint_before_page();
            drop(settle(&engine));
            assert_eq!(
                captures(&test),
                1,
                "the capture of the change waits for the engine to say"
            );
            test.release_paint();
            drop(engine.drain_events());
            let first = engine
                .paint_frame()
                .expect("the page, once the engine said");
            assert_eq!(first.seq, 1, "of the first capture");
            assert_eq!(
                family_of(&first, "Every control").as_deref(),
                Some("sans-serif")
            );
            drop(settle(&engine));
            assert_eq!(captures(&test), 2, "and the change is captured then");
        }

        phase a_capture_slower_than_it_is_waited_for_is_taken_and_never_asked_for_again;
        // An engine that takes longer over a capture than it is waited for is still at work on
        // it. The capture is never asked for again, however long it takes: the engine is asked
        // something small behind it, ever more seldom, and what it answers the capture with,
        // when it does, is the page.
        {
            use crate::time::testing::with_monotonic_time;
            let (test, engine) = controls_engine();
            test.set_paint_classes([("\"Times New Roman\"", "sans-serif")]);
            test.hold_paint();
            let start = crate::time::now();
            let at = |seconds| start + Duration::from_secs(seconds);
            for seconds in [0, 1, 4, 10, 30, 60, 200, 201] {
                with_monotonic_time(at(seconds), || {
                    drop(settle(&engine));
                    assert!(
                        engine.paint_frame().is_none(),
                        "the engine has not answered"
                    );
                });
                assert_eq!(
                    captures(&test),
                    1,
                    "after {seconds} s the capture is still the one asked for"
                );
            }
            assert_eq!(
                looks(&test),
                5,
                "asked whether it still has it after 3 s, and each time after twice as long"
            );
            with_monotonic_time(at(202), || {
                test.release_paint();
                drop(settle(&engine));
                let frame = engine
                    .paint_frame()
                    .expect("the page the engine was at work on");
                assert_eq!(
                    family_of(&frame, "Every control").as_deref(),
                    Some("sans-serif")
                );
                assert_eq!(frame.seq, 1, "published once");
            });
            assert_eq!(captures(&test), 1, "of the one capture");
            assert_eq!(
                looks(&test),
                5,
                "and the engine is asked nothing more of it"
            );
        }

        phase a_capture_the_engine_lost_is_asked_for_again;
        // An engine that lost a capture never answers it. It says so by answering what it is
        // asked behind the capture: the capture is asked for again at once, and not before.
        {
            use crate::time::testing::with_monotonic_time;
            let (test, engine) = controls_engine();
            test.set_paint_classes([("\"Times New Roman\"", "sans-serif")]);
            test.lose_paint_captures(1);
            let start = crate::time::now();
            let at = |seconds| start + Duration::from_secs(seconds);
            for seconds in [0, 2] {
                with_monotonic_time(at(seconds), || {
                    drop(settle(&engine));
                    assert!(
                        engine.paint_frame().is_none(),
                        "the engine lost the capture"
                    );
                });
                assert_eq!((captures(&test), looks(&test)), (1, 0), "after {seconds} s");
            }
            with_monotonic_time(at(3), || {
                drop(settle(&engine));
                let frame = engine.paint_frame().expect("the page, captured again");
                assert_eq!(
                    family_of(&frame, "Every control").as_deref(),
                    Some("sans-serif")
                );
            });
            assert_eq!(
                (captures(&test), looks(&test)),
                (2, 1),
                "asked whether it has the capture, and for the capture again when it had not"
            );
            with_monotonic_time(at(120), || drop(settle(&engine)));
            assert_eq!(
                (captures(&test), looks(&test)),
                (2, 1),
                "and for nothing more once the page is there"
            );
        }

        phase an_engine_that_predates_the_ask_answers_for_a_capture_it_lost_with_the_page;
        // An engine that predates the ask reads what it is asked behind a capture as a capture:
        // where it lost the first, what it answers is the page.
        {
            use crate::time::testing::with_monotonic_time;
            let (test, engine) = controls_engine();
            test.lose_paint_captures(1);
            let start = crate::time::now();
            let at = |seconds| start + Duration::from_secs(seconds);
            with_monotonic_time(at(0), || {
                drop(settle(&engine));
                assert!(
                    engine.paint_frame().is_none(),
                    "the engine lost the capture"
                );
            });
            with_monotonic_time(at(3), || {
                drop(settle(&engine));
                let frame = engine.paint_frame().expect("the page");
                assert_eq!(
                    family_of(&frame, "Every control").as_deref(),
                    Some("\"Times New Roman\""),
                    "drawn as its page gives it"
                );
            });
            with_monotonic_time(at(120), || drop(settle(&engine)));
            assert_eq!(captures(&test), 1);
            assert_eq!(
                asks(&test).len(),
                1,
                "an engine that cannot say is asked once"
            );
        }

        phase an_engine_that_never_says_its_classes_has_its_page_published_of_the_same_capture;
        // An engine that never says what face it sets a page's text in does not keep the page:
        // once it is given up, the page is published from the capture it gave, its text in the
        // class of face its lists ask for, and the engine is not asked again.
        {
            use crate::time::testing::with_monotonic_time;
            let (test, engine) = controls_engine();
            test.set_paint_classes([("\"Times New Roman\"", "sans-serif")]);
            test.lose_paint_asks();
            let start = crate::time::now();
            let at = |seconds| start + Duration::from_secs(seconds);
            with_monotonic_time(at(0), || {
                drop(settle(&engine));
                assert!(
                    engine.paint_frame().is_none(),
                    "a page waits for the engine to say what face its text is set in"
                );
            });
            with_monotonic_time(at(4), || {
                drop(settle(&engine));
                let frame = engine.paint_frame().expect("the page, the engine given up");
                assert_eq!(
                    family_of(&frame, "Every control").as_deref(),
                    Some("\"Times New Roman\""),
                    "set in the class its list asks for"
                );
                assert_eq!(frame.seq, 1, "published once");
            });
            with_monotonic_time(at(120), || drop(settle(&engine)));
            assert_eq!(captures(&test), 1, "of the capture the engine gave");
            assert_eq!(asks(&test).len(), 1, "and it is not asked again");
        }

        phase an_ask_the_engine_answers_late_leaves_text_in_the_class_it_said;
        // A page that loads faces of its own is asked again what class its text is set in. An
        // engine that does not answer that in time has not said its text is set differently:
        // the page stays in the class the engine said, and is not captured for it.
        {
            use crate::time::testing::with_monotonic_time;
            let (test, engine) = controls_engine();
            test.set_paint_classes([("\"Times New Roman\"", "sans-serif")]);
            test.set_paint_faces_loading();
            let start = crate::time::now();
            let at = |seconds| start + Duration::from_secs(seconds);
            with_monotonic_time(at(0), || {
                drop(settle(&engine));
                let frame = engine.paint_frame().expect("the page");
                assert_eq!(
                    family_of(&frame, "Every control").as_deref(),
                    Some("sans-serif")
                );
            });
            let asked = asks(&test).len();
            test.lose_paint_asks();
            for seconds in [1, 5, 9] {
                with_monotonic_time(at(seconds), || drop(settle(&engine)));
            }
            assert!(asks(&test).len() > asked, "the engine was asked again");
            let frame = engine.paint_frame().expect("the page");
            assert_eq!(
                family_of(&frame, "Every control").as_deref(),
                Some("sans-serif"),
                "text stays in the class of face the engine said"
            );
            assert_eq!(frame.seq, 1, "and the page is not published again");
            assert_eq!(captures(&test), 1, "nor captured again");
        }
    }

    /// How often a test engine was asked whether it still has a capture.
    fn looks(test: &TestEngine) -> usize {
        asks(test)
            .iter()
            .filter(|(nodes, families)| nodes.is_empty() && families.as_slice() == [LOOK_FAMILY])
            .count()
    }

    /// The family list a frame sends the run `text` in.
    fn family_of(frame: &Frame, text: &str) -> Option<String> {
        runs(frame)
            .into_iter()
            .find(|(said, _)| said == text)
            .map(|(_, family)| family)
    }

    /// One run of text on a white page of 400 by 80, and the columns its ink spans.
    fn inked(part: u32, rect: tilo_paint_stream::Rect, text: &str) -> (usize, usize) {
        let mut frame = Frame {
            seq: 1,
            viewport: Point { x: 400.0, y: 80.0 },
            background: PaintColor([255, 255, 255, 255]),
            ..Frame::default()
        };
        frame.layers.insert(
            ROOT,
            tilo_paint_stream::Layer {
                size: frame.viewport,
                opacity: 1.0,
                items: vec![Item::Text {
                    key: ItemKey { node: 1, part },
                    rect,
                    text: text.to_owned(),
                    font: tilo_paint_stream::FontSpec {
                        family: "sans-serif".into(),
                        size: 20.0,
                        weight: 400,
                        italic: false,
                    },
                    color: PaintColor([0, 0, 0, 255]),
                }],
                ..tilo_paint_stream::Layer::default()
            },
        );
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        let blobs = |_: &[u8; 32]| None::<Arc<Vec<u8>>>;
        PageRenderer::new().draw(
            &mut PageCanvas {
                scene: &mut scene,
                transform: Affine::IDENTITY,
                text: &mut text,
                blobs: &blobs,
            },
            &frame,
        );
        let pixels = tilo_ui_base::offscreen::OffscreenRenderer::new(400, 80)
            .render_resilient(&scene, Color::WHITE);
        let dark: Vec<usize> = pixels
            .chunks_exact(4)
            .enumerate()
            .filter(|(_, pixel)| pixel.iter().take(3).any(|channel| *channel < 128))
            .map(|(index, _)| index % 400)
            .collect();
        (
            dark.iter().copied().min().expect("the text left ink"),
            dark.iter().copied().max().expect("the text left ink"),
        )
    }

    /// A run this device's face sets wider than the engine's did is drawn narrower, so it
    /// ends where the engine's line ended and never runs into what follows it; what a
    /// field holds that is longer than its room stays in the room too. Text written right
    /// to left is drawn in its box like any other.
    #[test]
    fn a_run_wider_than_its_box_is_drawn_narrower_and_ends_inside_it() {
        let words = "The harbour kept its own hours";
        let style = TextStyle::new(20.0);
        let (natural, _) = TextRenderer::new().measure_style(words, None, &style);
        let narrow = tilo_paint_stream::Rect::new(20.0, 20.0, natural * 0.7, 30.0);
        let end = 20 + (natural * 0.7).ceil() as usize + 1;
        for part in [16, ItemKey::SLOT] {
            let (left, right) = inked(part, narrow, words);
            assert!(
                left >= 19 && right <= end,
                "part {part}: {left}..{right} in a box that ends at {end}"
            );
        }
        let hebrew = "\u{5e9}\u{5dc}\u{5d5}\u{5dd} \u{5e2}\u{5d5}\u{5dc}\u{5dd}";
        let (wide, _) = TextRenderer::new().measure_style(hebrew, None, &style);
        let boxed = tilo_paint_stream::Rect::new(100.0, 20.0, wide, 30.0);
        let (left, right) = inked(16, boxed, hebrew);
        assert!(
            left >= 98 && right <= 102 + wide.ceil() as usize,
            "{left}..{right} in a box from 100, {wide} wide"
        );
    }

    /// The toolkit draws the recorded page close to the engine's own pixels.
    #[test]
    fn the_renderer_reproduces_the_engines_pixels() {
        let (_, engine) = paint_engine(TOP);
        settle(&engine);
        let frame = engine.paint_frame().expect("frame");
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        let source = Arc::clone(&engine);
        let blobs = move |hash: &[u8; 32]| source.paint_blob(hash);
        PageRenderer::new().draw(
            &mut PageCanvas {
                scene: &mut scene,
                transform: Affine::IDENTITY,
                text: &mut text,
                blobs: &blobs,
            },
            &frame,
        );
        let mut renderer = tilo_ui_base::offscreen::OffscreenRenderer::new(800, 600);
        let ours = renderer.render_resilient(&scene, Color::WHITE);
        let reference = image::load_from_memory(TOP_PNG).expect("png").to_rgba8();
        let (mut close, mut total) = (0_usize, 0_usize);
        for (a, b) in ours.chunks_exact(4).zip(reference.as_raw().chunks_exact(4)) {
            total += 1;
            if a.iter().zip(b).take(3).all(|(x, y)| x.abs_diff(*y) <= 24) {
                close += 1;
            }
        }
        let share = close as f64 / total as f64;
        assert!(
            share > 0.95,
            "only {:.1}% of pixels match the engine",
            share * 100.0
        );
    }
}
