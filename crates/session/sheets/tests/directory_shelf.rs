//! An invitation's short link, asked for by a caller that awaits it: a page
//! copying a link in one press, or inviting an address from the wait list,
//! has no loop that takes the directory's answers in, as the sheet has.

use test_http::{Reply, Responder};
use tilo_sheets::directory::Store;

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
                    Some("https://workspaces.tilo.test"),
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

/// The sheet can hand over the signed invitation even when no site keeps it.
#[test]
fn a_link_no_site_keeps_carries_the_signed_invitation() {
    use tilo_sheets::invite::Making;

    let signed = b"the signed invitation";
    let native = tilo_workspace_join::native_invitation_link(signed);
    let store = Store::default();
    let mut made = Making::default();
    store
        .hand_over(
            &mut made,
            tilo_sheets::invite::made("invite", "editor", "", u64::MAX),
            &native,
            None,
            None,
        )
        .expect("the invitation can be handed over without a shelf");
    let shown = made.made().expect("a native link is shown immediately");
    assert_eq!(shown.link, native);
    assert_eq!(
        tilo_workspace_join::invitation_bytes(&shown.link).expect("the link's signed bytes"),
        signed
    );
    assert!(!made.making());

    // A refusal from the optional shelf still leaves a link to copy.
    made.opening();
    let site = Responder::start(vec![Reply::status(500)]);
    let site_url = url::Url::parse(&format!("{}/", site.url())).expect("the site's address");
    store
        .hand_over(
            &mut made,
            tilo_sheets::invite::made("invite", "editor", "", u64::MAX),
            &native,
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
    assert_eq!(made.made().expect("native link after refusal").link, native);
    assert!(made.note().is_none());
}

/// A community's join is under way until its site answers: what waits on it — the shell's first
/// workspace, held back so a community's home workspace is not made a person's first — is let go
/// once the answer comes, whether the site let the person in or not.
#[test]
fn a_community_join_is_over_once_its_site_answers() {
    let site = Responder::handle(|_| Some(Reply::status(404)));
    let site_url = url::Url::parse(&format!("{}/", site.url())).expect("the site's address");
    let store = Store::default();
    let person = tilo_workspace_directory::Person {
        user: [7; 32],
        name: "Ada".to_owned(),
    };
    assert!(
        !store.community_answered(&site_url),
        "no join was asked of the site"
    );
    store
        .join_community(&site_url, &person)
        .expect("the join is asked");
    assert!(
        !store.community_answered(&site_url),
        "the site has not answered yet, so its workspace may still come"
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !store.answered() {
        assert!(std::time::Instant::now() < deadline, "the site answers");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        store.community_answered(&site_url),
        "the answer, one way or the other, ends the join"
    );
    let other = url::Url::parse("https://another.site/").expect("another site's address");
    assert!(
        !store.community_answered(&other),
        "a site this store asked nothing of has not answered"
    );
}
