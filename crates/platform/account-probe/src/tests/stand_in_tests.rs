//! The stand-in mail server answers the sign-in this crate proves, over real sockets: it takes
//! its one login and refuses another, on ports that upgrade and on ports that are TLS from the
//! first byte. Every login here is made up for this file.

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::Arc;

use base64::Engine as _;

use crate::stand_in::{Serving, Started, pem, started};

const LOGIN: &str = "ada@example.test";
/// The one password the stand-in takes here, and one it does not.
const TAKEN: &str = "made-up-for-the-mail-stand-in";
const OTHER: &str = "not-what-the-mail-stand-in-takes";
const STEP: std::time::Duration = std::time::Duration::from_secs(8);

fn stand_in(wrapped: bool) -> Started {
    started(Serving {
        names: vec!["localhost".to_owned()],
        login: LOGIN.to_owned(),
        password: TAKEN.to_owned(),
        imap: "127.0.0.1:0".to_owned(),
        smtp: "127.0.0.1:0".to_owned(),
        imap_wrapped: wrapped,
        smtp_wrapped: wrapped,
    })
    .expect("the stand-in starts")
}

/// The roots that trust exactly the stand-in's certificate.
fn roots_of(up: &Started) -> rustls::RootCertStore {
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(up.certificate.clone())
        .expect("its certificate is a root");
    roots
}

/// Send `command`, when there is one, and read the reply's last line.
fn replied<S: Read + Write>(server: &mut BufReader<S>, command: &str) -> String {
    if !command.is_empty() {
        let to = server.get_mut();
        write!(to, "{command}\r\n").expect("the command is sent");
        to.flush().expect("the command is sent");
    }
    loop {
        let mut line = String::new();
        assert!(
            server.read_line(&mut line).expect("a reply") > 0,
            "the stand-in left"
        );
        if line.as_bytes().get(3) != Some(&b'-') {
            return line;
        }
    }
}

#[test]
fn the_sign_in_proof_is_taken_for_the_one_login_and_refused_for_another() {
    let up = stand_in(false);
    // The account the crate's own tests sign in with, on ports that start in the clear.
    let mut account = super::email_account(up.imap.port(), up.smtp.port());
    account.config["imap_tls"] = serde_json::json!(false);
    assert_eq!(
        crate::LiveAccountTester::email_with_roots(
            &account,
            &accounts::Secret::password(TAKEN),
            roots_of(&up),
        ),
        Ok("IMAP and SMTP sign-in succeeded".into())
    );
    assert_eq!(
        crate::LiveAccountTester::email_with_roots(
            &account,
            &accounts::Secret::password(OTHER),
            roots_of(&up),
        ),
        Err("IMAP server rejected the saved credential".into())
    );
    // A program that trusts nothing of the stand-in's does not sign in to it.
    assert_eq!(
        crate::LiveAccountTester::email_with_roots(
            &account,
            &accounts::Secret::password(TAKEN),
            rustls::RootCertStore::empty(),
        ),
        Err(
            "could not establish the secure IMAP connection: the server's certificate is not trusted"
                .into()
        )
    );
    assert!(pem(&up.certificate).starts_with("-----BEGIN CERTIFICATE-----\n"));
}

#[test]
fn on_ports_that_are_tls_from_the_first_byte_both_servers_take_the_one_login() {
    let up = stand_in(true);
    let tls = Arc::new(
        tilo_net::client_config_with_roots(roots_of(&up)).expect("a client configuration"),
    );
    // IMAP, by the connection the proof itself opens.
    let asking = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("sockets and a clock");
    asking.block_on(async {
        for (password, taken) in [(OTHER, false), (TAKEN, true)] {
            let client = crate::imap::open(
                &crate::imap::Reach::any(),
                "localhost",
                up.imap.port(),
                accounts::email::Security::Tls,
                Arc::clone(&tls),
                STEP,
            )
            .await
            .expect("it greets over TLS");
            assert_eq!(client.login(LOGIN, password).await.is_ok(), taken);
        }
    });
    // SMTP, by hand: the proof takes TLS from the first byte on port 465 alone.
    let name = rustls::pki_types::ServerName::try_from("localhost").expect("a name");
    let connection = rustls::ClientConnection::new(tls, name).expect("a client");
    let tcp = std::net::TcpStream::connect(("127.0.0.1", up.smtp.port())).expect("it answers");
    let mut server = BufReader::new(rustls::StreamOwned::new(connection, tcp));
    let plain = |password: &str| {
        let token =
            base64::engine::general_purpose::STANDARD.encode(format!("\0{LOGIN}\0{password}"));
        format!("AUTH PLAIN {token}")
    };
    assert!(replied(&mut server, "").starts_with("220 "));
    assert!(replied(&mut server, "EHLO [127.0.0.1]").starts_with("250 AUTH PLAIN LOGIN"));
    assert!(replied(&mut server, &plain(OTHER)).starts_with("535 "));
    assert!(replied(&mut server, &plain(TAKEN)).starts_with("235 "));
    // It sends no mail, and says so instead of taking a message it would drop.
    assert!(replied(&mut server, "MAIL FROM:<ada@example.test>").starts_with("554 "));
    assert!(replied(&mut server, "QUIT").starts_with("221 "));
}
