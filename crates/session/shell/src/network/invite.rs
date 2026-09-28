//! Signed invitation tickets for Iroh workspace collaboration.
//!
//! The token itself — its signed payload, the standing-invite attenuation and
//! validation — is [`neighbour_wire::invite`], shared with workspace hosts so
//! both validate the same bytes. This module mints tokens for the shell's own
//! workspaces and accepts them into the local tree.

pub(crate) mod enrollment_driver;
#[cfg(test)]
#[path = "invite/native_exchange_tests.rs"]
mod exchange_tests;
pub(crate) use tilo_joined_room::{VerifiedNativeEnrollment, VerifiedNativeEnrollmentIntent};

use tilo_user_identity::DeviceCert;

use crate::shell::state::{
    Collaborator, PeerOrigin, RoomId, Workspace, WorkspaceId, WorkspaceTree,
};

pub use neighbour_wire::invite::{
    HomeHostPin, InviteDraft, InviteRole, MAX_HOME_HOST_PINS, PeerInviteToken, invite_id,
    validate_invite_token, validate_invite_token_at,
};

fn now_unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Mint an invite for `workspace` with no always-on hosts and no bound
/// signing identity.
///
/// # Errors
///
/// Returns an error if `workspace` has no room identity, if `owner_addr`
/// does not belong to `key`, or if encoding the signed token fails.
pub fn make_invite_token(
    key: &iroh::SecretKey,
    workspace: &Workspace,
    owner_addr: &iroh::EndpointAddr,
    expiry_seconds_from_now: u64,
) -> std::io::Result<String> {
    make_invite_token_with_hosts(key, workspace, owner_addr, &[], expiry_seconds_from_now)
}

/// Mint an invite carrying optional always-on workspace hosts.
///
/// # Errors
///
/// Returns an error if `workspace` has no room identity, if `owner_addr`
/// does not belong to `key`, or if encoding the signed token fails.
pub fn make_invite_token_with_hosts(
    key: &iroh::SecretKey,
    workspace: &Workspace,
    owner_addr: &iroh::EndpointAddr,
    host_addrs: &[iroh::EndpointAddr],
    expiry_seconds_from_now: u64,
) -> std::io::Result<String> {
    make_invite_token_with_identity(
        key,
        workspace,
        owner_addr,
        host_addrs,
        None,
        expiry_seconds_from_now,
    )
}

/// Mint an invite, optionally binding the inviter's identity-signed device
/// certificate so the guest can verify who is inviting them, not just which
/// device.
///
/// # Errors
///
/// Returns an error if `workspace` has no room identity, if `owner_addr`
/// does not belong to `key`, if `owner_device_cert` is present but fails
/// verification or doesn't name `key`'s device, or if encoding the signed
/// token fails.
pub fn make_invite_token_with_identity(
    key: &iroh::SecretKey,
    workspace: &Workspace,
    owner_addr: &iroh::EndpointAddr,
    host_addrs: &[iroh::EndpointAddr],
    owner_device_cert: Option<DeviceCert>,
    expiry_seconds_from_now: u64,
) -> std::io::Result<String> {
    make_invite_token_with_identity_and_role(
        key,
        workspace,
        owner_addr,
        host_addrs,
        owner_device_cert,
        InviteRole::Editor,
        expiry_seconds_from_now,
    )
}

/// Mint an identity-bound invite whose signed payload limits the role granted
/// on redemption.
///
/// # Errors
///
/// Returns an error when the workspace or endpoint is invalid, the optional
/// device certificate does not match the signing key, or the token cannot be
/// encoded.
pub fn make_invite_token_with_identity_and_role(
    key: &iroh::SecretKey,
    workspace: &Workspace,
    owner_addr: &iroh::EndpointAddr,
    host_addrs: &[iroh::EndpointAddr],
    owner_device_cert: Option<DeviceCert>,
    role: InviteRole,
    expiry_seconds_from_now: u64,
) -> std::io::Result<String> {
    make_invite_token_with_recipient(
        key,
        workspace,
        owner_addr,
        host_addrs,
        &[],
        owner_device_cert,
        InviteTerms {
            role,
            recipient: None,
            expiry_seconds_from_now,
        },
    )
}

/// What a signed invite grants: the role, the person it is bound to (if
/// anyone), and how long it stays redeemable.
#[derive(Debug, Clone, Copy)]
pub struct InviteTerms<'a> {
    pub role: InviteRole,
    pub recipient: Option<InviteRecipient<'a>>,
    pub expiry_seconds_from_now: u64,
}

/// The person identity an invite is bound to, and the display name the
/// inviter signs for them.
#[derive(Debug, Clone, Copy)]
pub struct InviteRecipient<'a> {
    pub user_public_key: [u8; 32],
    pub name: Option<&'a str>,
}

/// Mint an invite bound to one recipient person identity. The optional name is
/// signed by the inviter and therefore safe for an offline host to project.
///
/// # Errors
///
/// Returns an error if the workspace, endpoint, device certificate, or
/// serialized signed token is invalid.
pub fn make_invite_token_with_recipient(
    key: &iroh::SecretKey,
    workspace: &Workspace,
    owner_addr: &iroh::EndpointAddr,
    host_addrs: &[iroh::EndpointAddr],
    home_host_pins: &[HomeHostPin],
    owner_device_cert: Option<DeviceCert>,
    terms: InviteTerms<'_>,
) -> std::io::Result<String> {
    let InviteTerms {
        role,
        recipient,
        expiry_seconds_from_now,
    } = terms;
    let recipient_user_public_key = recipient.map(|recipient| recipient.user_public_key);
    let invitee_name = recipient.and_then(|recipient| recipient.name);
    mint_for_workspace(
        key,
        workspace,
        InviteDraft {
            workspace_id: workspace.id.raw(),
            room_id: workspace.room.to_string(),
            owner_addr: owner_addr.clone(),
            host_addrs: host_addrs.to_vec(),
            home_host_pins: home_host_pins.to_vec(),
            owner_device_cert,
            role,
            recipient_user_public_key,
            invitee_name: invitee_name.map(str::to_string),
            standing: false,
            expiry_unix_seconds: now_unix_seconds().saturating_add(expiry_seconds_from_now),
        },
    )
}

/// Mint a **standing** invite to `workspace`, a public room: multi-use,
/// redeemable by any certified device, at most `role` (which may not exceed
/// Commenter), until `expiry_unix_seconds` (`None` for no expiry) or until
/// the room authority revokes it. `host_addrs` are the always-on hosts that
/// redeem it while this device is away.
///
/// # Errors
///
/// Refuses a room that is not public — an encrypted room admits people one
/// invitation at a time — plus every rule of [`neighbour_wire::invite::mint_invite`].
pub fn make_standing_invite_token(
    key: &iroh::SecretKey,
    workspace: &Workspace,
    owner_addr: &iroh::EndpointAddr,
    host_addrs: &[iroh::EndpointAddr],
    owner_device_cert: Option<DeviceCert>,
    role: InviteRole,
    expiry_unix_seconds: Option<u64>,
) -> std::io::Result<String> {
    if workspace.channel_class != collab_types::ChannelClass::Public {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a standing invite admits anyone, so only a public room may issue one",
        ));
    }
    mint_for_workspace(
        key,
        workspace,
        InviteDraft {
            host_addrs: host_addrs.to_vec(),
            owner_device_cert,
            standing: true,
            ..InviteDraft::new(
                workspace.id.raw(),
                workspace.room.to_string(),
                owner_addr.clone(),
                role,
                expiry_unix_seconds.unwrap_or(u64::MAX),
            )
        },
    )
}

fn mint_for_workspace(
    key: &iroh::SecretKey,
    workspace: &Workspace,
    draft: InviteDraft,
) -> std::io::Result<String> {
    if workspace.room.is_nil() {
        return Err(std::io::Error::other(
            "workspace has no room identity to invite into",
        ));
    }
    neighbour_wire::invite::mint_invite(key, draft)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))
}

/// Validate `token_str` and bind `local`'s workspace to the room it names.
///
/// # Errors
///
/// Returns an error if the token fails [`validate_invite_token`], if it
/// carries no valid room id, or if `local` names no local workspace to
/// accept the invite into.
pub fn accept_invite_into(
    tree: &mut WorkspaceTree,
    local: WorkspaceId,
    token_str: &str,
) -> Result<RoomId, String> {
    let token = validate_invite_token(token_str)?;
    let room = RoomId::parse(&token.room_id)
        .ok_or_else(|| "invite carries no valid room id".to_string())?;
    if tree.room_of(local).is_none() {
        return Err(format!(
            "no local workspace {} to accept the invite into",
            local.raw()
        ));
    }
    tree.set_room(local, room);
    tree.pin_room_authority(room, PeerOrigin::peer_id(&token.owner_peer_id));
    let endpoint: iroh::EndpointId = token
        .owner_peer_id
        .parse()
        .map_err(|e| format!("endpoint id parse: {e}"))?;
    let session = crate::network::behaviour::peer_liveness_handle(&endpoint);
    let handle = crate::shell::state::session_handle(session);
    let person = token.owner_device_cert.as_ref().map(|cert| {
        match collab_types::caps::Principal::person_from_key(
            collab_types::crypto::Ed25519PublicKey::from_bytes(cert.user_public_key),
        ) {
            collab_types::caps::Principal::Person(id) => id,
            _ => handle.clone(),
        }
    });
    tree.admit_collaborator(
        local,
        Collaborator::new(person.unwrap_or_else(|| handle.clone()), handle).with_session(session),
    );
    Ok(room)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;

    #[test]
    fn only_a_public_room_issues_a_standing_invite() {
        let key = iroh::SecretKey::from_bytes(&[51; 32]);
        let encrypted = workspace(51);
        assert!(
            make_standing_invite_token(
                &key,
                &encrypted,
                &endpoint(&key, 5151),
                &[],
                None,
                InviteRole::Commenter,
                None,
            )
            .unwrap_err()
            .to_string()
            .contains("only a public room")
        );
        let public = Workspace::new_public(WorkspaceId(52), "Community");
        let encoded = make_standing_invite_token(
            &key,
            &public,
            &endpoint(&key, 5252),
            &[],
            None,
            InviteRole::Commenter,
            None,
        )
        .expect("a public room's standing invite");
        let token = validate_invite_token(&encoded).expect("valid");
        assert!(token.standing);
        assert_eq!(token.room_id, public.room.to_string());
        assert_eq!(
            token.expiry_unix_seconds,
            u64::MAX,
            "no expiry was asked for"
        );
        assert!(
            make_standing_invite_token(
                &key,
                &public,
                &endpoint(&key, 5252),
                &[],
                None,
                InviteRole::Editor,
                None,
            )
            .is_err(),
            "a standing invite never grants Editor"
        );
    }

    fn workspace(id: u64) -> Workspace {
        Workspace::new(WorkspaceId(id), format!("ws-{id}"))
    }

    fn endpoint(key: &iroh::SecretKey, port: u16) -> iroh::EndpointAddr {
        iroh::EndpointAddr::new(key.public())
            .with_ip_addr(format!("127.0.0.1:{port}").parse().unwrap())
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn invitations_round_trip_their_room_hosts_and_roster() {
        // Phase `round_trip_binds_room_key_and_dialing_address`.
        {
            let key = iroh::SecretKey::from_bytes(&[7; 32]);
            let ws = workspace(99);
            let addr = endpoint(&key, 1234);
            let encoded = make_invite_token(&key, &ws, &addr, 3600).unwrap();
            let token = validate_invite_token(&encoded).unwrap();
            assert_eq!(token.workspace_id, 99);
            assert_eq!(token.room_id, ws.room.to_string());
            assert_eq!(token.owner_addr, addr);
            assert_eq!(token.owner_peer_id, key.public().to_string());
        }
        // Phase `round_trip_carries_signed_always_on_hosts`.
        {
            let owner = iroh::SecretKey::from_bytes(&[13; 32]);
            let host = iroh::SecretKey::from_bytes(&[14; 32]);
            let host_addr = endpoint(&host, 4321);
            let encoded = make_invite_token_with_hosts(
                &owner,
                &workspace(5),
                &endpoint(&owner, 1234),
                std::slice::from_ref(&host_addr),
                3600,
            )
            .unwrap();
            let token = validate_invite_token(&encoded).unwrap();
            assert_eq!(token.host_addrs, vec![host_addr]);
        }
        // Phase `invitations_minted_in_the_same_second_remain_distinct_and_valid`.
        {
            let owner = iroh::SecretKey::from_bytes(&[32; 32]);
            let workspace = workspace(32);
            let addr = endpoint(&owner, 3232);
            let first = make_invite_token_with_identity_and_role(
                &owner,
                &workspace,
                &addr,
                &[],
                None,
                InviteRole::Editor,
                3600,
            )
            .expect("first invite");
            let second = make_invite_token_with_identity_and_role(
                &owner,
                &workspace,
                &addr,
                &[],
                None,
                InviteRole::Editor,
                3600,
            )
            .expect("second invite");
            assert_ne!(first, second, "each invitation is independently revocable");
            assert_ne!(invite_id(&first), invite_id(&second));
            validate_invite_token(&first).expect("first remains valid");
            validate_invite_token(&second).expect("second remains valid");
        }
        // Phase `accepting_invite_adopts_room_and_roster_identity`.
        {
            let key = iroh::SecretKey::from_bytes(&[11; 32]);
            let host = workspace(11);
            let encoded = make_invite_token(&key, &host, &endpoint(&key, 3), 3600).unwrap();
            let mut guest = WorkspaceTree::default();
            let guest_id = WorkspaceId(4242);
            guest
                .desktop
                .workspaces
                .push(Workspace::new(guest_id, "guest"));
            assert_eq!(
                accept_invite_into(&mut guest, guest_id, &encoded).unwrap(),
                host.room
            );
            assert_eq!(guest.room_of(guest_id), Some(host.room));
            assert_eq!(guest.desktop.workspaces[0].collaborators.len(), 1);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn every_signed_invitation_field_is_tamper_evident() {
        // Phase `home_host_pin_is_signed_and_tamper_evident`.
        {
            use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

            let owner = iroh::SecretKey::from_bytes(&[0x21; 32]);
            let relay = iroh::SecretKey::from_bytes(&[0x22; 32]);
            let other_relay = iroh::SecretKey::from_bytes(&[0x23; 32]);
            let user = ed25519_dalek::SigningKey::from_bytes(&[0x24; 32]);
            let host_device = iroh::SecretKey::from_bytes(&[0x25; 32]);
            let mut owner_cert = DeviceCert {
                user_public_key: user.verifying_key().to_bytes(),
                device_public_key: *owner.public().as_bytes(),
                device_label: "Owner laptop".into(),
                capabilities: DeviceCapabilities::primary(),
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
            };
            sign_device_cert(&mut owner_cert, &user).expect("owner certificate");
            let pin = HomeHostPin {
                addr: endpoint(&relay, 4321),
                user_public_key: user.verifying_key().to_bytes(),
                device_public_key: *host_device.public().as_bytes(),
            };
            let encoded = make_invite_token_with_recipient(
                &owner,
                &workspace(25),
                &endpoint(&owner, 2525),
                std::slice::from_ref(&pin.addr),
                std::slice::from_ref(&pin),
                Some(owner_cert),
                InviteTerms {
                    role: InviteRole::Editor,
                    recipient: None,
                    expiry_seconds_from_now: 3600,
                },
            )
            .expect("pinned invite");
            let decoded = validate_invite_token(&encoded).expect("valid pinned invite");
            assert_eq!(decoded.home_host_pins, vec![pin]);

            let bytes = B64.decode(encoded).expect("base64");
            let mut forged: PeerInviteToken = serde_json::from_slice(&bytes).expect("json");
            forged.home_host_pins[0].addr = endpoint(&other_relay, 4343);
            let forged = B64.encode(serde_json::to_vec(&forged).expect("json"));
            assert!(
                validate_invite_token(&forged)
                    .expect_err("a changed host pin must invalidate the signature")
                    .contains("signature")
            );
        }
        // Phase `selected_role_is_signed_and_tamper_evident`.
        {
            let owner = iroh::SecretKey::from_bytes(&[31; 32]);
            let encoded = make_invite_token_with_identity_and_role(
                &owner,
                &workspace(31),
                &endpoint(&owner, 3131),
                &[],
                None,
                InviteRole::Viewer,
                3600,
            )
            .expect("viewer invite");
            assert_eq!(
                validate_invite_token(&encoded).expect("valid").role,
                InviteRole::Viewer
            );

            let bytes = B64.decode(encoded).expect("base64");
            let mut token: PeerInviteToken = serde_json::from_slice(&bytes).expect("json");
            token.role = InviteRole::Editor;
            let forged = B64.encode(serde_json::to_vec(&token).expect("json"));
            assert!(
                validate_invite_token(&forged)
                    .expect_err("role widening must invalidate the signature")
                    .contains("signature")
            );
        }
        // Phase `recipient_binding_and_name_are_signature_covered`.
        {
            let owner = iroh::SecretKey::from_bytes(&[18; 32]);
            let workspace = workspace(18);
            let recipient = [0x44; 32];
            let encoded = make_invite_token_with_recipient(
                &owner,
                &workspace,
                &endpoint(&owner, 1818),
                &[],
                &[],
                None,
                InviteTerms {
                    role: InviteRole::Commenter,
                    recipient: Some(InviteRecipient {
                        user_public_key: recipient,
                        name: Some("Ada"),
                    }),
                    expiry_seconds_from_now: 3600,
                },
            )
            .expect("bound invite");
            let decoded = validate_invite_token(&encoded).expect("valid bound invite");
            assert_eq!(decoded.recipient_user_public_key, Some(recipient));
            assert_eq!(decoded.invitee_name.as_deref(), Some("Ada"));

            let bytes = B64.decode(encoded).expect("base64");
            let mut forged: PeerInviteToken = serde_json::from_slice(&bytes).expect("json");
            forged.recipient_user_public_key = Some([0x55; 32]);
            let forged = B64.encode(serde_json::to_vec(&forged).expect("json"));
            assert!(validate_invite_token(&forged).is_err());
        }
        // Phase `an_unsigned_recipient_cannot_be_grafted_onto_a_bearer`.
        {
            let owner = iroh::SecretKey::from_bytes(&[19; 32]);
            let workspace = workspace(19);
            let encoded = make_invite_token(&owner, &workspace, &endpoint(&owner, 1919), 3600)
                .expect("bearer invite");
            let bytes = B64.decode(encoded).expect("base64");
            let mut forged: PeerInviteToken = serde_json::from_slice(&bytes).expect("json");
            forged.recipient_user_public_key = Some([0x66; 32]);
            let forged = B64.encode(serde_json::to_vec(&forged).expect("json"));
            assert!(validate_invite_token(&forged).is_err());
        }
        // Phase `tampering_any_signed_route_field_is_rejected`.
        {
            let key = iroh::SecretKey::from_bytes(&[8; 32]);
            let encoded = make_invite_token(&key, &workspace(1), &endpoint(&key, 1), 3600).unwrap();
            let bytes = B64.decode(encoded).unwrap();
            let mut token: PeerInviteToken = serde_json::from_slice(&bytes).unwrap();
            token.workspace_id = 2;
            let forged = B64.encode(serde_json::to_vec(&token).unwrap());
            assert!(
                validate_invite_token(&forged)
                    .unwrap_err()
                    .contains("signature")
            );
        }
        // Phase `substituted_endpoint_is_rejected`.
        {
            let owner = iroh::SecretKey::from_bytes(&[9; 32]);
            let attacker = iroh::SecretKey::from_bytes(&[10; 32]);
            let encoded =
                make_invite_token(&owner, &workspace(1), &endpoint(&owner, 1), 3600).unwrap();
            let bytes = B64.decode(encoded).unwrap();
            let mut token: PeerInviteToken = serde_json::from_slice(&bytes).unwrap();
            token.owner_addr = endpoint(&attacker, 2);
            let forged = B64.encode(serde_json::to_vec(&token).unwrap());
            assert!(validate_invite_token(&forged).is_err());
        }
    }

    #[test]
    fn invitation_identity_is_bound_and_unsupported_invites_are_refused() {
        // Phase `identity_certificate_binds_inviter_person_to_device_endpoint`.
        {
            use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

            let root = ed25519_dalek::SigningKey::from_bytes(&[41; 32]);
            let device = iroh::SecretKey::from_bytes(&[42; 32]);
            let mut cert = DeviceCert {
                user_public_key: root.verifying_key().to_bytes(),
                device_public_key: *device.public().as_bytes(),
                device_label: "Laptop".into(),
                capabilities: DeviceCapabilities::primary(),
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
            };
            sign_device_cert(&mut cert, &root).expect("cert");
            let ws = workspace(17);
            let encoded = make_invite_token_with_identity(
                &device,
                &ws,
                &endpoint(&device, 1717),
                &[],
                Some(cert.clone()),
                3600,
            )
            .expect("invite");
            let decoded = validate_invite_token(&encoded).expect("valid");
            assert_eq!(decoded.owner_device_cert, Some(cert));

            let mut tree = WorkspaceTree::default();
            let local = WorkspaceId(99);
            tree.desktop.workspaces.push(workspace(99));
            accept_invite_into(&mut tree, local, &encoded).expect("accept");
            let collab_types::caps::Principal::Person(person) =
                collab_types::caps::Principal::person_from_key(
                    collab_types::crypto::Ed25519PublicKey::from_bytes(
                        root.verifying_key().to_bytes(),
                    ),
                )
            else {
                unreachable!()
            };
            assert!(
                tree.desktop.workspaces[0]
                    .collaborators
                    .iter()
                    .any(|member| member.id == person)
            );
        }
        // Phase `nil_room_cannot_be_invited`.
        {
            let key = iroh::SecretKey::from_bytes(&[12; 32]);
            let mut ws = workspace(4);
            ws.room = RoomId::nil();
            assert!(make_invite_token(&key, &ws, &endpoint(&key, 4), 3600).is_err());
        }
    }

    /// Only a signed bearer invitation admits anyone. A token in an earlier
    /// shape (one that predates room hosts, roles and recipient binding, or a
    /// libp2p-era token naming a multiaddr) is refused even when the key that
    /// signed it is genuine, so it can never admit someone at a default role
    /// the inviter did not sign.
    #[test]
    fn tokens_in_an_earlier_shape_are_refused() {
        let key = iroh::SecretKey::from_bytes(&[16; 32]);
        let ws = workspace(7);
        let current = make_invite_token(&key, &ws, &endpoint(&key, 2468), 3600).expect("invite");
        assert!(validate_invite_token(&current).is_ok());
        let mut fields: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(&B64.decode(&current).unwrap()).unwrap();

        // A token that predates hosts, pins, roles and recipient binding:
        // the fields the signature now covers are simply absent.
        let mut legacy = fields.clone();
        for field in [
            "host_addrs",
            "home_host_pins",
            "owner_device_cert",
            "role",
            "invite_nonce",
            "recipient_user_public_key",
            "invitee_name",
        ] {
            legacy.remove(field);
        }
        let legacy = B64.encode(serde_json::to_vec(&legacy).unwrap());
        assert!(
            validate_invite_token(&legacy).is_err(),
            "a pre-bearer token must not validate"
        );

        // A libp2p-era token: a multiaddr and a protobuf public key in place
        // of an endpoint address, signed by the same key.
        let mut libp2p = serde_json::Map::new();
        libp2p.insert("workspace_id".into(), fields["workspace_id"].clone());
        libp2p.insert("room_id".into(), fields["room_id"].clone());
        libp2p.insert("owner_peer_id".into(), key.public().to_string().into());
        let mut public_protobuf = vec![0x08, 0x01, 0x12, 0x20];
        public_protobuf.extend_from_slice(key.public().as_bytes());
        libp2p.insert(
            "owner_public_key".into(),
            B64.encode(public_protobuf).into(),
        );
        libp2p.insert("owner_addr".into(), "/ip4/127.0.0.1/tcp/1234".into());
        libp2p.insert(
            "expiry_unix_seconds".into(),
            fields["expiry_unix_seconds"].clone(),
        );
        let signature = key.sign(serde_json::to_vec(&libp2p).unwrap().as_slice());
        libp2p.insert(
            "signature".into(),
            serde_json::to_value(signature.to_bytes().to_vec()).unwrap(),
        );
        let libp2p = B64.encode(serde_json::to_vec(&libp2p).unwrap());
        assert!(
            validate_invite_token(&libp2p).is_err(),
            "a libp2p-era token must not validate"
        );

        // Neither shape is accepted into a workspace either.
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces.push(workspace(99));
        assert!(accept_invite_into(&mut tree, WorkspaceId(99), &legacy).is_err());
        assert!(accept_invite_into(&mut tree, WorkspaceId(99), &libp2p).is_err());

        // And a current token stops validating the moment a signed field is
        // dropped from it.
        fields.remove("role");
        let stripped = B64.encode(serde_json::to_vec(&fields).unwrap());
        assert!(validate_invite_token(&stripped).is_err());
    }
}
