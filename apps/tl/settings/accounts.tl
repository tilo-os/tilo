/// The accounts the workspace keeps: the mail and calendar servers, the cloud storage and the
/// model providers its members connected. They are the workspace's, so every member uses the
/// same ones.
view accounts_pane() {
    ui.Section("The workspace's accounts") {
        ui.Text(
            "The mail, calendar and cloud accounts this workspace uses are kept with the workspace, so every member reads the same mail and calendars.",
            role: .body,
        )
        ui.Text(
            "An account is added where the workspace keeps it, by a member who may change the workspace. It then shows in Mail, Calendar and Files for everyone.",
            role: .caption,
            secondary: true,
        )
    }
    ui.Section("This device") {
        ui.Text(
            "No account secret is kept on this device: the workspace's services use them where the workspace runs.",
            role: .caption,
            secondary: true,
        )
    }
}
