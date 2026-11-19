//! A stand-in IMAP and SMTP server for a rehearsal of connecting a mail account: the test
//! servers this app's own tests use, on loopback ports, with a few messages in the inbox.
//!
//! ```text
//! cargo build -p mail --features fake-servers --example mail_server_stand_in
//! target/debug/examples/mail_server_stand_in CERTIFICATE_FILE PORTS_FILE
//! ```
//!
//! It writes the certificate both servers present (for `localhost`) to `CERTIFICATE_FILE` as
//! PEM, for the stack it rehearses with to trust and nothing else, and `<imap port> <smtp
//! port>` to `PORTS_FILE`, then serves until it is ended. The IMAP server is TLS from the first
//! byte and the SMTP server upgrades with `STARTTLS`. It holds made-up mail only, takes any
//! sign-in, and sends nothing anywhere.

use std::io::Write as _;

/// The messages its inbox holds.
const INBOX: [(&str, &str); 3] = [
    ("Quarterly plan", "The draft is in the shared folder."),
    ("Lunch on Friday?", "Does one o'clock work for you?"),
    (
        "Welcome to the lab",
        "Your badge is at the reception from Monday.",
    ),
];

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(certificate_file), Some(ports_file)) = (args.next(), args.next()) else {
        eprintln!("usage: mail_server_stand_in CERTIFICATE_FILE PORTS_FILE");
        std::process::exit(2);
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|error| {
            eprintln!("no runtime: {error}");
            std::process::exit(1);
        });
    runtime.block_on(async move {
        let (tls, certificate) = mail::fake_servers::server_identity();
        let imap =
            mail::fake_imap::FakeImap::start(mail::fake_imap::Layout::Plain, tls.clone()).await;
        let smtp = mail::fake_servers::FakeSmtp::start(tls).await;
        for (number, (subject, body)) in INBOX.iter().enumerate() {
            imap.append(
                "INBOX",
                &format!(
                    "Message-ID: <stand-in-{number}@example.test>\nFrom: Grace Hopper \
                     <grace@example.test>\nTo: Ada Lovelace <ada@example.test>\nSubject: \
                     {subject}\nDate: Wed, 18 Nov 2026 0{number}:30:00 +0000\nMIME-Version: \
                     1.0\nContent-Type: text/plain; charset=utf-8\n\n{body}\n"
                ),
                &[],
            );
        }
        let pem = pem(certificate.as_ref());
        let written = std::fs::write(&certificate_file, pem)
            .and_then(|()| std::fs::write(&ports_file, format!("{} {}\n", imap.port, smtp.port)));
        if let Err(error) = written {
            eprintln!("cannot write {certificate_file} or {ports_file}: {error}");
            std::process::exit(1);
        }
        let _ = writeln!(
            std::io::stderr(),
            "IMAP on localhost:{}, SMTP on localhost:{}",
            imap.port,
            smtp.port
        );
        std::future::pending::<()>().await;
    });
}

/// `der` as a PEM certificate.
fn pem(der: &[u8]) -> String {
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(der);
    let mut pem = String::from("-----BEGIN CERTIFICATE-----\n");
    for line in encoded.as_bytes().chunks(64) {
        pem.push_str(&String::from_utf8_lossy(line));
        pem.push('\n');
    }
    pem.push_str("-----END CERTIFICATE-----\n");
    pem
}
