//! An invitation's short link, asked for by a caller that awaits it: a page
//! copying a link in one press, or inviting an address from the wait list,
//! has no loop that takes the directory's answers in, as the sheet has.

#![allow(
    clippy::expect_used,
    reason = "a fixture that cannot see its own reply has no fallback"
)]

use test_http::{Reply, Responder};
use tilo_workspace_directory::Store;

/// A store nobody reads answers the caller that awaits its link: the site
/// kept the sealed invitation, and the link names that site.
#[test]
fn a_link_awaited_from_a_store_no_loop_reads_arrives() {
    let site = Responder::start(vec![Reply::status(201)]);
    let site_url = url::Url::parse(&format!("{}/", site.url())).expect("the site's address");
    let store = Store::default();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let link = runtime
        .block_on(async {
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                store.shelve_soon(
                    "invite",
                    b"the signed invitation".to_vec(),
                    u64::MAX,
                    &site_url,
                    Some("https://rooms.tilo.test"),
                ),
            )
            .await
        })
        .expect("the link arrives, though no loop takes the store's answers in")
        .expect("the site kept the invitation");
    assert!(
        link.link.starts_with(&format!("{site_url}i/")),
        "the link names the site that keeps it: {}",
        link.link
    );
    let left = site.next_request();
    assert_eq!(left.method, "POST");
    assert!(left.target.starts_with("/i/"), "{}", left.target);
    assert_eq!(
        store.shelved("invite").map(|shelved| shelved.link),
        Some(link.link),
        "the store still knows where it left it, to take it back"
    );
}

/// The sheet hands over one link form, the site's short one: with no site to
/// keep it, or a site that keeps nothing, it shows no link and says why,
/// never the invitation's own text.
#[test]
fn a_link_no_site_keeps_is_not_handed_over_as_the_invitations_text() {
    use tilo_sheets::invite::Making;

    let text = tilo_workspace_join::encode_invitation_text(b"the signed invitation");
    let store = Store::default();
    let mut made = Making::default();
    let refused = store.hand_over(
        &mut made,
        "invite",
        &text,
        u64::MAX,
        String::new(),
        None,
        None,
    );
    assert!(refused.is_err(), "no site, no link");
    assert!(made.made().is_none() && !made.making());

    // A site that refuses it: the sheet says why once its answer is in.
    let site = Responder::start(vec![Reply::status(500)]);
    let site_url = url::Url::parse(&format!("{}/", site.url())).expect("the site's address");
    store
        .hand_over(
            &mut made,
            "invite",
            &text,
            u64::MAX,
            String::new(),
            Some(&site_url),
            None,
        )
        .expect("the invitation is on its way to the site");
    assert!(made.making(), "the sheet waits for the site");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !store.answered() {
        assert!(std::time::Instant::now() < deadline, "the site answers");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(store.follow_shelf(&mut made));
    assert!(made.made().is_none(), "nothing is handed over");
    assert!(
        made.note().is_some_and(|note| note.bad),
        "the sheet says why"
    );
}
