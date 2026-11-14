//! The people a released relay kept: reading them out of that release's state
//! directory, and taking them into a release's own workspace.
//!
//! A release of tilo.sh starts its relay on fresh state, so the workspace it
//! serves does not read the earlier release's members, roles or invitations
//! back. The people are not lost with them: [`export`] reads the addresses and
//! people an earlier release kept ([`People`]) from a copy of its state
//! directory without writing to it, and [`import`] hands them to the running
//! relay, which keeps the addresses on its wait list and invites each person
//! through its door ([`Imported`]).
//!
//! The formats read here are the earlier release's, frozen: this build writes
//! none of them, and what it does not recognise it refuses rather than
//! half-reading.

use std::path::{Path, PathBuf};

use automerge::{AutoCommit, ROOT, ReadDoc as _, ScalarValue, Value};
use serde::Deserialize;
use tilo_relay_protocol::release_people::{Imported, Invitation, Member, People, Waiting};
use tilo_relay_protocol::workspaces::{
    Role, WaitListEntry, WorkspacesReply, WorkspacesRequest, contact_key,
};
use tilo_user_identity::DeviceCert;

use crate::workspaces::{WaitListSecrets, WaitListed, Workspaces};

/// How long an invitation the import makes can be redeemed: long enough that
/// someone who reads their mail a month late still gets in.
const INVITATION_LIFETIME_NS: u64 = 30 * 24 * 3600 * 1_000_000_000;

/// What the earlier release's workspace recorded on disk, as far as this build
/// reads it: its record's head, and its join ledger's invitations.
///
/// The record is one `postcard` struct whose field order is the format; the
/// fields after the ledger are not read, so a record that grew them still
/// reads here.
#[derive(Deserialize)]
struct Record {
    version: u32,
    workspace: String,
    #[expect(dead_code, reason = "read to reach the fields after it")]
    workspace_id: u64,
    name: String,
    #[expect(dead_code, reason = "read to reach the fields after it")]
    authority: DeviceCert,
    peer: Vec<PeerPart>,
    #[expect(dead_code, reason = "read to reach the fields after it")]
    generation: u64,
    #[expect(dead_code, reason = "read to reach the fields after it")]
    outgoing: Vec<Publication>,
    #[expect(dead_code, reason = "read to reach the fields after it")]
    published: usize,
    collaboration: LedgerHead,
}

/// The ledger's first field, which is all the export reads of it.
#[derive(Deserialize)]
struct LedgerHead {
    invitations: Vec<InvitationRecord>,
}

#[derive(Deserialize)]
struct Publication {
    #[expect(dead_code, reason = "read to reach the field after it")]
    bytes: Vec<u8>,
    #[expect(dead_code, reason = "read to reach the field after it")]
    checkpoint: bool,
}

#[derive(Deserialize)]
struct InvitationRecord {
    #[expect(dead_code, reason = "read to reach the fields after it")]
    request_id: [u8; 32],
    #[expect(dead_code, reason = "read to reach the fields after it")]
    request_digest: [u8; 32],
    issued: IssuedInvitation,
    grant: InviteGrant,
    #[expect(dead_code, reason = "read to reach the field after it")]
    cancellation: Option<CancellationRecord>,
}

#[derive(Deserialize)]
struct CancellationRecord {
    #[expect(dead_code, reason = "read to reach the fields after it")]
    request_id: [u8; 32],
    #[expect(dead_code, reason = "read to reach the field after it")]
    request_digest: [u8; 32],
}

#[derive(Deserialize)]
struct IssuedInvitation {
    invite_id: [u8; 32],
    #[expect(dead_code, reason = "read to reach the field after it")]
    signed_invitation: Vec<u8>,
}

/// An invitation the earlier release issued: whom it was for, and the address
/// its issuer named for them, which is where the release kept an address.
#[derive(Deserialize)]
struct InviteGrant {
    #[expect(dead_code, reason = "read to reach the fields after it")]
    workspace_id: u64,
    #[expect(dead_code, reason = "read to reach the fields after it")]
    room: String,
    #[expect(dead_code, reason = "read to reach the fields after it")]
    issuer: DeviceCert,
    #[expect(dead_code, reason = "read to reach the fields after it")]
    role: MemberRole,
    #[expect(dead_code, reason = "read to reach the fields after it")]
    nonce: [u8; 16],
    recipient_user_public_key: Option<[u8; 32]>,
    invitee_name: Option<String>,
    #[expect(dead_code, reason = "read to reach the fields after it")]
    expires_at_ns: u64,
    #[expect(dead_code, reason = "read to reach the fields after it")]
    transport: Vec<TransportHint>,
}

#[derive(Deserialize)]
struct TransportHint {
    #[expect(dead_code, reason = "read to reach the field after it")]
    kind: String,
    #[expect(dead_code, reason = "read to reach the field after it")]
    opaque_json: String,
    #[expect(dead_code, reason = "read to reach the field after it")]
    host: DeviceCert,
    #[expect(dead_code, reason = "read to reach the field after it")]
    service_public_key: [u8; 32],
    #[expect(dead_code, reason = "read to reach the field after it")]
    host_signature: Vec<u8>,
}

/// The roles the earlier release's grants named, in its order.
#[derive(Deserialize, Clone, Copy)]
enum MemberRole {
    Viewer,
    Commenter,
    Editor,
    Moderator,
}

/// Where a workspace's durable peer state was kept, in the record.
#[derive(Deserialize)]
enum PeerPart {
    Inline(Vec<u8>),
    Stored { digest: [u8; 32], len: u64 },
}

/// The wait list of the earlier release, version 2: each entry's address and
/// how it was left.
#[derive(Deserialize)]
struct WaitListStored {
    #[expect(
        dead_code,
        reason = "read to pick the layout, and the layout is this one"
    )]
    version: u32,
    entries: Vec<WaitListEntryStored>,
    #[expect(dead_code, reason = "read to reach the entries' format whole")]
    mailed: Vec<MailedStored>,
}

#[derive(Deserialize)]
struct WaitListEntryStored {
    #[expect(dead_code, reason = "read to reach the fields after it")]
    request_id: [u8; 32],
    contact: String,
    at_ns: u64,
    #[expect(dead_code, reason = "read to reach the fields after it")]
    removals: Vec<[u8; 32]>,
    #[expect(dead_code, reason = "read to reach the fields after it")]
    confirms: Vec<[u8; 32]>,
    #[expect(dead_code, reason = "read to reach the fields after it")]
    confirmed_at_ns: Option<u64>,
    language: String,
    #[expect(dead_code, reason = "read to reach the fields after it")]
    mailed_at_ns: Option<u64>,
    invites: Vec<[u8; 32]>,
    #[expect(dead_code, reason = "read to reach the fields after it")]
    invited_at_ns: Option<u64>,
    #[expect(dead_code, reason = "read to reach the fields after it")]
    invite_mailed_at_ns: Option<u64>,
}

#[derive(Deserialize)]
struct MailedStored {
    #[expect(dead_code, reason = "read to reach the fields after it")]
    key_sha256: [u8; 32],
    #[expect(dead_code, reason = "read to reach the fields after it")]
    at_ns: u64,
    #[expect(dead_code, reason = "read to reach the fields after it")]
    confirms: Vec<[u8; 32]>,
}

/// The earlier release's wait list, version 1: an entry was its address and
/// when it was left.
#[derive(Deserialize)]
struct WaitListStoredV1 {
    #[expect(
        dead_code,
        reason = "read to pick the layout, and the layout is this one"
    )]
    version: u32,
    entries: Vec<WaitListEntryStoredV1>,
}

#[derive(Deserialize)]
struct WaitListEntryStoredV1 {
    #[expect(dead_code, reason = "read to reach the fields after it")]
    request_id: [u8; 32],
    contact: String,
    at_ns: u64,
}

/// Read the people `state` holds, the state directory of the earlier release's
/// workspace, without writing anything under it.
///
/// # Errors
///
/// `state` holds no workspace record this build reads, or one of its files does
/// not read: the export says which, and writes nothing.
pub fn export(state: &Path) -> Result<People, String> {
    let rooms = rooms_directory(state)?;
    let record_path = one_record(&rooms)?;
    let record: Record =
        postcard::take_from_bytes(&std::fs::read(&record_path).map_err(saying(&record_path))?)
            .map(|(record, _)| record)
            .map_err(|error| format!("{}: {error}", record_path.display()))?;
    if record.version != 3 {
        return Err(format!(
            "{}: the record is version {}, and this export reads version 3",
            record_path.display(),
            record.version
        ));
    }
    let durable = durable_state(&record_path, &record.peer)?;
    let document = workspace_document(&durable)?;
    let document = AutoCommit::load(&document)
        .map_err(|error| format!("{}: the workspace document: {error}", record_path.display()))?;
    let kept = wait_list_of(&record_path.with_extension("wait-list"))?;
    let emails = addresses(
        &record.collaboration.invitations,
        &kept,
        &redeemed(&document),
    );
    let names = device_names(&document);
    let people = people_of(&document, &names, &emails)?;
    Ok(People {
        release: "v0.1.5".to_owned(),
        workspace: if record.name.is_empty() {
            record.workspace
        } else {
            record.name
        },
        wait_list: kept.iter().map(|entry| entry.waiting.clone()).collect(),
        members: people,
    })
}

/// The directory the earlier release kept its workspaces' records in: it was
/// handed the relay's state root, whose records live in `rooms/`; a deployment
/// that named `rooms` or the record's own directory is read too.
fn rooms_directory(state: &Path) -> Result<PathBuf, String> {
    for candidate in [
        state.join("relay").join("rooms"),
        state.join("rooms"),
        state.to_path_buf(),
    ] {
        if has_record(&candidate) {
            return Ok(candidate);
        }
    }
    Err(format!(
        "{} holds no workspace record: name the released relay's own state directory",
        state.display()
    ))
}

fn has_record(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "room")
        })
    })
}

/// The one workspace record in `rooms`: the released relay kept the workspace
/// it served there.
fn one_record(rooms: &Path) -> Result<PathBuf, String> {
    let records: Vec<PathBuf> = std::fs::read_dir(rooms)
        .map_err(saying(rooms))?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "room")
        })
        .collect();
    match records.as_slice() {
        [record] => Ok(record.clone()),
        [] => Err(format!("{} holds no workspace record", rooms.display())),
        many => Err(format!(
            "{} holds {} workspace records ({}); this export reads one workspace",
            rooms.display(),
            many.len(),
            many.iter()
                .filter_map(|path| path.file_name()?.to_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The record's durable peer state, whole: the parts it names inline, then the
/// large ones each from its parts directory.
fn durable_state(record_path: &Path, parts: &[PeerPart]) -> Result<Vec<u8>, String> {
    let directory = record_path.with_extension("parts");
    let mut whole = Vec::new();
    for part in parts {
        match part {
            PeerPart::Inline(bytes) => whole.extend_from_slice(bytes),
            PeerPart::Stored { digest, len } => {
                let path = directory.join(hex::encode(digest));
                let bytes = std::fs::read(&path).map_err(saying(&path))?;
                if bytes.len() as u64 != *len {
                    return Err(format!(
                        "{}: the part is {} bytes, and the record names {len}",
                        path.display(),
                        bytes.len()
                    ));
                }
                whole.extend_from_slice(&bytes);
            }
        }
    }
    Ok(whole)
}

/// The workspace's own document out of its durable peer state: the envelope's
/// version, then the base version, the workspace, its owner, and the document.
///
/// The envelope is the earlier release's (`DurableV4` inside `DurableV5` or
/// `DurableV6`); the rest of the state is not read, so its documents and
/// records may be any shape this build no longer knows.
fn workspace_document(durable: &[u8]) -> Result<Vec<u8>, String> {
    let reading = |error: postcard::Error| format!("the durable peer state: {error}");
    let (envelope, rest) = postcard::take_from_bytes::<u32>(durable).map_err(reading)?;
    let rest = match envelope {
        4 => rest,
        5 | 6 => postcard::take_from_bytes::<u32>(rest).map_err(reading)?.1,
        other => {
            return Err(format!(
                "the durable peer state is in envelope {other}, and this export reads 4, 5 or 6"
            ));
        }
    };
    let (base, rest) = postcard::take_from_bytes::<u32>(rest).map_err(reading)?;
    if base != 5 {
        return Err(format!(
            "the durable peer state is in base {base}, and this export reads 5"
        ));
    }
    let (_workspace, rest) = postcard::take_from_bytes::<String>(rest).map_err(reading)?;
    let (_owner, rest) = postcard::take_from_bytes::<[u8; 32]>(rest).map_err(reading)?;
    let (document, _) = postcard::take_from_bytes::<Option<Vec<u8>>>(rest).map_err(reading)?;
    document.ok_or_else(|| "the durable peer state holds no workspace document".to_owned())
}

/// One address as the earlier release kept it, and the invitations it was
/// mailed.
#[derive(Clone, Debug)]
struct Kept {
    waiting: Waiting,
    invites: Vec<[u8; 32]>,
}

/// The address each person came in at: an invitation that named them and an
/// address, or an address on the wait list an invitation was mailed to that
/// the person redeemed. The release kept no address for a person anywhere
/// else.
fn addresses(
    invitations: &[InvitationRecord],
    wait_list: &[Kept],
    redeemed: &std::collections::BTreeMap<[u8; 32], [u8; 32]>,
) -> std::collections::BTreeMap<[u8; 32], String> {
    let mut emails = std::collections::BTreeMap::new();
    for invitation in invitations {
        // The issuer named the address when it made the invitation, and the
        // person is whoever redeemed it.
        let Some(named) = invitation.grant.invitee_name.as_deref() else {
            continue;
        };
        let Ok(address) = tilo_relay_protocol::workspaces::contact_address(named) else {
            continue;
        };
        let person = invitation
            .grant
            .recipient_user_public_key
            .or_else(|| redeemed.get(&invitation.issued.invite_id).copied());
        if let Some(person) = person {
            emails.entry(person).or_insert(address);
        }
    }
    for entry in wait_list {
        let Some(person) = entry.invites.iter().find_map(|invite| redeemed.get(invite)) else {
            continue;
        };
        let Ok(address) = tilo_relay_protocol::workspaces::contact_address(&entry.waiting.contact)
        else {
            continue;
        };
        emails.entry(*person).or_insert(address);
    }
    emails
}

/// Who redeemed each invitation the workspace holds a receipt for, by the
/// invitation's id: the document keeps a receipt for every invitation that
/// brought someone in.
fn redeemed(document: &AutoCommit) -> std::collections::BTreeMap<[u8; 32], [u8; 32]> {
    let mut people = std::collections::BTreeMap::new();
    let Ok(rows) = document.get_all(&ROOT, "invitation_receipts_v1") else {
        return people;
    };
    for (value, object) in rows {
        if !matches!(value, Value::Object(automerge::ObjType::Map)) {
            continue;
        }
        for key in document.keys(&object) {
            let Some([Ok(invite), Ok(person)]) = receipt(document, &object, &key) else {
                continue;
            };
            people.insert(invite, person);
        }
    }
    people
}

/// The invitation id and the person a receipt names, as the document keeps
/// it.
fn receipt(
    document: &AutoCommit,
    object: &automerge::ObjId,
    key: &str,
) -> Option<[Result<[u8; 32], ()>; 2]> {
    let (Value::Scalar(value), _) = document.get(object, key).ok().flatten()? else {
        return None;
    };
    let ScalarValue::Bytes(bytes) = value.as_ref() else {
        return None;
    };
    let receipt: ReceiptRow = serde_json::from_slice(bytes).ok()?;
    Some([Ok(receipt.invite_id), Ok(receipt.user_public_key)])
}

/// The name the workspace knew each device by, from the document's own rows.
fn device_names(document: &AutoCommit) -> std::collections::BTreeMap<String, String> {
    let mut names = std::collections::BTreeMap::new();
    let Some((_, peers)) = document.get(&ROOT, "peers").ok().flatten() else {
        return names;
    };
    for key in document.keys(&peers) {
        if let Ok(Some((Value::Scalar(value), _))) = document.get(&peers, key.as_str())
            && let ScalarValue::Str(name) = value.as_ref()
        {
            names.insert(key, name.to_string());
        }
    }
    names
}

/// The device a person's certificate certifies, by user root, from the
/// document's own rows.
fn device_owners(document: &AutoCommit) -> std::collections::BTreeMap<String, String> {
    let mut owners = std::collections::BTreeMap::new();
    let Some((_, identities)) = document.get(&ROOT, "peer_identities_v1").ok().flatten() else {
        return owners;
    };
    for key in document.keys(&identities) {
        let Some((Value::Scalar(value), _)) =
            document.get(&identities, key.as_str()).ok().flatten()
        else {
            continue;
        };
        let ScalarValue::Bytes(bytes) = value.as_ref() else {
            continue;
        };
        let Ok(identity) = serde_json::from_slice::<IdentityRow>(bytes) else {
            continue;
        };
        owners.insert(key, hex::encode(identity.device_cert.user_public_key));
    }
    owners
}

#[derive(Deserialize)]
struct IdentityRow {
    device_cert: DeviceCert,
}

/// A receipt for one redeemed invitation, as the earlier release kept it.
#[derive(Deserialize)]
struct ReceiptRow {
    invite_id: [u8; 32],
    user_public_key: [u8; 32],
}

/// The workspace's people, its owner first: each person's root, the name the
/// workspace knew them by, and the address it kept for them.
fn people_of(
    document: &AutoCommit,
    names: &std::collections::BTreeMap<String, String>,
    emails: &std::collections::BTreeMap<[u8; 32], String>,
) -> Result<Vec<Member>, String> {
    let owner = document
        .get(&ROOT, "owner")
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_string().ok());
    let Some((_, members)) = document.get(&ROOT, "members").ok().flatten() else {
        return Err("the workspace document holds no members".to_owned());
    };
    let owners = device_owners(document);
    let mut people = Vec::new();
    for key in document.keys(&members) {
        let role = match document.get(&members, key.as_str()).ok().flatten() {
            Some((Value::Scalar(value), _)) => {
                let ScalarValue::Bytes(bytes) = value.as_ref() else {
                    continue;
                };
                let role: String = serde_json::from_slice(bytes)
                    .map_err(|error| format!("member {key}'s role: {error}"))?;
                role.to_lowercase()
            }
            _ => continue,
        };
        people.push(member(&owners, names, emails, &key, &role));
    }
    if let Some(owner) = owner {
        // The owner is not one of the document's member rows: they hold the
        // workspace from its beginning.
        people.retain(|person| !person.root.eq_ignore_ascii_case(&owner));
        people.insert(0, member(&owners, names, emails, &owner, "owner"));
    }
    people.sort_by(|left, right| {
        (left.role != "owner", &left.role, &left.name).cmp(&(
            right.role != "owner",
            &right.role,
            &right.name,
        ))
    });
    Ok(people)
}

fn member(
    owners: &std::collections::BTreeMap<String, String>,
    names: &std::collections::BTreeMap<String, String>,
    emails: &std::collections::BTreeMap<[u8; 32], String>,
    root: &str,
    role: &str,
) -> Member {
    let name = owners
        .iter()
        .find(|(_, person)| person.eq_ignore_ascii_case(root))
        .and_then(|(device, _)| names.get(device))
        .cloned()
        .unwrap_or_else(|| "Guest".to_owned());
    let email = hex::decode(root)
        .ok()
        .and_then(|root| <[u8; 32]>::try_from(root).ok())
        .and_then(|root| emails.get(&root).cloned());
    Member {
        root: root.to_owned(),
        name,
        email,
        role: role.to_owned(),
    }
}

/// The addresses the earlier release kept at its door, as they were left.
fn wait_list_of(path: &Path) -> Result<Vec<Kept>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let reading = |error: String| format!("{}: {error}", path.display());
    let (version, _) =
        postcard::take_from_bytes::<u32>(&bytes).map_err(|error| reading(error.to_string()))?;
    let entries = match version {
        2 => postcard::from_bytes::<WaitListStored>(&bytes)
            .map_err(|error| reading(error.to_string()))?
            .entries
            .into_iter()
            .map(|entry| Kept {
                waiting: Waiting {
                    contact: entry.contact,
                    language: entry.language,
                    added_at_ns: entry.at_ns,
                },
                invites: entry.invites,
            })
            .collect(),
        1 => postcard::from_bytes::<WaitListStoredV1>(&bytes)
            .map_err(|error| reading(error.to_string()))?
            .entries
            .into_iter()
            .map(|entry| Kept {
                waiting: Waiting {
                    contact: entry.contact,
                    language: "en".to_owned(),
                    added_at_ns: entry.at_ns,
                },
                invites: Vec::new(),
            })
            .collect(),
        other => {
            return Err(reading(format!(
                "the wait list is version {other}, and this export reads versions 1 and 2"
            )));
        }
    };
    Ok(entries)
}

/// Take `people` into the workspace `workspace` of `workspaces`, the relay's
/// own host device `host` asking: their addresses go on the door's wait list as
/// they were, and each person is invited through the door, mailed where their
/// address is known and handed over as a link otherwise.
///
/// Whoever was the earlier release's owner holds this workspace from its root,
/// so they are not invited: the workspace is kept for them.
pub async fn import(
    workspaces: &Workspaces,
    workspace: &str,
    host: &DeviceCert,
    people: People,
) -> Imported {
    let mut imported = Imported {
        waiting: Vec::new(),
        invitations: Vec::new(),
        refused: Vec::new(),
    };
    // The door keeps the addresses only where it takes people by invitation
    // and keeps a wait list. Where it does not, everyone is still invited, at a
    // link to hand over: nobody is dropped.
    let keeps = workspaces
        .door_policy(workspace)
        .await
        .is_some_and(|door| door.keeps_requests());
    if !keeps {
        imported.refused.push(
            "the workspace keeps no wait list: its addresses come over as invitations only"
                .to_owned(),
        );
    }
    let door = workspaces.mail_door();
    if door.is_none() && people.members.iter().any(|member| member.email.is_some()) {
        imported.refused.push(
            "the workspace sends no mail, so the people with an address are invited at a link \
             only (hand it over yourself), and none is mailed"
                .to_owned(),
        );
    }
    let owner = people
        .members
        .iter()
        .find(|member| member.role.eq_ignore_ascii_case("owner"));
    let inviter = owner.map(|owner| owner.name.clone());
    for waiting in &people.wait_list {
        if !keeps {
            imported.refused.push(format!(
                "{} was on the wait list, and this workspace keeps none: invite them by hand",
                waiting.contact
            ));
            continue;
        }
        match keep(workspaces, workspace, &waiting.contact, &waiting.language).await {
            Ok(()) => imported.waiting.push(waiting.clone()),
            Err(reason) => imported.refused.push(format!(
                "{} stays off the wait list: {reason}",
                waiting.contact
            )),
        }
    }
    for member in &people.members {
        if member.role.eq_ignore_ascii_case("owner") {
            continue;
        }
        let Some(role) = role_of(&member.role) else {
            imported.refused.push(format!(
                "{} was a {} and this workspace has no such role",
                member.name, member.role
            ));
            continue;
        };
        match invite(
            workspaces,
            workspace,
            host,
            member,
            role,
            inviter.clone(),
            door.as_deref(),
            keeps,
        )
        .await
        {
            Ok(invitation) => imported.invitations.push(invitation),
            Err(reason) => imported
                .refused
                .push(format!("{} was not invited: {reason}", member.name)),
        }
    }
    imported.waiting = listed(workspaces, workspace, host).await;
    imported
}

/// What the workspace's role `role` is called in this build, if it has one.
fn role_of(role: &str) -> Option<Role> {
    Some(match role.to_lowercase().as_str() {
        "viewer" => Role::Viewer,
        "commenter" => Role::Commenter,
        "editor" => Role::Editor,
        "moderator" => Role::Moderator,
        _ => return None,
    })
}

/// Keep `contact` on the workspace's wait list as the door lists an address
/// whose mail needs no confirming: the person left it at the door of a release
/// that listed it at once.
async fn keep(
    workspaces: &Workspaces,
    workspace: &str,
    contact: &str,
    language: &str,
) -> Result<(), String> {
    // No one holds this secret: the entry is listed at once, and whoever may
    // invite takes it off the list as any other entry.
    let secrets = WaitListSecrets {
        removal: rand::random(),
        confirm: None,
    };
    // No cap: the list held these before, and a full list would drop people the
    // earlier release kept.
    match workspaces
        .wait(workspace, contact, secrets, language, usize::MAX)
        .await
        .map_err(|error| error.to_string())?
    {
        WaitListed::Added { .. } | WaitListed::Already { .. } => Ok(()),
        WaitListed::Full => Err("the wait list is full".to_owned()),
    }
}

/// Invite `member` through the workspace's door: at the wait-list entry their
/// address keeps, so the door mails them the invitation, or as a link where no
/// address is known.
async fn invite(
    workspaces: &Workspaces,
    workspace: &str,
    host: &DeviceCert,
    member: &Member,
    role: Role,
    inviter: Option<String>,
    door: Option<&str>,
    keeps: bool,
) -> Result<Invitation, String> {
    let mut entry = None;
    if let (Some(email), true) = (&member.email, keeps) {
        keep(workspaces, workspace, email, "en").await?;
        entry = wait_list_entry(workspaces, workspace, host, email).await?;
    }
    let issued = workspaces
        .page_workspaces(
            host,
            WorkspacesRequest::Invite {
                workspace: workspace.to_owned(),
                request_id: rand::random(),
                role,
                expires_at_ns: tilo_clock::unix_now_ns().saturating_add(INVITATION_LIFETIME_NS),
                inviter_name: inviter,
                wait_list_entry: entry,
                recipient: None,
                administrator: false,
            },
        )
        .await;
    let WorkspacesReply::Invitation {
        signed_invitation, ..
    } = issued
    else {
        return Err(match issued {
            WorkspacesReply::Refused(reason) => reason,
            other => format!("the workspace answered no invitation: {other:?}"),
        });
    };
    let door = door.ok_or_else(|| {
        "the workspace sends no mail, so there is no door a link leads to".to_owned()
    })?;
    let link = format!("{door}?invite={}", hex::encode(signed_invitation));
    let mut mailed = false;
    if let Some(entry) = entry {
        let mailed_reply = workspaces
            .page_workspaces(
                host,
                WorkspacesRequest::MailInvite {
                    workspace: workspace.to_owned(),
                    wait_list_entry: entry,
                    link: link.clone(),
                },
            )
            .await;
        mailed = matches!(mailed_reply, WorkspacesReply::WaitList(_));
        if !mailed {
            return Err(format!(
                "the door did not mail the invitation: {mailed_reply:?}"
            ));
        }
    }
    Ok(Invitation {
        name: member.name.clone(),
        email: member.email.clone(),
        role: role.name().to_lowercase(),
        link,
        mailed,
    })
}

/// The wait-list entry `contact` holds now, as the door keys it.
async fn wait_list_entry(
    workspaces: &Workspaces,
    workspace: &str,
    host: &DeviceCert,
    contact: &str,
) -> Result<Option<[u8; 32]>, String> {
    let listed = workspaces
        .page_workspaces(
            host,
            WorkspacesRequest::WaitList {
                workspace: workspace.to_owned(),
            },
        )
        .await;
    let WorkspacesReply::WaitList(entries) = listed else {
        return Err(format!("the wait list does not read: {listed:?}"));
    };
    let key = contact_key(contact);
    Ok(entries
        .into_iter()
        .find(|entry| contact_key(&entry.contact) == key)
        .map(|entry: WaitListEntry| entry.request_id))
}

/// The workspace's wait list as the door lists it, for whoever administers it.
async fn listed(workspaces: &Workspaces, workspace: &str, host: &DeviceCert) -> Vec<Waiting> {
    let listed = workspaces
        .page_workspaces(
            host,
            WorkspacesRequest::WaitList {
                workspace: workspace.to_owned(),
            },
        )
        .await;
    let WorkspacesReply::WaitList(entries) = listed else {
        return Vec::new();
    };
    entries
        .into_iter()
        .map(|entry| Waiting {
            contact: entry.contact,
            language: "en".to_owned(),
            added_at_ns: entry.added_at_ns,
        })
        .collect()
}

fn saying(path: &Path) -> impl Fn(std::io::Error) -> String + '_ {
    move |error| format!("{}: {error}", path.display())
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion"
)]
mod tests {
    use super::*;

    /// The durable peer state an earlier release wrote, with `document` as its
    /// workspace document.
    fn durable(envelope: u32, document: &[u8]) -> Vec<u8> {
        let mut bytes = postcard::to_stdvec(&envelope).unwrap();
        if envelope != 4 {
            bytes.extend(postcard::to_stdvec(&4u32).unwrap());
        }
        bytes.extend(postcard::to_stdvec(&5u32).unwrap());
        bytes.extend(
            postcard::to_stdvec(&"9f9c1e1e-1a2b-4c3d-8e4f-0123456789ab".to_owned()).unwrap(),
        );
        bytes.extend(postcard::to_stdvec(&[0u8; 32]).unwrap());
        bytes.extend(postcard::to_stdvec(&Some(document.to_vec())).unwrap());
        // What the rest of the state holds, which this build does not read.
        bytes.extend(postcard::to_stdvec(&7u32).unwrap());
        bytes
    }

    #[test]
    fn the_workspace_document_reads_out_of_every_envelope() {
        for envelope in [4, 5, 6] {
            assert_eq!(
                workspace_document(&durable(envelope, b"the document")).unwrap(),
                b"the document"
            );
        }
    }

    #[test]
    fn a_durable_state_of_another_shape_is_refused() {
        let refused = workspace_document(&durable(7, b"the document")).unwrap_err();
        assert!(refused.contains("envelope 7"), "{refused}");
    }

    /// The wait list an earlier release wrote, as its version holds it.
    fn wait_list(version: u32, contacts: &[&str]) -> Vec<u8> {
        use serde::Serialize;
        #[derive(Serialize)]
        struct Stored {
            version: u32,
            entries: Vec<Entry>,
            mailed: Vec<Mailed>,
        }
        #[derive(Serialize)]
        struct Entry {
            request_id: [u8; 32],
            contact: String,
            at_ns: u64,
            removals: Vec<[u8; 32]>,
            confirms: Vec<[u8; 32]>,
            confirmed_at_ns: Option<u64>,
            language: String,
            mailed_at_ns: Option<u64>,
            invites: Vec<[u8; 32]>,
            invited_at_ns: Option<u64>,
            invite_mailed_at_ns: Option<u64>,
        }
        #[derive(Serialize)]
        struct Mailed {
            key_sha256: [u8; 32],
            at_ns: u64,
            confirms: Vec<[u8; 32]>,
        }
        #[derive(Serialize)]
        struct StoredV1 {
            version: u32,
            entries: Vec<EntryV1>,
        }
        #[derive(Serialize)]
        struct EntryV1 {
            request_id: [u8; 32],
            contact: String,
            at_ns: u64,
        }
        let bytes = if version == 1 {
            postcard::to_stdvec(&StoredV1 {
                version,
                entries: contacts
                    .iter()
                    .enumerate()
                    .map(|(index, contact)| EntryV1 {
                        request_id: [index as u8; 32],
                        contact: (*contact).to_owned(),
                        at_ns: 9,
                    })
                    .collect(),
            })
            .unwrap()
        } else {
            postcard::to_stdvec(&Stored {
                version,
                entries: contacts
                    .iter()
                    .enumerate()
                    .map(|(index, contact)| Entry {
                        request_id: [index as u8; 32],
                        contact: (*contact).to_owned(),
                        at_ns: 9,
                        removals: Vec::new(),
                        confirms: Vec::new(),
                        confirmed_at_ns: Some(9),
                        language: "de".to_owned(),
                        mailed_at_ns: None,
                        invites: vec![[index as u8; 32]],
                        invited_at_ns: None,
                        invite_mailed_at_ns: None,
                    })
                    .collect(),
                mailed: Vec::new(),
            })
            .unwrap()
        };
        bytes
    }

    #[test]
    fn the_wait_lists_addresses_are_read_in_both_versions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lounge.wait-list");
        std::fs::write(&path, wait_list(2, &["ada@example.org"])).unwrap();
        let kept = wait_list_of(&path).unwrap();
        assert_eq!(kept[0].waiting.contact, "ada@example.org");
        assert_eq!(kept[0].waiting.language, "de");
        assert_eq!(kept[0].invites, vec![[0; 32]]);

        std::fs::write(&path, wait_list(1, &["grace@example.org"])).unwrap();
        let kept = wait_list_of(&path).unwrap();
        assert_eq!(kept[0].waiting.contact, "grace@example.org");
        assert_eq!(kept[0].waiting.language, "en");
        assert!(kept[0].invites.is_empty());
    }

    #[test]
    fn a_wait_list_of_another_version_is_refused_rather_than_half_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lounge.wait-list");
        std::fs::write(&path, wait_list(3, &["ada@example.org"])).unwrap();
        let refused = wait_list_of(&path).unwrap_err();
        assert!(refused.contains("version 3"), "{refused}");
    }

    #[test]
    fn a_state_with_no_workspace_record_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let refused = export(dir.path()).unwrap_err();
        assert!(refused.contains("no workspace record"), "{refused}");
    }
}
