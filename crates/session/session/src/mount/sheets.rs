//! The sheets the session lends its seats to: the power menu, a workspace's
//! settings, the directory, the portal apps open links through, the file
//! picker, the guide and the lock screen — each mounted over the desktop and
//! what it shows, and handed to the session as the seat it speaks through.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_compositor::Compositor;
use tilo_sheets::Seat as _;
use tilo_sheets::directory::DirectorySeat;
use tilo_sheets::display_confirm::ConfirmSeat;
use tilo_sheets::file_picker::FilePickerSeat;
use tilo_sheets::lock::LockSeat;
use tilo_sheets::portal::PortalSeat;
use tilo_sheets::power::PowerSeat;
use tilo_sheets::shortcut_guide::GuideSeat;
use tilo_sheets::workspace_settings::WorkspaceSettingsSeat;
use tilo_surface_host::Doors;

use crate::Options;
use crate::panels::Held;
use crate::sheets::Sheet;

/// The sheets the session lends its seats to.
pub(crate) struct Sheets {
    pub(crate) power: Option<PowerSeat>,
    pub(crate) workspace_settings: Option<WorkspaceSettingsSeat>,
    pub(crate) directory: Option<DirectorySeat>,
    pub(crate) portal: Option<PortalSeat>,
    pub(crate) file_picker: Option<FilePickerSeat>,
    pub(crate) lock: Option<LockSeat>,
    pub(crate) display_confirm: Option<ConfirmSeat>,
    pub(crate) shortcut_guide: Option<GuideSeat>,
}

pub(crate) fn mount_sheets(
    compositor: &mut Compositor,
    doors: &mut Doors,
    options: &Options,
) -> Sheets {
    let who = (options.device.as_str(), options.person.as_str());
    let power = mount_sheet(
        compositor,
        doors,
        tilo_sheets::power::definition(),
        PowerSeat::default(),
        who,
        options,
    );
    if let Some(power) = &power {
        // The host's power is the host's: a session in a window offers no
        // switching user, suspending or restarting, and a session that
        // cannot check a password offers no locking.
        power.set_offers(tilo_sheets::power::Offers {
            user_switching: !options.hosted,
            host_power: !options.hosted,
            locking: options.verifier.available(),
            hangs_from: 0.0,
        });
    }
    let workspace_settings = mount_sheet(
        compositor,
        doors,
        tilo_sheets::workspace_settings::definition(),
        WorkspaceSettingsSeat::default(),
        who,
        options,
    );
    let directory = mount_sheet(
        compositor,
        doors,
        tilo_sheets::directory::definition(),
        DirectorySeat::default(),
        who,
        options,
    );
    let portal = mount_sheet(
        compositor,
        doors,
        tilo_sheets::portal::definition(),
        PortalSeat::default(),
        who,
        options,
    );
    let file_picker = mount_sheet(
        compositor,
        doors,
        tilo_sheets::file_picker::definition(),
        FilePickerSeat::default(),
        who,
        options,
    );
    let shortcut_guide = mount_sheet(
        compositor,
        doors,
        tilo_sheets::shortcut_guide::definition(),
        GuideSeat::default(),
        who,
        options,
    );
    // The question about a display change stands on every display: it may
    // be about the display it would otherwise be put on.
    let display_confirm = mount_sheet(
        compositor,
        doors,
        tilo_sheets::display_confirm::definition(),
        ConfirmSeat::default(),
        who,
        options,
    );
    if let Some(confirm) = &display_confirm {
        confirm.presentation().cover_every_display();
    }
    // Last, so it covers every other sheet, the guide among them: the lock
    // screen shows no key of the desktop behind it.
    let lock = mount_lock_screen(compositor, doors, who, options);
    Sheets {
        power,
        workspace_settings,
        directory,
        portal,
        file_picker,
        lock,
        display_confirm,
        shortcut_guide,
    }
}

fn mount_sheet<S: tilo_sheets::Seat + 'static>(
    compositor: &mut Compositor,
    doors: &mut Doors,
    definition: &'static ui::application::ApplicationDefinition,
    seat: S,
    who: (&str, &str),
    options: &Options,
) -> Option<S> {
    match Sheet::mount(
        definition,
        seat,
        who,
        (options.presentation)(),
        options.scale,
    ) {
        Ok(sheet) => {
            let seat = sheet.seat().clone();
            let sheet = Rc::new(RefCell::new(sheet));
            compositor.add_host(Box::new(Held(Rc::clone(&sheet))));
            doors.add(sheet);
            Some(seat)
        }
        Err(error) => {
            tracing::warn!(%error, app = definition.app_id(), "sheet not mounted");
            None
        }
    }
}

/// The lock screen, hosted over nothing while the session is locked; its
/// seat, when it mounted.
fn mount_lock_screen(
    compositor: &mut Compositor,
    doors: &mut Doors,
    who: (&str, &str),
    options: &Options,
) -> Option<LockSeat> {
    match Sheet::mount(
        tilo_sheets::lock::definition(),
        LockSeat::default(),
        who,
        (options.presentation)(),
        options.scale,
    ) {
        Ok(sheet) => {
            let seat = sheet.seat().clone();
            let sheet = Rc::new(RefCell::new(sheet.locking()));
            compositor.add_host(Box::new(Held(Rc::clone(&sheet))));
            doors.add(sheet);
            Some(seat)
        }
        Err(error) => {
            tracing::warn!(%error, "lock screen not mounted");
            None
        }
    }
}
