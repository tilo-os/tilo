//! What the desktop's layers ask of the native session: the bar's source
//! (the clock, the person, their notifications) and the rules of what the
//! person may change.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use tilo_action_policy::Role;
use tilo_desktop::chrome::{
    ChromeIntent, ClockSnapshot, DesktopChromeSnapshot, DesktopChromeSource, OwnChip,
};
use tilo_desktop::desktop_host::DesktopRules;
use tilo_desktop_model::DesktopTree;
use tilo_desktop_model::commands::{DesktopCommand, Standing, may_command, may_create_workspace};
use tilo_desktop_model::ownership::Authors;
use tilo_desktop_surface::DesktopHandle;

/// The bar's view of the session: the desktop's tree, the wall clock, and
/// the person at this device.
pub(crate) struct SessionChrome {
    pub(crate) handle: DesktopHandle,
    /// Where the bar's intents go: the chrome carries them out.
    pub(crate) intents: Rc<RefCell<Vec<ChromeIntent>>>,
    pub(crate) person: String,
    /// `(tree revision, minute)` the snapshot revision was last bumped for.
    seen: Cell<(u64, i64)>,
    revision: Cell<u64>,
}

impl SessionChrome {
    pub(crate) fn new(
        handle: DesktopHandle,
        intents: Rc<RefCell<Vec<ChromeIntent>>>,
        person: String,
    ) -> Self {
        Self {
            handle,
            intents,
            person,
            seen: Cell::new((u64::MAX, 0)),
            revision: Cell::new(0),
        }
    }
}

impl DesktopChromeSource for SessionChrome {
    fn snapshot(&self) -> DesktopChromeSnapshot {
        let clock = ClockSnapshot::now();
        let seen = (self.handle.revision(), clock.unix_seconds.div_euclid(60));
        if self.seen.replace(seen) != seen {
            self.revision.set(self.revision.get().wrapping_add(1));
        }
        DesktopChromeSnapshot {
            revision: self.revision.get(),
            clock,
            own: Some(OwnChip {
                name: self.person.clone(),
                rgb: ui::presence::user_color(&self.person),
                identity: String::new(),
            }),
            ..DesktopChromeSnapshot::default()
        }
    }

    fn tree(&self) -> DesktopTree {
        self.handle.read(Clone::clone)
    }

    fn intent(&self, intent: ChromeIntent) -> bool {
        self.intents.borrow_mut().push(intent);
        true
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
