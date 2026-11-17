//! The people a released relay kept: reading them out of that release's state
//! directory.
//!
//! A release of tilo.sh starts its relay on fresh state, so the workspace it
//! serves does not read the earlier release's members, roles, wait list or
//! invitations back. The people are not lost with them: [`export`] reads the
//! addresses and people an earlier release kept ([`People`]) from a copy of its
//! state directory without writing to it, and the running relay takes them in
//! over its operator socket (`tilo-relay release-people import`), where
//! `persistent_workspace_cli::import_people` keeps the addresses on its wait
//! list as they stood and declares or invites each person.
//!
//! The formats read here are the earlier release's, frozen: this build writes
//! none of them, and what it does not recognise it refuses rather than
//! half-reading.

use std::path::{Path, PathBuf};

use automerge::{AutoCommit, ROOT, ReadDoc as _, ScalarValue, Value};
use serde::Deserialize;
use tilo_relay_protocol::release_people::{Member, People, Waiting};
use tilo_user_identity::DeviceCert;

/// The grant ids the earlier release gave an administrator: the release's
/// governance names the grant `room-admin-v1:<root>`, and the rename of the
/// workspace words left states holding either prefix.
const ADMINISTRATOR_GRANTS: [&str; 2] = ["room-admin-v1:", "workspace-admin-v1:"];

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
    removals: Vec<[u8; 32]>,
    confirms: Vec<[u8; 32]>,
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
    let resource_path = one_record(state)?;
    let record: Record =
        postcard::take_from_bytes(&std::fs::read(&resource_path).map_err(saying(&resource_path))?)
            .map(|(record, _)| record)
            .map_err(|error| format!("{}: {error}", resource_path.display()))?;
    if record.version != 3 {
        return Err(format!(
            "{}: the record is version {}, and this export reads version 3",
            resource_path.display(),
            record.version
        ));
    }
    let durable = durable_state(&resource_path, &record.peer)?;
    let document = workspace_document(&durable)?;
    let document = AutoCommit::load(&document).map_err(|error| {
        format!(
            "{}: the workspace document: {error}",
            resource_path.display()
        )
    })?;
    let kept = wait_list_of(&resource_path.with_extension("wait-list"))?;
    let emails = addresses(
        &record.collaboration.invitations,
        &kept,
        &redeemed(&document),
    );
    let names = device_names(&document);
    let administrators = administrators(&document);
    let people = people_of(&document, &names, &emails, &administrators)?;
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

/// The workspace record the released relay kept under `state`: the one it
/// was told to serve, named by the `room-id` file beside its state (the
/// deployment passes it to the release's `--room-id-file`). A state holding
/// exactly one record needs no such file; a state holding several is refused
/// rather than guessed between.
fn one_record(state: &Path) -> Result<PathBuf, String> {
    let (records, served) = walk(state);
    if let Some(served) = served {
        if let Some(record) = records.iter().find(|record| record.served_as == served) {
            return Ok(record.path.clone());
        }
        return Err(format!(
            "{} names the workspace {served}, and no record under {} is it",
            state.display(),
            state.display()
        ));
    }
    match records.as_slice() {
        [record] => Ok(record.path.clone()),
        [] => Err(format!(
            "{} holds no workspace record: name the released relay's own state directory",
            state.display()
        )),
        many => Err(format!(
            "{} holds {} workspace records ({}); name the directory holding the one this \
             relay served",
            state.display(),
            many.len(),
            many.iter()
                .map(FoundRecord::name)
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// A workspace record found under the state, and the workspace it names.
struct FoundRecord {
    path: PathBuf,
    served_as: String,
}

impl FoundRecord {
    fn name(&self) -> String {
        self.path.file_name().map_or_else(
            || self.served_as.clone(),
            |name| name.to_string_lossy().into_owned(),
        )
    }
}

/// Every workspace record the release kept under `state`, and the workspace it was told to
/// serve, from the `room-id` file beside its state. The release kept its records in its own
/// `relay/` directory (`--state <state>/relay`), and only those are read: a copy kept beside it
/// (an older `relay.<when>-unreadable/`, say) holds records of the same workspace that are not
/// the release's, and reading one of those would export what no longer reads, or what the
/// release no longer held. A `state` with no `relay/` is read whole, as the release's relay
/// directory itself.
fn walk(state: &Path) -> (Vec<FoundRecord>, Option<String>) {
    let served = std::fs::read_to_string(state.join("room-id"))
        .ok()
        .map(|id| id.trim().to_lowercase())
        .filter(|id| !id.is_empty());
    let own = state.join("relay");
    let root = if own.is_dir() {
        own
    } else {
        state.to_path_buf()
    };
    let mut pending = vec![(root, 0_u32)];
    let mut records = Vec::new();
    while let Some((directory, depth)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && depth < 4 {
                pending.push((path, depth.saturating_add(1)));
            } else if path
                .extension()
                .is_some_and(|extension| extension == "room")
                && let Some(served_as) = names(&path)
            {
                records.push(FoundRecord { path, served_as });
            }
        }
    }
    (records, served)
}

/// The workspace a record serves, from the head of its bytes: its version,
/// then its workspace.
fn names(record: &Path) -> Option<String> {
    let bytes = std::fs::read(record).ok()?;
    let (_, rest) = postcard::take_from_bytes::<u32>(&bytes).ok()?;
    let (workspace, _) = postcard::take_from_bytes::<String>(rest).ok()?;
    Some(workspace.to_lowercase())
}

/// The record's durable peer state, whole: the parts it names inline, then the
/// large ones each from its parts directory.
fn durable_state(resource_path: &Path, parts: &[PeerPart]) -> Result<Vec<u8>, String> {
    let directory = resource_path.with_extension("parts");
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
/// The envelope is the earlier release's (version 4, or 5 or 6 around a
/// version 4 envelope); the rest of the state is not read, so its documents and
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
    // Base 4 is the layout the earlier release wrote; 5 is the same fields,
    // with the existing state the release's owner began under.
    if base != 4 && base != 5 {
        return Err(format!(
            "the durable peer state is in base {base}, and this export reads 4 and 5"
        ));
    }
    let (_workspace, rest) = postcard::take_from_bytes::<String>(rest).map_err(reading)?;
    let (_owner, rest) = postcard::take_from_bytes::<[u8; 32]>(rest).map_err(reading)?;
    let (document, _) = postcard::take_from_bytes::<Option<Vec<u8>>>(rest).map_err(reading)?;
    document.ok_or_else(|| "the durable peer state holds no workspace document".to_owned())
}

/// Who redeemed an invitation, and the address its issuer named for them.
struct Redemption {
    person: [u8; 32],
    email: Option<String>,
}

/// One address as the earlier release kept it, and the invitations it was
/// mailed.
#[derive(Clone, Debug)]
struct Kept {
    waiting: Waiting,
    invites: Vec<[u8; 32]>,
}

/// The address each person came in at. The release kept one only where it
/// invited them at it: on an invitation the ledger still holds, or on the
/// invitation a redeemed one leaves in its receipt (the ledger drops an
/// invitation once it is redeemed), or on the wait-list entry an invitation
/// was mailed to.
fn addresses(
    invitations: &[InvitationRecord],
    wait_list: &[Kept],
    redeemed: &std::collections::BTreeMap<[u8; 32], Redemption>,
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
        let person = invitation.grant.recipient_user_public_key.or_else(|| {
            redeemed
                .get(&invitation.issued.invite_id)
                .map(|redemption| redemption.person)
        });
        if let Some(person) = person {
            emails.entry(person).or_insert(address);
        }
    }
    for redemption in redeemed.values() {
        if let Some(address) = &redemption.email {
            emails
                .entry(redemption.person)
                .or_insert_with(|| address.clone());
        }
    }
    for entry in wait_list {
        let Some(redemption) = entry.invites.iter().find_map(|invite| redeemed.get(invite)) else {
            continue;
        };
        let Ok(address) = tilo_relay_protocol::workspaces::contact_address(&entry.waiting.contact)
        else {
            continue;
        };
        emails.entry(redemption.person).or_insert(address);
    }
    emails
}

/// Who redeemed each invitation the workspace holds a receipt for, by the
/// invitation's id: the document keeps a receipt for every invitation that
/// brought someone in, with the invitation itself, whose grant names the
/// address its issuer wrote for them.
fn redeemed(document: &AutoCommit) -> std::collections::BTreeMap<[u8; 32], Redemption> {
    let mut people = std::collections::BTreeMap::new();
    let Ok(rows) = document.get_all(&ROOT, "invitation_receipts_v1") else {
        return people;
    };
    for (value, object) in rows {
        if !matches!(value, Value::Object(automerge::ObjType::Map)) {
            continue;
        }
        for key in document.keys(&object) {
            let Some(row) = receipt(document, &object, &key) else {
                continue;
            };
            people.insert(row.invite_id, row.redemption());
        }
    }
    people
}

/// A receipt as the document keeps it.
fn receipt(document: &AutoCommit, object: &automerge::ObjId, key: &str) -> Option<ReceiptRow> {
    let (value, _) = document.get(object, key).ok().flatten()?;
    let Value::Scalar(value) = value else {
        return None;
    };
    let ScalarValue::Bytes(bytes) = value.as_ref() else {
        return None;
    };
    serde_json::from_slice(bytes).ok()
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
    /// The signed invitation, in the release's text form: its grant names
    /// the address its issuer wrote for the person.
    #[serde(default)]
    invitation: Option<String>,
}

impl ReceiptRow {
    fn redemption(&self) -> Redemption {
        Redemption {
            person: self.user_public_key,
            email: self
                .invitation
                .as_deref()
                .and_then(invited_address)
                .filter(|named| tilo_relay_protocol::workspaces::contact_address(named).is_ok()),
        }
    }
}

/// The address the invitation `text` names, if it names one: the release's
/// text form is the signed invitation, base64url, whose grant carries the
/// invitee's name.
fn invited_address(text: &str) -> Option<String> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text.trim())
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(text.trim()))
        .ok()?;
    let (invitation, _) = postcard::take_from_bytes::<SignedInvitation>(&bytes).ok()?;
    invitation.grant.invitee_name
}

/// A signed invitation as the earlier release's text form holds it: its
/// version, its grant, and its signature.
#[derive(Deserialize)]
struct SignedInvitation {
    #[expect(dead_code, reason = "read to reach the fields after it")]
    version: u32,
    grant: InviteGrant,
    #[expect(dead_code, reason = "read to reach the field after it")]
    signature: Vec<u8>,
}

/// The workspace's people, its owner first: each person's root, the name the
/// workspace knew them by, and the address it kept for them.
fn people_of(
    document: &AutoCommit,
    names: &std::collections::BTreeMap<String, String>,
    emails: &std::collections::BTreeMap<[u8; 32], String>,
    administrators: &std::collections::BTreeSet<String>,
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
        people.push(member(&owners, names, emails, administrators, &key, &role));
    }
    if let Some(owner) = owner {
        // The owner is not one of the document's member rows: they hold the
        // workspace from its beginning.
        people.retain(|person| !person.root.eq_ignore_ascii_case(&owner));
        people.insert(
            0,
            member(&owners, names, emails, administrators, &owner, "owner"),
        );
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
    administrators: &std::collections::BTreeSet<String>,
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
        // The earlier release's moderator governs the workspace as an
        // administrator does, and its administrators held a grant the member
        // row does not show: both come over as this release's administrator,
        // never as the editor their row would make them.
        role: if role == "moderator" || administrators.contains(&root.to_lowercase()) {
            "administrator".to_owned()
        } else {
            role.to_owned()
        },
    }
}

/// The people the earlier release's governance made administrators, by root.
fn administrators(document: &AutoCommit) -> std::collections::BTreeSet<String> {
    let mut administrators = std::collections::BTreeSet::new();
    let Ok(Some((_, grants))) = document.get(&ROOT, "grants") else {
        return administrators;
    };
    for key in document.keys(&grants) {
        let Some(root) = ADMINISTRATOR_GRANTS
            .iter()
            .find_map(|prefix| key.strip_prefix(prefix))
        else {
            continue;
        };
        administrators.insert(root.to_lowercase());
    }
    administrators
}

/// The hashes as the export writes them: lowercase hex.
fn hashes(hashes: &[[u8; 32]]) -> Vec<String> {
    hashes.iter().map(hex::encode).collect()
}

/// The addresses the earlier release kept at its entry, as they were left.
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
                    confirmed: entry.confirmed_at_ns.is_some(),
                    removal_sha256: hashes(&entry.removals),
                    confirm_sha256: hashes(&entry.confirms),
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
                    // The first layout had no confirmation: every entry it
                    // kept stood on the list, and its removal links go on
                    // working.
                    confirmed: true,
                    removal_sha256: Vec::new(),
                    confirm_sha256: Vec::new(),
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

fn saying(path: &Path) -> impl Fn(std::io::Error) -> String + '_ {
    move |error| format!("{}: {error}", path.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The durable peer state an earlier release wrote, with `document` as its
    /// workspace document.
    fn durable(envelope: u32, document: &[u8]) -> Vec<u8> {
        durable_in(envelope, 5, document)
    }

    /// [`durable`] with its base version chosen: the earlier release wrote 4.
    fn durable_in(envelope: u32, base: u32, document: &[u8]) -> Vec<u8> {
        let mut bytes = postcard::to_stdvec(&envelope).unwrap();
        if envelope != 4 {
            bytes.extend(postcard::to_stdvec(&4u32).unwrap());
        }
        bytes.extend(postcard::to_stdvec(&base).unwrap());
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
        for base in [4, 5] {
            assert_eq!(
                workspace_document(&durable_in(5, base, b"the document")).unwrap(),
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
        if version == 1 {
            postcard::to_stdvec(&StoredV1 {
                version,
                entries: contacts
                    .iter()
                    .enumerate()
                    .map(|(index, contact)| EntryV1 {
                        request_id: [u8::try_from(index).unwrap(); 32],
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
                        request_id: [u8::try_from(index).unwrap(); 32],
                        contact: (*contact).to_owned(),
                        at_ns: 9,
                        removals: vec![[0xaa; 32]],
                        confirms: vec![[0xbb; 32]],
                        confirmed_at_ns: Some(9),
                        language: "de".to_owned(),
                        mailed_at_ns: None,
                        invites: vec![[u8::try_from(index).unwrap(); 32]],
                        invited_at_ns: None,
                        invite_mailed_at_ns: None,
                    })
                    .collect(),
                mailed: Vec::new(),
            })
            .unwrap()
        }
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

    /// A copy of the same workspace kept beside the release's own `relay/` (an older state set
    /// aside as unreadable) is not what the release held, so the export never reads it.
    #[test]
    fn a_copy_kept_beside_the_release_state_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = "74a479e4-757a-4efb-aa82-a9acf0c72d45";
        let mut record = postcard::to_stdvec(&3u32).unwrap();
        record.extend(postcard::to_stdvec(&workspace.to_owned()).unwrap());
        let own = dir.path().join("relay").join("relay").join("rooms");
        let aside = dir
            .path()
            .join("relay.oct27-unreadable")
            .join("relay")
            .join("rooms");
        for rooms in [&own, &aside] {
            std::fs::create_dir_all(rooms).unwrap();
            std::fs::write(rooms.join(format!("{workspace}.room")), &record).unwrap();
        }
        std::fs::write(dir.path().join("room-id"), format!("{workspace}\n")).unwrap();
        assert_eq!(
            one_record(dir.path()).unwrap(),
            own.join(format!("{workspace}.room"))
        );
    }

    #[test]
    fn a_record_is_found_at_the_depth_a_release_kept_it() {
        let dir = tempfile::tempdir().unwrap();
        let rooms = dir.path().join("relay").join("relay").join("rooms");
        std::fs::create_dir_all(&rooms).unwrap();
        let mut record = postcard::to_stdvec(&3u32).unwrap();
        record.extend(
            postcard::to_stdvec(&"74a479e4-757a-4efb-aa82-a9acf0c72d45".to_owned()).unwrap(),
        );
        std::fs::write(rooms.join("lounge.room"), &record).unwrap();
        std::fs::write(rooms.join("other.room"), b"another workspace").unwrap();
        std::fs::write(
            dir.path().join("room-id"),
            b"74a479e4-757a-4efb-aa82-a9acf0c72d45\n",
        )
        .unwrap();
        assert_eq!(one_record(dir.path()).unwrap(), rooms.join("lounge.room"));
    }
}
