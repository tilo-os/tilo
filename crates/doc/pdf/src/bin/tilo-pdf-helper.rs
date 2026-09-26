//! `tilo-pdf-helper` — the sandboxed PDF engine process. See
//! [`tilo_pdf::helper`]; it is started by `tilo_pdf::Engine`, never by hand.

fn main() {
    std::process::exit(tilo_pdf::helper::run());
}
