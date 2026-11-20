//! A stand-in mail server for a rehearsal: an IMAP and an SMTP server with real sockets that
//! take one made-up login and refuse every other, show an empty inbox, and send no mail.
//!
//! ```text
//! mail_stand_in --name imap.NAME --name smtp.NAME --login LOGIN --password-file FILE \
//!     --write-certificate FILE [--imap ADDRESS] [--smtp ADDRESS] [--imap-tls] [--smtp-tls]
//! ```
//!
//! As a mail server does, port 993 and port 465 are TLS from the first byte and every other
//! port greets in the clear and upgrades with `STARTTLS`; `--imap-tls` and `--smtp-tls` say
//! the first of it for another port, where the client is told to expect it. The program makes
//! one certificate for the names it is given and writes it to the file named: whoever signs in
//! has to trust it, so it is added to the trust bundle of the stack that is rehearsed and of
//! nothing else. The password is read from a file, so it stands on no command line. No real
//! mail server, no real address and nobody's password belong here.

#[path = "../src/stand_in.rs"]
mod stand_in;

use std::convert::Infallible;
use std::process::ExitCode;

const USAGE: &str = "usage: mail_stand_in --name NAME [--name NAME ..] --login LOGIN \
     --password-file FILE --write-certificate FILE [--imap ADDRESS] [--smtp ADDRESS] \
     [--imap-tls] [--smtp-tls]";

fn main() -> ExitCode {
    match stood_in() {
        Ok(never) => match never {},
        Err(why) => {
            eprintln!("mail stand-in: {why}\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}

/// Start both servers as the arguments say, and answer until the program is ended.
fn stood_in() -> Result<Infallible, String> {
    let (mut imap, mut smtp) = ("127.0.0.1:993".to_owned(), "127.0.0.1:465".to_owned());
    let mut names = Vec::new();
    let (mut imap_tls, mut smtp_tls) = (false, false);
    let (mut login, mut password_file, mut certificate_file) = (None, None, None);
    let mut said = std::env::args().skip(1);
    while let Some(flag) = said.next() {
        match flag.as_str() {
            "--imap-tls" => {
                imap_tls = true;
                continue;
            }
            "--smtp-tls" => {
                smtp_tls = true;
                continue;
            }
            _ => {}
        }
        let value = said.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--imap" => imap = value,
            "--smtp" => smtp = value,
            "--name" => names.push(value),
            "--login" => login = Some(value),
            "--password-file" => password_file = Some(value),
            "--write-certificate" => certificate_file = Some(value),
            _ => return Err(format!("unknown argument {flag}")),
        }
    }
    let needed =
        |value: Option<String>, flag: &str| value.ok_or_else(|| format!("{flag} is needed"));
    let login = needed(login, "--login")?;
    let password_file = needed(password_file, "--password-file")?;
    let certificate_file = needed(certificate_file, "--write-certificate")?;
    if names.is_empty() {
        return Err("--name is needed: a name the certificate is for".to_owned());
    }
    let password = std::fs::read_to_string(&password_file)
        .map_err(|error| format!("cannot read {password_file}: {error}"))?
        .trim()
        .to_owned();
    if password.is_empty() {
        return Err(format!("{password_file} holds no password"));
    }
    let on_port = |address: &str, port: &str| {
        address
            .rsplit_once(':')
            .is_some_and(|(_, said)| said == port)
    };
    let up = stand_in::started(stand_in::Serving {
        imap_wrapped: imap_tls || on_port(&imap, "993"),
        smtp_wrapped: smtp_tls || on_port(&smtp, "465"),
        names,
        login,
        password,
        oauth_token: None,
        imap,
        smtp,
    })?;
    std::fs::write(&certificate_file, stand_in::pem(&up.certificate))
        .map_err(|error| format!("cannot write {certificate_file}: {error}"))?;
    println!(
        "mail stand-in: IMAP at {}, SMTP at {}; its certificate is in {certificate_file}. It \
         takes one login, shows an empty inbox and sends no mail.",
        up.imap, up.smtp
    );
    loop {
        std::thread::park();
    }
}
