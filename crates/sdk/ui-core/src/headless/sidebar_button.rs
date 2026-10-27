//! The window bar's sidebar button, clicked as a person clicks it.
//!
//! The bar is the desktop's ([`super::window_bar`]): a click on its sidebar
//! button lands on the bar's row as the desktop lays it out, and the row
//! hands the app what was pressed, as the live window's bar does. What the
//! press did is read back from the window's sidebar ([`NavShell`]), the
//! slots the app declares and the frame laid out after it.

use super::FrameHarness;
use crate::widget::{NavPane, NavShell, SidebarPlacement, Widget};

/// Where the window's sidebar stands after a press on the bar's button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SidebarShown {
    /// The bar shows its sidebar button on ("Hide sidebar").
    pub bar: bool,
    /// The window's shell holds its sidebar shown.
    pub shell: bool,
    /// The frame shows the sidebar: a docked or drawn column with width, or
    /// the push stack's front screen.
    pub frame: bool,
}

impl SidebarShown {
    /// The bar, the shell and the frame agree on one answer.
    #[must_use]
    pub fn agreed(self) -> Option<bool> {
        (self.bar == self.shell && self.shell == self.frame).then_some(self.shell)
    }
}

impl FrameHarness {
    /// Click the sidebar button where the window's bar, as the desktop
    /// draws it across the window, puts it, and hand the app what the press
    /// runs ([`Self::press_window_slot`]).
    ///
    /// # Errors
    /// When the bar shows no sidebar button, or the click runs nothing.
    pub fn click_window_sidebar_button(&mut self) -> Result<(), String> {
        let width = f64::from(self.viewport.0);
        let mut bar = self.painted_window_bar(width);
        let rect = bar.toolbar_row().slots().sidebar;
        if rect.area() <= 0.0 {
            return Err(format!("the bar shows no sidebar button at {width} px"));
        }
        let at = rect.center();
        let row = bar.toolbar_row_mut();
        if !(row.press(at) && row.release(at)) {
            return Err(format!("the sidebar button took no press at {at:?}"));
        }
        self.deliver_bar_events(&mut bar)
    }

    /// Where the window's sidebar stands now: what the bar's button shows,
    /// what the window's shell holds and what its frame lays out. `None`
    /// while the bar has no sidebar button or no shell with a sidebar is
    /// laid out.
    #[must_use]
    pub fn window_sidebar_shown(&mut self) -> Option<SidebarShown> {
        let width = f64::from(self.viewport.0);
        let bar = self
            .painted_window_bar(width)
            .toolbar_row()
            .toolbar()?
            .sidebar?;
        let shell = window_sidebar_shell(self.root.as_ref())?;
        let geometry = shell.geometry();
        let frame = match geometry.sidebar {
            SidebarPlacement::None => false,
            SidebarPlacement::Screen => shell.front_pane() == NavPane::Sidebar,
            SidebarPlacement::Docked | SidebarPlacement::Drawer => {
                geometry.sidebar_column.width() > 0.5
            }
        };
        Some(SidebarShown {
            bar,
            shell: shell.is_sidebar_open(),
            frame,
        })
    }

    /// Click the window bar's sidebar button `presses` times, each followed
    /// by the frames the window draws until its motion has landed, and
    /// return where the sidebar stood before and after each.
    ///
    /// # Errors
    /// When a click fails ([`Self::click_window_sidebar_button`]) or the
    /// window has no sidebar to read back.
    pub fn press_window_sidebar_button(
        &mut self,
        presses: usize,
    ) -> Result<Vec<SidebarShown>, String> {
        let _landed = crate::motion::testing::settled();
        self.relayout_until_still();
        let read = |harness: &mut Self| {
            harness
                .window_sidebar_shown()
                .ok_or_else(|| "the window shows no sidebar button over a sidebar".to_owned())
        };
        let mut states = vec![read(self)?];
        for _ in 0..presses {
            self.click_window_sidebar_button()?;
            self.relayout_until_still();
            states.push(read(self)?);
        }
        Ok(states)
    }

    /// Lay out frames as the window's host draws them: the one asked for,
    /// then more only while the app asks.
    fn relayout_until_still(&mut self) {
        self.relayout();
        for _ in 0..64 {
            let schedule = self.frame_schedule();
            if !(schedule.needs_animation_frame || schedule.queued_work) {
                return;
            }
            self.relayout();
        }
    }
}

/// The shell whose sidebar the window bar's button toggles: the first laid
/// out shell, depth first, that has a sidebar.
fn window_sidebar_shell(widget: &dyn Widget) -> Option<&NavShell> {
    if let Some(shell) = widget
        .as_any()
        .and_then(|any| any.downcast_ref::<NavShell>())
        && shell.has_sidebar()
    {
        return Some(shell);
    }
    widget.children().into_iter().find_map(window_sidebar_shell)
}
