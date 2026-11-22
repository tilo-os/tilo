//! Renewing this device's certificate before a join presents it.
//!
//! A device that holds its own root reissues what is due itself
//! ([`tilo_onboarding_session::firstboot::renew_due_device_certificates`]). A
//! certificate still outside its validity afterwards is said here, so what a
//! host is shown is never one it refuses.

/// Renew this device's certificate before it is presented to a host: an
/// enrollment that joins a workspace.
///
/// # Errors
///
/// What the reissue of a certificate held here failed on, or that a
/// certificate outside its validity could not be renewed.
pub(crate) fn before_presenting() -> Result<(), String> {
    tilo_onboarding_session::firstboot::renew_due_device_certificates()
        .map_err(|error| error.to_string())?;
    tilo_identity::provisioning::device_certificate_clock_refusal(
        &tilo_identity::provisioning::collaboration_identity_paths(),
        tilo_clock::unix_now_ns(),
    )
    .map_or(Ok(()), |refusal| {
        Err(format!(
            "this device's certificate is outside its validity and could not be renewed \
             before the join: {refusal}"
        ))
    })
}
