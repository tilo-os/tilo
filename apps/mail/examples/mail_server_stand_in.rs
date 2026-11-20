//! A stand-in IMAP and SMTP server for a rehearsal of connecting a mail account: the test
//! servers this app's own tests use, on loopback ports, with a few messages in the inbox.
//!
//! ```text
//! cargo build -p mail --features fake-servers --example mail_server_stand_in
//! target/debug/examples/mail_server_stand_in CERTIFICATE_FILE PORTS_FILE [SUBMISSION_PORT]
//! ```
//!
//! It writes the certificate both servers present (for `localhost`) to `CERTIFICATE_FILE` as
//! PEM, for the stack it rehearses with to trust and nothing else, and `<imap port> <smtp
//! port>` to `PORTS_FILE`, then serves until it is ended, saying the subject of each message its
//! SMTP server takes. The IMAP server is TLS from the first byte and the SMTP server upgrades
//! with `STARTTLS`. Mail takes an account's SMTP server with `STARTTLS` on the submission port
//! (587) alone, as Thunderbird's settings name it: with `SUBMISSION_PORT`, the SMTP server
//! answers there too, and that is the port it writes. It holds made-up mail only, takes any
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
    let (Some(certificate_file), Some(ports_file), submission) =
        (args.next(), args.next(), args.next())
    else {
        eprintln!("usage: mail_server_stand_in CERTIFICATE_FILE PORTS_FILE [SUBMISSION_PORT]");
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
        let smtp_port = match submission {
            Some(port) => {
                let Ok(port) = port.parse::<u16>() else {
                    eprintln!("{port} is not a port");
                    std::process::exit(2);
                };
                if let Err(error) = forward(port, smtp.port).await {
                    eprintln!("cannot listen on localhost:{port}: {error}");
                    std::process::exit(1);
                }
                port
            }
            None => smtp.port,
        };
        let pem = pem(certificate.as_ref());
        let written = std::fs::write(&certificate_file, pem)
            .and_then(|()| std::fs::write(&ports_file, format!("{} {smtp_port}\n", imap.port)));
        if let Err(error) = written {
            eprintln!("cannot write {certificate_file} or {ports_file}: {error}");
            std::process::exit(1);
        }
        let _ = writeln!(
            std::io::stderr(),
            "IMAP on localhost:{}, SMTP on localhost:{}",
            imap.port,
            smtp_port
        );
        // Each message the SMTP server takes, said by its subject line as it arrives.
        let mut said = 0;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let sent = smtp.sent();
            for message in sent.iter().skip(said) {
                let text = String::from_utf8_lossy(message);
                let subject = text
                    .lines()
                    .find_map(|line| line.strip_prefix("Subject: "))
                    .unwrap_or("(no subject)");
                let _ = writeln!(std::io::stderr(), "sent: {subject}");
            }
            said = sent.len();
        }
    });
}

/// Every connection to loopback `port` carried to the SMTP server at loopback `to`.
async fn forward(port: u16, to: u16) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    tokio::spawn(async move {
        while let Ok((mut incoming, _)) = listener.accept().await {
            tokio::spawn(async move {
                if let Ok(mut server) = tokio::net::TcpStream::connect(("127.0.0.1", to)).await {
                    let _ = tokio::io::copy_bidirectional(&mut incoming, &mut server).await;
                }
            });
        }
    });
    Ok(())
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
