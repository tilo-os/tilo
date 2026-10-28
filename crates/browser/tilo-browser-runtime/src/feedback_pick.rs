//! Reporting a problem by pointing at it, on the page's desktop: the shared
//! picker over the desktop's own widget tree, and the report the picks
//! become. What is picked is read from the retained tree (each part's
//! path, role and texts, the app and record of its window), never from
//! pixels.

use std::collections::VecDeque;

use tilo_app::feedback::report::{
    Area, FrameTimings, Interaction, Outcome, Pick, PickedText, ReportDraft, TextKind,
};
use tilo_desktop::feedback_picker::{FeedbackPicker, PickSource, PickTarget, PickerOutcome};
use ui::kurbo::{Point, Rect};
use ui::widget::Widget;

/// How many frames' build times are kept for "this was slow".
const FRAMES: usize = tilo_app::feedback::report::MAX_FRAMES;
/// A frame's budget at 60 frames a second, in microseconds.
const BUDGET_US: u32 = 16_666;

/// A window on screen, as a report names what is in it.
#[derive(Clone, Debug)]
pub(crate) struct PickWindow {
    pub rect: Rect,
    /// The component instance showing it (its tab).
    pub instance: u64,
    pub app: String,
    pub name: String,
    pub record: Option<String>,
}

/// What a report knows of the page beside the picks.
pub(crate) struct ReportContext {
    pub windows: Vec<PickWindow>,
    /// The build this page runs (its commit), when the page said.
    pub build: Option<String>,
    /// The names of the people here, this page's person among them.
    pub names: Vec<String>,
}

/// The picker on the page's desktop, and how long its recent frames took.
#[derive(Default)]
pub(crate) struct PagePicker {
    picker: FeedbackPicker,
    frames: VecDeque<u32>,
}

struct Source<'a> {
    root: &'a dyn Widget,
    windows: &'a [PickWindow],
}

impl Source<'_> {
    fn target(&self, target: ui::pick::PickTarget) -> PickTarget {
        let app = window_of(self.windows, target.rect).map_or("Tilo", |w| w.name.as_str());
        PickTarget {
            caption: PickTarget::caption(
                app,
                &target.role_name(),
                target.kind.as_deref(),
                target.name.as_deref().filter(|_| target.interface),
            ),
            key: target.path,
            rect: target.rect,
        }
    }
}

impl PickSource for Source<'_> {
    fn at(&self, point: Point) -> Vec<PickTarget> {
        ui::pick::targets_at(self.root, point)
            .into_iter()
            .map(|target| self.target(target))
            .collect()
    }

    fn within(&self, area: Rect, limit: usize) -> Vec<PickTarget> {
        ui::pick::targets_within(self.root, area, limit)
            .into_iter()
            .map(|target| self.target(target))
            .collect()
    }
}

impl PagePicker {
    pub fn open(&mut self) {
        self.picker.open(ui::time::now());
    }

    pub fn is_open(&self) -> bool {
        self.picker.is_open()
    }

    pub fn needs_animation_frame(&self) -> bool {
        self.picker.needs_animation_frame(ui::time::now())
    }

    /// Note how long building a frame took.
    pub fn frame_built(&mut self, micros: u32) {
        if self.frames.len() == FRAMES {
            self.frames.pop_front();
        }
        self.frames.push_back(micros);
    }

    /// Take an event while picking over `root`; whether it was taken.
    pub fn handle_event(
        &mut self,
        event: &ui::winit::event::WindowEvent,
        cursor: Option<Point>,
        modifiers: ui::winit::keyboard::ModifiersState,
        root: &dyn Widget,
        windows: &[PickWindow],
    ) -> bool {
        self.picker.handle_event(
            event,
            cursor,
            modifiers,
            &Source { root, windows },
            ui::time::now(),
        )
    }

    pub fn draw(
        &mut self,
        scene: &mut ui::vello::Scene,
        transform: ui::kurbo::Affine,
        text: &mut ui::text_render::TextRenderer,
        theme: &ui::theme::Theme,
        bounds: Rect,
    ) {
        self.picker
            .draw(scene, transform, text, theme, bounds, ui::time::now());
    }

    /// The report the person's picks became, once they finished picking
    /// over `root`, with each pick's place and where its picture must be
    /// masked; `None` while picking, or when they left without a pick.
    pub fn take_report(
        &mut self,
        root: &dyn Widget,
        cx: &ReportContext,
    ) -> Option<(ReportDraft, crate::feedback_crops::Crops)> {
        let PickerOutcome::Picked(targets) = self.picker.take_outcome()? else {
            return None;
        };
        let (picks, crops): (Vec<Pick>, Vec<(Rect, Vec<Rect>)>) = targets
            .iter()
            .filter_map(|target| {
                let pick = pick(root, target, cx)?;
                let masked = ui::pick::sensitive_areas(root, &target.key, target.rect);
                Some((pick, (target.rect, masked)))
            })
            .unzip();
        if picks.is_empty() {
            return None;
        }
        let interactions = ui::pick::recent_invocations()
            .into_iter()
            .filter_map(|invocation| {
                let window = cx
                    .windows
                    .iter()
                    .find(|w| w.instance == invocation.instance)?;
                Some(Interaction {
                    at: invocation.at,
                    app: window.app.clone(),
                    action: invocation.action,
                    resource: invocation.resource,
                    label: invocation.label,
                    outcome: if invocation.done {
                        Outcome::Done
                    } else {
                        Outcome::Refused
                    },
                })
            })
            .collect();
        let draft = ReportDraft {
            picks,
            interactions,
            timings: (!self.frames.is_empty()).then(|| FrameTimings {
                frames: self.frames.iter().copied().collect(),
                budget: BUDGET_US,
            }),
            names: cx.names.clone(),
            ..ReportDraft::default()
        };
        Some((draft, crops))
    }
}

/// One picked part, read again from the tree as it is now.
fn pick(root: &dyn Widget, target: &PickTarget, cx: &ReportContext) -> Option<Pick> {
    let captured = ui::pick::capture(root, &target.key)?;
    let window = window_of(&cx.windows, target.rect);
    let text = |text: ui::pick::PickedText| PickedText {
        kind: if text.interface {
            TextKind::Interface
        } else {
            TextKind::Content
        },
        text: text.text,
        area: text.rect.map(area),
    };
    let app_name = window.map_or_else(|| "Tilo".into(), |w| w.name.clone());
    let role = captured
        .role
        .map_or_else(|| "Group".into(), ui::pick::role_name);
    let copy = captured
        .label
        .as_ref()
        .filter(|label| label.interface)
        .map(|label| label.text.clone());
    let mut pick = Pick {
        app: window.map_or_else(|| tilo_desktop::feedback::PACKAGE.into(), |w| w.app.clone()),
        caption: PickTarget::caption(&app_name, &role, captured.kind.as_deref(), copy.as_deref()),
        app_name,
        build: cx.build.clone(),
        path: target.key.clone(),
        role,
        kind: captured.kind.clone(),
        label: captured.label.map(text),
        record: window.and_then(|w| w.record.clone()),
        area: area(target.rect),
        texts: captured.texts.into_iter().map(text).collect(),
    };
    pick.bound();
    Some(pick)
}

fn area(rect: Rect) -> Area {
    Area {
        x: rect.x0,
        y: rect.y0,
        width: rect.width(),
        height: rect.height(),
    }
}

/// The window a part at `rect` is in: the one holding its middle.
fn window_of(windows: &[PickWindow], rect: Rect) -> Option<&PickWindow> {
    windows
        .iter()
        .find(|window| window.rect.contains(rect.center()))
}
