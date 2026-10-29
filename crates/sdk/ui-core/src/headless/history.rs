//! The headless host's side of the window's History: the registered History
//! door, as the native App serves it.

use super::FrameHarness;

impl FrameHarness {
    /// Present the window's History in the app's own header rather than a
    /// fallback bar of its own, as a native App window does.
    pub fn present_history_in_header(&mut self) {
        self.invalidate_local();
        if let Some(history) = &mut self.history {
            history.remove_fallback_bar();
            self.invalidate_layout_tree();
            self.relayout();
        }
    }

    /// The doors the window answers for the app, whatever its tree
    /// publishes: opening search, and History. A window whose app registers
    /// no History still publishes the door, unavailable, as a native window
    /// does.
    pub(super) fn host_door_resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        let mut doors = vec![crate::search::search_resource()];
        doors.push(self.history.as_ref().map_or_else(
            || crate::history_mode::door::history_resource(false, false),
            |history| history.door_resource(),
        ));
        doors
    }

    /// Invoke only this host's local History door. This is not a
    /// collaborator transport entry point and never searches app destinations.
    ///
    /// # Errors
    /// Refuses a window without History and invalid arguments.
    pub fn invoke_local_history_action(
        &mut self,
        address: &tilo_actions::ActionAddress,
        argument: Option<&str>,
    ) -> Result<(), String> {
        self.invalidate_local();
        self.run_local_history_action(address, argument)?;
        self.idle_control_tick();
        self.relayout();
        Ok(())
    }

    /// Open History as the History shortcut does.
    ///
    /// # Errors
    /// Refuses a window whose app registers no History.
    pub fn open_history(&mut self) -> Result<(), String> {
        self.invalidate_local();
        let history = self.history.as_ref().ok_or("History is unavailable")?;
        if !history.request_open() {
            return Err("History is unavailable".into());
        }
        self.relayout();
        Ok(())
    }

    /// Resolve and run one History door action; the next frame presents it.
    pub(super) fn run_local_history_action(
        &mut self,
        address: &tilo_actions::ActionAddress,
        argument: Option<&str>,
    ) -> Result<(), String> {
        let history = self.history.as_mut().ok_or("History is unavailable")?;
        let encoded = crate::bound_dispatch::encode_resource_invocation(
            &[history.door_resource()],
            address,
            argument,
            None,
            false,
        )
        .map_err(|error| error.to_string())?;
        history.invoke_door(&encoded)
    }
}
