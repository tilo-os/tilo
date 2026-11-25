//! The record of the person's rollback floor key and their servers' floor requests, kept in
//! their own workspace beside the servers ([`super::SERVERS`]): its type, declared on every
//! device that holds the workspace, so any of them keeps and judges its writes. What signs and
//! answers the requests is the server's.

use tilo_resource::{Collection, Id, Model, Refusal, resource};
use tilo_seal::Label;

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
    /// The updates the image came from ([`super::UPDATES`]).
    pub updates: String,
    /// What its TPM states, as its rollback approval request (JSON).
    pub request: String,
    /// Empty while pending, then [`APPROVED`] or [`REFUSED`].
    pub answer: String,
    /// The device that answered (hex).
    pub answered_by: String,
    /// The rollback approvals signed (JSON).
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

/// What the person's root signs to vouch for the floor key `public`.
#[must_use]
pub fn signed(public: &str) -> Vec<u8> {
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
