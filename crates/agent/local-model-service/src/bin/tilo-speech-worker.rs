//! One private CPU speech worker. It has no microphone, network, model download,
//! persistent transcript, user account or program-execution capability.
#[cfg(target_os = "linux")]
mod speech_worker;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    speech_worker::main()
}
#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    std::process::ExitCode::FAILURE
}
