//! The installer on the live medium: the guided setup's destination side.

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Settings this process's host installed none of are what the profile's session left
    // for processes outside it.
    settings_store::register_outside();
    imager::setup::destination::run()
}
