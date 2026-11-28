//! The window chrome a driven app lays out: its sidebar, header row and columns.
//!
//! Everything here is read off the app as its last frame left it: the row of parts the
//! driver's window draws ([`super::super::part_bar::PartBar`]) and the header row and columns
//! the app's own widgets laid out, so what a test reads is what the window shows.

use vello::kurbo::Rect;

use super::Driver;
use crate::headless::InspectionNode;
use crate::widget::window_header::{content_bar, root_header_row};
use crate::widget::window_toolbar_row::SIDEBAR_TARGET;
use crate::{widget::ColumnFrame, widget::WindowFrame, widget::WindowHeader};
use tilo_ui_base::a11y::AccessNode;
use tilo_ui_base::widget::Widget;
use tilo_ui_nav::{NavPane, NavShell, SidebarPlacement};

/// The sidebar toggle a shell's own header row holds while the app's parts are not in a bar.
const ROW_SIDEBAR_TOGGLE: &str = "nav-shell-sidebar";

/// The sidebar button where the window shows it.
struct SidebarButton {
    /// Where it stands in the window; zero while it is folded into the bar's menu.
    spot: Rect,
    /// It reads "Hide sidebar".
    shown: bool,
}

/// Where the window's sidebar stands after a press on the bar's button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SidebarShown {
    /// The bar shows its sidebar button on ("Hide sidebar").
    pub bar: bool,
    /// The window's shell holds its sidebar shown.
    pub shell: bool,
    /// The frame shows the sidebar: a docked or drawn column with width, or the push stack's front
    /// screen.
    pub frame: bool,
}

impl SidebarShown {
    /// The bar, the shell and the frame agree on one answer.
    #[must_use]
    pub fn agreed(self) -> Option<bool> {
        (self.bar == self.shell && self.shell == self.frame).then_some(self.shell)
    }
}

impl Driver {
    /// Show `body` as a window of a Tilo session shows a tile: inside the shell's window frame
    /// ([`WindowFrame`]), sized to the window, which hands its bar to the header row `body`
    /// draws, or draws a bar of its own above a `body` that draws none.
    #[must_use]
    pub fn in_window_frame(
        state: crate::state::State,
        body: tilo_ui_base::widget::Element,
        viewport: (u32, u32),
        theme: tilo_ui_base::theme::Theme,
    ) -> Self {
        Self::framed(state, body, None, viewport, theme)
    }

    /// [`Self::in_window_frame`], the window named `name` as the session names a tile by what
    /// it shows: its frame's own bar shows the name while `body` draws no header row to hold
    /// the bar, and assistive technology reads it as the window's.
    #[must_use]
    pub fn in_named_window_frame(
        state: crate::state::State,
        body: tilo_ui_base::widget::Element,
        name: impl Into<String>,
        viewport: (u32, u32),
        theme: tilo_ui_base::theme::Theme,
    ) -> Self {
        Self::framed(state, body, Some(name.into()), viewport, theme)
    }

    fn framed(
        state: crate::state::State,
        body: tilo_ui_base::widget::Element,
        name: Option<String>,
        viewport: (u32, u32),
        theme: tilo_ui_base::theme::Theme,
    ) -> Self {
        use crate::widget::arrange::{Fit, Part, Stack};
        use tilo_ui_base::component::Described as _;
        let frame = WindowFrame::from_props(
            "window",
            crate::widget::WindowFrameProps {
                name,
                focused: true,
                body: Some(body),
                ..crate::widget::WindowFrameProps::default()
            },
        );
        // A frame takes the size its host gives it, as the session's canvas places it.
        let mut part = Part::new(Box::new(frame));
        part.fit = Fit::fill(1.0);
        let window = Stack::stack().fill_width().fill_height().push_part(part);
        Self::new_with_theme(state, Box::new(window), viewport, theme)
    }

    /// The sidebar button the window shows: the part in its bar's row (or folded into the
    /// row's menu), or, while the app's own header row stands at the top, the toggle in it.
    fn sidebar_button(&self) -> Option<SidebarButton> {
        fn in_bar(nodes: &[AccessNode]) -> Option<&AccessNode> {
            nodes.iter().find_map(|node| {
                if node.action_target.as_deref() == Some(SIDEBAR_TARGET) {
                    Some(node)
                } else {
                    in_bar(&node.children)
                }
            })
        }
        fn in_row(node: &InspectionNode) -> Option<(Rect, Option<String>)> {
            if node.test_id.as_deref() == Some(ROW_SIDEBAR_TOGGLE)
                && node.rect[2] > 0.0
                && node.rect[3] > 0.0
            {
                let [x, y, width, height] = node.rect;
                let name = node.semantics.as_ref().and_then(|a| a.name.clone());
                return Some((Rect::new(x, y, x + width, y + height), name));
            }
            node.children.iter().find_map(in_row)
        }
        let parts = self.window_parts();
        if let Some(part) = in_bar(&parts) {
            return Some(SidebarButton {
                spot: part.bounds.map_or(Rect::ZERO, |b| b.to_rect()),
                shown: part.state.checked == Some(true),
            });
        }
        let (spot, name) = in_row(&self.inspection().root)?;
        let hide = tilo_ui_base::i18n::I18nManager::lookup_plain("window-toolbar-hide-sidebar");
        Some(SidebarButton {
            spot,
            shown: name.as_deref() == Some(hide.as_str()),
        })
    }

    /// Whether the window shows a sidebar button a person can click
    /// ([`Self::click_window_sidebar_button`]): not none at all, nor one folded into the bar's menu.
    #[must_use]
    pub fn shows_window_sidebar_button(&self) -> bool {
        self.sidebar_button()
            .is_some_and(|button| button.spot.area() > 0.0)
    }

    /// Click the window's sidebar button where it stands, as a person clicks it: the part in the
    /// window bar's row, or the toggle in the app's own header row while that row is the
    /// window's top.
    ///
    /// # Errors
    /// When the window shows no sidebar button it can be clicked on: none at all, or one folded
    /// into the bar's menu.
    pub fn click_window_sidebar_button(&mut self) -> Result<(), String> {
        let width = self.viewport().0;
        let button = self
            .sidebar_button()
            .ok_or_else(|| format!("the window shows no sidebar button at {width} px"))?;
        if button.spot.area() <= 0.0 {
            return Err(format!(
                "the window's sidebar button is folded into its bar's menu at {width} px"
            ));
        }
        let at = button.spot.center();
        self.click_at(at.x, at.y);
        Ok(())
    }

    /// Where the window's sidebar stands: what the window's sidebar button says (where it shows
    /// the way back in the button's place, the sidebar's own screen counts as shown), what the
    /// app's shell holds and what its frame lays out.
    ///
    /// # Errors
    /// While the app lays out no shell with a sidebar.
    pub fn window_sidebar_shown(&self) -> Result<SidebarShown, String> {
        let root = self.app.root_widget.as_ref();
        let shell = sidebar_shell(root).ok_or_else(|| {
            format!(
                "the app lays out no shell with a sidebar: {}",
                outline(root, 4)
            )
        })?;
        let geometry = shell.geometry();
        let front_is_sidebar = shell.front_pane() == NavPane::Sidebar;
        let bar = self.sidebar_button().map_or(
            geometry.sidebar == SidebarPlacement::Screen && front_is_sidebar,
            |button| button.shown,
        );
        let frame = match geometry.sidebar {
            SidebarPlacement::None => false,
            SidebarPlacement::Screen => front_is_sidebar,
            SidebarPlacement::Docked | SidebarPlacement::Drawer => {
                geometry.sidebar_column.width() > 0.5
            }
        };
        Ok(SidebarShown {
            bar,
            shell: shell.is_sidebar_open(),
            frame,
        })
    }

    /// The shell's window frame the window is, when it is one.
    fn window_frame(&self) -> Option<&WindowFrame> {
        crate::widget::window_header::window_frame_of(self.app.root_widget.as_ref())
    }

    /// The header row the window's content draws: under a shell's window frame, its content's.
    fn header_row(&self) -> Option<&dyn WindowHeader> {
        match self.window_frame() {
            Some(frame) => frame.content_header_row(),
            None => root_header_row(self.app.root_widget.as_ref()),
        }
    }

    /// Whether the app's own header row holds the window's bar: the row stands at the window's
    /// top with the window's controls at its trailing end, and no bar stands above it. A shell's
    /// window frame hands its bar to the row of the content it hosts
    /// ([`crate::widget::WindowFrame`]); an app shown on its own wears the driver's bar above its
    /// row instead.
    #[must_use]
    pub fn row_holds_bar(&self) -> bool {
        match self.window_frame() {
            Some(frame) => frame.held_bar().is_some(),
            None => content_bar(self.app.root_widget.as_ref()).is_some(),
        }
    }

    /// The height of the header row the app draws of its own, below whatever bar the window
    /// shows: zero while it draws none, or while that row is the window's bar
    /// ([`Self::row_holds_bar`]). Beside a bar, a row this tall is a second one.
    #[must_use]
    pub fn window_header_row_height(&self) -> f64 {
        if self.row_holds_bar() {
            return 0.0;
        }
        self.header_row()
            .map_or(0.0, WindowHeader::header_row_height)
    }

    /// The height of the bar the window shows above the app's own content, as the last frame
    /// laid it out: under a shell's window frame, the frame's own bar (none while the content's
    /// header row holds the bar); otherwise the driver's row of parts
    /// ([`Self::present_in_window_bar`]). Beside a header row of the app's
    /// ([`Self::window_header_row_height`]), it is the first of two bars.
    #[must_use]
    pub fn window_bar_height(&self) -> f64 {
        match self.window_frame() {
            Some(frame) => frame.own_bar_rect().height(),
            None => self.app.layout.part_bar_inset,
        }
    }

    /// The window's bar as a person reads it across the window's top: every control it shows,
    /// where it stands and what it is called. Under a shell's window frame that is the controls
    /// standing in the frame's bar row, which is the content's header row while that row holds the
    /// bar (`part` `"row"`) and the frame's own bar otherwise (`"frame"`); outside one, the parts of
    /// the driver's row ([`Self::window_parts`], `"part"`). The bar paints no words outside the
    /// window's tree, so `labels` is empty and the window leaves no `spare` to a location.
    #[must_use]
    pub fn window_bar_layout(&self) -> crate::window_toolbar::BarLayout {
        use crate::window_toolbar::{BarControl, BarLayout};
        fn in_row(node: &InspectionNode, part: &'static str, row: Rect, out: &mut Vec<BarControl>) {
            let [x, y, width, height] = node.rect;
            let rect = Rect::new(x, y, x + width, y + height);
            if let Some(access) = &node.semantics
                && access.role == tilo_ui_base::a11y::Role::Button
                && rect.area() > 0.0
                && row.contains(rect.center())
            {
                let mut control =
                    BarControl::new(part, node.test_id.clone().unwrap_or_default(), rect)
                        .named(access.name.clone().unwrap_or_default());
                if node.widget == "OverflowButton" {
                    control = control.menu();
                }
                out.push(control);
                return;
            }
            for child in &node.children {
                in_row(child, part, row, out);
            }
        }
        fn parts(nodes: &[AccessNode], out: &mut Vec<BarControl>) {
            for node in nodes {
                if let (Some(target), Some(bounds)) = (node.action_target.as_deref(), node.bounds) {
                    out.push(
                        BarControl::new("part", target, bounds.to_rect())
                            .named(node.name.clone().unwrap_or_default()),
                    );
                }
                parts(&node.children, out);
            }
        }
        let mut controls = Vec::new();
        if let Some(frame) = self.window_frame() {
            let part = if frame.held_bar().is_some() {
                "row"
            } else {
                "frame"
            };
            let row = frame.bar_rect();
            if row.area() > 0.0 {
                in_row(&self.inspection().root, part, row, &mut controls);
            }
        } else if let Some(part) =
            crate::widget::window_header::content_bar(self.app.root_widget.as_ref())
        {
            // The app's own row holds the bar: its furniture stands at the row's trailing end.
            use crate::widget::window_controls::WindowControl;
            let furniture = crate::widget::window_header::RowFurniture::of(part);
            for (control, rect) in furniture.controls.iter() {
                let part = match control {
                    WindowControl::Minimize => "minimize",
                    WindowControl::Maximize => "maximize",
                    WindowControl::Close => "close",
                };
                controls.push(BarControl::new(part, control.target(), rect));
            }
        } else if !self.bar_at_rest {
            // At rest the bar stands over the window's top edge, out of the way: nothing of it
            // shows until the person reaches for it.
            parts(&self.window_parts(), &mut controls);
        }
        BarLayout {
            spare: 0.0,
            controls,
            labels: Vec::new(),
        }
    }

    /// Where each column of the app's window stands and the header row it draws at its top, in
    /// window coordinates, as the last frame laid them out.
    #[must_use]
    pub fn column_frames(&self) -> Vec<ColumnFrame> {
        self.header_row()
            .map(WindowHeader::column_frames)
            .unwrap_or_default()
    }

    /// What the window's header rows say, as a person reads them across the top of its
    /// columns: the names of the titles and controls standing in a column's header row, left to
    /// right.
    #[must_use]
    pub fn header_row_names(&self) -> Vec<String> {
        fn visit(node: &InspectionNode, rows: &[Rect], names: &mut Vec<(String, Rect)>) {
            let [x, y, width, height] = node.rect;
            let rect = Rect::new(x, y, x + width, y + height);
            if rect.area() > 0.0
                && rows.iter().any(|row| row.contains(rect.center()))
                && let Some(name) = node
                    .semantics
                    .as_ref()
                    .and_then(|access| access.name.as_deref())
                    .filter(|name| !name.trim().is_empty())
                // A control and the label inside it say one thing once.
                && names.last().map(|(last, _)| last.as_str()) != Some(name)
            {
                names.push((name.to_owned(), rect));
            }
            for child in &node.children {
                visit(child, rows, names);
            }
        }
        let rows: Vec<Rect> = self
            .column_frames()
            .into_iter()
            .map(|frame| frame.header)
            .filter(|header| header.area() > 0.0)
            .collect();
        let mut names = Vec::new();
        if !rows.is_empty() {
            visit(&self.inspection().root, &rows, &mut names);
        }
        // The tree holds the columns in the order the app built them, not where they stand.
        names.sort_by(|(_, a), (_, b)| a.x0.total_cmp(&b.x0).then(a.y0.total_cmp(&b.y0)));
        names.into_iter().map(|(name, _)| name).collect()
    }

    /// Where the button named `name` stands in the window's header rows, as a person finds it
    /// reading across the top of the window's columns ([`Self::header_row_names`]).
    pub(super) fn header_row_button(&self, name: &str) -> Option<Rect> {
        fn visit(node: &InspectionNode, rows: &[Rect], name: &str) -> Option<Rect> {
            let [x, y, width, height] = node.rect;
            let rect = Rect::new(x, y, x + width, y + height);
            if rect.area() > 0.0
                && rows.iter().any(|row| row.contains(rect.center()))
                && node.semantics.as_ref().is_some_and(|access| {
                    access.role == tilo_ui_base::a11y::Role::Button
                        && access.name.as_deref() == Some(name)
                })
            {
                return Some(rect);
            }
            node.children
                .iter()
                .find_map(|child| visit(child, rows, name))
        }
        let rows: Vec<Rect> = self
            .column_frames()
            .into_iter()
            .map(|frame| frame.header)
            .filter(|header| header.area() > 0.0)
            .collect();
        if rows.is_empty() {
            return None;
        }
        visit(&self.inspection().root, &rows, name)
    }
}

/// The shell whose sidebar the window's sidebar button toggles: the first laid out shell, depth
/// first, that has a sidebar.
fn sidebar_shell(widget: &dyn Widget) -> Option<&NavShell> {
    let shell = widget
        .as_any()
        .and_then(|any| any.downcast_ref::<NavShell>())
        .or_else(|| tilo_ui_base::widget::capability::capability::<NavShell>(widget));
    if let Some(shell) = shell
        && shell.has_sidebar()
    {
        return Some(shell);
    }
    widget.children().into_iter().find_map(sidebar_shell)
}

/// The widget tree under `widget`, `depth` levels deep, by name: what a failure that found no
/// shell shows instead.
fn outline(widget: &dyn Widget, depth: usize) -> String {
    let children = widget.children();
    if depth == 0 || children.is_empty() {
        return widget.name().to_owned();
    }
    let inner: Vec<String> = children
        .into_iter()
        .map(|child| outline(child, depth - 1))
        .collect();
    format!("{}[{}]", widget.name(), inner.join(", "))
}
