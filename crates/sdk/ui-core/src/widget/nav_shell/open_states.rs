//! Binding whether the sidebar and the inspector show to an app's
//! [`LocalState`]s, and showing the inspector for a shell built to show
//! something in it.

use super::{NavShell, OpenBinding};
use crate::LocalState;
use crate::local_state::LocalBinding;

impl NavShell {
    /// Keep whether the sidebar shows in `state`: the shell writes where it
    /// stands once laid out and every choice the person makes; setting the
    /// state shows or hides the sidebar (opens the drawer in a narrow
    /// window, from `ViewCx::on_stage` for a capture).
    #[must_use]
    pub fn with_sidebar_state(mut self, state: &LocalState<bool>) -> Self {
        let (_, binding) = LocalBinding::new(state);
        self.sidebar_state = Some(OpenBinding {
            binding,
            wanted: None,
        });
        self
    }

    /// Keep whether the inspector shows in `state`, as
    /// [`Self::with_sidebar_state`].
    #[must_use]
    pub fn with_inspector_state(mut self, state: &LocalState<bool>) -> Self {
        let (_, binding) = LocalBinding::new(state);
        self.inspector_state = Some(OpenBinding {
            binding,
            wanted: None,
        });
        self
    }

    /// With [`Self::with_inspector_state`], show the inspector once laid
    /// out (docked beside the content, or over it in a narrow window), for
    /// a shell built to show what was just chosen in it: a deep link, a
    /// selection made in another place of the app.
    #[must_use]
    pub fn with_inspector_revealed(mut self) -> Self {
        if let Some(state) = self.inspector_state.as_mut() {
            state.wanted = Some(true);
        }
        self
    }
}
