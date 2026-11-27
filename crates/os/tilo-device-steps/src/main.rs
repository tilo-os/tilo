//! `tilo-device-step`: one of the device's steps, run as root by its own unit, a helper the steps
//! start, or the catalogue of steps the device's daemon publishes.

fn main() -> std::process::ExitCode {
    // Settings this process's host installed none of are what the profile's session left
    // for processes outside it.
    settings_store::register_outside();
    tilo_device_steps::step::main(std::env::args_os().skip(1).collect())
}
