//! What the desktop's layers ask of the native session: the rules of what
//! the person may change. The bar above them is the shell's
//! (`tilo_surface_host::shell_part`), which reads the session's desk
//! (`crate::desk`) through the door.

use std::cell::Cell;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use tilo_action_policy::Role;
use tilo_desktop::about::{AboutLinks, AboutVariant, TiloMenu};
use tilo_desktop::chrome::ChromeIntent;
use tilo_desktop::desktop_host::DesktopRules;
use tilo_desktop::desktop_host::{DesktopAsk, DesktopChrome};
use tilo_desktop_model::DesktopTree;
use tilo_desktop_model::commands::{DesktopCommand, Standing, may_command, may_create_workspace};
use tilo_desktop_model::ownership::Authors;
use tilo_desktop_surface::DesktopHandle;
use tilo_surface_host::{Placement, WidgetHost};

/// The app whose live activity the bar shows as its clock.
pub(crate) const CLOCKS_APP: &str = "so.tilo.clocks";

/// The clock's live activity: the board every app shows its activity on,
/// and the moment the person hid the floating timer from the bar.
pub(crate) struct Clock {
    board: Arc<runtime_collab::LiveActivities>,
    /// The board revision the person hid the overlay at; a change of the
    /// activity is the app's answer and shows it again.
    hidden_at: Cell<Option<u64>>,
}

impl Clock {
    pub(crate) fn new(board: Arc<runtime_collab::LiveActivities>) -> Self {
        Self {
            board,
            hidden_at: Cell::new(None),
        }
    }

    /// The board the apps show their live activities on.
    pub(crate) fn board(&self) -> &Arc<runtime_collab::LiveActivities> {
        &self.board
    }

    /// Hand a control pressed on the floating clock back to Clocks.
    pub(crate) fn press(&self, command: &str, activity_id: Option<&str>) {
        self.board.press(
            CLOCKS_APP,
            serde_json::json!({ "command": command, "activity_id": activity_id }),
        );
    }

    /// The person closed the overlay: it stays away until Clocks shows
    /// something new.
    pub(crate) fn hide(&self) {
        self.hidden_at.set(Some(self.board.revision()));
    }

    /// Whether the bar's clock shows the activity floating over the windows: it asks to, and
    /// the person has not put this very activity away.
    pub(crate) fn floating(&self, overlay: bool) -> bool {
        overlay && self.hidden_at.get() != Some(self.board.revision())
    }
}

/// The person at this device owns its desktop: the desktop's one policy
/// answers what they may change, as it answers a room's owner.
pub(crate) struct SessionRules {
    pub(crate) handle: DesktopHandle,
    pub(crate) person: String,
    /// Who made each strip and opened each window.
    pub(crate) authors: RefCell<Authors>,
}

impl SessionRules {
    fn standing(&self) -> Standing<'_> {
        Standing {
            person: &self.person,
            role: Some(Role::Owner),
            layout_shared: true,
            members: None,
        }
    }
}

impl DesktopRules for SessionRules {
    fn tree(&self) -> DesktopTree {
        self.handle.read(Clone::clone)
    }

    fn author(&self) -> String {
        self.person.clone()
    }

    fn may(&self, command: &DesktopCommand) -> Result<(), String> {
        let authors = self.authors.borrow();
        self.handle.read(|tree| {
            let workspace = tree
                .active_workspace()
                .ok_or_else(|| "There is no workspace open".to_owned())?;
            may_command(&self.standing(), &authors, workspace, command)
                .map_err(|refusal| refusal.to_string())
        })
    }

    fn may_create_workspace(&self) -> Result<(), String> {
        may_create_workspace(&self.standing()).map_err(|refusal| refusal.to_string())
    }

    fn made_strip(&self, strip: tilo_desktop_model::StripId) {
        self.authors
            .borrow_mut()
            .insert(strip.0, self.person.clone());
    }
}

/// The chrome above the windows (overview, menus, the strips' navigator)
/// as the session hosts it: its own tree beside the desk's, covering the
/// outputs, under the band the shell's bar stands in.
pub(crate) fn host(
    handle: &DesktopHandle,
    options: &crate::Options,
    intents: Rc<RefCell<Vec<ChromeIntent>>>,
    asks: Rc<RefCell<Vec<DesktopAsk>>>,
    overview: &crate::mount::overview::Overview,
) -> WidgetHost {
    let rules = Rc::new(SessionRules {
        handle: handle.clone(),
        person: options.person.clone(),
        authors: RefCell::new(std::collections::BTreeMap::new()),
    });
    let mut chrome = DesktopChrome::under_shell_bar(
        handle.clone(),
        crate::GENERATION.into(),
        rules,
        TiloMenu::new(AboutVariant::Native, &AboutLinks::default()),
        false,
    )
    .with_intents(intents)
    .with_asks(asks)
    .with_overview(overview.flight(), overview.open_flag());
    chrome.set_shortcuts(options.config.shortcuts.clone());
    let keyboard = chrome.keyboard();
    let placement = Placement {
        origin: (0, 0),
        size: (1, 1),
        scale: options.scale,
    };
    let mut host = WidgetHost::new(Box::new(chrome), placement, false);
    host.set_theme((options.presentation)().theme);
    let pictures = overview.clone();
    host.with_window_pictures(Rc::new(move || pictures.pictures()))
        .covering_outputs()
        .with_keyboard(keyboard)
}
