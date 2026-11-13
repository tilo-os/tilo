//! The installer on the live medium: the guided setup's destination side.

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    imager::setup::destination::run()
}
