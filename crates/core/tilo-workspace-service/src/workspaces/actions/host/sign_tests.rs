//! A signature made with an ssh key the workspace's vault keeps, by a turn this server carries
//! out, with a key made up for the test. Each signature waits for its person and their approve
//! signs that one; the answer verifies against the key's public line, as SSHSIG in its
//! namespace and as an ssh sign-in; an "Always" lets the agent sign for that key and server
//! unasked until it is taken back; another server, another kind of item and every caller that
//! is no such turn are refused; and the private key is in no answer, refusal, asked record,
//! use, log line or file.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, the server handle a test binds is kept until the test ends, and a flow is told in one piece"
)]

use russh::keys::signature::Verifier;
use russh::keys::ssh_encoding::Decode as _;
use russh::keys::ssh_encoding::base64::{Base64, Encoding as _};
use russh::keys::ssh_key::private::Ed25519Keypair;
use russh::keys::ssh_key::{LineEnding, Signature, SshSig};
use russh::keys::{PrivateKey, PublicKey};
use serde_json::{Value, json};
use tilo_actions::InvocationId;
use tilo_actions::remote::ActionCallError::{self, NotPermitted, Refused, UnknownAction};
use tilo_actions::remote::person_vault::{self, ItemRef};
use tilo_actions::remote::{Caller, InvokeArgument, InvokeRequest};
use tilo_user_identity::DeviceCert;

use super::super::tests::{admit, caller, governed};
use super::turn_use_tests::{
    always, approved, asked_by, asks, dispatched, fact, narrowed_elsewhere, narrowing,
    not_hosted_turns, on_turn,
};
use super::vault_tests::{ask, ask_at, files_holding};
use super::web_call_tests::logged;
use crate::workspaces::Workspaces;
use crate::workspaces::fixtures::test_host;
use crate::workspaces::hosted_use::HostedTurn;
use crate::workspaces::tests::governor;

const SERVER: &str = "ssh://git.example.test";
const SIGN_USE: &str = "tilo.vault/sign";
/// What a commit signature signs, as git hands it over.
const COMMIT: &[u8] = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\nauthor Ada <ada@example.test> 0 +0000\n\nFirst\n";

/// A key made up for this test: no server ever trusted it.
struct MadeUp {
    private: PrivateKey,
    pem: String,
}

impl MadeUp {
    fn new() -> Self {
        let private = PrivateKey::from(Ed25519Keypair::from_seed(&rand::random()));
        let pem = private.to_openssh(LineEnding::LF).unwrap().to_string();
        Self { private, pem }
    }

    fn public(&self) -> &PublicKey {
        self.private.public_key()
    }

    /// Pieces of the private key as it could be written down: each line of its PEM body, and
    /// the key's secret bytes.
    fn pieces(&self) -> Vec<Vec<u8>> {
        let mut pieces: Vec<Vec<u8>> = self
            .pem
            .lines()
            .filter(|line| !line.starts_with("-----") && line.len() > 16)
            .map(|line| line.as_bytes().to_vec())
            .collect();
        let secret = self
            .private
            .key_data()
            .ed25519()
            .unwrap()
            .private
            .to_bytes();
        pieces.push(secret.to_vec());
        pieces.push(hex::encode(secret).into_bytes());
        pieces.push(Base64::encode_string(&secret).into_bytes());
        pieces
    }
}

/// An ssh sign-in request for `public` at a server whose session is `session`, as an ssh
/// client signs it.
fn sign_in_request(public: &PublicKey, session: &[u8]) -> Vec<u8> {
    fn put(data: &mut Vec<u8>, part: &[u8]) {
        data.extend_from_slice(&u32::try_from(part.len()).unwrap().to_be_bytes());
        data.extend_from_slice(part);
    }
    let mut data = Vec::new();
    put(&mut data, session);
    data.push(50);
    put(&mut data, b"git");
    put(&mut data, b"ssh-connection");
    put(&mut data, b"publickey");
    data.push(1);
    put(&mut data, public.algorithm().as_str().as_bytes());
    put(&mut data, &public.to_bytes().unwrap());
    data
}

/// The signature of `data` with `item` for `place`, in `namespace` where it names one.
fn signing(item: &ItemRef, place: &str, data: &[u8], namespace: &str) -> InvokeRequest {
    let mut argument = serde_json::Map::new();
    argument.insert("place".into(), json!(place));
    argument.insert("data".into(), json!(Base64::encode_string(data)));
    if !namespace.is_empty() {
        argument.insert("namespace".into(), json!(namespace));
    }
    argument.insert("reason".into(), json!("To sign your commit."));
    InvokeRequest {
        invocation: InvocationId::new(),
        target: person_vault::sign_target(item),
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// An item of `kind` kept for `place` with `secret` by `who` from their own device.
async fn kept(
    service: &super::super::ServiceWorkspaceActions,
    workspace_id: crate::workspaces::WorkspaceId,
    who: (&DeviceCert, &Caller),
    (kind, place, secret): (&str, &str, &str),
) -> ItemRef {
    let Ok(kept) = ask(
        service,
        workspace_id,
        who,
        person_vault::ADD,
        json!({"kind": kind, "belongsTo": [place], "secret": secret}),
    )
    .await
    else {
        unreachable!("the person keeps an item");
    };
    let Some(reference) = kept.get("reference").and_then(Value::as_str) else {
        unreachable!("an item kept answers its reference");
    };
    ItemRef::parse(reference).unwrap()
}

/// What `item` remembers, as its person reads it from their own device.
async fn remembered(
    service: &super::super::ServiceWorkspaceActions,
    workspace_id: crate::workspaces::WorkspaceId,
    who: (&DeviceCert, &Caller),
    item: &ItemRef,
) -> Value {
    let Ok(remembers) = ask(
        service,
        workspace_id,
        who,
        person_vault::USES,
        json!({"item": item}),
    )
    .await
    else {
        unreachable!("a person reads what their item remembers");
    };
    remembers
}

fn signed(taken: Result<Value, ActionCallError>) -> Value {
    taken.unwrap_or_else(|error| panic!("it is signed: {error:?}"))
}

fn refusal(taken: Result<Value, ActionCallError>) -> String {
    match taken {
        Err(Refused { reason }) => reason,
        other => panic!("it is refused: {other:?}"),
    }
}

/// Whether an SSHSIG answer verifies for `public` over `data` in `namespace`, and names that
/// key.
fn verifies_as_sshsig(answer: &Value, public: &PublicKey, data: &[u8], namespace: &str) -> bool {
    let signature = SshSig::from_pem(answer["signature"].as_str().unwrap()).unwrap();
    answer["publicKey"] == json!(public.to_openssh().unwrap())
        && answer["namespace"] == json!(namespace)
        && public.verify(namespace, data, &signature).is_ok()
}

/// Whether an ssh sign-in answer verifies for `public` over `data`, and names that key.
fn verifies_as_sign_in(answer: &Value, public: &PublicKey, data: &[u8]) -> bool {
    let blob = Base64::decode_vec(answer["signature"].as_str().unwrap()).unwrap();
    let signature = Signature::decode(&mut blob.as_slice()).unwrap();
    answer["publicKey"] == json!(public.to_openssh().unwrap())
        && answer["algorithm"] == json!(public.algorithm().as_str())
        && Verifier::verify(public, data, &signature).is_ok()
}

/// What `request` comes to for `turn`, as the entry decides it for a turn the server carries
/// out ([`Workspaces::use_for_turn`]).
async fn taken(
    workspaces: &Workspaces,
    turn: &HostedTurn<'_>,
    request: &InvokeRequest,
) -> Result<Value, ActionCallError> {
    assert!(
        crate::workspaces::hosted_use::takes(&request.target),
        "the device's dispatch hands a signature to the entry"
    );
    workspaces.use_for_turn(turn, request).await
}

/// Everything the workspace's asked records keep, as text.
fn asked_records(workspaces: &Workspaces, workspace: &str) -> String {
    let store = workspaces.asked_store().unwrap();
    let mut text = String::new();
    for waiting in store.waiting_in(workspace) {
        text.push_str(&serde_json::to_string(&waiting.asked).unwrap());
        if let Some(kept) = store.kept_ask(&waiting.asked.record) {
            text.push_str(&serde_json::to_string(&kept.request).unwrap());
            text.push_str(&serde_json::to_string(&kept.facts).unwrap());
        }
    }
    text
}

/// An agent's signature waits for its person, shown the key by its kind, its server and what
/// it signs, never the data. Their approve signs that one call, which verifies as SSHSIG in its
/// namespace and is written down as a use naming the agent; the next one asks again. An ssh
/// sign-in is signed the same way and verifies as the blob public-key authentication sends.
/// The private key is in no answer, asked record, use, log line or file.
#[tokio::test]
async fn an_agents_signature_waits_for_its_person_and_verifies() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let hers = (&ada, &as_ada);
    let made_up = MadeUp::new();
    let ssh_key = kept(
        &service,
        workspace_id,
        hers,
        ("ssh-key", SERVER, &made_up.pem),
    )
    .await;
    let scout = on_turn(&ada, &workspace, "scout");
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        policy: None,
    };
    let (lines, guard) = logged();
    tracing::info!("the line that shows the log is read");

    let first = signing(&ssh_key, SERVER, COMMIT, "git");
    let facts = asks(taken(workspaces, &turn, &first).await);
    assert!(asked_by(&facts, "scout", "Scout"), "{facts:?}");
    assert_eq!(facts.words, "To sign your commit.");
    assert_eq!(fact(&facts, "Item"), "ssh-key");
    assert!(fact(&facts, "Goes to").starts_with(SERVER), "{facts:?}");
    assert_eq!(fact(&facts, "Signs"), "in the namespace git");
    assert_eq!(facts.verb, SIGN_USE);
    assert!(facts.plural.is_some(), "Always is offered");
    let shown = serde_json::to_string(&facts).unwrap();
    assert!(
        !shown.contains("First") && !shown.contains(ssh_key.id()),
        "{shown}"
    );
    assert_eq!(
        remembered(&service, workspace_id, hers, &ssh_key).await,
        json!({"uses": []}),
        "waiting signs nothing"
    );
    // Asked again, it waits again.
    asks(taken(workspaces, &turn, &first).await);

    let answer = signed(taken(workspaces, &turn, &approved(&first, &workspace)).await);
    assert!(
        verifies_as_sshsig(&answer, made_up.public(), COMMIT, "git"),
        "{answer}"
    );
    let signature = SshSig::from_pem(answer["signature"].as_str().unwrap()).unwrap();
    assert!(
        made_up.public().verify("file", COMMIT, &signature).is_err(),
        "a signature stands in its own namespace alone"
    );
    let remembers = remembered(&service, workspace_id, hers, &ssh_key).await;
    let uses = remembers["uses"].as_array().unwrap();
    assert_eq!(uses.len(), 1, "{remembers}");
    assert_eq!(uses[0]["by"], "Scout");
    assert_eq!(uses[0]["using"], SIGN_USE);
    assert_eq!(uses[0]["what"], "signs in the namespace git");
    assert_eq!(uses[0]["allowed"], "once");
    assert_eq!(remembers.get("always"), None, "once leaves no leave");

    // Once is once: the next signature asks again.
    asks(taken(workspaces, &turn, &signing(&ssh_key, SERVER, COMMIT, "git")).await);

    // An ssh sign-in, as a client that signs in with the key asks for it.
    let request = sign_in_request(made_up.public(), b"a session of the server's");
    let sign_in = signing(&ssh_key, SERVER, &request, "");
    let facts = asks(taken(workspaces, &turn, &sign_in).await);
    assert_eq!(fact(&facts, "Signs"), "an ssh sign-in");
    let answer = signed(taken(workspaces, &turn, &approved(&sign_in, &workspace)).await);
    assert!(
        verifies_as_sign_in(&answer, made_up.public(), &request),
        "{answer}"
    );

    drop(guard);
    let logged = String::from_utf8(lines.0.lock().unwrap().clone()).unwrap();
    assert!(logged.contains("the line that shows the log is read"));
    let records = asked_records(workspaces, &workspace);
    let remembers = remembered(&service, workspace_id, hers, &ssh_key)
        .await
        .to_string();
    let answers = answer.to_string();
    for piece in made_up.pieces() {
        let text = String::from_utf8_lossy(&piece);
        assert!(
            files_holding(dir.path(), &piece).is_empty(),
            "a file holds the key"
        );
        assert!(!logged.contains(text.as_ref()), "a log line holds the key");
        assert!(
            !records.contains(text.as_ref()),
            "an asked record holds the key"
        );
        assert!(!remembers.contains(text.as_ref()), "a use holds the key");
        assert!(!answers.contains(text.as_ref()), "the answer holds the key");
    }
    server.shutdown().await;
}

/// After "Always" the agent signs with that key for that server without a question, each
/// signature written down as allowed always; the item's uses list the leave, another agent is
/// still asked, and once she takes the leave back her agent is asked again.
#[tokio::test]
async fn after_always_the_agent_signs_unasked_until_the_leave_is_taken_back() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let hers = (&ada, &as_ada);
    let made_up = MadeUp::new();
    let ssh_key = kept(
        &service,
        workspace_id,
        hers,
        ("ssh-key", SERVER, &made_up.pem),
    )
    .await;
    let scout = on_turn(&ada, &workspace, "scout");
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        policy: None,
    };

    let first = signing(&ssh_key, SERVER, COMMIT, "git");
    let facts = asks(taken(workspaces, &turn, &first).await);
    let server_place = fact(&facts, "Goes to").to_owned();
    always(
        workspaces,
        (&workspace, &owner, &ada),
        "scout",
        (&first.target, &json!(ssh_key), &[server_place.as_str()]),
    )
    .await;
    let remembers = remembered(&service, workspace_id, hers, &ssh_key).await;
    assert_eq!(
        remembers["always"],
        json!([{
            "agent": "scout",
            "using": SIGN_USE,
            "places": [server_place],
            "since": remembers["always"][0]["since"],
        }]),
        "{remembers}"
    );

    for data in [COMMIT, b"another commit".as_slice()] {
        let answer =
            signed(taken(workspaces, &turn, &signing(&ssh_key, SERVER, data, "git")).await);
        assert!(verifies_as_sshsig(&answer, made_up.public(), data, "git"));
    }
    let request = sign_in_request(made_up.public(), b"session");
    let answer = signed(taken(workspaces, &turn, &signing(&ssh_key, SERVER, &request, "")).await);
    assert!(verifies_as_sign_in(&answer, made_up.public(), &request));
    let remembers = remembered(&service, workspace_id, hers, &ssh_key).await;
    let uses = remembers["uses"].as_array().unwrap();
    assert_eq!(uses.len(), 3, "{remembers}");
    for used in uses {
        assert_eq!(used["allowed"], "always", "{remembers}");
        assert_eq!(used["using"], SIGN_USE, "{remembers}");
    }

    // Another agent of hers is still asked: the leave names one agent.
    let other = on_turn(&ada, &workspace, "other");
    let others = HostedTurn {
        caller: &other,
        ..turn
    };
    asks(
        taken(
            workspaces,
            &others,
            &signing(&ssh_key, SERVER, COMMIT, "git"),
        )
        .await,
    );

    // She takes the leave back from her own device, and then her agent is asked again.
    let took = ask(
        &service,
        workspace_id,
        hers,
        person_vault::TAKE_BACK,
        json!({"item": ssh_key}),
    )
    .await
    .expect("she takes her leave back");
    assert_eq!(took.get("always"), None, "{took}");
    asks(taken(workspaces, &turn, &signing(&ssh_key, SERVER, COMMIT, "git")).await);
    server.shutdown().await;
}

/// Only an ssh key signs: a login kept for the same server is refused saying so, before its
/// person is asked.
#[tokio::test]
async fn a_login_signs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let login = kept(
        &service,
        workspace_id,
        (&ada, &as_ada),
        ("login", SERVER, "correct horse battery staple"),
    )
    .await;
    let scout = on_turn(&ada, &workspace, "scout");
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        policy: None,
    };
    let reason = refusal(taken(workspaces, &turn, &signing(&login, SERVER, COMMIT, "git")).await);
    assert_eq!(reason, "Only an ssh key signs");
    server.shutdown().await;
}

/// A key signs only for a server it belongs to, what is signed reads first, and a sign-in
/// request for another key signs nothing: each refused in words that never quote the data. A caller that is no turn this server carries out, a narrowed agent with no
/// rule for signing and the person's own device sign nothing.
#[tokio::test]
async fn what_signs_is_an_ssh_key_for_its_own_server_asked_by_a_turn() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let hers = (&ada, &as_ada);
    let made_up = MadeUp::new();
    let ssh_key = kept(
        &service,
        workspace_id,
        hers,
        ("ssh-key", SERVER, &made_up.pem),
    )
    .await;
    let scout = on_turn(&ada, &workspace, "scout");
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        policy: None,
    };
    let secret_data = b"data nobody repeats";

    for (why, request, said) in [
        (
            "another server",
            signing(&ssh_key, "ssh://elsewhere.example.test", secret_data, "git"),
            "That item belongs somewhere else",
        ),
        (
            "a web address",
            signing(&ssh_key, "https://git.example.test", secret_data, "git"),
            "A signature is for an ssh server, such as ssh://git.example.com",
        ),
        (
            "no sign-in request",
            signing(&ssh_key, SERVER, secret_data, ""),
            "Without a namespace, what is signed is an ssh sign-in request for this key: give a \
             namespace to sign anything else",
        ),
        (
            "a namespace that is no word",
            signing(&ssh_key, SERVER, secret_data, "git commits"),
            "A namespace is one short word, such as git or file, of letters, digits and . - _ @",
        ),
    ] {
        let reason = refusal(taken(workspaces, &turn, &request).await);
        assert_eq!(reason, said, "{why}");
    }
    let mut not_base64 = signing(&ssh_key, SERVER, secret_data, "git");
    if let InvokeArgument::Json(argument) = &mut not_base64.argument {
        argument.insert("data".into(), json!("data nobody repeats!"));
    }
    let reason = refusal(taken(workspaces, &turn, &not_base64).await);
    assert!(!reason.contains("nobody"), "{reason}");

    // A sign-in request for another key is asked about, and signs nothing once allowed.
    let stranger = MadeUp::new();
    let request = sign_in_request(stranger.public(), b"session");
    let theirs = signing(&ssh_key, SERVER, &request, "");
    asks(taken(workspaces, &turn, &theirs).await);
    let reason = refusal(taken(workspaces, &turn, &approved(&theirs, &workspace)).await);
    assert_eq!(
        reason,
        "Without a namespace, what is signed is an ssh sign-in request for this key: give a \
         namespace to sign anything else"
    );
    assert_eq!(
        remembered(&service, workspace_id, hers, &ssh_key).await,
        json!({"uses": []}),
        "nothing refused is written down"
    );

    // Callers that are no turn this server carries out.
    let wanted = signing(&ssh_key, SERVER, COMMIT, "git");
    for (who, asking) in not_hosted_turns(&ada, &workspace) {
        let turn = HostedTurn {
            workspace: workspace_id,
            caller: &asking,
            policy: None,
        };
        let refused = dispatched(workspaces, &turn, &wanted).await;
        assert!(
            matches!(refused, Err(UnknownAction | Refused { .. })),
            "{who}: {refused:?}"
        );
    }
    // An agent something narrows holds no rule that names signing.
    let policy = narrowing(&workspace, &scout, &narrowed_elsewhere(&workspace));
    let narrowed = HostedTurn {
        policy: Some(&policy),
        ..turn
    };
    assert_eq!(
        workspaces.use_for_turn(&narrowed, &wanted).await,
        Err(NotPermitted)
    );
    // The person's own device signs nothing through the vault's own requests.
    let InvokeArgument::Json(argument) = &wanted.argument else {
        panic!("arguments are an object");
    };
    assert_eq!(
        ask_at(
            &service,
            workspace_id,
            hers,
            person_vault::sign_target(&ssh_key),
            Value::Object(argument.clone()),
        )
        .await,
        Err(UnknownAction)
    );
    assert_eq!(
        remembered(&service, workspace_id, hers, &ssh_key).await,
        json!({"uses": []})
    );
    server.shutdown().await;
}
