//! The rollback floor of a person's servers, raised by their other devices.
//!
//! A server's TPM lets its rollback floor advance only with a signed,
//! single-use approval from the RSA key its floor counter was defined
//! against (docs/security/rollback.md). The person's root is Ed25519 and
//! cannot be that signer, so the person has one RSA floor key ([`Floor`]):
//! made on a device holding their root ([`make_key`]), its public half signed
//! by the root, and its private half sealed to the root-derived secret-sync
//! key, so it opens only on a device that holds the root, as a sibling's
//! unlock half does. Each server says in its [`SERVERS`](super::SERVERS)
//! entry which key its own floor counter takes approvals from.
//!
//! After an update booted healthy, a server asks for its floor to move: a
//! typed action naming the generation and image it booted and the request
//! its TPM states ([`Floor::request_floor_advance`]). Any other device of the
//! person that opens the floor key answers by itself ([`answer`]): it checks
//! the server is one of the person's, its counter takes the person's key, the
//! request follows the updates the server is set to follow, and approves no step
//! past the generation that booted, then signs the approvals. A server never
//! answers its own request: the resource refuses it. Nobody answering leaves
//! the floor where it is, and the disk keeps unlocking. Each request and its
//! answer is an action on the resource, so they read as the admin workspace's
//! history.

use tilo_resource::{Collection, Id, Invoke, Model, Refusal, Replica, resource};
use tilo_rollback::approval::{self, Approval};
use tilo_seal::Label;

use super::{Servers, UPDATES, device_id, person_workspace_id};
use crate::workspaces::Workspaces;

/// The admin workspace's resource of the floor key and the floor requests.
pub const FLOOR: (&str, &str) = ("so.tilo.servers", "floor");
/// What the person's root signs to vouch for their floor key.
const KEY_CONTEXT: &[u8] = b"tilo rollback floor key v1\0";
/// A request's answer: approvals were signed.
pub const APPROVED: &str = "approved";
/// A request's answer: it was refused, for the reason given.
pub const REFUSED: &str = "refused";

/// The person's floor key and their servers' floor requests.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Floor {
    /// The floor key's public half (PEM); empty until one was made.
    pub key: String,
    /// Its private half (PKCS#8 PEM) sealed to the secret-sync key, hex.
    pub sealed_key: String,
    /// The root's signature over [`KEY_CONTEXT`] and the public half, hex.
    pub key_signature: String,
    /// Requests by [`request_id`].
    pub requests: Collection<FloorRequest>,
}

/// One server's request to move its floor after a healthy boot.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct FloorRequest {
    /// The server, by device key (hex).
    pub server: String,
    /// The generation it booted healthy.
    pub generation: u64,
    /// The image it booted.
    pub image: String,
    /// The updates the image came from ([`UPDATES`]).
    pub updates: String,
    /// What its TPM states ([`approval::Request`], JSON).
    pub request: String,
    /// Empty while pending, then [`APPROVED`] or [`REFUSED`].
    pub answer: String,
    /// The device that answered (hex).
    pub answered_by: String,
    /// The approvals signed ([`Approval`]s, JSON).
    pub approvals: String,
    /// Why it was refused.
    pub why: String,
}

/// The id of `server`'s request for `generation`.
#[must_use]
pub fn request_id(server: &str, generation: u64) -> Id {
    Id::from_raw(format!("{server}-{generation}"))
}

impl Floor {
    fn pending(&mut self, id: &Id, by: &str) -> Result<&mut FloorRequest, Refusal> {
        let request = self
            .requests
            .get_mut(id)
            .ok_or_else(|| Refusal::new("no such floor request"))?;
        if request.server == by {
            return Err(Refusal::new("a server never answers its own floor request"));
        }
        if !request.answer.is_empty() {
            return Err(Refusal::new("this floor request was answered"));
        }
        Ok(request)
    }
}

#[resource(kind = "tilo.servers.floor", read_content = tilo_resource::read::fields)]
impl Floor {
    /// Keep the person's floor key: its public half, its private half sealed
    /// to their secret-sync key, and their root's signature over the public
    /// half.
    #[action(external)]
    pub fn set_floor_key(
        &mut self,
        key: String,
        sealed_key: String,
        key_signature: String,
    ) -> Result<(), Refusal> {
        if key.is_empty() || sealed_key.is_empty() || key_signature.is_empty() {
            return Err(Refusal::new(
                "a floor key has a public half, a sealed private half and a signature",
            ));
        }
        self.key = key;
        self.sealed_key = sealed_key;
        self.key_signature = key_signature;
        Ok(())
    }

    /// A server booted `generation` (`image`, from `updates`) healthy and
    /// asks for its floor to move as its TPM's `request` states. A pending
    /// request for the same generation is replaced by the newer statement.
    #[action(external)]
    pub fn request_floor_advance(
        &mut self,
        server: String,
        generation: u64,
        image: String,
        updates: String,
        request: String,
    ) -> Result<(), Refusal> {
        let id = request_id(&server, generation);
        if self
            .requests
            .get(&id)
            .is_some_and(|asked| !asked.answer.is_empty())
        {
            return Err(Refusal::new("this floor request was answered"));
        }
        let asked = FloorRequest {
            server,
            generation,
            image,
            updates,
            request,
            ..FloorRequest::default()
        };
        match self.requests.get_mut(&id) {
            Some(pending) => *pending = asked,
            None => {
                self.requests.push(id, asked);
            }
        }
        Ok(())
    }

    /// Answer a request with the approvals `by` signed for it.
    #[action(external)]
    pub fn approve_floor_advance(
        &mut self,
        id: Id,
        by: String,
        approvals: String,
    ) -> Result<(), Refusal> {
        let request = self.pending(&id, &by)?;
        APPROVED.clone_into(&mut request.answer);
        request.answered_by = by;
        request.approvals = approvals;
        Ok(())
    }

    /// Refuse a request, saying why.
    #[action(external)]
    pub fn refuse_floor_advance(&mut self, id: Id, by: String, why: String) -> Result<(), Refusal> {
        let request = self.pending(&id, &by)?;
        REFUSED.clone_into(&mut request.answer);
        request.answered_by = by;
        request.why = why;
        Ok(())
    }
}

/// A floor key as made on a device holding the person's root.
pub struct MadeKey {
    /// The public half (PEM), for a server's floor counter.
    pub public: String,
    /// What [`Floor::set_floor_key`] takes.
    pub action: Invoke<Floor>,
}

/// Make the person's floor key: an RSA key whose public half `root` signs
/// and whose private half is sealed to `secret_sync`, and never kept in the
/// clear.
///
/// # Errors
/// The key could not be made or sealed.
pub fn make_key(
    root: &ed25519_dalek::SigningKey,
    secret_sync: &tilo_seal::PublicKey,
) -> Result<MadeKey, String> {
    use ed25519_dalek::Signer as _;
    let key = tilo_crypto::generate_tpm_authority_key()?;
    let sealed = tilo_seal::hpke::seal_to_bytes(
        secret_sync,
        Label::RollbackFloorKey,
        key.public_key_pem().as_bytes(),
        key.private_key_pem().as_bytes(),
    )
    .map_err(|error| error.to_string())?;
    let signature = root.sign(&signed(key.public_key_pem()));
    Ok(MadeKey {
        public: key.public_key_pem().to_owned(),
        action: Floor::SET_FLOOR_KEY.with((
            key.public_key_pem().to_owned(),
            hex::encode(sealed),
            hex::encode(signature.to_bytes()),
        )),
    })
}

fn signed(public: &str) -> Vec<u8> {
    [KEY_CONTEXT, public.as_bytes()].concat()
}

impl Floor {
    /// The floor key's public half, if the person's root signed it.
    #[must_use]
    pub fn verified_key(&self, person: &[u8; 32]) -> Option<&str> {
        let root = ed25519_dalek::VerifyingKey::from_bytes(person).ok()?;
        let mut signature = [0; 64];
        hex::decode_to_slice(&self.key_signature, &mut signature).ok()?;
        root.verify_strict(
            &signed(&self.key),
            &ed25519_dalek::Signature::from_bytes(&signature),
        )
        .ok()
        .map(|()| self.key.as_str())
    }

    /// The floor key's private half (PEM), opened with `secret_sync`.
    ///
    /// # Errors
    /// There is no key, or it does not open with this secret-sync key.
    pub fn open_key(
        &self,
        secret_sync: &tilo_seal::SecretKey,
    ) -> Result<zeroize::Zeroizing<String>, String> {
        let sealed =
            hex::decode(&self.sealed_key).map_err(|_| "the person has no floor key".to_owned())?;
        let opened = tilo_seal::hpke::open_bytes(
            secret_sync,
            Label::RollbackFloorKey,
            self.key.as_bytes(),
            &sealed,
        )
        .map_err(|error| format!("the floor key does not open here: {error}"))?;
        String::from_utf8(opened.to_vec())
            .map(zeroize::Zeroizing::new)
            .map_err(|_| "the floor key is not PEM".to_owned())
    }
}

/// What `server`'s `request` is answered with: the approvals the floor key
/// (`private_pem`) signs for it, or why none are signed.
///
/// # Errors
/// Why the request is refused.
pub fn decide(
    servers: &Servers,
    floor: &Floor,
    request: &FloorRequest,
    private_pem: &str,
) -> Result<Vec<Approval>, String> {
    let entry = Id::from_raw(request.server.as_str());
    let server = servers
        .servers
        .get(&entry)
        .filter(|server| !server.retired)
        .ok_or("it is none of the person's servers")?;
    if server.floor_key != floor.key {
        return Err("its floor counter does not take the person's floor key".to_owned());
    }
    if !UPDATES.contains(&request.updates.as_str())
        || !(server.updates.is_empty() || server.updates == request.updates)
    {
        return Err(format!(
            "its image came from the {} updates, which the server is not set to follow",
            request.updates
        ));
    }
    let stated: approval::Request = serde_json::from_str(&request.request)
        .map_err(|error| format!("its TPM's request does not read: {error}"))?;
    // Never past the generation that booted healthy, however new a keyslot
    // the server says it has.
    let up_to = stated.safe_target_floor.min(request.generation);
    approval::issue_with_key(&stated, private_pem, up_to, false)
        .map_err(|error| format!("{error:#}"))
}

/// The admin workspace's [`FLOOR`] resource as `workspaces` reads it.
///
/// # Errors
/// The resource could not be read.
pub async fn read(workspaces: &Workspaces) -> Result<Floor, String> {
    let admin = person_workspace_id(&workspaces.device_cert().user_public_key);
    let Some(bytes) = workspaces
        .resource_document(&admin, FLOOR.0, FLOOR.1)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(Floor::default());
    };
    Replica::<Floor>::load(&bytes)
        .and_then(|resource| resource.state())
        .map_err(|error| error.to_string())
}

/// Apply `action` to the admin workspace's [`FLOOR`] resource as this device.
///
/// # Errors
/// The action was refused, or the resource could not be written.
pub async fn apply(workspaces: &Workspaces, action: Invoke<Floor>) -> Result<(), String> {
    let admin = person_workspace_id(&workspaces.device_cert().user_public_key);
    let cert = workspaces.device_cert().clone();
    workspaces
        .author_workspace_document(&admin, &cert, FLOOR, |doc| {
            let mut replica = Replica::<Floor>::from_doc(std::mem::take(doc))
                .in_app(FLOOR.0, "Servers");
            let applied = replica
                .apply(action)
                .map(drop)
                .map_err(|error| error.to_string());
            std::mem::swap(doc, replica.doc_mut());
            applied
        })
        .await
        .map_err(|error| error.to_string())
}

/// Answer every pending floor request of another server with the floor key,
/// opened with `secret_sync`: what a device holding the person's root does
/// whenever the resource changes. Answers how many it answered.
///
/// # Errors
/// The resources could not be read or written, or the key does not open.
pub async fn answer(
    workspaces: &Workspaces,
    secret_sync: &tilo_seal::SecretKey,
) -> Result<usize, String> {
    let floor = read(workspaces).await?;
    let own = device_id(&workspaces.device_cert().device_public_key);
    let pending: Vec<(Id, FloorRequest)> = floor
        .requests
        .iter()
        .filter(|(_, request)| request.answer.is_empty() && request.server != own.as_str())
        .map(|(id, request)| (id.clone(), request.clone()))
        .collect();
    if pending.is_empty() {
        return Ok(0);
    }
    let person = workspaces.device_cert().user_public_key;
    if floor.verified_key(&person).is_none() {
        return Err("the floor key is not signed by the person's root".to_owned());
    }
    let private = floor.open_key(secret_sync)?;
    let servers = super::read(workspaces).await?;
    for (id, request) in &pending {
        let action = match decide(&servers, &floor, request, &private) {
            Ok(approvals) => Floor::APPROVE_FLOOR_ADVANCE.with((
                id.clone(),
                own.as_str().to_owned(),
                serde_json::to_string(&approvals).map_err(|error| error.to_string())?,
            )),
            Err(why) => {
                Floor::REFUSE_FLOOR_ADVANCE.with((id.clone(), own.as_str().to_owned(), why))
            }
        };
        apply(workspaces, action).await?;
    }
    Ok(pending.len())
}

/// What a server's TPM floor gets after it booted `generation` healthy: the
/// approvals answered for it, or why there are none yet.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Asked {
    pub generation: u64,
    pub image: String,
    pub updates: String,
    /// The public half (PEM) of the key the server's floor counter takes
    /// approvals from.
    pub authority: String,
    /// What its TPM states ([`approval::Request`], JSON).
    pub request: String,
}

/// As `workspaces`' server: keep which key its floor counter takes in its
/// entry, ask for its floor to move after `asked.generation` booted
/// healthy, and answer the approvals answered for it, or why there are none.
///
/// # Errors
/// The resources could not be read or written.
pub async fn ask(
    workspaces: &Workspaces,
    asked: Asked,
) -> Result<Result<Vec<Approval>, String>, String> {
    let own = device_id(&workspaces.device_cert().device_public_key);
    let servers = super::read(workspaces).await?;
    let entry = servers
        .servers
        .get(&own)
        .filter(|server| !server.retired)
        .ok_or("this server is not among the person's servers")?;
    if entry.floor_key != asked.authority {
        super::apply(
            workspaces,
            Servers::SET_FLOOR_KEY.with((own.clone(), asked.authority.clone())),
        )
        .await?;
    }
    let floor = read(workspaces).await?;
    let id = request_id(own.as_str(), asked.generation);
    match floor.requests.get(&id) {
        Some(request) if request.answer == APPROVED => {
            return serde_json::from_str(&request.approvals)
                .map(Ok)
                .map_err(|error| format!("the approvals do not read: {error}"));
        }
        Some(request) if request.answer == REFUSED => {
            return Ok(Err(format!(
                "{} refused: {}",
                request.answered_by, request.why
            )));
        }
        Some(request) if request.request == asked.request && request.image == asked.image => {}
        _ => {
            apply(
                workspaces,
                Floor::REQUEST_FLOOR_ADVANCE.with((
                    own.as_str().to_owned(),
                    asked.generation,
                    asked.image,
                    asked.updates,
                    asked.request,
                )),
            )
            .await?;
        }
    }
    Ok(Err(
        "waiting for another of the person's devices to approve it".to_owned(),
    ))
}

#[cfg(test)]
#[path = "floor_tests.rs"]
mod tests;
