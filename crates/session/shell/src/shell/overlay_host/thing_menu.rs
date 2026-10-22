//! A right-click on anything the desktop itself draws (a search result, a
//! notification, a strip or one of its windows, a desk widget, a launchpad
//! card, a file in the file chooser) opens the shared menu on it, which
//! offers to tell an agent about that thing. The thing travels as a typed
//! reference, never as text: the agent reaches it only through actions.

use ui::shared_selection::SharedSelection;
use vello::kurbo::Point;
use winit::event::{ElementState, MouseButton, WindowEvent};

use super::ShellOverlayHost;
use crate::shell::notification::ChromeNotifications;
use crate::shell::selection_agent::SelectionSeed;

/// `label`, the item `id` of kind `kind`, as the desktop shows it.
fn desktop_thing(app_label: String, id: String, kind: &str, label: &str) -> SelectionSeed {
    let reference = SharedSelection::of_text(label)
        .at(tilo_actions::SelectionPlace::item(id.as_str()))
        .in_resource(tilo_actions::ActionResource {
            id,
            kind: kind.to_owned(),
            label: label.to_owned(),
        });
    SelectionSeed::new(
        tilo_desktop_model::commands::APP_ID,
        app_label,
        Some(reference),
    )
}

impl ShellOverlayHost {
    /// A secondary press on something the desktop draws opens its menu.
    /// Whether one opened (the press is then the menu's).
    pub(super) fn open_thing_menu_on_press(&mut self, event: &WindowEvent) -> bool {
        if !matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            }
        ) {
            return false;
        }
        let Some(at) = self.last_cursor else {
            return false;
        };
        let Some(seed) = self.thing_at(at) else {
            return false;
        };
        self.open_thing_menu(seed, at)
    }

    /// What the desktop draws at `at`, topmost first, as an agent is told
    /// of it.
    pub(super) fn thing_at(&self, at: Point) -> Option<SelectionSeed> {
        let desktop = self.app_label(tilo_desktop_model::commands::APP_ID);
        if let Some((name, path, folder)) = self.file_picker_overlay.entry_at(at) {
            let kind = if folder { "folder" } else { "file" };
            let id = format!("file:{}", path.display());
            return Some(desktop_thing(desktop, id, kind, &name));
        }
        if self.palette.is_open()
            && let Some(result) = self
                .palette
                .id_at(at)
                .and_then(|id| id.strip_prefix("result:"))
                .and_then(|index| index.parse::<usize>().ok())
                .and_then(|index| self.search_results.get(index))
        {
            let reference = SharedSelection::of_text(&result.title)
                .at(tilo_actions::SelectionPlace::item(result.id.as_str()))
                .in_resource(tilo_actions::ActionResource {
                    id: result.provider_id.clone(),
                    kind: result.kind.id().to_owned(),
                    label: result.title.clone(),
                });
            return Some(SelectionSeed::new(
                tilo_desktop_model::commands::APP_ID,
                desktop,
                Some(reference),
            ));
        }
        if let Some(seed) = self.notification_at(at) {
            return Some(seed);
        }
        if let Some((widget, ..)) = self.desk_hit(at) {
            let id = format!("desk:{}", widget.key());
            return Some(desktop_thing(desktop, id, "desktop.widget", widget.key()));
        }
        if let Some(seed) = self.strip_thing_at(at) {
            return Some(seed);
        }
        self.launchpad_thing_at(at)
    }

    /// The notification whose banner, or whose row in the open centre, is
    /// drawn at `at`.
    fn notification_at(&self, at: Point) -> Option<SelectionSeed> {
        let chrome = self.chrome.as_ref()?;
        let host = ChromeNotifications::new(chrome).with_agents(&self.room_activity);
        let in_center = {
            let state = chrome.borrow();
            state.overlays.quick_settings
                && matches!(
                    state.quick_settings.current_level(),
                    crate::shell::quick_settings::QsLevel::Notifications
                )
        };
        let id = in_center
            .then(|| {
                self.notification_center
                    .notification_at(&host, &self.qs_notification_layout(), at)
            })
            .flatten()
            .or_else(|| self.notification_stack.banner_at(at))?;
        let (title, app_id) = {
            let state = chrome.borrow();
            let log = state.notification_log();
            let notice = log
                .active()
                .iter()
                .chain(log.history())
                .find(|notice| notice.id == id)?;
            (notice.title.clone(), notice.app_id.clone())
        };
        let mut seed = desktop_thing(
            self.app_label(tilo_desktop_model::commands::APP_ID),
            format!("notification:{id}"),
            "desktop.notification",
            &title,
        );
        // Credited to the app that sent it, as the launcher vouched.
        if let Some(app_id) = app_id {
            seed.app_label = self.app_label(&app_id);
            seed.app_id = app_id;
        }
        Some(seed)
    }

    /// A strip's mark on the ribbon, or a window's segment on the rail.
    fn strip_thing_at(&self, at: Point) -> Option<SelectionSeed> {
        use tilo_desktop::strip_navigator::{StripNavigation, StripNavigator};
        let frame = self.strip_frame();
        let navigation = StripNavigator::ribbon_target(&frame, at)
            .or_else(|| StripNavigator::rail_target(&frame, at))?;
        let chrome = self.chrome.as_ref()?;
        match navigation {
            StripNavigation::Window { strip, group } => {
                let state = chrome.borrow();
                let tree = state.tree();
                let tab = tree
                    .desktop
                    .active_workspace()?
                    .strips
                    .iter()
                    .find(|s| s.id == strip)?
                    .window_groups
                    .iter()
                    .find(|g| g.id == group)?
                    .active_tab;
                drop(tree);
                let app_id = state.tab_app_id(strip, group, tab)?;
                drop(state);
                let app_id = app_id.as_str().to_owned();
                let label = self.app_label(&app_id);
                let window = self.selection_requests.window_of(&app_id).map(|resource| {
                    SharedSelection::of_text(&resource.label)
                        .at(tilo_actions::SelectionPlace::item(resource.id.as_str()))
                        .in_resource(resource.clone())
                });
                Some(SelectionSeed::new(app_id, label, window))
            }
            StripNavigation::Strip(command) => {
                let tilo_desktop::strip_switcher::Command::Focus(strip) = command else {
                    return None;
                };
                let state = chrome.borrow();
                let tree = state.tree();
                let (index, found) = tree
                    .desktop
                    .active_workspace()?
                    .strips
                    .iter()
                    .enumerate()
                    .find(|(_, s)| s.id == strip)?;
                let name = crate::shell::window_menu::strip_shown_name(&found.name, index);
                drop(tree);
                drop(state);
                Some(desktop_thing(
                    self.app_label(tilo_desktop_model::commands::APP_ID),
                    format!("strip:{}", strip.raw()),
                    "desktop.strip",
                    &name,
                ))
            }
        }
    }

    /// A card of the empty workspace's launchpad: the app it opens.
    fn launchpad_thing_at(&self, at: Point) -> Option<SelectionSeed> {
        fn find(widget: &dyn ui::widget::Widget, at: Point) -> Option<(String, String)> {
            if let Some(launchpad) = widget.as_any().and_then(|any| {
                any.downcast_ref::<tilo_desktop::launchpad::CollaborationLaunchpad>()
            }) {
                let (app, name) = launchpad.action_at(at)?.app_target()?;
                return Some((app.to_owned(), name.to_owned()));
            }
            widget
                .children()
                .into_iter()
                .find_map(|child| find(child, at))
        }
        let (app_id, name) = find(self.content.as_ref(), at)?;
        Some(SelectionSeed::new(app_id, name, None))
    }
}
