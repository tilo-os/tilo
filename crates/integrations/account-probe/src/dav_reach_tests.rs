//! Where a server's own HTTP connections go for a person's accounts: the
//! settings documents a look for mail servers reads, a calendar server's
//! discovery and a DAV account's sign-in. Each is judged where it is
//! connected ([`crate::imap::Reach`]), not by a look-up made before it: a name
//! that answered publicly then and answers with this machine at the
//! connection is connected to by nobody, and neither is an address written
//! into the URL, which nothing looks up.

use std::net::{IpAddr, Ipv4Addr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::imap::Reach;
use crate::mail_servers::{Live, Sources as _};
use crate::{AccountTester as _, FindingAccountTester, LiveAccountTester};

/// A listener of this machine that waits for nobody, and its port.
fn listening() -> (TcpListener, u16) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("a port here");
    listener
        .set_nonblocking(true)
        .expect("a listener that waits for nobody");
    let port = listener.local_addr().expect("its address").port();
    (listener, port)
}

/// How many connections were made to `listener` since it was last asked.
fn connections(listener: &TcpListener) -> usize {
    std::iter::from_fn(|| listener.accept().ok()).count()
}

/// A look-up that answers with a public address the first time it is asked
/// and with this machine every time after, and counts how often it was
/// asked: a name that moves between the look-up made before a connection and
/// the connection's own.
fn turning(asked: &Arc<AtomicUsize>) -> tilo_net::address::Lookup {
    let asked = Arc::clone(asked);
    Arc::new(move |_name: String| {
        let nth = asked.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(vec![if nth == 0 {
                IpAddr::from([93, 184, 216, 34])
            } else {
                IpAddr::from(Ipv4Addr::LOCALHOST)
            }])
        })
    })
}

fn calendar_account(url: &str) -> accounts::Account {
    let mut account = accounts::Account::new("calendar", "caldav");
    account.config = serde_json::json!({ "server_url": url, "username": "ada" });
    account
}

fn dav_account(url: &str) -> accounts::Account {
    let mut account = accounts::Account::new("files", "webdav");
    account.config = serde_json::json!({ "base_url": url, "username": "ada" });
    account
}

/// The settings documents of a look for mail servers are read only within
/// the look's reach. The look-up made before said the name may be asked: it
/// answered publicly then. At the connection the name answers with this
/// machine, and nobody is connected to: it is looked up again there, and
/// judged there.
#[test]
fn a_settings_document_whose_name_answers_with_this_machine_is_read_from_nobody() {
    let (listener, port) = listening();
    let urls = [format!("https://localhost:{port}/mail/config-v1.1.xml")];
    let asked = Arc::new(AtomicUsize::new(0));
    let reach = Reach::public().looked_up_by(turning(&asked));
    let before = reach.clone();
    let allowed = move |name: &str| before.takes(name);
    assert!(
        allowed("localhost"),
        "the look-up made before answers publicly"
    );
    let live = Live::new(&allowed).within(reach);
    assert_eq!(live.documents(&urls), vec![None]);
    assert_eq!(connections(&listener), 0, "nothing is connected to");
    assert!(
        asked.load(Ordering::SeqCst) >= 2,
        "the name is looked up again at the connection, and judged there"
    );

    // What the test counts is seen: a device, which reaches its own network,
    // connects.
    let everyone = |_: &str| true;
    let _ = Live::new(&everyone).documents(&urls);
    assert!(connections(&listener) >= 1, "a device's own look connects");
}

/// A server's tester discovers a calendar account's calendars and proves a
/// DAV account only within its reach: a name that answered publicly at the
/// look-up made before and answers with this machine at the connection is
/// connected to by nobody, and neither is an address of this machine that
/// the account's URL writes out.
#[test]
fn a_calendar_whose_name_answers_with_this_machine_is_connected_to_by_nobody() {
    let (listener, port) = listening();
    let secret = accounts::Secret::password("a made-up password");
    let named = format!("https://localhost:{port}/dav/");
    let asked = Arc::new(AtomicUsize::new(0));
    let reach = Reach::public().looked_up_by(turning(&asked));
    assert!(
        reach.takes("localhost"),
        "the look-up made before answers publicly"
    );
    let server = FindingAccountTester(reach);
    assert!(
        server
            .discover_calendars(&calendar_account(&named), &secret)
            .is_err()
    );
    assert!(server.test(&dav_account(&named), &secret).is_err());
    assert_eq!(connections(&listener), 0, "nothing is connected to");
    assert!(
        asked.load(Ordering::SeqCst) >= 3,
        "each connection looks the name up again, and is judged there"
    );

    // An address written into the URL is looked up by nobody, and is judged
    // all the same.
    let written = format!("https://127.0.0.1:{port}/dav/");
    let public = FindingAccountTester(Reach::public());
    assert!(
        public
            .discover_calendars(&calendar_account(&written), &secret)
            .is_err()
    );
    assert!(public.test(&dav_account(&written), &secret).is_err());
    assert_eq!(connections(&listener), 0, "nothing is connected to");

    // What the test counts is seen: the device's tester, which reaches its
    // own network, connects.
    let _ = LiveAccountTester.discover_calendars(&calendar_account(&named), &secret);
    assert!(
        connections(&listener) >= 1,
        "a device's own tester connects"
    );
}
