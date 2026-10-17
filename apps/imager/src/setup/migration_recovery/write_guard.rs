//! Frontend adapter to native physical write-range admission.
use super::RecoveryCopy;
use crate::setup::staging::plan::Plan;
use std::io;

pub(super) fn validate(copy: &RecoveryCopy, plan: &Plan) -> io::Result<()> {
    imager_backend::recovery::write_guard::validate(&copy.source, &copy.destination, plan)
}
