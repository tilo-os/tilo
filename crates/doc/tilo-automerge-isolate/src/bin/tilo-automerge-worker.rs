//! An Automerge worker and nothing else: what a library loaded into an
//! executable that is not Tilo's (the SDKs' Python extension and Node addon)
//! starts its workers as, named with
//! `tilo_automerge_isolate::serve_workers_from`. Started any other way it
//! says what it is and exits.

fn main() -> std::process::ExitCode {
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return std::process::ExitCode::from(code);
    }
    eprintln!(
        "tilo-automerge-worker serves the Automerge workers of a Tilo library that named it; it does nothing on its own"
    );
    std::process::ExitCode::from(2)
}
