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
    /// what the window's shell holds and what its frame lays out.
    ///
    /// # Errors
    /// While the bar has no sidebar button or no shell with a sidebar is
    /// laid out.
    pub fn window_sidebar_shown(&mut self) -> Result<SidebarShown, String> {
        let width = f64::from(self.viewport.0);
        let toolbar = self
            .painted_window_bar(width)
            .toolbar_row()
            .toolbar()
            .cloned()
            .ok_or("the window bar shows none of the app's slots")?;
        let bar = toolbar
            .sidebar
            .ok_or_else(|| format!("the window bar shows no sidebar button: {toolbar:?}"))?;
        let shell = window_sidebar_shell(self.root.as_ref()).ok_or_else(|| {
            format!(
                "no nav shell with a sidebar is laid out under the bar: {}",
                widget_outline(self.root.as_ref(), 4)
            )
        })?;
        let geometry = shell.geometry();
        let frame = match geometry.sidebar {
            SidebarPlacement::None => false,
            SidebarPlacement::Screen => shell.front_pane() == NavPane::Sidebar,
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
        self.relayout_until_still()?;
        let read = |harness: &mut Self| harness.window_sidebar_shown();
        let mut states = vec![read(self)?];
        for _ in 0..presses {
            self.click_window_sidebar_button()?;
            self.relayout_until_still()
                .map_err(|error| format!("after click {}: {error}", states.len()))?;
            states.push(read(self)?);
        }
        Ok(states)
    }

    /// Click the window bar's sidebar button three times and check that
    /// each click flipped the sidebar, with the bar, the window's shell and
    /// its frame agreeing every time, whatever opened it. Returns whether
    /// the sidebar was shown before the first click.
    ///
    /// # Errors
    /// What the clicks did instead, as a sentence naming each state.
    pub fn check_window_sidebar_button(&mut self) -> Result<bool, String> {
        let states = self.press_window_sidebar_button(3)?;
        let mut expected = None;
        for (presses, state) in states.iter().enumerate() {
            let Some(shown) = state.agreed() else {
                return Err(format!(
                    "after {presses} clicks the bar, shell and frame disagree: {state:?}; \
                     the shell now: {}",
                    self.sidebar_diagnosis()
                ));
            };
            let wanted = expected.unwrap_or(shown);
            if shown != wanted {
                return Err(format!(
                    "click {presses} left the sidebar {} (states: {states:?})",
                    if shown { "shown" } else { "hidden" }
                ));
            }
            expected = Some(!shown);
        }
        Ok(states.first().and_then(|state| state.agreed()) == Some(true))
    }

    /// Where the window's shell stands, for a failure to say.
    fn sidebar_diagnosis(&self) -> String {
        let schedule = self.frame_schedule();
        window_sidebar_shell(self.root.as_ref()).map_or_else(
            || "none laid out".to_owned(),
            |shell| {
                let geometry = shell.geometry();
                format!(
                    "{:?} at {:?}, open {}, animating {}, window {schedule:?}",
                    geometry.sidebar,
                    geometry.sidebar_column,
                    shell.is_sidebar_open(),
                    shell.needs_animation_frame(),
                )
            },
        )
    }

    /// Lay out frames as the window's host draws them: the one asked for,
    /// then more only while the app asks. Motion lands at once here, so a
    /// window still asking after many frames never comes to rest.
    ///
    /// # Errors
    /// When the window never stops asking for frames.
    fn relayout_until_still(&mut self) -> Result<(), String> {
        self.relayout();
        for _ in 0..64 {
            let schedule = self.frame_schedule();
            if !(schedule.needs_animation_frame || schedule.queued_work) {
                return Ok(());
            }
            self.relayout();
        }
        Err(format!(
            "the window still asks for frames 64 frames after the motion landed: {}",
            self.sidebar_diagnosis()
        ))
    }
}

/// The shell whose sidebar the window bar's button toggles: the first laid
/// out shell, depth first, that has a sidebar. A widget standing in for a
/// shell (a framework app's root, which lists the shell's parts as its own)
/// answers for it as a capability.
fn window_sidebar_shell(widget: &dyn Widget) -> Option<&NavShell> {
    let shell = widget
        .as_any()
        .and_then(|any| any.downcast_ref::<NavShell>())
        .or_else(|| crate::widget::capability::capability::<NavShell>(widget));
    if let Some(shell) = shell
        && shell.has_sidebar()
    {
        return Some(shell);
    }
    widget.children().into_iter().find_map(window_sidebar_shell)
}

/// The names of `widget` and its descendants `depth` levels down, for a
/// failure that found no shell.
fn widget_outline(widget: &dyn Widget, depth: usize) -> String {
    let children = widget.children();
    if depth == 0 || children.is_empty() {
        return widget.name().to_owned();
    }
    let inner: Vec<String> = children
        .into_iter()
        .map(|child| widget_outline(child, depth - 1))
        .collect();
    format!("{}[{}]", widget.name(), inner.join(", "))
}
