//! The sheets the session mounts: the power menu, a workspace's settings, the
//! directory, the portal apps open links through, the file picker, the guide, the display sheet,
//! the questions about the person's apps and the one deleting a workspace asks — each mounted over
//! the desktop and what it shows, and handed to the session as the state it speaks through.

use tilo_compositor::Compositor;
use tilo_session_core::surfaces::Surfaces;
use tilo_sheets::SheetState as _;
use tilo_sheets::add_app::AddState;
use tilo_sheets::delete_workspace::DeleteWorkspaceState;
use tilo_sheets::directory::DirectoryState;
use tilo_sheets::display_confirm::ConfirmState;
use tilo_sheets::displays::DisplaysState;
use tilo_sheets::file_picker::FilePickerState;
use tilo_sheets::invite::InviteState;
use tilo_sheets::lock::LockState;
use tilo_sheets::pair_device::AddDeviceState;
use tilo_sheets::portal::PortalState;
use tilo_sheets::power::PowerState;
use tilo_sheets::shortcut_guide::GuideState;
use tilo_sheets::uninstall::UninstallState;
use tilo_sheets::workspace_settings::WorkspaceSettingsState;

use crate::Options;
use crate::sheets::Sheet;

/// The sheets the session mounts.
pub(crate) struct Sheets {
    pub(crate) power: Option<PowerState>,
    pub(crate) workspace_settings: Option<WorkspaceSettingsState>,
    pub(crate) directory: Option<DirectoryState>,
    pub(crate) invite: Option<InviteState>,
    pub(crate) pair_device: Option<AddDeviceState>,
    pub(crate) delete_workspace: Option<DeleteWorkspaceState>,
    pub(crate) portal: Option<PortalState>,
    pub(crate) file_picker: Option<FilePickerState>,
    pub(crate) display_confirm: Option<ConfirmState>,
    pub(crate) displays: Option<DisplaysState>,
    pub(crate) shortcut_guide: Option<GuideState>,
    pub(crate) add_app: Option<AddState>,
    pub(crate) uninstall: Option<UninstallState>,
    /// The lock screen, drawn in the session's own process when the app that normally draws it is
    /// not there.
    pub(crate) lock_view: Option<LockState>,
}

impl Sheets {
    /// Every sheet that asks the person something over the shell: while one is drawn it holds the
    /// keys, so the desktop's chords wait until it is answered. Not the guide, whose own state
    /// holds its keys, nor the lock's view, which locking holds.
    pub(crate) fn modal(&self) -> Vec<tilo_sheets::Presentation> {
        fn of<S: tilo_sheets::SheetState>(state: Option<&S>) -> Option<tilo_sheets::Presentation> {
            state.map(|state| state.presentation().clone())
        }
        [
            of(self.power.as_ref()),
            of(self.workspace_settings.as_ref()),
            of(self.directory.as_ref()),
            of(self.invite.as_ref()),
            of(self.pair_device.as_ref()),
            of(self.delete_workspace.as_ref()),
            of(self.portal.as_ref()),
            of(self.file_picker.as_ref()),
            of(self.display_confirm.as_ref()),
            of(self.displays.as_ref()),
            of(self.add_app.as_ref()),
            of(self.uninstall.as_ref()),
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}

/// What this session offers of locking and the computer's power, for the power menu and for the
/// rows search lists: the host's power is the host's, so a session in a window of
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
    surfaces: &mut Surfaces,
    options: &Options,
) -> Sheets {
    let who = (options.device.as_str(), options.person.as_str());
    let power = mount_sheet(
        compositor,
        surfaces,
        tilo_sheets::power::definition(),
        PowerState::default(),
        who,
        options,
    );
    if let Some(power) = &power {
        power.set_offers(power_offers(options));
    }
    let workspace_settings = mount_sheet(
        compositor,
        surfaces,
        tilo_sheets::workspace_settings::definition(),
        WorkspaceSettingsState::default(),
        who,
        options,
    );
    let directory = mount_sheet(
        compositor,
        surfaces,
        tilo_sheets::directory::definition(),
        DirectoryState::default(),
        who,
        options,
    );
    let invite = mount_sheet(
        compositor,
        surfaces,
        tilo_sheets::invite::definition(),
        InviteState::default(),
        who,
        options,
    );
    let pair_device = mount_sheet(
        compositor,
        surfaces,
        tilo_sheets::pair_device::definition(),
        AddDeviceState::default(),
        who,
        options,
    );
    // The question deleting a workspace asks: over whatever the person was looking at, whichever
    // surface asked it.
    let delete_workspace = mount_sheet(
        compositor,
        surfaces,
        tilo_sheets::delete_workspace::definition(),
        DeleteWorkspaceState::default(),
        who,
        options,
    );
    let portal = mount_sheet(
        compositor,
        surfaces,
        tilo_sheets::portal::definition(),
        PortalState::default(),
        who,
        options,
    );
    let file_picker = mount_sheet(
        compositor,
        surfaces,
        tilo_sheets::file_picker::definition(),
        FilePickerState::default(),
        who,
        options,
    );
    let shortcut_guide = mount_sheet(
        compositor,
        surfaces,
        tilo_sheets::shortcut_guide::definition(),
        GuideState::default(),
        who,
        options,
    );
    let (display_confirm, displays) = mount_displays(compositor, surfaces, who, options);
    let (add_app, uninstall) = mount_app_questions(compositor, surfaces, who, options);
    let lock_view = mount_lock_view(compositor, surfaces, who, options);
    Sheets {
        power,
        workspace_settings,
        directory,
        invite,
        pair_device,
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

/// The sheets about displays: the question a display change asks, on every display because
/// it may be about the display it would otherwise be put on, and where the desktop appears, with
/// the layouts and the wireless displays it streams to.
fn mount_displays(
    compositor: &mut Compositor,
    surfaces: &mut Surfaces,
    who: (&str, &str),
    options: &Options,
) -> (Option<ConfirmState>, Option<DisplaysState>) {
    let display_confirm = mount_sheet(
        compositor,
        surfaces,
        tilo_sheets::display_confirm::definition(),
        ConfirmState::default(),
        who,
        options,
    );
    if let Some(confirm) = &display_confirm {
        confirm.presentation().cover_every_display();
    }
    let displays = mount_sheet(
        compositor,
        surfaces,
        tilo_sheets::displays::definition(),
        DisplaysState::default(),
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
    surfaces: &mut Surfaces,
    who: (&str, &str),
    options: &Options,
) -> Option<LockState> {
    match Sheet::mount(
        tilo_sheets::lock::definition(),
        LockState::default(),
        who,
        (options.presentation)(),
        options.scale,
    ) {
        Ok(sheet) => {
            let sheet = sheet.over_lock();
            let state = sheet.state().clone();
            state.presentation().cover_every_display();
            let sheet = sheet.layer();
            compositor.add_surface(sheet.clone());
            surfaces.add(sheet);
            Some(state)
        }
        Err(error) => {
            tracing::warn!(%error, app = tilo_sheets::lock::APP_ID, "lock view not mounted");
            None
        }
    }
}

/// The questions about the person's apps: adding one from an installer, and uninstalling one — each
/// its own sheet over the shell.
fn mount_app_questions(
    compositor: &mut Compositor,
    surfaces: &mut Surfaces,
    who: (&str, &str),
    options: &Options,
) -> (Option<AddState>, Option<UninstallState>) {
    let add_app = mount_sheet(
        compositor,
        surfaces,
        tilo_sheets::add_app::definition(),
        AddState::default(),
        who,
        options,
    );
    let uninstall = mount_sheet(
        compositor,
        surfaces,
        tilo_sheets::uninstall::definition(),
        UninstallState::default(),
        who,
        options,
    );
    (add_app, uninstall)
}

fn mount_sheet<S: tilo_sheets::SheetState + 'static>(
    compositor: &mut Compositor,
    surfaces: &mut Surfaces,
    definition: &'static ui::application::ApplicationDefinition,
    state: S,
    who: (&str, &str),
    options: &Options,
) -> Option<S> {
    match Sheet::mount(
        definition,
        state,
        who,
        (options.presentation)(),
        options.scale,
    ) {
        Ok(sheet) => {
            let state = sheet.state().clone();
            let sheet = sheet.layer();
            compositor.add_surface(sheet.clone());
            surfaces.add(sheet);
            Some(state)
        }
        Err(error) => {
            tracing::warn!(%error, app = definition.app_id(), "sheet not mounted");
            None
        }
    }
}
