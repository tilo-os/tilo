//! The host's network, as the broker reaches another of the person's own
//! devices through it.

/// How an invocation on another of the person's devices ended there.
pub type OwnDeviceOutcome = Result<tilo_actions::ActionOutput, Box<tilo_actions::ActionFailure>>;

/// Carrying an application's action to another of the person's own
/// devices and bringing its outcome back, run by the host's network. The
/// broker answers it under [`tilo_actions::remote::own_device::PROVIDER`]
/// only to the person at this device, for an application's own actions;
/// the host decides whether the device is one of theirs.
pub trait OwnDeviceActions: Send + Sync {
    /// Send `request` to the device `checked` names, for the person
    /// `caller` is, addressed by [`Checked::forwarded`] to the workspace the
    /// host picks. The other device's outcome arrives on the returned
    /// answer; nothing waits for it here.
    ///
    /// [`Checked::forwarded`]: tilo_actions::remote::own_device::Checked::forwarded
    ///
    /// # Errors
    ///
    /// Why it cannot be sent: the device is not one of the person's
    /// enrolled, authenticated and unrevoked devices, the two share no
    /// workspace, or the device is not reachable.
    fn invoke(
        &self,
        caller: &tilo_actions::remote::Caller,
        checked: &tilo_actions::remote::own_device::Checked,
        request: &tilo_actions::remote::InvokeRequest,
    ) -> Result<
        tilo_actions::remote::Pending<OwnDeviceOutcome>,
        tilo_actions::remote::ActionHostError,
    >;
}
