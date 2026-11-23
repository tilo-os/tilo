//! The list of a person's other workspaces on a server that keeps people's own workspaces, as a
//! site's server does, for a person who is not the server's own: their assistant lives in the
//! workspace the server keeps as theirs, and somebody else shares a workspace with them. Every
//! call goes through a whole turn of the assistant, with a model that calls the tools.
use super::*;
use tilo_server::workspaces::IssueInvitationRequest;

/// What the server calls itself to the people it keeps workspaces for.
const SITE: &str = "tilo.test";

/// A server that keeps people's own workspaces, and on it Bo, who is not the server's person.
struct Kept {
    /// The server, which is Ada's.
    served: Served,
    /// The workspace Ada shares with Bo: her agent in it, its chat, what she tells her agent
    /// there, which is hers alone, and a letter every member reads. Her page on it.
    shared: (String, Browser),
    /// The server's home workspace, through which it lets people in. It invited Bo, and he
    /// never joined it.
    home: String,
    /// Bo's root key and the one device he has.
    bo: (SigningKey, crate::support::Device),
    /// The workspace the server keeps as Bo's own, with his assistant in it. His page on it.
    own: (String, Browser),
}

impl Kept {
    /// The server, with Bo admitted to the workspace Ada shares as `role`, the workspace of his
    /// own made when his device asked the server for it, and his assistant in it.
    async fn start(role: Role) -> Self {
        // Ada's server and the workspace she shares, set up as every server of these tests is.
        let mut served = Served::start(1).await;
        let (shared, shared_page, _) = served.workspace(|_| Vec::new()).await;
        // From here on it keeps workspaces of people's own, for the people its home workspace
        // lets in: what a site's server is told when it starts.
        let workspaces = served.workspaces.clone();
        assert!(
            workspaces
                .offer_own_workspaces(tilo_server::workspaces::OwnWorkspaceOffer::free(SITE))
                .unwrap(),
            "the server keeps people's own workspaces from now on"
        );
        let home = workspaces.create("Lounge").await.unwrap().workspace;
        assert!(
            workspaces.admit_own_through(&home),
            "the home is named once"
        );

        // Bo is somebody else. His one device is certified now, so it is the newest he has
        // wherever that is asked: the fixture certifies every other device at one early moment.
        let (bo, _) = tilo_user_identity::UserIdentity::generate("Bo", 1);
        let mut device = enrol(&bo, "browser", DeviceCapabilities::member());
        device.cert.issued_at_ns = tilo_clock::unix_now_ns();
        tilo_user_identity::sign_device_cert(&mut device.cert, &bo)
            .expect("his root certifies his browser");
        let person = bo.verifying_key().to_bytes();
        // The site lets him in as it does anyone it invites: an invitation into its home names
        // him. And the workspace Ada shares admits him.
        workspaces
            .issue(
                &home,
                &served.devices.0.cert,
                IssueInvitationRequest {
                    request_id: person,
                    role: Role::Viewer,
                    recipient_user_public_key: Some(person),
                    invitee_name: Some("Bo".into()),
                    inviter_name: None,
                    expires_at_ns: tilo_clock::unix_now_ns()
                        .checked_add(600_000_000_000)
                        .unwrap(),
                },
            )
            .await
            .unwrap();
        admit_as(&served, &shared, &device, role).await;

        // His device asks the server for a workspace of his own, as a page does, redeems the
        // invitation into it itself, and joins it.
        let server = &served.devices.0;
        let peer = Browser::session(server_of(&served), &device, server);
        let events = peer.events();
        crate::support::ready(&[&peer]).await;
        let asking = tilo_session_client::device::Device::of(
            peer.clone(),
            device.cert.clone(),
            server.cert.clone(),
            "browser",
        );
        let own = asking
            .enter_own_workspace("en-US", None, "Bo", |_| false, None)
            .await
            .expect("the server makes Bo a workspace of his own and admits his device")
            .workspace;
        assert!(
            workspaces.owns(&person, &own),
            "the server keeps that workspace as his"
        );
        let own_id = asking
            .join(&own)
            .await
            .expect("his device joins his own workspace");
        crate::support::admitted_to(&peer, own_id).await;
        let own_page = Browser::joined((peer, events), &device, server, &own);

        // His assistant, made as the Agents tab of his page makes it, and the workspace's chat.
        make_agent(&own_page, &workspaces, &own, &hex::encode(person), None).await;
        let governor = workspaces.device_cert().clone();
        workspaces
            .prepare_application(&own, &governor, &crate::common::catalogue(), CHAT, "")
            .await
            .unwrap();
        Self {
            served,
            shared: (shared, shared_page),
            home,
            bo: (bo, device),
            own: (own, own_page),
        }
    }

    /// Bo writes `words` to his assistant in his own workspace, and the server answers that one
    /// turn with a model that calls `calls`. What each call was answered, in order.
    async fn bo_asks(&self, words: &str, calls: &[(&str, serde_json::Value)]) -> Vec<String> {
        let placed = server_of(&self.served).environments();
        asked_by(
            &self.served,
            (&self.own.0, Some((&self.bo.0, &self.bo.1))),
            (&self.bo.0, &self.bo.1),
            (words, calls),
            Some(&placed),
        )
        .await
    }

    /// Close the pages the fixture opened.
    fn close(self) {
        for page in [self.shared.1, self.own.1] {
            page.peer.close();
        }
    }
}

/// On a server that keeps people's own workspaces, a person who is not the server's person keeps
/// their assistant in the workspace the server keeps as theirs. A turn of it there finds the
/// list among its tools and is told the workspace somebody else shares with them, where they
/// only read, and no other: not the one the turn is in, and not the site's home, which invited
/// them and which they never joined. The assistant of the server's own person lives in a
/// workspace no server keeps as theirs, and is told of no list: its call is answered what a
/// call of something that does not exist is answered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn on_a_server_that_keeps_peoples_own_workspaces_a_persons_assistant_lists_where_else_they_are()
 {
    let kept = Box::pin(Kept::start(Role::Viewer)).await;
    let calls = everything_then_the_list();
    let shown = kept
        .bo_asks("Which other workspaces am I in?", &calls)
        .await;
    assert_eq!(shown.len(), 2, "two tool calls were answered: {shown:?}");
    assert!(
        shown[0].contains(NAMED),
        "the turn's listing of everything names the list: {}",
        shown[0]
    );
    assert_eq!(
        listed_in(&shown[1]),
        BTreeMap::from([(kept.shared.0.clone(), "not_asked".to_owned())]),
        "the workspace shared with him and no other, and nothing read there unasked: {}",
        shown[1]
    );
    assert!(
        !shown[1].contains(&kept.home) && !shown[1].contains(&kept.own.0),
        "neither the site's home nor the workspace the turn is in is named: {}",
        shown[1]
    );

    // The server is Ada's. The workspace she shares is hers as its owner, and no server keeps
    // it as hers: her agent there is told of no list, in the words of something that does not
    // exist and not in words that say why.
    let nothing = "no-such-thing/workspaces#list";
    let hers = [
        ("tilo_do", serde_json::json!({"path": LIST, "args": {}})),
        ("tilo_do", serde_json::json!({"path": nothing, "args": {}})),
    ];
    let placed = server_of(&kept.served).environments();
    let shown = asked_by(
        &kept.served,
        (&kept.shared.0, None),
        (&kept.served.ada, &kept.served.devices.1),
        ("Which other workspaces am I in?", &hers),
        Some(&placed),
    )
    .await;
    assert_eq!(shown.len(), 2, "two tool calls were answered: {shown:?}");
    assert_eq!(
        shown[0].replace(NAMED, "no-such-thing"),
        shown[1],
        "nothing tells the list apart from something that does not exist"
    );
    assert!(
        !shown[0].contains(&kept.own.0) && !shown[0].contains("another member"),
        "{}",
        shown[0]
    );
    kept.close();
}

/// Once somebody else is a member of the workspace a server keeps as a person's own, a turn of
/// theirs that begins there is still named the list, and its call says why nothing is listed:
/// whoever is in that workspace can read the conversation. It names no workspace of theirs. A
/// turn the other member starts on the assistant is told of no list at all, in the words of
/// something that does not exist.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn once_somebody_else_is_in_a_persons_own_workspace_their_assistant_says_why_it_lists_nothing()
 {
    let kept = Box::pin(Kept::start(Role::Viewer)).await;
    // Cy joins the workspace Bo's assistant lives in.
    let (cy, _) = tilo_user_identity::UserIdentity::generate("Cy", 1);
    let cy_device = enrol(&cy, "browser", DeviceCapabilities::member());
    admit_as(&kept.served, &kept.own.0, &cy_device, Role::Editor).await;

    let calls = everything_then_the_list();
    let shown = kept
        .bo_asks("Which other workspaces am I in?", &calls)
        .await;
    assert_eq!(shown.len(), 2, "two tool calls were answered: {shown:?}");
    assert!(
        shown[0].contains(NAMED),
        "a turn of his that begins now is still named the list: {}",
        shown[0]
    );
    assert!(
        shown[1].contains("has another member now"),
        "and its call says why nothing is listed: {}",
        shown[1]
    );
    assert!(
        listed_in(&shown[1]).is_empty() && !shown[1].contains(&kept.shared.0),
        "it names no workspace of his: {}",
        shown[1]
    );

    // A turn Cy starts on Bo's assistant acts for Cy: it is told of no list, and nothing of why.
    let nothing = "no-such-thing/workspaces#list";
    let his = [
        ("tilo_ls", serde_json::json!({})),
        ("tilo_do", serde_json::json!({"path": LIST, "args": {}})),
        ("tilo_do", serde_json::json!({"path": nothing, "args": {}})),
    ];
    let placed = server_of(&kept.served).environments();
    let shown = asked_by(
        &kept.served,
        (&kept.own.0, Some((&kept.bo.0, &kept.bo.1))),
        (&cy, &cy_device),
        ("Which other workspaces is Bo in?", &his),
        Some(&placed),
    )
    .await;
    assert_eq!(shown.len(), 3, "three tool calls were answered: {shown:?}");
    assert!(
        !shown[0].contains(NAMED),
        "his turn's tools name no list of other workspaces: {}",
        shown[0]
    );
    assert_eq!(
        shown[1].replace(NAMED, "no-such-thing"),
        shown[2],
        "nothing tells the list apart from something that does not exist"
    );
    assert!(
        !shown[1].contains(&kept.shared.0) && !shown[1].contains("another member"),
        "{}",
        shown[1]
    );
    kept.close();
}
