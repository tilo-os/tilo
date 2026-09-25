//! No connected-folder native proof on this platform; Copy is independent.
use super::{EndpointError, NativeEvidence};
use std::fs::File;
pub(super) fn inspect(_file: &File) -> Result<NativeEvidence, EndpointError> {
    Err(EndpointError::Unsupported)
}
pub(super) fn same_mount(_a: &File, _b: &File) -> Result<(), EndpointError> {
    Err(EndpointError::Unsupported)
}
pub(super) fn check_metadata(_file: &File, _directory: bool) -> Result<bool, EndpointError> {
    Err(EndpointError::Unsupported)
}
