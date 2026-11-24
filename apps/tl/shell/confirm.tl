/// The system's question before an action that cannot be undone runs: the action's own label, what
/// it acts on, and its two answers. It stands over everything, and Escape runs nothing.
///
/// The question is the session's reading (`device.confirm`), not this view's state: any view of the
/// shell presses a destructive action, the session keeps the call and says what it asks, and the
/// answer here runs that call through the API's confirmed path or lets it go.
view confirm_dialog() {
    let c = device.confirm
    if c.open {
        ui.ModalDialog(
            title: c.title,
            description: c.message,
            open: c.open,
            primary: c.accept,
            secondary: c.cancel,
            destructive: true,
            icon: .triangle_alert,
        ) {
            on act_primary: device.approve(device.confirm)
            on act_secondary: device.cancel(device.confirm)
            on dismiss: device.cancel(device.confirm)
            ui.Text(c.message, role: .body)
        }
    }
}
