//! Write the sample Thunderbird profile (two accounts, an address book, a calendar) into the
//! home directory named, for an end-to-end run of a migration against a computer that has no
//! real Thunderbird: `cargo run -p migration-sources --example sample_thunderbird_profile
//! --features fixture -- HOME [--large]`. With `--large` the work Inbox also holds a message of
//! about 40 MiB, its attachments larger than one upload carries.

fn main() {
    let mut args = std::env::args_os().skip(1);
    let Some(home) = args.next() else {
        eprintln!("usage: sample_thunderbird_profile HOME [--large]");
        std::process::exit(2);
    };
    let home = std::path::Path::new(&home);
    migration_sources::thunderbird::fixture::write_sample_profile(home);
    if args.any(|arg| arg == "--large") {
        migration_sources::thunderbird::fixture::append_large_message(home);
    }
}
