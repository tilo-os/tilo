use std::collections::HashMap;
use std::path::PathBuf;
use ui::widget::{AppModel, ModelPath};

use ui::a11y::{AccessNode, AccessState, Role};
use ui::chart_palette::ChartPalette;
use ui::framework::{Dispatcher, EffectCx, Effected, LiveRow};
use ui::presence::PresenceCtx;
use ui::taffy::{
    Dimension, LengthPercentageAuto, NodeId, Position, Rect as TRect, Size, Style, TaffyTree,
};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Circle, Point, Rect};
use ui::vello::peniko::Fill;
use ui::widget::{Button, ButtonFace, PieChart, PieSlice, PieStyle, Widget};
use ui::winit::event::{ElementState, MouseButton, WindowEvent};

use crate::resource::{DiskUsage, DiskUsageAction, DiskUsageEffect, analyze, folder_row};
use crate::scan::{ScanCancellation, ScanResult};
use ui::format::bytes_si;

const TOP_H: f64 = 78.0;

#[derive(Clone)]
struct Place {
    label: String,
    caption: String,
    path: PathBuf,
    choose: bool,
}

enum View {
    Home,
    Loading(PathBuf),
    Analysis(ScanResult),
    Error(PathBuf, String),
}

pub struct DiskUsageApp {
    view: View,
    history: Vec<PathBuf>,
    pub(crate) recent: Vec<PathBuf>,
    places: Vec<Place>,
    chart: PieChart,
    rect: Rect,
    pointer: Point,
    /// Stops the scan in progress when another view replaces it.
    scan_cancellation: Option<ScanCancellation>,
    /// Slices appear at once instead of growing in: a screenshot shows the
    /// finished chart.
    pub(crate) immediate: bool,
    /// Where clicks send the effects they invoke: the same door as the
    /// palette and agents.
    pub(crate) dispatcher: Option<Dispatcher<DiskUsage>>,
}

impl DiskUsageApp {
    pub fn new() -> Self {
        let home = tilo_paths::home().unwrap_or_else(|| PathBuf::from("/"));
        let workspace = std::env::var_os("TILO_WORKSPACE_ROOTS")
            .and_then(|s| s.to_string_lossy().split(':').next().map(PathBuf::from))
            .or_else(tilo_paths::workspace_root)
            .unwrap_or_else(|| PathBuf::from(tilo_paths::WORKSPACE_MOUNT));
        let places = vec![
            Place {
                label: "Home".into(),
                caption: "Documents, downloads and personal files".into(),
                path: home,
                choose: false,
            },
            Place {
                label: "Workspace".into(),
                caption: "Shared projects and hydrated files".into(),
                path: workspace,
                choose: false,
            },
            Place {
                label: "File system".into(),
                caption: "Everything on this system".into(),
                path: PathBuf::from("/"),
                choose: false,
            },
        ];
        Self {
            view: View::Home,
            history: Vec::new(),
            recent: Vec::new(),
            places,
            chart: PieChart::new(Vec::new()).with_style(PieStyle::default()),
            rect: Rect::ZERO,
            pointer: Point::ZERO,
            scan_cancellation: None,
            immediate: false,
            dispatcher: None,
        }
    }

    fn palette(theme: &Theme, index: usize) -> ui::vello::peniko::Color {
        ChartPalette::from_theme(theme).categorical(index)
    }

    pub(crate) fn set_result(&mut self, result: ScanResult) {
        let total = result.total;
        let slices = result
            .entries
            .iter()
            .map(|entry| {
                let slice = PieSlice::new(
                    entry.path.to_string_lossy(),
                    &entry.name,
                    entry.bytes as f64,
                    ui::vello::peniko::Color::TRANSPARENT,
                )
                .with_detail(format!(
                    "{} · {} files",
                    bytes_si(entry.bytes),
                    entry.files
                ));
                if entry.path.file_name().is_some_and(|name| name == ".") {
                    slice.non_interactive()
                } else {
                    slice
                }
            })
            .collect();
        if self.immediate {
            self.chart.set_slices_immediate(slices);
        } else {
            self.chart.set_slices(slices);
        }
        self.chart
            .set_center(bytes_si(total), "used in this folder");
        self.view = View::Analysis(result);
    }

    /// Invoke `effect` through the door every caller uses.
    fn invoke(&self, effect: &DiskUsageEffect) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect(effect);
        }
    }

    /// Scan `path` in the background and show it when done, remembering the
    /// folder in the shared recents.
    pub(crate) fn scan(
        &mut self,
        path: PathBuf,
        push_history: bool,
        cx: &mut EffectCx<DiskUsage, Self>,
    ) -> Effected {
        self.cancel_scan();
        if push_history && let View::Analysis(current) = &self.view {
            self.history.push(current.path.clone());
        }
        let cancellation = ScanCancellation::default();
        self.scan_cancellation = Some(cancellation.clone());
        self.view = View::Loading(path.clone());
        let worker = cancellation.clone();
        cx.thread(
            move |_| crate::scan::scan_with_cancellation(path, &worker),
            move |app: &mut Self, result: ScanResult, cx| {
                if cancellation.is_cancelled() {
                    return Err("The scan was stopped".into());
                }
                app.scan_cancellation = None;
                if let Some(message) = result.error.clone() {
                    app.view = View::Error(result.path, message.clone());
                    return Err(message);
                }
                cx.apply(&DiskUsageAction::Remember {
                    path: result.path.to_string_lossy().into_owned(),
                });
                let answer = serde_json::json!({
                    "path": result.path,
                    "bytes": result.total,
                    "entries": result.entries.len(),
                });
                app.set_result(result);
                Ok(Effected::value(answer))
            },
        )
    }

    /// Ask for a folder, then analyze it.
    pub(crate) fn choose_folder(cx: &mut EffectCx<DiskUsage, Self>) -> Effected {
        let picker = ui::portal::PickRequest::directory("Choose a folder to analyze", "Analyze")
            .thread_name("disk-usage-folder-picker")
            .send();
        cx.thread(
            move |_| {
                picker.recv().unwrap_or_else(|_| {
                    ui::portal::PortalOutcome::Failed("The folder picker stopped responding".into())
                })
            },
            |app: &mut Self, outcome, cx| match outcome {
                ui::portal::PortalOutcome::Opened(path) => Ok(app.scan(path, false, cx)),
                ui::portal::PortalOutcome::Cancelled => Ok(Effected::done()),
                ui::portal::PortalOutcome::Failed(message) => {
                    app.view = View::Error(PathBuf::new(), message.clone());
                    Err(message)
                }
            },
        )
    }

    fn cancel_scan(&mut self) {
        if let Some(cancellation) = self.scan_cancellation.take() {
            cancellation.cancel();
        }
    }

    pub(crate) fn go_home(&mut self) {
        self.cancel_scan();
        self.history.clear();
        self.view = View::Home;
    }

    /// The folder analyzed before this one, or the locations.
    pub(crate) fn go_back(&mut self, cx: &mut EffectCx<DiskUsage, Self>) -> Effected {
        self.cancel_scan();
        if let Some(path) = self.history.pop() {
            self.scan(path, false, cx)
        } else {
            self.view = View::Home;
            Effected::done()
        }
    }

    fn back_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0 + 18.0,
            self.rect.y0 + 18.0,
            self.rect.x0 + 116.0,
            self.rect.y0 + 58.0,
        )
    }

    fn cards(&self) -> Vec<Place> {
        let recent_limit = if self.rect.width() < 700.0 { 2 } else { 5 };
        let mut out = self
            .recent
            .iter()
            .take(recent_limit)
            .map(|path| Place {
                label: path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("File system")
                    .into(),
                caption: path.to_string_lossy().into_owned(),
                path: path.clone(),
                choose: false,
            })
            .collect::<Vec<_>>();
        out.extend(self.places.clone());
        out.push(Place {
            label: "Choose folder".into(),
            caption: "Analyze another location".into(),
            path: PathBuf::new(),
            choose: true,
        });
        out
    }

    fn card_rect(&self, index: usize) -> Rect {
        let content_w = (self.rect.width() - 64.0).min(900.0);
        let cols = if content_w >= 700.0 { 2 } else { 1 };
        let w = (content_w - 16.0 * (cols - 1) as f64) / cols as f64;
        let narrow = self.rect.width() < 700.0;
        let height = if narrow { 78.0 } else { 88.0 };
        let stride = if narrow { 90.0 } else { 104.0 };
        let x0 = self.rect.center().x - content_w / 2.0 + (index % cols) as f64 * (w + 16.0);
        let y0 = self.rect.y0 + if narrow { 160.0 } else { 190.0 } + (index / cols) as f64 * stride;
        Rect::new(x0, y0, x0 + w, y0 + height)
    }
}

impl Default for DiskUsageApp {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for DiskUsageApp {
    fn drop(&mut self) {
        if let Some(cancellation) = &self.scan_cancellation {
            cancellation.cancel();
        }
    }
}

impl Widget for DiskUsageApp {
    fn update(&mut self, _state: &mut AppModel, _path: &ModelPath) -> bool {
        if let Some(id) = self.chart.take_activated() {
            let path = PathBuf::from(id);
            if path.file_name().is_some_and(|n| n != ".") {
                self.invoke(&analyze(&path));
                return true;
            }
        }
        false
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child = self.chart.build_layout(taffy, theme, node_map);
        let _ = taffy.set_style(
            child,
            Style {
                position: Position::Absolute,
                inset: TRect {
                    left: LengthPercentageAuto::length(24.0),
                    right: LengthPercentageAuto::length(24.0),
                    top: LengthPercentageAuto::length(TOP_H as f32),
                    bottom: LengthPercentageAuto::length(18.0),
                },
                size: Size {
                    width: Dimension::auto(),
                    height: Dimension::auto(),
                },
                ..Default::default()
            },
        );
        let node = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap_or(child);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x + l.size.width),
            origin.y + f64::from(l.location.y + l.size.height),
        );
        if let Ok(children) = taffy.children(node)
            && let Some(child) = children.first()
        {
            self.chart.apply_layout(taffy, *child, origin);
        }
    }
    #[allow(
        clippy::too_many_lines,
        reason = "the four small visual states share one retained canvas and theme context"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );
        match &self.view {
            View::Home => {
                tr.draw_text_weighted(
                    scene,
                    "Analyze storage",
                    theme.tokens.typography.scale.x3l,
                    theme.tokens.colors.text_primary,
                    transform
                        * Affine::translate((self.rect.center().x - 220.0, self.rect.y0 + 76.0)),
                    None,
                    TextWeight::Bold,
                );
                tr.draw_text(
                    scene,
                    "Choose a location to see what is taking up space",
                    theme.tokens.typography.scale.base,
                    theme.tokens.colors.text_dim,
                    transform
                        * Affine::translate((self.rect.center().x - 220.0, self.rect.y0 + 128.0)),
                    None,
                );
                for (i, place) in self.cards().iter().enumerate() {
                    let r = self.card_rect(i);
                    let hover = r.contains(self.pointer);
                    Button::paint(
                        scene,
                        transform,
                        tr,
                        theme,
                        &ButtonFace::new(r, &place.label).hovered(hover),
                    );
                    tr.draw_text(
                        scene,
                        &place.caption,
                        theme.tokens.typography.scale.xs,
                        theme.tokens.colors.text_dim,
                        transform * Affine::translate((r.x0 + 18.0, r.y1 - 25.0)),
                        Some((r.width() - 36.0) as f32),
                    );
                }
            }
            View::Loading(path) => {
                let back = self.back_rect();
                Button::paint(
                    scene,
                    transform,
                    tr,
                    theme,
                    &ButtonFace::new(back, "← Back").hovered(back.contains(self.pointer)),
                );
                let title = path.to_string_lossy();
                tr.draw_text_weighted(
                    scene,
                    "Calculating folder sizes…",
                    theme.tokens.typography.scale.x2l,
                    theme.tokens.colors.text_primary,
                    transform
                        * Affine::translate((
                            self.rect.center().x - 170.0,
                            self.rect.center().y - 24.0,
                        )),
                    None,
                    TextWeight::Semibold,
                );
                tr.draw_text(
                    scene,
                    &title,
                    theme.tokens.typography.scale.sm,
                    theme.tokens.colors.text_dim,
                    transform
                        * Affine::translate((
                            self.rect.center().x - 170.0,
                            self.rect.center().y + 18.0,
                        )),
                    None,
                );
            }
            View::Analysis(result) => {
                let back = self.back_rect();
                Button::paint(
                    scene,
                    transform,
                    tr,
                    theme,
                    &ButtonFace::new(back, "← Back").hovered(back.contains(self.pointer)),
                );
                let title = result.path.to_string_lossy();
                tr.draw_text_weighted(
                    scene,
                    &title,
                    theme.tokens.typography.scale.lg,
                    theme.tokens.colors.text_primary,
                    transform * Affine::translate((self.rect.x0 + 132.0, self.rect.y0 + 25.0)),
                    Some((self.rect.width() - 360.0).max(160.0) as f32),
                    TextWeight::Semibold,
                );
                let stats = format!(
                    "{} items  ·  {} unreadable",
                    result.files, result.unreadable
                );
                tr.draw_text(
                    scene,
                    &stats,
                    theme.tokens.typography.scale.xs,
                    theme.tokens.colors.text_dim,
                    transform * Affine::translate((self.rect.x1 - 220.0, self.rect.y0 + 30.0)),
                    None,
                );
                // Assign theme-derived colours immediately before the shared chart paints.
                let slices = result
                    .entries
                    .iter()
                    .enumerate()
                    .map(|(i, e)| {
                        let slice = PieSlice::new(
                            e.path.to_string_lossy(),
                            &e.name,
                            e.bytes as f64,
                            Self::palette(theme, i),
                        )
                        .with_detail(format!(
                            "{} · {} files",
                            bytes_si(e.bytes),
                            e.files
                        ));
                        if e.path.file_name().is_some_and(|name| name == ".") {
                            slice.non_interactive()
                        } else {
                            slice
                        }
                    })
                    .collect();
                self.chart.style_mut().tooltip_min_width = 220.0;
                self.chart.style_mut().tooltip_right_inset = if self.rect.width() >= 900.0 {
                    245.0
                } else {
                    0.0
                };
                if self.immediate {
                    self.chart.set_slices_immediate(slices);
                } else {
                    self.chart.set_slices(slices);
                }
                self.chart.draw(scene, transform, tr, theme, presence);
                if self.rect.width() >= 900.0 {
                    let x = self.rect.x1 - 245.0;
                    let mut y = self.rect.y0 + 122.0;
                    tr.draw_text_weighted(
                        scene,
                        "Largest folders",
                        theme.tokens.typography.scale.sm,
                        theme.tokens.colors.text_primary,
                        transform * Affine::translate((x, y - 34.0)),
                        None,
                        TextWeight::Semibold,
                    );
                    for (i, entry) in result.entries.iter().take(7).enumerate() {
                        scene.fill(
                            Fill::NonZero,
                            transform,
                            Self::palette(theme, i),
                            None,
                            &Circle::new((x + 5.0, y + 7.0), 5.0),
                        );
                        tr.draw_text(
                            scene,
                            &entry.name,
                            theme.tokens.typography.scale.sm,
                            theme.tokens.colors.text_primary,
                            transform * Affine::translate((x + 20.0, y)),
                            Some(125.0),
                        );
                        let percent = if result.total == 0 {
                            0.0
                        } else {
                            entry.bytes as f64 / result.total as f64 * 100.0
                        };
                        tr.draw_text(
                            scene,
                            &format!("{} · {percent:.0}%", bytes_si(entry.bytes)),
                            theme.tokens.typography.scale.xs,
                            theme.tokens.colors.text_dim,
                            transform * Affine::translate((x + 20.0, y + 22.0)),
                            None,
                        );
                        y += 58.0;
                    }
                } else {
                    // Preserve the category-to-colour mapping on compact
                    // screens instead of dropping the legend altogether.
                    let columns = 2;
                    let column_width = (self.rect.width() - 40.0) / f64::from(columns);
                    let y0 = self.rect.y1 - 68.0;
                    for (i, entry) in result.entries.iter().take(6).enumerate() {
                        let column = i % columns as usize;
                        let row = i / columns as usize;
                        let x = self.rect.x0 + 20.0 + column as f64 * column_width;
                        let y = y0 + row as f64 * 22.0;
                        scene.fill(
                            Fill::NonZero,
                            transform,
                            Self::palette(theme, i),
                            None,
                            &Circle::new((x + 4.0, y + 6.0), 4.0),
                        );
                        let percent = if result.total == 0 {
                            0.0
                        } else {
                            entry.bytes as f64 / result.total as f64 * 100.0
                        };
                        tr.draw_text(
                            scene,
                            &format!("{}  {percent:.0}%", entry.name),
                            theme.tokens.typography.scale.xs,
                            theme.tokens.colors.text_dim,
                            transform * Affine::translate((x + 14.0, y)),
                            Some((column_width - 18.0).max(40.0) as f32),
                        );
                    }
                }
            }
            View::Error(path, message) => {
                let back = self.back_rect();
                Button::paint(
                    scene,
                    transform,
                    tr,
                    theme,
                    &ButtonFace::new(back, "← Back").hovered(back.contains(self.pointer)),
                );
                tr.draw_text_weighted(
                    scene,
                    "Couldn’t analyze this folder",
                    theme.tokens.typography.scale.x2l,
                    theme.tokens.colors.text_primary,
                    transform * Affine::translate((48.0, 120.0)),
                    None,
                    TextWeight::Bold,
                );
                let detail = if path.as_os_str().is_empty() {
                    message.clone()
                } else {
                    format!("{} — {message}", path.display())
                };
                tr.draw_text(
                    scene,
                    &detail,
                    theme.tokens.typography.scale.sm,
                    theme.tokens.colors.text_dim,
                    transform * Affine::translate((48.0, 170.0)),
                    None,
                );
            }
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.pointer = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            }
        ) {
            match &self.view {
                View::Home => {
                    if let Some(place) = self
                        .cards()
                        .iter()
                        .enumerate()
                        .find(|(i, _)| self.card_rect(*i).contains(self.pointer))
                        .map(|(_, p)| p.clone())
                    {
                        if place.choose {
                            self.invoke(&DiskUsageEffect::Choose);
                        } else {
                            self.invoke(&analyze(&place.path));
                        }
                        *ctx.handled = true;
                    }
                }
                _ => {
                    if self.back_rect().contains(self.pointer) {
                        self.invoke(&DiskUsageEffect::Back);
                        *ctx.handled = true;
                    }
                }
            }
        }
    }
    fn accessibility(&self) -> Option<AccessNode> {
        let mut root = AccessNode::new(Role::Window).with_name("Disk Usage Analyzer");
        if matches!(self.view, View::Home) {
            root.children = self
                .cards()
                .iter()
                .map(|p| {
                    AccessNode::new(Role::Button)
                        .with_action_target(if p.choose {
                            "disk-usage.choose".into()
                        } else {
                            p.path.to_string_lossy().into_owned()
                        })
                        .with_name(&p.label)
                        .with_description(&p.caption)
                        .with_state(AccessState {
                            focusable: true,
                            ..Default::default()
                        })
                })
                .collect();
        } else {
            root.children = vec![
                AccessNode::new(Role::Button)
                    .with_action_target("disk-usage.back")
                    .with_name("Back")
                    .with_state(AccessState {
                        focusable: true,
                        ..Default::default()
                    }),
            ];
        }
        Some(root)
    }
    fn perform_action_at(
        &mut self,
        action: &ui::a11y::Action,
        key_path: &[u64],
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        if !matches!(action, ui::a11y::Action::Activate) {
            return false;
        }
        let Some(key) = key_path.last() else {
            return false;
        };
        if *key == ui::a11y::hash_key(&"disk-usage.back") {
            self.invoke(&DiskUsageEffect::Back);
            return true;
        }
        if *key == ui::a11y::hash_key(&"disk-usage.choose") {
            self.invoke(&DiskUsageEffect::Choose);
            return true;
        }
        if let Some(place) = self
            .cards()
            .into_iter()
            .filter(|place| !place.choose)
            .find(|place| ui::a11y::hash_key(&place.path.to_string_lossy()) == *key)
        {
            self.invoke(&analyze(&place.path));
            return true;
        }
        false
    }
    fn children(&self) -> Vec<&dyn Widget> {
        if matches!(self.view, View::Analysis(_)) {
            vec![&self.chart]
        } else {
            vec![]
        }
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if matches!(self.view, View::Analysis(_)) {
            vec![&mut self.chart]
        } else {
            vec![]
        }
    }
    fn name(&self) -> &'static str {
        "DiskUsageApp"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

impl DiskUsageApp {
    /// Show the chart's details for one slice, as hovering it does.
    pub(crate) fn inspect(&mut self, id: &str) {
        let _ = self.chart.inspect(id);
    }

    pub(crate) fn in_analysis(&self) -> bool {
        matches!(self.view, View::Analysis(_))
    }

    /// The common places, and the folders of the scan on screen: live
    /// folders on this machine that an agent or the palette can analyze.
    pub(crate) fn folder_rows(&self) -> Vec<LiveRow> {
        let mut rows: Vec<LiveRow> = self
            .places
            .iter()
            .map(|place| folder_row(&place.label, &place.path))
            .collect();
        if let View::Analysis(result) = &self.view {
            rows.extend(
                result
                    .entries
                    .iter()
                    .filter(|entry| entry.path.file_name().is_some_and(|name| name != "."))
                    .map(|entry| folder_row(&entry.name, &entry.path)),
            );
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The narrow home view keeps the common places and the folder picker
    /// visible beside the remembered folders.
    #[test]
    fn narrow_home_keeps_common_places_visible() {
        let mut app = DiskUsageApp::new();
        app.recent.extend([
            PathBuf::from("/home/you/Projects/tilo"),
            PathBuf::from(tilo_paths::WORKSPACE_MOUNT).join("design-system"),
            PathBuf::from("/one"),
            PathBuf::from("/two"),
            PathBuf::from("/three"),
        ]);
        app.rect = Rect::new(0.0, 0.0, 560.0, 720.0);
        assert_eq!(app.cards().len(), 6);
        assert!(app.cards().iter().any(|place| place.label == "File system"));
        assert_eq!(
            app.cards().last().map(|place| place.label.as_str()),
            Some("Choose folder")
        );
    }
}
