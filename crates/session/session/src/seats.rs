//! The seats the session lends: the sheets it puts up, and the questions they ask.
//! Each sheet is the session's: it keeps it current and puts it up, and whoever asks reaches it
//! here through the session that holds it.

use tilo_sheets::delete_workspace::DeleteWorkspaceSeat;
use tilo_sheets::directory::DirectorySeat;
use tilo_sheets::display_confirm::ConfirmSeat;
use tilo_sheets::file_picker::FilePickerSeat;
use tilo_sheets::invite::InviteSeat;
use tilo_sheets::portal::PortalSeat;
use tilo_sheets::power::PowerSeat;
use tilo_sheets::shortcut_guide::GuideSeat;
use tilo_sheets::uninstall::UninstallSeat;
use tilo_sheets::workspace_settings::WorkspaceSettingsSeat;

use crate::Session;

impl Session {
    /// The power menu's seat: the session puts it up.
    #[must_use]
    pub fn power_menu(&self) -> Option<&PowerSeat> {
        self.shared.power.as_ref()
    }

    /// The add sheet's seat: the program the person is asked about, when one is; `None` when the
    /// sheet did not mount.
    #[must_use]
    pub fn add_app_sheet(&self) -> Option<&tilo_sheets::add_app::AddSeat> {
        self.shared.app_sheets.add()
    }

    /// The uninstall sheet's seat: the app the person is asked about, when one is; `None` when the
    /// sheet did not mount.
    #[must_use]
    pub fn uninstall_sheet(&self) -> Option<&UninstallSeat> {
        self.shared.app_sheets.uninstall()
    }

    /// Ask the person whether to uninstall `app` , by its entry's id in the downloaded programs'
    /// registry or by the name they know it by.
    pub fn ask_uninstall(&self, app: &str) {
        self.shared.ask_uninstall(app);
    }

    /// Workspace settings' seat: what it shows, kept current by whoever knows the workspace.
    #[must_use]
    pub fn workspace_settings(&self) -> Option<&WorkspaceSettingsSeat> {
        self.shared.workspace_settings.as_ref()
    }

    /// The invite sheet's seat: the link into the workspace in front, kept current while it is up.
    #[must_use]
    pub fn invite_sheet(&self) -> Option<&InviteSeat> {
        self.shared.invite.as_ref()
    }

    /// The delete question's seat: the workspace it is asking about, shown when the person asks to
    /// delete one.
    #[must_use]
    pub fn delete_workspace_sheet(&self) -> Option<&DeleteWorkspaceSeat> {
        self.shared.delete_workspace.as_ref()
    }

    /// The workspace directory's seat: the workspaces this computer may join or open, shown when
    /// the person asks for them.
    #[must_use]
    pub fn workspace_directory(&self) -> Option<&DirectorySeat> {
        self.shared.directory.as_ref()
    }

    /// The portal sheet's seat: the portal, authentication and network-secret services ask through
    /// it and read the answers.
    #[must_use]
    pub fn portal(&self) -> Option<&PortalSeat> {
        self.shared.portal.as_ref()
    }

    /// The file chooser's seat: the portal service asks it for files and reads the answer.
    #[must_use]
    pub fn file_picker(&self) -> Option<&FilePickerSeat> {
        self.shared.file_picker.as_ref()
    }

    /// The shortcut guide's seat: the session puts it up with F1, and it shows the chords the
    /// session answers.
    #[must_use]
    pub fn shortcut_guide(&self) -> Option<&GuideSeat> {
        self.shared.shortcut_guide.seat()
    }

    /// The seat of the question about a display change: what it asks now,
    /// and where it stands.
    #[must_use]
    pub fn display_confirm(&self) -> Option<&ConfirmSeat> {
        self.shared.display_confirm.as_ref()
    }

    /// The display sheet: the layouts the attached displays allow and the wireless displays the
    /// desktop can be streamed to, as it shows them.
    #[must_use]
    pub fn display_sheet(&self) -> Option<&tilo_sheets::displays::DisplaysSeat> {
        self.display_sheet.seat()
    }
}
