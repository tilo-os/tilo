//! Reporting a problem by pointing at it on the native desktop: the shared
//! [`FeedbackPicker`] over the whole screen. The shell's own parts (its bar,
//! its menus, the windows it draws itself) are read from its retained tree;
//! a window another process draws is asked over that app's shell-ui
//! connection ([`FloatingSurfaceHostMessage::Pick`]), which answers from the
//! app's own tree. Nothing is read from pixels.
//!
//! The picker is synchronous and the apps are not: what lies under the
//! pointer in a window is the window's last answer, asked again as the
//! pointer moves, one question at a time; the release that ends a dragged
//! rectangle waits until the windows under it said what is inside; and the
//! report is built once every picked window read its picks again, or had
//! [`ANSWER_WAIT`] to.

use std::collections::{HashMap, VecDeque};

use tilo_app::feedback::report::{
    Area, FrameTimings, Interaction, MAX_INTERACTIONS, Outcome, Pick, PickedText, ReportDraft,
    TextKind,
};
use tilo_desktop::feedback_picker::{FeedbackPicker, PickSource, PickTarget, PickerOutcome};
use ui::floating_surface::FloatingSurfaceHostMessage;
use ui::pick_wire::{
    CapturedPart, CapturedText, PartToCapture, PickAnswer, PickQuery, PickedPart, RecentAction,
};
use ui::time::Instant;
use ui::widget::Widget;
use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::shell::selection_agent::ANSWER_WAIT;

/// How many frames' build times are kept for "this was slow".
const FRAMES: usize = tilo_app::feedback::report::MAX_FRAMES;
/// A frame's budget at 60 frames a second, in microseconds.
const BUDGET_US: u32 = 16_666;
/// What the shell's own parts are reported as.
const SHELL_NAME: &str = "Tilo";

/// A window on screen that another process draws.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PickWindow {
    pub surface: u64,
    /// Where it is on the desktop.
    pub rect: Rect,
    /// Where the window's own `(0, 0)` is on the desktop.
    pub origin: Point,
    /// The app, as the launcher knows it.
    pub app: String,
    pub name: String,
    /// The kind of record it shows, when it said.
    pub record: Option<String>,
    /// The app's shell-ui connections that answer what is in it.
    pub connections: Vec<u64>,
}

impl PickWindow {
    fn local(&self, point: Point) -> [f64; 2] {
        [point.x - self.origin.x, point.y - self.origin.y]
    }

    fn local_rect(&self, rect: Rect) -> [f64; 4] {
        let Point { x, y } = self.origin;
        [rect.x0 - x, rect.y0 - y, rect.x1 - x, rect.y1 - y]
    }

    /// A rectangle the app named, on the desktop.
    fn desktop(&self, rect: [f64; 4]) -> Option<Rect> {
        let Point { x, y } = self.origin;
        ui::pick_wire::rect_of(rect)
            .map(|rect| Rect::new(rect.x0 + x, rect.y0 + y, rect.x1 + x, rect.y1 + y))
    }

    fn key(&self, path: &str) -> String {
        format!("@{}:{path}", self.surface)
    }
}

/// What a report knows besides the picks.
pub(crate) struct ReportContext {
    /// The names of the people here, this person's among them.
    pub names: Vec<String>,
}

/// A question to a window, waiting for its answer.
#[derive(Clone, Copy, Debug)]
struct Asked {
    request: u64,
    window: usize,
    by: Instant,
}

/// A dragged rectangle's release, held until the windows under it said
/// what is inside.
struct Release {
    point: Point,
    shift: bool,
    asked: Vec<Asked>,
    found: Vec<PickTarget>,
    by: Instant,
}

/// The picks, being read again by the windows that hold them.
struct Capturing {
    targets: Vec<PickTarget>,
    asked: Vec<Asked>,
    /// Each pick's texts, by its key, on the desktop.
    parts: HashMap<String, CapturedPart>,
    /// What the person last did in each window that answered.
    recent: Vec<(usize, RecentAction)>,
    /// The windows that answered.
    answered: Vec<usize>,
    by: Instant,
}

/// The picker on the native desktop, and the questions it has out.
#[derive(Default)]
pub(crate) struct NativePicker {
    picker: FeedbackPicker,
    windows: Vec<PickWindow>,
    next_request: u64,
    outbox: Vec<(u64, FloatingSurfaceHostMessage)>,
    cursor: Option<Point>,
    pressed: bool,
    /// What lies under `point` in this window, asked.
    hovering: Option<(Asked, Point)>,
    /// Where the pointer went since, still to be asked about.
    wanted: Option<(usize, Point)>,
    /// What the window under the pointer last named there, on the desktop.
    under: Option<(usize, Vec<PickTarget>)>,
    release: Option<Release>,
    capturing: Option<Capturing>,
    /// Each remote part's role, for a pick its window did not read again.
    roles: HashMap<String, String>,
    frames: VecDeque<u32>,
}

/// The shell's tree beside what the windows last said.
struct Source<'a> {
    root: &'a dyn Widget,
    windows: &'a [PickWindow],
    under: Option<&'a (usize, Vec<PickTarget>)>,
    found: &'a [PickTarget],
}

fn local_target(target: ui::pick::PickTarget) -> PickTarget {
    PickTarget {
        caption: PickTarget::caption(
            SHELL_NAME,
            &target.role_name(),
            target.kind.as_deref(),
            target.name.as_deref().filter(|_| target.interface),
        ),
        key: target.path,
        rect: target.rect,
    }
}

impl PickSource for Source<'_> {
    fn at(&self, point: Point) -> Vec<PickTarget> {
        let mut chain: Vec<PickTarget> = ui::pick::targets_at(self.root, point)
            .into_iter()
            .map(local_target)
            .collect();
        if let Some(window) = window_at(self.windows, point)
            && let Some((under, targets)) = self.under
            && *under == window
        {
            chain.extend(
                targets
                    .iter()
                    .filter(|target| target.rect.contains(point))
                    .cloned(),
            );
        }
        chain
    }

    fn within(&self, area: Rect, limit: usize) -> Vec<PickTarget> {
        ui::pick::targets_within(self.root, area, limit)
            .into_iter()
            .map(local_target)
            // A window another process draws says itself what is in it.
            .filter(|target| window_at(self.windows, target.rect.center()).is_none())
            .chain(
                self.found
                    .iter()
                    .filter(|target| area.contains_rect(target.rect))
                    .cloned(),
            )
            .take(limit)
            .collect()
    }
}

/// The front-most window at `point`.
fn window_at(windows: &[PickWindow], point: Point) -> Option<usize> {
    windows
        .iter()
        .position(|window| window.rect.contains(point))
}

impl NativePicker {
    /// Start picking over `windows`, front-most first.
    pub(crate) fn open(&mut self, windows: Vec<PickWindow>, now: Instant) {
        let frames = std::mem::take(&mut self.frames);
        let next_request = self.next_request;
        *self = Self {
            windows,
            next_request,
            frames,
            ..Self::default()
        };
        self.picker.open(now);
    }

    /// Whether the person is picking: it takes every pointer and key event.
    pub(crate) fn is_open(&self) -> bool {
        self.picker.is_open()
    }

    pub(crate) fn needs_animation_frame(&self, now: Instant) -> bool {
        self.picker.needs_animation_frame(now)
    }

    /// Note how long drawing a frame took.
    pub(crate) fn frame_built(&mut self, micros: u32) {
        if self.frames.len() == FRAMES {
            self.frames.pop_front();
        }
        self.frames.push_back(micros);
    }

    /// The questions to send now, to which shell-ui connections.
    pub(crate) fn take_outbox(&mut self) -> Vec<(u64, FloatingSurfaceHostMessage)> {
        std::mem::take(&mut self.outbox)
    }

    /// When the picker next needs [`Self::tick`]: at once with questions to
    /// send, then when an unanswered one gives up.
    pub(crate) fn deadline(&self, now: Instant) -> Option<Instant> {
        if !self.outbox.is_empty() {
            return Some(now);
        }
        self.hovering
            .map(|(asked, _)| asked.by)
            .into_iter()
            .chain(self.release.as_ref().map(|release| release.by))
            .chain(self.capturing.as_ref().map(|capturing| capturing.by))
            .min()
    }

    fn ask(&mut self, window: usize, query: &PickQuery, now: Instant) -> Option<Asked> {
        let connections = self.windows.get(window)?.connections.clone();
        if connections.is_empty() {
            return None;
        }
        self.next_request = self.next_request.wrapping_add(1);
        let request = self.next_request;
        for connection in connections {
            self.outbox.push((
                connection,
                FloatingSurfaceHostMessage::Pick {
                    request,
                    query: query.clone(),
                },
            ));
        }
        Some(Asked {
            request,
            window,
            by: now.checked_add(ANSWER_WAIT).unwrap_or(now),
        })
    }

    /// Ask the window under `point` what is there, unless a question is out.
    fn ask_at(&mut self, point: Point, now: Instant) {
        let Some(window) = window_at(&self.windows, point) else {
            self.wanted = None;
            return;
        };
        if self.hovering.is_some() {
            self.wanted = Some((window, point));
            return;
        }
        self.wanted = None;
        let Some(local) = self.windows.get(window).map(|w| w.local(point)) else {
            return;
        };
        if let Some(asked) = self.ask(window, &PickQuery::At { point: local }, now) {
            self.hovering = Some((asked, point));
        }
    }

    /// Take an event while picking over the shell's `root`; whether it was
    /// taken (every event is, while picking). `cursor` is the pointer on the
    /// desktop.
    pub(crate) fn handle_event(
        &mut self,
        event: &WindowEvent,
        cursor: Option<Point>,
        modifiers: ModifiersState,
        root: &dyn Widget,
        now: Instant,
    ) -> bool {
        if !self.picker.is_open() {
            return false;
        }
        self.cursor = cursor.or(self.cursor);
        if self.release.is_some() {
            // Only leaving is taken while the windows say what is inside.
            if let WindowEvent::KeyboardInput { event: key, .. } = event
                && key.state == ElementState::Pressed
                && key.logical_key == Key::Named(NamedKey::Escape)
            {
                self.release = None;
                self.picker.key(&key.logical_key, now);
                self.follow(root, now);
            }
            return true;
        }
        match event {
            WindowEvent::CursorMoved { .. } if !self.pressed => {
                if let Some(point) = self.cursor {
                    self.ask_at(point, now);
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                self.pressed = *state == ElementState::Pressed;
                if !self.pressed
                    && let (Some(area), Some(point)) = (self.picker.dragging(), self.cursor)
                    && self.hold_release(area, point, modifiers.shift_key(), now)
                {
                    return true;
                }
            }
            _ => {}
        }
        let source = Source {
            root,
            windows: &self.windows,
            under: self.under.as_ref(),
            found: &[],
        };
        self.picker
            .handle_event(event, self.cursor, modifiers, &source, now);
        self.follow(root, now);
        true
    }

    /// Ask the windows under a dragged `area` what is inside it, and hold
    /// the release until they said; whether any was asked.
    fn hold_release(&mut self, area: Rect, point: Point, shift: bool, now: Instant) -> bool {
        let room = tilo_desktop::feedback_picker::MAX_PICKS
            .saturating_sub(self.picker.selected().len())
            .max(1);
        let limit = u32::try_from(room).unwrap_or(u32::MAX);
        let questions: Vec<(usize, PickQuery)> = self
            .windows
            .iter()
            .enumerate()
            .filter_map(|(window, on)| {
                let inside = on.rect.intersect(area);
                if inside.width() <= 0.0 || inside.height() <= 0.0 {
                    return None;
                }
                let area = on.local_rect(inside);
                Some((window, PickQuery::Within { area, limit }))
            })
            .collect();
        let asked: Vec<Asked> = questions
            .iter()
            .filter_map(|(window, query)| self.ask(*window, query, now))
            .collect();
        if asked.is_empty() {
            return false;
        }
        self.release = Some(Release {
            point,
            shift,
            asked,
            found: Vec::new(),
            by: now.checked_add(ANSWER_WAIT).unwrap_or(now),
        });
        true
    }

    fn let_release_go(&mut self, root: &dyn Widget, now: Instant) {
        let Some(release) = self.release.take() else {
            return;
        };
        let source = Source {
            root,
            windows: &self.windows,
            under: self.under.as_ref(),
            found: &release.found,
        };
        self.picker
            .button(release.point, false, release.shift, &source);
        self.follow(root, now);
    }

    /// Picking ended: read the picks again, the shell's own here and each
    /// window's by asking it.
    fn follow(&mut self, root: &dyn Widget, now: Instant) {
        let Some(outcome) = self.picker.take_outcome() else {
            return;
        };
        self.hovering = None;
        self.wanted = None;
        self.release = None;
        let PickerOutcome::Picked(targets) = outcome else {
            self.capturing = None;
            return;
        };
        let mut parts = HashMap::new();
        let local: Vec<PartToCapture> = targets
            .iter()
            .filter(|target| !target.key.starts_with('@'))
            .map(|target| PartToCapture {
                path: target.key.clone(),
                rect: ui::pick_wire::rect_array(target.rect),
            })
            .collect();
        if !local.is_empty()
            && let PickAnswer::Captured {
                parts: captured, ..
            } = ui::pick_wire::answer(root, &PickQuery::Capture { parts: local })
        {
            for part in captured {
                parts.insert(part.path.clone(), part);
            }
        }
        let questions: Vec<(usize, PickQuery)> = self
            .windows
            .iter()
            .enumerate()
            .filter_map(|(window, on)| {
                let prefix = on.key("");
                let wanted: Vec<PartToCapture> = targets
                    .iter()
                    .filter_map(|target| {
                        let path = target.key.strip_prefix(&prefix)?;
                        Some(PartToCapture {
                            path: path.to_owned(),
                            rect: on.local_rect(target.rect),
                        })
                    })
                    .collect();
                (!wanted.is_empty()).then_some((window, PickQuery::Capture { parts: wanted }))
            })
            .collect();
        let asked: Vec<Asked> = questions
            .iter()
            .filter_map(|(window, query)| self.ask(*window, query, now))
            .collect();
        self.capturing = Some(Capturing {
            targets,
            asked,
            parts,
            recent: Vec::new(),
            answered: Vec::new(),
            by: now.checked_add(ANSWER_WAIT).unwrap_or(now),
        });
    }

    /// `app` (as the launcher vouched for it) answered `request`; whether
    /// anything changed.
    pub(crate) fn answered(
        &mut self,
        app: &str,
        request: u64,
        answer: PickAnswer,
        root: &dyn Widget,
        now: Instant,
    ) -> bool {
        let from = |asked: &Asked, windows: &[PickWindow]| {
            asked.request == request && windows.get(asked.window).is_some_and(|w| w.app == app)
        };
        if let Some((asked, _)) = self.hovering
            && from(&asked, &self.windows)
        {
            self.hovering = None;
            if let PickAnswer::Targets { parts } = answer {
                let targets = self.targets(asked.window, parts);
                self.under = Some((asked.window, targets));
            }
            self.rehover(root, now);
            return true;
        }
        if let Some(release) = self.release.as_mut()
            && let Some(at) = release.asked.iter().position(|a| from(a, &self.windows))
        {
            let asked = release.asked.remove(at);
            let done = release.asked.is_empty();
            if let PickAnswer::Targets { parts } = answer {
                let targets = self.targets(asked.window, parts);
                if let Some(release) = self.release.as_mut() {
                    release.found.extend(targets);
                }
            }
            if done {
                self.let_release_go(root, now);
            }
            return true;
        }
        if let Some(capturing) = self.capturing.as_mut()
            && let Some(at) = capturing.asked.iter().position(|a| from(a, &self.windows))
        {
            let asked = capturing.asked.remove(at);
            let Some(window) = self.windows.get(asked.window) else {
                return false;
            };
            capturing.answered.push(asked.window);
            if let PickAnswer::Captured { parts, recent } = answer {
                for part in parts {
                    let key = window.key(&part.path);
                    capturing.parts.insert(key, on_desktop(window, part));
                }
                capturing.recent.extend(
                    recent
                        .into_iter()
                        .take(ui::pick_wire::MAX_RECENT)
                        .map(|action| (asked.window, action)),
                );
            }
            return true;
        }
        false
    }

    /// The parts a window named, on the desktop, keyed apart from every
    /// other window's.
    fn targets(&mut self, window: usize, parts: Vec<PickedPart>) -> Vec<PickTarget> {
        let Some(on) = self.windows.get(window) else {
            return Vec::new();
        };
        let mut targets = Vec::new();
        for part in parts.into_iter().take(ui::pick_wire::MAX_PARTS) {
            let Some(rect) = on.desktop(part.rect) else {
                continue;
            };
            let key = on.key(&part.path);
            self.roles.insert(key.clone(), part.role.clone());
            targets.push(PickTarget {
                caption: PickTarget::caption(
                    &on.name,
                    &part.role,
                    part.kind.as_deref(),
                    part.copy.as_deref(),
                ),
                key,
                rect,
            });
        }
        targets
    }

    /// Light up again what is under the pointer, with what a window just
    /// said, and ask about where the pointer went since.
    fn rehover(&mut self, root: &dyn Widget, now: Instant) {
        if let Some((_, point)) = self.wanted.take() {
            self.ask_at(point, now);
        }
        if self.pressed {
            return;
        }
        if let Some(point) = self.cursor {
            let source = Source {
                root,
                windows: &self.windows,
                under: self.under.as_ref(),
                found: &[],
            };
            self.picker.hover(point, &source, now);
        }
    }

    /// Give up on questions not answered in time; whether anything changed.
    pub(crate) fn tick(&mut self, root: &dyn Widget, now: Instant) -> bool {
        let mut changed = false;
        if let Some((asked, _)) = self.hovering
            && now >= asked.by
        {
            self.hovering = None;
            self.rehover(root, now);
            changed = true;
        }
        if self
            .release
            .as_ref()
            .is_some_and(|release| now >= release.by)
        {
            self.let_release_go(root, now);
            changed = true;
        }
        if let Some(capturing) = self.capturing.as_mut()
            && !capturing.asked.is_empty()
            && now >= capturing.by
        {
            capturing.asked.clear();
            changed = true;
        }
        changed
    }

    /// The report the picks became, once every picked window read them
    /// again or had its time to; `None` until then, and after a picker left
    /// without a pick.
    pub(crate) fn take_report(&mut self, cx: &ReportContext) -> Option<ReportDraft> {
        if !self
            .capturing
            .as_ref()
            .is_some_and(|capturing| capturing.asked.is_empty())
        {
            return None;
        }
        let capturing = self.capturing.take()?;
        let picks: Vec<Pick> = capturing
            .targets
            .iter()
            .filter_map(|target| self.pick(target, &capturing))
            .collect();
        if picks.is_empty() {
            return None;
        }
        let mut interactions: Vec<Interaction> = capturing
            .recent
            .into_iter()
            .filter_map(|(window, action)| {
                Some(Interaction {
                    at: action.at,
                    app: self.windows.get(window)?.app.clone(),
                    action: action.action,
                    resource: action.resource,
                    label: action.label,
                    outcome: if action.done {
                        Outcome::Done
                    } else {
                        Outcome::Refused
                    },
                })
            })
            .collect();
        interactions.sort_by_key(|interaction| interaction.at);
        let surplus = interactions.len().saturating_sub(MAX_INTERACTIONS);
        interactions.drain(..surplus);
        Some(ReportDraft {
            picks,
            interactions,
            timings: (!self.frames.is_empty()).then(|| FrameTimings {
                frames: self.frames.iter().copied().collect(),
                budget: BUDGET_US,
            }),
            names: cx.names.clone(),
            ..ReportDraft::default()
        })
    }

    /// One pick, as its window read it again. A pick of a window that did
    /// not answer in time keeps what the picker knew of it; one its window
    /// or the shell no longer has is left out.
    fn pick(&self, target: &PickTarget, capturing: &Capturing) -> Option<Pick> {
        let (window, path) = match target.key.strip_prefix('@') {
            Some(rest) => {
                let (surface, path) = rest.split_once(':')?;
                let surface: u64 = surface.parse().ok()?;
                let index = self.windows.iter().position(|w| w.surface == surface)?;
                (Some(index), path.to_owned())
            }
            None => (None, target.key.clone()),
        };
        let captured = capturing.parts.get(&target.key);
        if captured.is_none() && window.is_none_or(|window| capturing.answered.contains(&window)) {
            return None;
        }
        let on = window.and_then(|window| self.windows.get(window));
        let role = captured
            .and_then(|part| part.role.clone())
            .or_else(|| self.roles.get(&target.key).cloned())
            .unwrap_or_else(|| "Group".into());
        let app_name = on.map_or_else(|| SHELL_NAME.into(), |window| window.name.clone());
        let kind = captured.and_then(|part| part.kind.clone());
        let copy = captured
            .and_then(|part| part.label.as_ref())
            .filter(|label| label.interface)
            .map(|label| label.text.clone());
        let mut pick = Pick {
            app: on.map_or_else(
                || tilo_desktop::feedback::PACKAGE.into(),
                |window| window.app.clone(),
            ),
            caption: PickTarget::caption(&app_name, &role, kind.as_deref(), copy.as_deref()),
            app_name,
            build: None,
            path,
            role,
            kind,
            label: captured
                .and_then(|part| part.label.clone())
                .map(picked_text),
            record: on.and_then(|window| window.record.clone()),
            area: area(target.rect),
            texts: captured
                .map(|part| part.texts.iter().cloned().map(picked_text).collect())
                .unwrap_or_default(),
        };
        pick.bound();
        Some(pick)
    }

    pub(crate) fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text: &mut ui::text_render::TextRenderer,
        theme: &ui::theme::Theme,
        bounds: Rect,
        now: Instant,
    ) {
        self.picker.draw(scene, transform, text, theme, bounds, now);
    }
}

/// A part a window read again, with its texts' places on the desktop.
fn on_desktop(window: &PickWindow, part: CapturedPart) -> CapturedPart {
    let shift = |rect: [f64; 4]| window.desktop(rect).map(ui::pick_wire::rect_array);
    let text = |text: CapturedText| CapturedText {
        rect: text.rect.and_then(shift),
        ..text
    };
    CapturedPart {
        label: part.label.map(text),
        texts: part.texts.into_iter().map(text).collect(),
        masked: part.masked.into_iter().filter_map(shift).collect(),
        ..part
    }
}

fn picked_text(text: CapturedText) -> PickedText {
    PickedText {
        kind: if text.interface {
            TextKind::Interface
        } else {
            TextKind::Content
        },
        text: text.text,
        area: text.rect.and_then(ui::pick_wire::rect_of).map(area),
    }
}

fn area(rect: Rect) -> Area {
    Area {
        x: rect.x0,
        y: rect.y0,
        width: rect.width(),
        height: rect.height(),
    }
}

#[cfg(test)]
#[path = "feedback_pick_tests.rs"]
mod tests;
