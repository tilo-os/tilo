//! The questions about the person's apps as the shell hosts them, one at a
//! time, modal above the desktop, their answers carried out on a worker:
//! "Uninstall <name>?" ([`crate::shell::uninstall_sheet`]), asked for
//! through [`crate::shell::app_list::ask`], and "Add <name> to apps?"
//! ([`crate::shell::add_app_sheet`]), asked the first time a downloaded
//! program runs ([`crate::shell::downloaded_apps::take_ask`]).

use taffy::NodeId;
use tilo_desktop::notifications::{Notification, NotificationSeverity};
use ui::widget::Widget as _;
use winit::event::WindowEvent;

use super::ShellOverlayHost;
use crate::shell::i18n::{t, t_args};
use crate::shell::uninstall_sheet::{Answer, Asking};
use crate::shell::{add_app_sheet, app_list, downloaded_apps};

/// The question on screen.
pub enum AppSheet {
    Uninstall(Asking),
    Add(add_app_sheet::Asking),
}

impl AppSheet {
    pub fn dialog(&self) -> &dyn ui::widget::Widget {
        match self {
            Self::Uninstall(asking) => &asking.dialog,
            Self::Add(asking) => &asking.dialog,
        }
    }

    pub fn dialog_mut(&mut self) -> &mut ui::widget::ModalDialog<ui::widget::Column> {
        match self {
            Self::Uninstall(asking) => &mut asking.dialog,
            Self::Add(asking) => &mut asking.dialog,
        }
    }
}

impl ShellOverlayHost {
    /// Whether "Uninstall <name>?" is up.
    pub fn uninstall_sheet_open(&self) -> bool {
        matches!(self.app_sheet, Some(AppSheet::Uninstall(_)))
    }

    /// The uninstall sheet on screen, for tests and captures.
    pub fn uninstall_sheet(&self) -> Option<&Asking> {
        match &self.app_sheet {
            Some(AppSheet::Uninstall(asking)) => Some(asking),
            _ => None,
        }
    }

    /// The "Add <name> to apps?" sheet on screen, for tests and captures.
    pub fn add_app_sheet(&self) -> Option<&add_app_sheet::Asking> {
        match &self.app_sheet {
            Some(AppSheet::Add(asking)) => Some(asking),
            _ => None,
        }
    }

    /// Ask about `item` as [`app_list::ask`] does once the list has it: the
    /// seam for captures and tests, which never read the machine's apps.
    #[doc(hidden)]
    pub fn ask_uninstall(&mut self, item: app_list::AppListItem) {
        self.show_app_sheet(AppSheet::Uninstall(Asking::new(item)));
    }

    /// Ask about a downloaded program as its first run does: the seam for
    /// captures and tests.
    #[doc(hidden)]
    pub fn ask_add_app(&mut self, ask: add_app_sheet::Ask) {
        self.show_app_sheet(AppSheet::Add(add_app_sheet::Asking::new(ask)));
    }

    fn show_app_sheet(&mut self, sheet: AppSheet) {
        self.app_sheet = Some(sheet);
        self.app_sheet_nodes = None;
        self.app_sheet_changed = true;
    }

    /// Carry out the answer the person gave, then open the next question
    /// asked for. Returns whether anything on screen changed.
    pub(super) fn pump_app_sheets(&mut self) -> bool {
        // What finished uninstalls did, or why they could not.
        let notices = app_list::drain_notices();
        let posted = !notices.is_empty();
        for notice in notices {
            self.post_uninstall_notice(notice);
        }
        posted | self.pump_app_sheet()
    }

    fn pump_app_sheet(&mut self) -> bool {
        if let Some(Some(answer)) = match &self.app_sheet {
            Some(AppSheet::Add(asking)) => Some(asking.answer()),
            _ => None,
        } {
            self.close_app_sheet();
            downloaded_apps::answer(&answer);
            return true;
        }
        let uninstall_answer = match &self.app_sheet {
            Some(AppSheet::Uninstall(asking)) => asking.answer(),
            _ => None,
        };
        if let Some(answer) = uninstall_answer {
            self.close_app_sheet();
            if let Answer::Uninstall { item, remove_data } = answer {
                let name = item.name.clone();
                if let Err(detail) = app_list::uninstall(item, remove_data) {
                    self.post_uninstall_notice(
                        Notification::new(
                            t_args("app-list-uninstall-failed", &[("name", name.into())]),
                            detail,
                        )
                        .with_severity(NotificationSeverity::Error),
                    );
                }
            }
            return true;
        }
        if self.app_sheet.is_some() {
            return false;
        }
        match app_list::take_asked() {
            Some(Ok(item)) => {
                self.show_app_sheet(AppSheet::Uninstall(Asking::new(item)));
                return true;
            }
            Some(Err(notice)) => {
                self.post_uninstall_notice(notice);
                return true;
            }
            None => {}
        }
        match downloaded_apps::take_ask() {
            Some(ask) => {
                self.show_app_sheet(AppSheet::Add(add_app_sheet::Asking::new(ask)));
                true
            }
            None => false,
        }
    }

    fn close_app_sheet(&mut self) {
        if self.app_sheet.take().is_some() {
            self.app_sheet_nodes = None;
            self.app_sheet_changed = true;
        }
    }

    fn post_uninstall_notice(&self, notice: Notification) {
        if let Some(chrome) = &self.chrome {
            let mut state = chrome.borrow_mut();
            state.push_notification_full(notice.with_app_name(t("app-list-app-name")));
            state.bump();
        }
    }

    /// The sheet's window node, placed over the display it opens on, out of
    /// the desktop's flex flow, holding the sheet.
    pub(super) fn build_app_sheet_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &ui::theme::Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn ui::widget::Widget>,
    ) -> Option<NodeId> {
        let area = self.overlay_area();
        let origin = self.rect.origin();
        let sheet = self.app_sheet.as_mut()?;
        let dialog = sheet.dialog_mut().build_layout(taffy, theme, node_map);
        let style = taffy::Style {
            position: taffy::Position::Absolute,
            inset: taffy::Rect {
                left: taffy::LengthPercentageAuto::length(ui::num::f32_of_f64(area.x0 - origin.x)),
                top: taffy::LengthPercentageAuto::length(ui::num::f32_of_f64(area.y0 - origin.y)),
                right: taffy::LengthPercentageAuto::auto(),
                bottom: taffy::LengthPercentageAuto::auto(),
            },
            size: taffy::Size {
                width: taffy::Dimension::length(ui::num::f32_of_f64(area.width())),
                height: taffy::Dimension::length(ui::num::f32_of_f64(area.height())),
            },
            ..taffy::Style::default()
        };
        let window = ui::widget::TaffyTreeExt::parent_node(taffy, style, &[dialog]);
        self.app_sheet_nodes = Some((window, dialog));
        Some(window)
    }

    /// Place the sheet in its window, measured against the display.
    pub(super) fn apply_app_sheet_layout(&mut self, taffy: &taffy::TaffyTree<()>) {
        let area = self.overlay_area();
        let origin = self.rect.origin();
        let (Some(sheet), Some((window, dialog))) = (self.app_sheet.as_mut(), self.app_sheet_nodes)
        else {
            return;
        };
        let placed = ui::widget::layout_rect(taffy, window, origin);
        ui::widget::modal_dialog::within_window(window, area, || {
            sheet
                .dialog_mut()
                .apply_layout(taffy, dialog, placed.origin());
        });
    }

    /// Route one event to the sheet while it is up. It is modal: every
    /// event is its own, except Tab and the activation keys it left unused,
    /// which go on to the window's focus handling.
    pub(super) fn handle_app_sheet_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        let (Some(sheet), Some((_, node))) = (self.app_sheet.as_mut(), self.app_sheet_nodes) else {
            // Up but not laid out yet: nothing on screen to act on, and
            // nothing behind it may act either.
            *ctx.handled = true;
            return;
        };
        sheet
            .dialog_mut()
            .handle_event(event, &mut ctx.reborrow(ctx.path, node));
        *ctx.handled = true;
        self.pump_app_sheets();
    }

    pub(super) fn draw_app_sheet(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::theme::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if self.app_sheet_nodes.is_none() {
            return;
        }
        let area = self.overlay_area();
        let Some(sheet) = self.app_sheet.as_mut() else {
            return;
        };
        // The sheet's scrim covers the display it measures against.
        let outer = presence.viewport.replace(Some(area));
        sheet
            .dialog_mut()
            .draw_overlay(scene, transform, text_renderer, theme, presence);
        presence.viewport.set(outer);
    }

    /// Whether the tree must be rebuilt for the sheet.
    pub(super) fn take_app_sheet_changed(&mut self) -> bool {
        std::mem::take(&mut self.app_sheet_changed)
            | self
                .app_sheet
                .as_mut()
                .is_some_and(|sheet| sheet.dialog_mut().take_structure_dirty())
    }
}
