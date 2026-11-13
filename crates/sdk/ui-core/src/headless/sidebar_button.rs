//! The window bar's sidebar button, clicked as a person clicks it. The bar is the desktop's
//! ([`super::window_bar`]): a click on its sidebar button lands on the bar's row as the desktop lays it
//! out, and the row hands the app what was pressed, as the live window's bar does.

use super::FrameHarness;
use crate::widget::{NavPane, NavShell, SidebarPlacement, Widget};
use vello::kurbo::Rect;

/// The shell's own sidebar toggle, the button the app's header row holds while the row is the window's
/// bar.
const SIDEBAR_TOGGLE: &str = "nav-shell-sidebar";

const BACK_BUTTON: &str = "nav-shell-back";

struct BarSidebarButton {
    spot: Rect,
    /// The desktop's bar draws it as one of the app's slots, so the press lands on the bar's row.
    on_the_row: bool,
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

impl FrameHarness {
    /// The sidebar button the window's bar shows: the desktop's own slot, or — while the app's header
    /// row is the bar — the shell's toggle in that row.
    fn bar_sidebar_button(&mut self) -> Option<BarSidebarButton> {
        let width = f64::from(self.viewport.0);
        let bar = self.painted_window_bar(width);
        let row = bar.toolbar_row();
        if let Some(shown) = row.toolbar().and_then(|toolbar| toolbar.sidebar) {
            let spot = row.slots().sidebar;
            return (spot.area() > 0.0).then_some(BarSidebarButton {
                spot,
                on_the_row: true,
                shown,
            });
        }
        let (spot, name) = self.row_button(SIDEBAR_TOGGLE)?;
        let hide = crate::i18n::I18nManager::lookup_plain("window-toolbar-hide-sidebar");
        Some(BarSidebarButton {
            spot,
            on_the_row: false,
            shown: name.as_deref() == Some(hide.as_str()),
        })
    }

    /// The button the app's header row holds for `id`, while that row is the window's bar
    /// ([`FrameHarness::row_holds_bar`]): where it stands in the window, and the words assistive
    /// technology reads for it.
    fn row_button(&self, id: &str) -> Option<(Rect, Option<String>)> {
        fn find(node: &super::InspectionNode, id: &str) -> Option<(Rect, Option<String>)> {
            if node.test_id.as_deref() == Some(id) && node.rect[2] > 0.0 && node.rect[3] > 0.0 {
                let [x, y, width, height] = node.rect;
                return Some((
                    Rect::new(x, y, x + width, y + height),
                    node.semantics
                        .as_ref()
                        .and_then(|semantics| semantics.name.clone()),
                ));
            }
            node.children.iter().find_map(|child| find(child, id))
        }
        if !self.row_holds_bar() {
            return None;
        }
        find(&self.inspection().root, id)
    }

    /// Click the sidebar button where the window's bar, as the desktop or the app's own row draws it
    /// across the window, puts it, and hand the app what the press runs ([`Self::press_window_slot`]).
    pub fn click_window_sidebar_button(&mut self) -> Result<(), String> {
        self.invalidate_local();
        let width = f64::from(self.viewport.0);
        let Some(button) = self.bar_sidebar_button() else {
            return Err(format!("the bar shows no sidebar button at {width} px"));
        };
        let at = button.spot.center();
        if !button.on_the_row {
            self.click_at(at.x, at.y);
            return Ok(());
        }
        let mut bar = self.painted_window_bar(width);
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
    /// Quiet: reads the bar, the window's shell and its frame; paints a scratch bar and changes
    /// nothing a frame reads.
    ///
    /// # Errors
    /// While no shell with a sidebar is laid out under the bar.
    pub fn window_sidebar_shown(&mut self) -> Result<SidebarShown, String> {
        let button = self.bar_sidebar_button();
        let shell = window_sidebar_shell(self.root.as_ref()).ok_or_else(|| {
            format!(
                "no nav shell with a sidebar is laid out under the bar: {}",
                widget_outline(self.root.as_ref(), 4)
            )
        })?;
        let geometry = shell.geometry();
        let front_is_sidebar = shell.front_pane() == NavPane::Sidebar;
        let bar = match button {
            Some(button) => button.shown,
            // The bar shows the way back in the button's place: hidden on a screen in front of
            // the sidebar's own, and shown on the sidebar's own screen, which is what Back there
            // returns to. A shell whose own row is the bar may hold neither on that screen: the
            // person leaves it by choosing what it lists, and nothing on the row says where the
            // sidebar stands but the row the shell laid out.
            None => geometry.sidebar == SidebarPlacement::Screen && front_is_sidebar,
        };
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

    /// Click the window bar's sidebar button `presses` times, each followed by the frames the window
    /// draws until its motion has landed, and return where the sidebar stood before and after each.
    pub fn press_window_sidebar_button(
        &mut self,
        presses: usize,
    ) -> Result<Vec<SidebarShown>, String> {
        self.invalidate_local();
        let _landed = crate::motion::testing::settled();
        self.relayout_until_still()?;
        let read = |harness: &mut Self| harness.window_sidebar_shown();
        let mut states = vec![read(self)?];
        if !self.window_bar_controls().0 {
            self.reach_the_bars_sidebar_button()?;
            self.relayout_until_still()?;
            let reached = read(self)?;
            if !self.window_bar_controls().0 && reached.agreed() != Some(true) {
                return Err(format!(
                    "the screen the bar's way back leads to shows no sidebar button at {} px and \
                     is not the sidebar's own; the shell now: {}",
                    self.viewport.0,
                    self.sidebar_diagnosis()
                ));
            }
            // Getting to the screen that holds the button is navigation, not a click: it
            // counts where it showed the sidebar (the screen it led to is the sidebar's
            // own), and otherwise stands where the first state would.
            if reached.agreed() == states[0].agreed() {
                states[0] = reached;
            } else {
                states.push(reached);
            }
        }
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

    /// Whether the window's bar shows its sidebar button, and — where it shows none — whether it shows
    /// its way back to the screen that holds one.
    fn window_bar_controls(&mut self) -> (bool, bool) {
        let button = self.bar_sidebar_button().is_some();
        (button, !button && self.window_bar_shows_back())
    }

    /// Whether the window's bar shows the way back to the screen that holds its sidebar button: the
    /// desktop's own slot, or — while the app's header row is the bar — the shell's Back button in that
    /// row.
    fn window_bar_shows_back(&mut self) -> bool {
        let width = f64::from(self.viewport.0);
        let toolbar = self
            .painted_window_bar(width)
            .toolbar_row()
            .toolbar()
            .cloned()
            .unwrap_or_default();
        toolbar
            .leading
            .iter()
            .any(|action| action.id == crate::widget::WINDOW_BACK)
            || self.row_button(BACK_BUTTON).is_some()
    }

    fn press_the_bars_back(&mut self) -> Result<(), String> {
        let width = f64::from(self.viewport.0);
        let toolbar = self
            .painted_window_bar(width)
            .toolbar_row()
            .toolbar()
            .cloned()
            .unwrap_or_default();
        if toolbar
            .leading
            .iter()
            .any(|action| action.id == crate::widget::WINDOW_BACK)
        {
            self.press_window_slot(crate::window_toolbar::ToolbarEvent::Back);
            return Ok(());
        }
        let (spot, _) = self
            .row_button(BACK_BUTTON)
            .ok_or_else(|| format!("the bar shows no way back at {width} px"))?;
        let at = spot.center();
        self.click_at(at.x, at.y);
        Ok(())
    }

    /// Take the bar's way back: a narrow window puts its sidebar button aside for it on a screen that
    /// went somewhere deeper, and the screen it leads to holds the button — or is the sidebar's own, in
    /// an app whose header row is the bar.
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
        self.press_the_bars_back()?;
        Ok(())
    }

    /// Click the window bar's sidebar button up to three times and check that each click flipped the
    /// sidebar, with the bar, the window's shell and its frame agreeing every time, whatever opened it.
    /// Returns whether the sidebar was shown before the first click.
    pub fn check_window_sidebar_button(&mut self) -> Result<bool, String> {
        self.invalidate_local();
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

    /// Lay out frames as the window's host draws them: the one asked for, then more while the app asks,
    /// until the shell's motion has landed. Motion lands at once here, so a shell still moving after
    /// many frames never comes to rest. Work the window queues of its own (a scenario's service that
    /// never answers) is not the sidebar's: it may go on.
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

/// The shell whose sidebar the window bar's button toggles: the first laid out shell, depth first, that
/// has a sidebar.
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

/// The paths of the deepest widgets under `widget` that ask for animation frames, for a failure whose
/// window never comes to rest.
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
