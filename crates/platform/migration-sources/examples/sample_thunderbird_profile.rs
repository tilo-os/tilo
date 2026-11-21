//! Write the sample Thunderbird profile (two accounts, an address book, a calendar) into the
//! home directory named, for an end-to-end run of a migration against a computer that has no
//! real Thunderbird: `cargo run -p migration-sources --example sample_thunderbird_profile
//! --features fixture -- HOME`.
//!
//! `-- HOME --live EMAIL IMAP_HOST:PORT SMTP_HOST:PORT PASSWORD_FILE` writes a profile of one
//! IMAP account at those servers instead, its password (read from the file, so it is on no
//! command line) saved as Thunderbird saves one, for trying a migration against a stand-in mail server.

fn main() {
    let mut args = std::env::args_os().skip(1);
    let Some(home) = args.next() else {
        eprintln!("usage: sample_thunderbird_profile HOME");
        std::process::exit(2);
    };
    let home = std::path::Path::new(&home);
    let rest: Vec<String> = args.map(|arg| arg.to_string_lossy().into_owned()).collect();
    if let [live, email, imap, smtp, password_file] = rest.as_slice()
        && live == "--live"
    {
        let at = |address: &str| -> (String, u16) {
            let (host, port) = address.rsplit_once(':').unwrap_or_else(|| {
                eprintln!("{address} is not HOST:PORT");
                std::process::exit(2);
            });
            (host.to_owned(), port.parse().unwrap_or(993))
        };
        let (imap_host, imap_port) = at(imap);
        let (smtp_host, smtp_port) = at(smtp);
        let password = std::fs::read_to_string(password_file).unwrap_or_default();
        migration_sources::thunderbird::fixture::write_live_profile(
            home,
            &migration_sources::thunderbird::fixture::LiveAccount {
                name: "Ada Lovelace".into(),
                email: email.clone(),
                also_sends_as: Some(("Ada at the lab".into(), "ada.lab@example.test".into())),
                imap_host,
                imap_port,
                smtp_host,
                smtp_port,
                password: password.trim().to_owned(),
            },
        );
        return;
    }
    migration_sources::thunderbird::fixture::write_sample_profile(home);
}
