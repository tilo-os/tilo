//! "Uninstall <name>?" ([`crate::shell::uninstall_sheet`]) as the shell
//! hosts it: one question at a time from [`crate::shell::app_list::ask`],
//! modal above the desktop, its answer carried out on a worker.

use taffy::NodeId;
use tilo_desktop::notifications::{Notification, NotificationSeverity};
use ui::widget::Widget as _;
use winit::event::WindowEvent;

use super::ShellOverlayHost;
use crate::shell::app_list;
use crate::shell::i18n::{t, t_args};
use crate::shell::uninstall_sheet::{Answer, Asking};

impl ShellOverlayHost {
    /// Whether "Uninstall <name>?" is up.
    pub fn uninstall_sheet_open(&self) -> bool {
        self.uninstall_sheet.is_some()
    }

    /// The sheet on screen, for tests and captures.
    pub fn uninstall_sheet(&self) -> Option<&Asking> {
        self.uninstall_sheet.as_ref()
    }

    /// Ask about `item` as [`app_list::ask`] does once the list has it: the
    /// seam for captures and tests, which never read the machine's apps.
    #[doc(hidden)]
    pub fn ask_uninstall(&mut self, item: app_list::AppListItem) {
        self.uninstall_sheet = Some(Asking::new(item));
        self.uninstall_nodes = None;
        self.uninstall_changed = true;
    }

    /// Carry out the answer the person gave, then open the next question
    /// asked for. Returns whether anything on screen changed.
    pub(super) fn pump_uninstall(&mut self) -> bool {
        // What finished uninstalls did, or why they could not.
        let notices = app_list::drain_notices();
        let posted = !notices.is_empty();
        for notice in notices {
            self.post_uninstall_notice(notice);
        }
        posted | self.pump_uninstall_sheet()
    }

    fn pump_uninstall_sheet(&mut self) -> bool {
        if let Some(answer) = self.uninstall_sheet.as_ref().and_then(Asking::answer) {
            self.close_uninstall_sheet();
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
        if self.uninstall_sheet.is_some() {
            return false;
        }
        match app_list::take_asked() {
            Some(Ok(item)) => {
                self.uninstall_sheet = Some(Asking::new(item));
                self.uninstall_changed = true;
                true
            }
            Some(Err(notice)) => {
                self.post_uninstall_notice(notice);
                true
            }
            None => false,
        }
    }

    fn close_uninstall_sheet(&mut self) {
        if self.uninstall_sheet.take().is_some() {
            self.uninstall_nodes = None;
            self.uninstall_changed = true;
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
    pub(super) fn build_uninstall_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &ui::theme::Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn ui::widget::Widget>,
    ) -> Option<NodeId> {
        let area = self.overlay_area();
        let origin = self.rect.origin();
        let asking = self.uninstall_sheet.as_mut()?;
        let dialog = asking.dialog.build_layout(taffy, theme, node_map);
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
        self.uninstall_nodes = Some((window, dialog));
        Some(window)
    }

    /// Place the sheet in its window, measured against the display.
    pub(super) fn apply_uninstall_layout(&mut self, taffy: &taffy::TaffyTree<()>) {
        let area = self.overlay_area();
        let origin = self.rect.origin();
        let (Some(asking), Some((window, dialog))) =
            (self.uninstall_sheet.as_mut(), self.uninstall_nodes)
        else {
            return;
        };
        let placed = ui::widget::layout_rect(taffy, window, origin);
        ui::widget::modal_dialog::within_window(window, area, || {
            asking.dialog.apply_layout(taffy, dialog, placed.origin());
        });
    }

    /// Route one event to the sheet while it is up. It is modal: every
    /// event is its own, except Tab and the activation keys it left unused,
    /// which go on to the window's focus handling.
    pub(super) fn handle_uninstall_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        let (Some(asking), Some((_, node))) = (self.uninstall_sheet.as_mut(), self.uninstall_nodes)
        else {
            // Up but not laid out yet: nothing on screen to act on, and
            // nothing behind it may act either.
            *ctx.handled = true;
            return;
        };
        asking
            .dialog
            .handle_event(event, &mut ctx.reborrow(ctx.path, node));
        *ctx.handled = true;
        self.pump_uninstall();
    }

    pub(super) fn draw_uninstall(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::theme::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if self.uninstall_nodes.is_none() {
            return;
        }
        let area = self.overlay_area();
        let Some(asking) = self.uninstall_sheet.as_mut() else {
            return;
        };
        // The sheet's scrim covers the display it measures against.
        let outer = presence.viewport.replace(Some(area));
        asking
            .dialog
            .draw_overlay(scene, transform, text_renderer, theme, presence);
        presence.viewport.set(outer);
    }

    /// Whether the tree must be rebuilt for the sheet.
    pub(super) fn take_uninstall_changed(&mut self) -> bool {
        std::mem::take(&mut self.uninstall_changed)
            | self
                .uninstall_sheet
                .as_mut()
                .is_some_and(|asking| asking.dialog.take_structure_dirty())
    }
}
