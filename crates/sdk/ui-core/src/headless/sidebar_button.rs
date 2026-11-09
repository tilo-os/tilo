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

    /// Where the window's sidebar stands now: what the bar's button shows
    /// (hidden where the bar shows the way back in its place), what the
    /// window's shell holds and what its frame lays out.
    ///
    /// # Errors
    /// While the bar has neither a sidebar button nor the way back, or no
    /// shell with a sidebar is laid out.
    pub fn window_sidebar_shown(&mut self) -> Result<SidebarShown, String> {
        let width = f64::from(self.viewport.0);
        let toolbar = self
            .painted_window_bar(width)
            .toolbar_row()
            .toolbar()
            .cloned()
            .ok_or("the window bar shows none of the app's slots")?;
        // A narrow window's bar puts the button aside for the way back once a
        // screen in front covers the sidebar's own: that bar shows it hidden.
        let back = toolbar
            .leading
            .iter()
            .any(|action| action.id == crate::widget::WINDOW_BACK);
        let bar = match toolbar.sidebar {
            Some(shown) => shown,
            None if back => false,
            None => {
                return Err(format!(
                    "the window bar shows no sidebar button: {toolbar:?}"
                ));
            }
        };
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
    /// return where the sidebar stood before and after each. A narrow window
    /// whose button lives on the sidebar's own screen stops after the click
    /// that puts the sidebar away: the screen in front has its way back
    /// alone.
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
        self.reach_the_bars_sidebar_button()?;
        let read = |harness: &mut Self| harness.window_sidebar_shown();
        let mut states = vec![read(self)?];
        for _ in 0..presses {
            if !self.window_bar_controls().0 {
                break;
            }
            self.click_window_sidebar_button()?;
            self.relayout_until_still()
                .map_err(|error| format!("after click {}: {error}", states.len()))?;
            states.push(read(self)?);
        }
        Ok(states)
    }

    /// Whether the window's bar shows its sidebar button, and whether it
    /// shows its way back to the screen that holds one.
    fn window_bar_controls(&mut self) -> (bool, bool) {
        let width = f64::from(self.viewport.0);
        let toolbar = self
            .painted_window_bar(width)
            .toolbar_row()
            .toolbar()
            .cloned()
            .unwrap_or_default();
        let back = toolbar
            .leading
            .iter()
            .any(|action| action.id == crate::widget::WINDOW_BACK);
        (toolbar.sidebar.is_some(), back)
    }

    /// Get to the screen whose bar holds the sidebar button: a narrow window
    /// puts the button aside for the way back on a screen that went
    /// somewhere deeper, and the sidebar's own screen holds it.
    ///
    /// # Errors
    /// When the bar shows no sidebar button and no way back to the screen
    /// that holds one.
    fn reach_the_bars_sidebar_button(&mut self) -> Result<(), String> {
        let width = f64::from(self.viewport.0);
        let (sidebar, back) = self.window_bar_controls();
        if sidebar {
            return Ok(());
        }
        if !back {
            return Err(format!(
                "the bar shows no sidebar button at {width} px and no way back to the screen that \
                 holds it; the shell now: {}",
                self.sidebar_diagnosis()
            ));
        }
        self.press_window_slot(crate::window_toolbar::ToolbarEvent::Back);
        self.relayout_until_still()?;
        if !self.window_bar_controls().0 {
            return Err(format!(
                "the screen the bar's way back leads to shows no sidebar button at {width} px; \
                 the shell now: {}",
                self.sidebar_diagnosis()
            ));
        }
        Ok(())
    }

    /// Click the window bar's sidebar button up to three times and check
    /// that each click flipped the sidebar, with the bar, the window's shell
    /// and its frame agreeing every time, whatever opened it. Returns
    /// whether the sidebar was shown before the first click.
    ///
    /// # Errors
    /// What the clicks did instead, as a sentence naming each state.
    pub fn check_window_sidebar_button(&mut self) -> Result<bool, String> {
        let states = self.press_window_sidebar_button(3)?;
        if states.len() < 2 {
            return Err(format!(
                "the bar's sidebar button took no click at {} px; the shell now: {}",
                self.viewport.0,
                self.sidebar_diagnosis()
            ));
        }
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
                let mut askers = Vec::new();
                animation_askers(shell, "NavShell", &mut askers);
                format!(
                    "{:?} at {:?}, open {}, animating {} (asked by {askers:?}), motions {:?}, \
                     owner wakes pending {}, window {schedule:?}",
                    geometry.sidebar,
                    geometry.sidebar_column,
                    shell.is_sidebar_open(),
                    shell.needs_animation_frame(),
                    crate::motion::frames_wanted_by(crate::time::now()),
                    self.has_pending_owner_models(),
                )
            },
        )
    }

    /// Lay out frames as the window's host draws them: the one asked for,
    /// then more while the app asks, until the shell's motion has landed.
    /// Motion lands at once here, so a shell still moving after many
    /// frames never comes to rest. Work the window queues of its own (a
    /// scenario's service that never answers) is not the sidebar's: it may
    /// go on.
    ///
    /// # Errors
    /// When the shell's motion never lands.
    fn relayout_until_still(&mut self) -> Result<(), String> {
        self.relayout();
        for _ in 0..64 {
            let schedule = self.frame_schedule();
            let shell_moving = window_sidebar_shell(self.root.as_ref())
                .is_some_and(NavShell::needs_animation_frame);
            if !(shell_moving || schedule.needs_animation_frame) {
                return Ok(());
            }
            self.relayout();
        }
        Err(format!(
            "the sidebar still moves 64 frames after its motion landed: {}",
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

/// The paths of the deepest widgets under `widget` that ask for animation
/// frames, for a failure whose window never comes to rest.
fn animation_askers(widget: &dyn Widget, path: &str, askers: &mut Vec<String>) {
    if !widget.needs_animation_frame() {
        return;
    }
    let before = askers.len();
    widget.for_each_child(&mut |child| {
        animation_askers(child, &format!("{path}/{}", child.name()), askers);
    });
    if askers.len() == before {
        askers.push(path.to_owned());
    }
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
