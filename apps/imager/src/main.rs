//! The installer's binary entry point. All logic lives in the library.

fn main() {
    // Settings this process's host installed none of are what the profile's session left
    // for processes outside it.
    settings_store::register_outside();
    imager::run();
}
