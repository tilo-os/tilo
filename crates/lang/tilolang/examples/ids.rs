//! `tilolang check --write-ids`: the language tool without the host a window needs. A scratch
//! entry point for regenerating a package's ids lock and catalogue while the workspace builds.
fn main() -> std::process::ExitCode {
    tilolang::cli::main(&std::env::args().skip(1).collect::<Vec<_>>())
}
