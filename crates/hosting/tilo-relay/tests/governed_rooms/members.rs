//! Admitting a member to a governed room, as every test of its members
//! starts.
use super::*;

/// Admit `member`'s person to `room` as an Editor, through an invitation
/// `host` issues and `member`'s device redeems.
pub(super) async fn admit(relay: &Relay, host: &TestDevice, member: &TestDevice, room: &str) {
    let issued = relay
        .rooms()
        .unwrap()
        .issue(
            room,
            &host.cert,
            tilo_relay::rooms::IssueInvitationRequest {
                // One per member, so a room admits several this way.
                request_id: *blake3::hash(&member.device_pubkey()).as_bytes(),
                role: MemberRole::Editor,
                recipient_user_public_key: Some(member.cert.user_public_key),
                invitee_name: None,
                inviter_name: None,
                expires_at_ns: tilo_clock::unix_now_ns()
                    .checked_add(60_000_000_000)
                    .expect("fixture expiry fits"),
            },
        )
        .await
        .unwrap();
    let verified = decode_and_verify_invitation(
        &issued.signed_invitation,
        Some(member.cert.user_public_key),
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    WorkspaceJoinClient::new(device_endpoint(member).await)
        .redeem_hint(
            &verified.grant().transport[0],
            issued.signed_invitation,
            member.cert.clone(),
            &member.sk,
            *blake3::hash(&member.cert.user_public_key).as_bytes(),
        )
        .await
        .unwrap();
}
