//! Where the conversation sits: the rects of the timeline, the composers and
//! the call tile, and the two drawers that flank the conversation — docked
//! beside it on a wide window, floating over it on a narrow one.
//!
//! The shell keeps the room's channels in its sidebar (`resource.rs`); this
//! window is the conversation beside them, and the detail drawer (people, or
//! the open thread) at its trailing edge. The toolkit's `SidebarDrawer`
//! decides, from the width, whether that drawer docks beside the conversation
//! or floats over it behind a scrim.

use ui::theme::Theme;
use ui::widget::{SidebarDrawer, WindowBarLink};
use vello::kurbo::Rect;

use crate::geometry::Chrome;

use super::ChatApp;

impl ChatApp {
    /// The trailing detail drawer, summoned on demand. It docks while the
    /// conversation beside it keeps its minimum width.
    pub(super) fn detail_drawer() -> SidebarDrawer {
        let m = ui::tokens::Metrics::BASE;
        SidebarDrawer::new()
            .right()
            .closed()
            .docked_width(f64::from(m.inspector_width))
            .overlay_width(f64::from(m.inspector_width))
            .breakpoint(f64::from(m.content_min_width + m.inspector_width))
    }

    /// True when the shell's sidebar, the channels, shows.
    pub(crate) fn sidebar_open(&self) -> bool {
        self.bar
            .as_ref()
            .is_some_and(WindowBarLink::is_sidebar_open)
    }

    /// True when the detail drawer (people / thread) shows.
    pub(crate) fn right_flap_open(&self) -> bool {
        self.right_flap.is_open()
    }

    /// The detail drawer floats over the conversation.
    pub(crate) fn right_overlay(&self) -> bool {
        self.right_flap.is_overlay()
    }

    /// The conversation column: what the docked detail drawer leaves.
    pub(crate) fn content_rect(&self) -> Rect {
        let content = self.right_flap.content_rect();
        if content.width() > 0.0 {
            content
        } else {
            self.rect
        }
    }

    pub(crate) fn center_left(&self) -> f64 {
        self.content_rect().x0
    }

    pub(crate) fn center_right(&self) -> f64 {
        self.content_rect().x1
    }

    /// Reserve the height measured by the same editor that will be drawn.
    pub(crate) fn composer_height(&self) -> f64 {
        let chrome = &self.chrome;
        let pad = chrome.composer_min_h - chrome.body_line;
        (self.composer_main.measured_height() + pad)
            .clamp(chrome.composer_min_h, chrome.composer_max_h)
    }

    pub(crate) fn composer_rect(&self) -> Rect {
        let pad = self.chrome.composer_pad;
        // The note strip under the field is always kept, so a line appearing
        // in it never moves the field.
        let bottom = self.rect.y1 - pad.max(self.chrome.composer_note_h);
        let top_limit = self.rect.y0 + pad;
        let h = self
            .composer_height()
            .min((bottom - top_limit).max(self.chrome.composer_min_h));
        Rect::new(
            self.center_left() + self.chrome.gutter,
            bottom - h,
            self.center_right() - self.chrome.gutter,
            bottom,
        )
    }

    /// The thread's composer: the same control and height rule as the main
    /// composer, at the bottom of the thread panel `panel`.
    pub(crate) fn thread_composer_rect(&self, panel: Rect) -> Rect {
        let pad = self.chrome.composer_pad;
        let chrome = &self.chrome;
        let inset = chrome.composer_min_h - chrome.body_line;
        let h = (self.composer_thread.measured_height() + inset)
            .clamp(chrome.composer_min_h, chrome.composer_max_h);
        let bottom = panel.y1 - pad.max(chrome.composer_note_h);
        Rect::new(panel.x0 + pad, bottom - h, panel.x1 - pad, bottom)
    }

    /// Whether this window draws the floating call window over its content.
    /// At enlarged text the labelled Call control remains reachable in the
    /// header instead; a fixed floating huddle would cover the thread and
    /// composer instead of forming a usable secondary surface.
    pub(crate) fn app_draws_huddle(&self) -> bool {
        self.call.active
            && !self.call.expanded
            && self.layout_text_scale <= 1.25
            && ((!self.call.placement.is_return_pending()
                && matches!(
                    self.call.placement.attachment(),
                    ui::floating_surface::Attachment::App { .. }
                ))
                || !self.call.placement.is_desktop_covered())
    }

    /// The timeline: from the top of the conversation to the composer; the
    /// conversation's name and controls are the window bar's. While the call
    /// window floats over it, the messages keep clear of it and wrap beside
    /// it, so it never covers what is being said.
    pub(crate) fn messages_rect(&self) -> Rect {
        let composer = self.composer_rect();
        let mut rect = Rect::new(
            self.center_left(),
            self.rect.y0,
            self.center_right(),
            composer.y0 - self.chrome.avatar_gap,
        );
        if self.app_draws_huddle() {
            let call = self.huddle_panel();
            let overlaps = call.x0 < rect.x1 && call.x1 > rect.x0 && call.y0 < rect.y1;
            if overlaps {
                rect.x1 = rect.x1.min(call.x0 - self.chrome.avatar_gap).max(rect.x0);
            }
        }
        rect
    }

    /// The detail drawer's panel. A thread is content, not a secondary
    /// sidebar: on a narrow window it becomes a pushed page over the whole
    /// window rather than a cramped floating panel.
    pub(crate) fn thread_rect(&self) -> Rect {
        if self.right_overlay() && self.thread_open.is_some() {
            return self.rect;
        }
        self.right_flap.sidebar_rect()
    }

    /// The open thread fills the window as a pushed page.
    pub(crate) fn thread_is_page(&self) -> bool {
        self.right_overlay() && self.thread_open.is_some() && self.right_flap_open()
    }

    /// Resolve the detail drawer against the window: docked or floating, and
    /// the rect it leaves the conversation. Cheap and deterministic, so the
    /// event path calls it too. Returns true while it slides.
    pub(crate) fn reconcile_flaps(&mut self) -> bool {
        if self.rect.width() < 1.0 {
            return false;
        }
        self.resolve_detail_drawer(self.rect);
        self.flap_animating = self.right_flap.is_animating();
        self.flap_animating
    }

    /// Resolve the detail drawer against `content`, the conversation.
    /// Floating over the conversation, pins and
    /// people end above the composer (and so does the scrim that dismisses
    /// them), so its emoji and Send stay in reach; a thread is a page of its
    /// own there and takes the whole window.
    fn resolve_detail_drawer(&mut self, content: Rect) {
        self.right_flap.resolve(content);
        if !self.right_flap.is_overlay() || self.thread_open.is_some() {
            return;
        }
        if let Some(top) = self.composer_top
            && top > content.y0
            && top < content.y1
        {
            self.right_flap
                .resolve(Rect::new(content.x0, content.y0, content.x1, top));
        }
    }

    /// Adopt `theme`'s metrics and advance the detail drawer's reveal for
    /// this frame (once per paint).
    pub(crate) fn advance_flaps(&mut self, theme: &Theme) {
        self.chrome = Chrome::of(theme);
        self.right_flap.scale_for(theme);
        if !self.reconcile_flaps() && self.rect.width() < 1.0 {
            return;
        }
        self.resolve_detail_drawer(self.rect);
        self.right_flap.tick(theme);
        self.flap_animating = self.right_flap.is_animating();
    }

    /// The host-tile video rect for the current call mode, including the
    /// dragged overlay offset (docked huddle only — the expanded grid is
    /// anchored). The single source of truth for both the `VideoPlayer` node
    /// placement and the surrounding call chrome.
    pub(crate) fn call_video_rect(&self) -> Rect {
        if self.call.expanded {
            // The host is the first cell of the responsive participant grid,
            // so the tile (and the VideoPlayer surface beneath it) fills its
            // share of the surface at every width — never a fixed fraction
            // that leaves the rest of the overlay empty.
            self.overlay_host_rect()
        } else {
            self.call
                .placement
                .rect(self.rect, self.desktop_floating_bounds())
        }
    }

    pub(crate) fn desktop_floating_bounds(&self) -> Rect {
        if self.floating_surface_bounds.area() > 0.0 {
            self.floating_surface_bounds
        } else {
            self.rect
        }
    }
}
