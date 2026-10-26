//! Closed native-operation refusal marker, distinct from transport/authority failure.
use runtime_portal::NotAvailable;
use std::io;
#[derive(Debug)]
struct NativeRefused;
impl std::fmt::Display for NativeRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("native entry operation refused under live scope")
    }
}
impl std::error::Error for NativeRefused {}
/// True only for an explicit decoded native reply, never an arbitrary IO error.
pub fn is_native_refusal(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(<dyn std::error::Error + Send + Sync>::is::<NativeRefused>)
}
pub(crate) fn check(status: crate::fs_capnp::NativeEntryStatus) -> Result<(), NotAvailable> {
    match status {
        crate::fs_capnp::NativeEntryStatus::Ok => Ok(()),
        crate::fs_capnp::NativeEntryStatus::Refused => Err(NotAvailable::ConnectionFailed(
            io::Error::new(io::ErrorKind::PermissionDenied, NativeRefused),
        )),
    }
}
#[cfg(unix)]
pub(crate) fn client_error(error: NotAvailable) -> io::Error {
    match error {
        NotAvailable::ConnectionFailed(error) if is_native_refusal(&error) => error,
        _ => io::Error::new(
            io::ErrorKind::PermissionDenied,
            "retained capability authority or transport failed",
        ),
    }
}

#[derive(Debug)]
struct BoundedAdmission;
impl std::fmt::Display for BoundedAdmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("live capability resident bound reached")
    }
}
impl std::error::Error for BoundedAdmission {}
/// Local finite resource refusal; carries no wire/native authority claim.
#[cfg(unix)]
pub(crate) fn bounded_refusal() -> io::Error {
    io::Error::new(io::ErrorKind::WouldBlock, BoundedAdmission)
}
/// Per-operation failures carry no data and never imply successful effects.
pub fn is_operation_refusal(error: &io::Error) -> bool {
    is_native_refusal(error)
        || error
            .get_ref()
            .is_some_and(<dyn std::error::Error + Send + Sync>::is::<BoundedAdmission>)
}

pub(crate) fn check_payload(
    status: crate::fs_capnp::NativeEntryStatus,
    empty: bool,
) -> Result<(), NotAvailable> {
    if status == crate::fs_capnp::NativeEntryStatus::Refused && !empty {
        return Err(capnp::Error::failed(
            "refused native result contains successful payload".into(),
        )
        .into());
    }
    check(status)
}
