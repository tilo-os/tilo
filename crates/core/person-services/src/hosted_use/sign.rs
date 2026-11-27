//! A signature made with an ssh key its person's vault keeps: the one use whose secret never
//! leaves the vault at all.
//!
//! An agent names the item by its address (`vault.item:<id>#sign`), the ssh server the
//! signature is for, which must be one of the item's places, and what is signed. The key is
//! read out of the vault inside this server, the signature is made, and the key is dropped:
//! what goes back is the signature and the key's public line. The private key is in no answer,
//! no refusal, no log line and no record, and no refusal quotes what was to be signed.
//!
//! There are two forms. With a namespace the answer is an armored SSHSIG signature, what
//! `ssh-keygen -Y sign -n <namespace>` makes; git signs commits in the namespace `git`. Without
//! one the answer is the signature blob ssh public-key authentication sends, so a client such
//! as tilo-git can sign in with a key it never holds. That form signs only an ssh sign-in
//! request for this very key (a session, a user, the `publickey` method, the key's own
//! algorithm and public key) and nothing else, so it cannot be used to sign an SSHSIG message
//! or any other text in the key's name.
//!
//! Only ed25519 and ecdsa keys sign here, and a key kept under a passphrase is refused: the
//! vault keeps no passphrase for it.
//!
//! The person's "Always" stands for a signature, as it does for filling a page's field, and
//! not as for a web call. A web call is shown each time because the agent writes the request
//! that carries the key and could carry it somewhere else, or make the service hand out a new
//! credential. Here nothing the agent writes carries the key anywhere: a signature does not
//! reveal its key, and each one is written down as a use of the item naming who asked. What a
//! leave lets the agent do is sign as its person for that one key and the ssh servers it
//! names, which is the leave a person means to give when they let an agent push to a server
//! or sign its commits. The leave names the item and the server, and the person reads it in
//! the item's uses and takes it back there. A namespace signature is not bound to the server
//! by its bytes: the server is what the person allowed and what the item's history names.

use accounts::vault_items::{ItemError, ItemId, ItemKind, Place, UsedFor, VaultItems};
use russh::keys::signature::Signer as _;
use russh::keys::ssh_encoding::Encode as _;
use russh::keys::ssh_encoding::base64::{Base64, Encoding as _};
use russh::keys::ssh_key::{LineEnding, Signature};
use russh::keys::{Algorithm, HashAlg, PrivateKey, PublicKey};
use serde_json::{Value, json};

pub(super) use tilo_workspace_records::vault::Sign;

use crate::hosted_use::Workspaces;
use tilo_actions::remote::InvokeRequest;

/// The most one signature signs, decoded.
const MAX_DATA_BYTES: usize = 1024 * 1024;
/// The longest namespace.
const MAX_NAMESPACE_BYTES: usize = 64;
/// The longest session id and user name a sign-in request names.
const MAX_SESSION_BYTES: usize = 64;
const MAX_USER_BYTES: usize = 256;
/// The message number of an ssh sign-in request (RFC 4252).
const USERAUTH_REQUEST: u8 = 50;

const NOT_DATA: &str = "What is signed is given as base64, up to a megabyte";
const NOT_A_NAMESPACE: &str =
    "A namespace is one short word, such as git or file, of letters, digits and . - _ @";
const NOT_A_SIGN_IN: &str = "Without a namespace, what is signed is an ssh sign-in request for \
     this key: give a namespace to sign anything else";
const NOT_SSH: &str = "Only an ssh key signs";
const NOT_A_SERVER: &str = "A signature is for an ssh server, such as ssh://git.example.com";
const LOCKED: &str = "That ssh key is kept under a passphrase, and the vault keeps no \
     passphrase for it";
const UNSIGNED_KIND: &str = "Only an ed25519 or ecdsa ssh key signs here";
const NOT_SIGNED: &str = "The key could not make that signature";

/// What is signed, and how.
struct Signed {
    data: Vec<u8>,
    form: Form,
}

enum Form {
    /// An SSHSIG signature in this namespace.
    Namespace(String),
    /// An ssh sign-in request, naming the key by its algorithm and its public key.
    SignIn { algorithm: String, key: Vec<u8> },
}

impl Signed {
    /// What a signature does, as its key's use is written down: never what is signed.
    fn in_a_line(&self) -> String {
        match &self.form {
            Form::Namespace(namespace) => format!("signs in the namespace {namespace}"),
            Form::SignIn { .. } => "signs an ssh sign-in".to_owned(),
        }
    }

    /// Whether `public` is a key this signs with: one this server signs with, and for a sign-in
    /// the very key the request names.
    fn fits(&self, public: &PublicKey) -> Result<(), String> {
        if !matches!(
            public.algorithm(),
            Algorithm::Ed25519 | Algorithm::Ecdsa { .. }
        ) {
            return Err(UNSIGNED_KIND.to_owned());
        }
        match &self.form {
            Form::Namespace(_) => Ok(()),
            Form::SignIn { algorithm, key } => {
                let own = public.to_bytes().map_err(|_| damaged())?;
                if *algorithm == public.algorithm().as_str() && *key == own {
                    Ok(())
                } else {
                    Err(NOT_A_SIGN_IN.to_owned())
                }
            }
        }
    }

    /// The signature `key` makes, answered with the key's public line.
    fn made_with(&self, key: &PrivateKey) -> Result<Value, String> {
        let public = key.public_key().to_openssh().map_err(|_| damaged())?;
        match &self.form {
            Form::Namespace(namespace) => {
                let signature = key
                    .sign(namespace, HashAlg::Sha512, &self.data)
                    .and_then(|signature| signature.to_pem(LineEnding::LF))
                    .map_err(|_| NOT_SIGNED.to_owned())?;
                Ok(json!({
                    "signature": signature,
                    "namespace": namespace,
                    "publicKey": public,
                }))
            }
            Form::SignIn { algorithm, .. } => {
                let signature: Signature = key
                    .try_sign(&self.data)
                    .map_err(|_| NOT_SIGNED.to_owned())?;
                let blob = signature.encode_vec().map_err(|_| NOT_SIGNED.to_owned())?;
                Ok(json!({
                    "signature": Base64::encode_string(&blob),
                    "algorithm": algorithm,
                    "publicKey": public,
                }))
            }
        }
    }
}

fn damaged() -> String {
    ItemError::Damaged.to_string()
}

/// The next ssh string of `rest`, which it moves past.
fn string<'a>(rest: &mut &'a [u8]) -> Option<&'a [u8]> {
    let (length, after) = rest.split_first_chunk::<4>()?;
    let length = usize::try_from(u32::from_be_bytes(*length)).ok()?;
    if after.len() < length {
        return None;
    }
    let (string, after) = after.split_at(length);
    *rest = after;
    Some(string)
}

/// The algorithm and the public key `data` names, where it is an ssh sign-in request with a
/// public key and a signature (RFC 4252, 7), and nothing after it.
fn sign_in(data: &[u8]) -> Option<(String, Vec<u8>)> {
    let mut rest = data;
    let session = string(&mut rest)?;
    if session.is_empty() || session.len() > MAX_SESSION_BYTES {
        return None;
    }
    let (&message, after) = rest.split_first()?;
    rest = after;
    if message != USERAUTH_REQUEST {
        return None;
    }
    let user = string(&mut rest)?;
    if user.len() > MAX_USER_BYTES
        || string(&mut rest)? != b"ssh-connection"
        || string(&mut rest)? != b"publickey"
    {
        return None;
    }
    let (&signed, after) = rest.split_first()?;
    rest = after;
    if signed != 1 {
        return None;
    }
    let algorithm = std::str::from_utf8(string(&mut rest)?).ok()?.to_owned();
    let key = string(&mut rest)?.to_vec();
    rest.is_empty().then_some((algorithm, key))
}

/// What `sign` asks to have signed, read. Refusals never quote it.
fn read(sign: &Sign) -> Result<Signed, String> {
    let text = sign.data.trim();
    if text.is_empty() || text.len() > MAX_DATA_BYTES.div_ceil(3).saturating_mul(4) {
        return Err(NOT_DATA.to_owned());
    }
    let data = Base64::decode_vec(text).map_err(|_| NOT_DATA.to_owned())?;
    let namespace = sign.namespace.trim();
    let form = if namespace.is_empty() {
        let (algorithm, key) = sign_in(&data).ok_or_else(|| NOT_A_SIGN_IN.to_owned())?;
        Form::SignIn { algorithm, key }
    } else {
        if namespace.len() > MAX_NAMESPACE_BYTES
            || !namespace
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".-_@".contains(&byte))
        {
            return Err(NOT_A_NAMESPACE.to_owned());
        }
        Form::Namespace(namespace.to_owned())
    };
    Ok(Signed { data, form })
}

/// What `sign` wants of the vault: the item its address names, an ssh key, and the one ssh
/// server it signs for.
pub(super) fn wanted(
    sign: &Sign,
    item: Option<&str>,
) -> Result<(ItemId, ItemKind, Vec<Place>), String> {
    let item = item
        .and_then(ItemId::parse)
        .ok_or_else(|| ItemError::NotFound.to_string())?;
    let place = Place::parse(sign.place.trim()).map_err(|_| NOT_A_SERVER.to_owned())?;
    if place.scheme() != "ssh" {
        return Err(NOT_A_SERVER.to_owned());
    }
    Ok((item, ItemKind::SshKey, vec![place]))
}

/// Whether `items` keep `item` as an ssh key for `places`: an item of another kind is refused
/// saying what signs.
pub(super) fn fits(items: &VaultItems, item: &ItemId, places: &[Place]) -> Result<(), ItemError> {
    match items.fits(item, ItemKind::SshKey, places) {
        Err(ItemError::WrongKind) => Err(ItemError::Invalid(NOT_SSH)),
        fitted => fitted.map(drop),
    }
}

/// What `sign` would be refused for before the key is read: said before anyone is asked.
pub(super) fn sendable(sign: &Sign) -> Result<(), String> {
    read(sign).map(drop)
}

/// Sign what `sign` names for `by`, serving the call `asked`, with the ssh key `item` of `workspace`'s vault keeps for
/// the server `sign` names. `purpose` is what the use is written down with; the use is written to the vault
/// before the signature is made, and nothing is written for a signature refused.
pub(super) async fn sign(
    workspaces: &Workspaces,
    workspace: &str,
    sign: &Sign,
    item: &ItemId,
    (by, acting, asked): (&str, &tilo_member_types::Acting, &InvokeRequest),
    purpose: UsedFor<'_>,
) -> Result<Value, String> {
    let signed = read(sign)?;
    let (_, _, places) = wanted(sign, Some(item.as_str()))?;
    let opened = workspaces.open_vault(workspace).await?;
    let mut items = opened.items();
    let what = signed.in_a_line();
    let purpose = UsedFor {
        what: &what,
        ..purpose
    };
    let key = {
        let released = items
            .use_for(item, ItemKind::SshKey, &places, by, purpose)
            .map_err(|error| match error {
                ItemError::WrongKind => NOT_SSH.to_owned(),
                error => error.to_string(),
            })?;
        PrivateKey::from_openssh(released.secret.as_bytes()).map_err(|_| damaged())?
    };
    if key.is_encrypted() {
        return Err(LOCKED.to_owned());
    }
    signed.fits(key.public_key())?;
    workspaces
        .write_vault(&opened, items.take_changes(), Some(acting), Some(asked))
        .await?;
    signed.made_with(&key)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "in a test an unwrap is the assertion")]
mod tests {
    use super::*;

    fn put(data: &mut Vec<u8>, part: &[u8]) {
        data.extend_from_slice(&u32::try_from(part.len()).unwrap().to_be_bytes());
        data.extend_from_slice(part);
    }

    fn request(algorithm: &str, key: &[u8]) -> Vec<u8> {
        let mut data = Vec::new();
        put(&mut data, b"session-id");
        data.push(USERAUTH_REQUEST);
        for part in [b"git".as_slice(), b"ssh-connection", b"publickey"] {
            put(&mut data, part);
        }
        data.push(1);
        put(&mut data, algorithm.as_bytes());
        put(&mut data, key);
        data
    }

    /// A sign-in request reads as the key it names, and what is not one, or carries anything
    /// after the key, reads as nothing.
    #[test]
    fn only_a_whole_sign_in_request_reads_as_one() {
        let data = request("ssh-ed25519", b"key");
        assert_eq!(
            sign_in(&data),
            Some(("ssh-ed25519".to_owned(), b"key".to_vec()))
        );
        let mut longer = data.clone();
        longer.push(0);
        assert_eq!(sign_in(&longer), None);
        assert_eq!(sign_in(&data[..data.len() - 1]), None);
        assert_eq!(sign_in(b"SSHSIG\0\0\0\x03git"), None);
        let mut other_method = data;
        other_method[4 + 10 + 1 + 4 + 3 + 4 + 14 + 4] = b'P';
        assert_eq!(sign_in(&other_method), None);
    }
}
