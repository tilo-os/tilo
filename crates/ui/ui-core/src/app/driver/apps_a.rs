//! What a window that is not on a display still answers: the notice its bar shows, the buttons
//! there that run it, and the browser fonts its text asks a host to load.

use vello::kurbo::Rect;
use winit::event::{ElementState, MouseButton};

use super::Driver;
use tilo_ui_base::a11y::AccessNode;
use crate::part_row::{PartRow, PartRowHost, RowInput, RowPlace};
use crate::widget::tabs::TOOLBAR_ROW_HEIGHT;
use crate::widget::window_toolbar_row::{NOTICE_TARGET_PREFIX, ToolbarRow};
use crate::window_toolbar::ToolbarNotice;

/// How the window's bar shows the notice up ([`Driver::window_bar_notice`]).
#[derive(Clone, Debug, PartialEq)]
pub struct WindowBarNotice {
    /// What it says.
    pub text: String,
    /// Where its text stands, its kind's icon first.
    pub text_bounds: Rect,
    /// Its text stands whole, not cut short.
    pub whole: bool,
    /// Its buttons, in order, where they stand (none: it has none, or the row cannot fit its
    /// text).
    pub buttons: Vec<WindowBarNoticeButton>,
    /// How many of the app's actions and the leading actions that do not move through the content
    /// stand in the row beside it, not in the overflow menu: what yields to it.
    pub beside: usize,
    /// Its strip under the bar's row, where the window is compact (none: it stands inline, in the
    /// location's place).
    pub strip: Option<Rect>,
    /// The whole bar's height, the strip included: where the app's frame starts.
    pub bar_height: f64,
}

/// The notice's button as the window's bar shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowBarNoticeButton {
    /// What a press on it does.
    pub name: String,
    /// Where it stands.
    pub bounds: Rect,
    /// It names itself, not only its icon.
    pub labelled: bool,
}

impl Driver {
    /// How the window's bar, as the shell draws it across a window `width` wide, shows the notice
    /// up (none: no notice is up).
    #[must_use]
    pub fn window_bar_notice(&self, width: f64) -> Option<WindowBarNotice> {
        let bar = self.notice_bar(width);
        let row = bar.toolbar_row();
        shown_notice(row, TOOLBAR_ROW_HEIGHT + row.strip_height())
    }

    /// What the window's bar, as the shell draws it across a window `width` wide, tells assistive
    /// technology it shows: the app's parts, and the notice where the location stands.
    #[must_use]
    pub fn window_bar_accessibility(&self, width: f64) -> Vec<tilo_ui_base::a11y::AccessNode> {
        self.notice_bar(width).toolbar_row().accessibility()
    }

    /// Click the notice's button `index` where the window's bar, as the shell draws it across a
    /// window `width` wide, puts it, and hand the app what the press runs.
    ///
    /// # Errors
    /// When the notice has no button `index`, or the press runs nothing.
    pub fn click_window_notice_action(&mut self, width: f64, index: usize) -> Result<(), String> {
        let mut bar = self.notice_bar(width);
        let height = TOOLBAR_ROW_HEIGHT + bar.toolbar_row().strip_height();
        let at = shown_notice(bar.toolbar_row(), height)
            .and_then(|notice| {
                notice
                    .buttons
                    .get(index)
                    .map(|button| button.bounds.center())
            })
            .ok_or_else(|| format!("no notice button {index} stands in the bar at {width} px"))?;
        let scale = self.scale();
        let modifiers = self.app.input.modifiers;
        let input = RowInput {
            scale,
            modifiers: &modifiers,
            platform: self.app.input.platform,
        };
        // What the press runs reaches the app on the window's bar, as a press on the live bar's
        // does.
        let running = self.running();
        let host: &mut dyn PartRowHost = &mut *self.app;
        let _ = bar.route(
            host,
            &tilo_ui_base::synthetic_input::cursor_moved(at.x * scale, at.y * scale),
            &input,
        );
        let (pressed, _) = bar.route(
            host,
            &tilo_ui_base::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Left),
            &input,
        );
        let _ = bar.route(
            host,
            &tilo_ui_base::synthetic_input::mouse_input(ElementState::Released, MouseButton::Left),
            &input,
        );
        drop(running);
        self.relayout();
        if !pressed {
            return Err(format!("the notice button {index} took no press at {at:?}"));
        }
        Ok(())
    }

    /// Run the notice's button `index` through its semantic action, as assistive technology and
    /// automation do, on the window's bar as the shell draws it across a window `width` wide, and
    /// hand the app what it runs.
    ///
    /// # Errors
    /// When the bar lists no notice button `index`, or it runs nothing.
    pub fn invoke_window_notice_action(&mut self, width: f64, index: usize) -> Result<(), String> {
        let mut bar = self.notice_bar(width);
        fn targets(node: &AccessNode, out: &mut Vec<String>) {
            if let Some(target) = node.action_target.as_deref()
                && target.starts_with(NOTICE_TARGET_PREFIX)
            {
                out.push(target.to_owned());
            }
            for child in &node.children {
                targets(child, out);
            }
        }
        let mut found = Vec::new();
        for node in bar.accessibility() {
            targets(&node, &mut found);
        }
        let target = found
            .get(index)
            .ok_or_else(|| format!("the bar lists no notice button {index} at {width} px"))?
            .clone();
        let running = self.running();
        let ran = bar.perform(&mut *self.app, &target, &tilo_ui_base::a11y::Action::Activate);
        drop(running);
        self.relayout();
        if !ran {
            return Err(format!("{target} ran nothing"));
        }
        Ok(())
    }

    /// Whether [`Self::take_browser_font_demands`] has anything to hand over.
    #[must_use]
    pub fn browser_font_demands_pending(&self) -> bool {
        self.app.text_renderer.browser_font_demands_pending()
    }

    /// Drain optional canonical font assets demanded by the most recent authoritative shaping pass.
    pub fn take_browser_font_demands(&mut self) -> Vec<String> {
        self.app.text_renderer.take_browser_font_demands()
    }

    /// Install one verified optional font and ask for the layout that takes it up, so the next
    /// frame lays out once for every font installed before it. Whether it changed the shaping.
    ///
    /// # Errors
    /// When the font is not one the catalog offers or its bytes do not match `sha256`.
    pub fn install_browser_font(
        &mut self,
        name: &str,
        bytes: Vec<u8>,
        sha256: &str,
    ) -> Result<bool, tilo_ui_base::font_catalog::FontCatalogError> {
        let changed = self
            .app
            .text_renderer
            .install_browser_font(name, bytes, sha256)?;
        if changed {
            self.request_layout();
        }
        Ok(changed)
    }

    /// The window's bar laid out across a window `width` wide, as the app declares it, without
    /// showing it: where a notice's geometry is read from.
    fn notice_bar(&self, width: f64) -> Box<PartRow> {
        let _settled = tilo_ui_base::motion::testing::settled();
        let _running = self.running();
        let safe = self.app.layout.safe_area;
        let mut row = Box::new(PartRow::new());
        row.refresh(
            &self.app.theme,
            Some(RowPlace {
                left: safe.left,
                top: safe.top,
                right: safe.right,
                window: (width, f64::from(self.viewport().1)),
                app_name: "",
            }),
            self.window_history_control(),
        );
        row
    }

    /// Device pixels per logical pixel, as the driver's surface scales input.
    fn scale(&self) -> f64 {
        self.surface.scale.get()
    }
}

/// How `row` shows the notice up: in the strip a compact window puts under it, or in the
/// location's place.
fn shown_notice(row: &ToolbarRow, bar_height: f64) -> Option<WindowBarNotice> {
    let toolbar = row.toolbar()?;
    let parts = row.parts();
    let plain_leading = parts
        .leading
        .iter()
        .zip(&toolbar.leading)
        .filter(|(rect, action)| !action.navigates && rect.area() > 0.0)
        .count();
    let beside = parts.actions.len() + plain_leading;
    let buttons = |notice: &ToolbarNotice, rects: &[Rect], labelled: &dyn Fn(usize) -> bool| {
        notice
            .actions
            .iter()
            .zip(rects)
            .enumerate()
            .map(|(i, (action, rect))| WindowBarNoticeButton {
                name: action.name.clone(),
                bounds: *rect,
                labelled: labelled(i),
            })
            .collect()
    };
    if let Some((notice, text, whole, rects)) = row.strip_notice() {
        return Some(WindowBarNotice {
            text: notice.text.clone(),
            text_bounds: text,
            whole,
            buttons: buttons(notice, &rects, &|_| true),
            beside,
            strip: Some(row.strip_rect()),
            bar_height,
        });
    }
    let notice = toolbar.notice.as_ref()?;
    Some(WindowBarNotice {
        text: notice.text.clone(),
        text_bounds: parts.location,
        whole: row.notice_whole().unwrap_or(false),
        buttons: buttons(notice, &parts.notice, &|i| {
            parts.notice_labelled.get(i).copied().unwrap_or(false)
        }),
        beside,
        strip: None,
        bar_height,
    })
}
