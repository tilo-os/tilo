//! `tilo-pdf-helper` — the sandboxed PDF engine process. See
//! [`tilo_pdf::helper`]; it is started by `tilo_pdf::Engine`, never by hand.

#[cfg(unix)]
fn main() {
    std::process::exit(tilo_pdf::helper::run());
}

/// The helper confines pdfium through `tilo-sandbox` (Landlock and seccomp on
/// Linux, Seatbelt on macOS); elsewhere the portable engine parses in process.
#[cfg(not(unix))]
fn main() {
    eprintln!("tilo-pdf-helper runs on Unix only");
    std::process::exit(2);
}
