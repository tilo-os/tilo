//! The sheets the session lends its seats to: the power menu, a workspace's settings, the
//! directory, the portal apps open links through, the file picker, the guide, the display sheet,
//! the questions about the person's apps and the one deleting a workspace asks — each mounted over
//! the desktop and what it shows, and handed to the session as the seat it speaks through.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_compositor::Compositor;
use tilo_session_core::door::Doors;
use tilo_sheets::Seat as _;
use tilo_sheets::add_app::AddSeat;
use tilo_sheets::delete_workspace::DeleteWorkspaceSeat;
use tilo_sheets::directory::DirectorySeat;
use tilo_sheets::display_confirm::ConfirmSeat;
use tilo_sheets::displays::DisplaysSeat;
use tilo_sheets::file_picker::FilePickerSeat;
use tilo_sheets::invite::InviteSeat;
use tilo_sheets::lock::LockSeat;
use tilo_sheets::portal::PortalSeat;
use tilo_sheets::power::PowerSeat;
use tilo_sheets::shortcut_guide::GuideSeat;
use tilo_sheets::uninstall::UninstallSeat;
use tilo_sheets::workspace_settings::WorkspaceSettingsSeat;

use crate::Options;
use crate::sheets::Sheet;

/// The sheets the session lends its seats to.
pub(crate) struct Sheets {
    pub(crate) power: Option<PowerSeat>,
    pub(crate) workspace_settings: Option<WorkspaceSettingsSeat>,
    pub(crate) directory: Option<DirectorySeat>,
    pub(crate) invite: Option<InviteSeat>,
    pub(crate) delete_workspace: Option<DeleteWorkspaceSeat>,
    pub(crate) portal: Option<PortalSeat>,
    pub(crate) file_picker: Option<FilePickerSeat>,
    pub(crate) display_confirm: Option<ConfirmSeat>,
    pub(crate) displays: Option<DisplaysSeat>,
    pub(crate) shortcut_guide: Option<GuideSeat>,
    pub(crate) add_app: Option<AddSeat>,
    pub(crate) uninstall: Option<UninstallSeat>,
    /// The lock screen, drawn in the session's own process when the app that normally draws it is
    /// not there.
    pub(crate) lock_view: Option<LockSeat>,
}

/// What this session offers of locking and the computer's power, for the power menu and for the
/// rows the Tilo menu and search list: the host's power is the host's, so a session in a window of
/// another desktop offers no switching user, suspending or restarting, and a session that could not
/// be left again offers no locking. Something to show the lock is always there — the lock screen's
/// app, or the session's own view of it — so a password that can be checked is all locking needs.
pub(crate) fn power_offers(options: &Options) -> tilo_sheets::power::Offers {
    tilo_sheets::power::Offers {
        user_switching: options.machine.is_owned(),
        host_power: options.machine.is_owned(),
        locking: options.verifier.available(),
        hangs_from: 0.0,
    }
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
        power.set_offers(power_offers(options));
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
    let invite = mount_sheet(
        compositor,
        doors,
        tilo_sheets::invite::definition(),
        InviteSeat::default(),
        who,
        options,
    );
    // The question deleting a workspace asks: over whatever the person was looking at, whichever
    // surface asked it.
    let delete_workspace = mount_sheet(
        compositor,
        doors,
        tilo_sheets::delete_workspace::definition(),
        DeleteWorkspaceSeat::default(),
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
    let (display_confirm, displays) = mount_displays(compositor, doors, who, options);
    let (add_app, uninstall) = mount_app_questions(compositor, doors, who, options);
    let lock_view = mount_lock_view(compositor, doors, who, options);
    Sheets {
        power,
        workspace_settings,
        directory,
        invite,
        delete_workspace,
        portal,
        file_picker,
        display_confirm,
        displays,
        shortcut_guide,
        add_app,
        uninstall,
        lock_view,
    }
}

/// The sheets about displays: the question a display change asks, standing on every display because
/// it may be about the display it would otherwise be put on, and where the desktop appears, with
/// the layouts and the wireless displays it streams to.
fn mount_displays(
    compositor: &mut Compositor,
    doors: &mut Doors,
    who: (&str, &str),
    options: &Options,
) -> (Option<ConfirmSeat>, Option<DisplaysSeat>) {
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
    let displays = mount_sheet(
        compositor,
        doors,
        tilo_sheets::displays::definition(),
        DisplaysSeat::default(),
        who,
        options,
    );
    (display_confirm, displays)
}

/// The lock screen as a sheet of the session's own: the same view the lock screen's app shows,
/// drawn here when that app is not there to draw it. The session opens it when the session is
/// locked and the app cannot show the lock, so the machine is never left locked with nothing that
/// could unlock it. It stands on every display while it is up.
fn mount_lock_view(
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
            let sheet = sheet.standing_over_lock();
            let seat = sheet.seat().clone();
            seat.presentation().cover_every_display();
            let sheet = Rc::new(RefCell::new(sheet));
            compositor.add_host(sheet.clone());
            doors.add(sheet);
            Some(seat)
        }
        Err(error) => {
            tracing::warn!(%error, app = tilo_sheets::lock::APP_ID, "lock view not mounted");
            None
        }
    }
}

/// The questions about the person's apps: adding one from an installer, and uninstalling one — each
/// its own sheet over the desk.
fn mount_app_questions(
    compositor: &mut Compositor,
    doors: &mut Doors,
    who: (&str, &str),
    options: &Options,
) -> (Option<AddSeat>, Option<UninstallSeat>) {
    let add_app = mount_sheet(
        compositor,
        doors,
        tilo_sheets::add_app::definition(),
        AddSeat::default(),
        who,
        options,
    );
    let uninstall = mount_sheet(
        compositor,
        doors,
        tilo_sheets::uninstall::definition(),
        UninstallSeat::default(),
        who,
        options,
    );
    (add_app, uninstall)
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
            compositor.add_host(sheet.clone());
            doors.add(sheet);
            Some(seat)
        }
        Err(error) => {
            tracing::warn!(%error, app = definition.app_id(), "sheet not mounted");
            None
        }
    }
}
