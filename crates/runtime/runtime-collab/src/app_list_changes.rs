//! Waiting for the person's apps to change, without asking on a timer.
//!
//! A place that lists the person's apps shows a list that changes when
//! someone installs, removes or uninstalls an app — rarely, and never on a
//! clock. Asking the tools service on a timer costs a wakeup per poll to
//! find out that nothing changed, which is what this saves: the shell
//! announces a change once, and a waiting route learns of it by
//! [`ChannelHandle::applications_changed`].

use crate::{ChannelHandle, NotAvailable};

impl ChannelHandle {
    /// The person's apps may have changed since `after` (0 at first, then
    /// the value the previous wait returned), with the value to wait from
    /// next. The answer names nothing: the caller asks the tools service for
    /// the list again. The call stays pending, without traffic, while
    /// nothing changes.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when this route may not list the person's
    /// apps, the host cannot say when they change (an older broker answers
    /// unimplemented), or the broker is unavailable.
    pub async fn applications_changed(&self, after: u64) -> Result<u64, NotAvailable> {
        let mut request = self.inner.applications_changed_request();
        request.get().set_after(after);
        let reply = request.send().promise.await?;
        Ok(reply.get()?.get_change())
    }
}
