//! Write the sample Thunderbird profile (two accounts, an address book, a calendar) into the
//! home directory named, for an end-to-end run of a migration against a computer that has no
//! real Thunderbird: `cargo run -p migration-sources --example sample_thunderbird_profile
//! --features fixture -- HOME`.

fn main() {
    let Some(home) = std::env::args_os().nth(1) else {
        eprintln!("usage: sample_thunderbird_profile HOME");
        std::process::exit(2);
    };
    migration_sources::thunderbird::fixture::write_sample_profile(std::path::Path::new(&home));
}
