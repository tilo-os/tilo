//! One invocation as its caller signed it: who asked for which action, on
//! which resource of which workspace, with which arguments.
//!
//! A host runs an action for a caller; the signature is what makes that
//! claim checkable by anyone, not only by the host that read the caller's
//! connection. The caller's key signs a statement binding the workspace, the
//! invocation id, the target (provider, resource and action), a BLAKE3 hash
//! of the canonical arguments, the invocation it continues
//! ([`InvokeRequest::on_behalf_of`]), the issue time and the signing key. The
//! envelope carries the signer's certificate (with the certificate that
//! issued it, for a key another key minted), so the statement checks against
//! the person's root and their revocations with nothing else at hand.
//!
//! This module holds the statement and its exact bytes. Signing with a device
//! key and checking the certificate are the identity layer's
//! (`tilo_server_protocol::invocation`): this crate sits below it.

use super::{InvokeArgument, InvokeRequest, RemoteActionTarget};
use crate::InvocationId;
use serde::{Deserialize, Serialize};

/// Domain separator for a caller's signature over one invocation.
pub const INVOCATION_SIGNATURE_DOMAIN: &[u8] = b"tilo-invocation-v1\0";

/// How far a signed invocation's issue time may lie from the verifier's
/// clock, either way. Wide enough for a retry after a reconnect and for the
/// clocks of a phone and a server to disagree; an older statement is refused
/// as stale, and within the window the invocation id names the one run.
pub const INVOCATION_SIGNATURE_TOLERANCE_NS: u64 = 5 * 60 * 1_000_000_000;

/// The largest signer certificate an envelope carries: a certificate and
/// its issuer's, with room for long labels.
pub const MAX_SIGNER_CERT_BYTES: usize = 4 * 1024;

/// What the caller's key states about one invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvocationStatement {
    /// The workspace the invocation acts in, as its id's text.
    pub workspace: String,
    pub invocation: InvocationId,
    pub target: RemoteActionTarget,
    /// [`argument_hash`] of the request's argument.
    pub arguments: [u8; 32],
    pub on_behalf_of: Option<InvocationId>,
    /// The signer's clock when it signed, in ns since the epoch.
    pub issued_at_ns: u64,
    /// The key that signs: the one the carried certificate certifies.
    pub signer: [u8; 32],
}

/// Which part of a request its signed statement does not cover.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StatementMismatch {
    #[error("the signed invocation names another workspace")]
    Workspace,
    #[error("the signed invocation names another invocation")]
    Invocation,
    #[error("the signed invocation names another action")]
    Target,
    #[error("the request's arguments are not the ones signed")]
    Arguments,
    #[error("the signed invocation continues another invocation")]
    OnBehalfOf,
}

impl InvocationStatement {
    /// The statement `signer` makes at `issued_at_ns` about `request` in
    /// `workspace`.
    #[must_use]
    pub fn of(
        workspace: &str,
        request: &InvokeRequest,
        issued_at_ns: u64,
        signer: [u8; 32],
    ) -> Self {
        Self {
            workspace: workspace.to_owned(),
            invocation: request.invocation,
            target: request.target.clone(),
            arguments: argument_hash(&request.argument),
            on_behalf_of: request.on_behalf_of,
            issued_at_ns,
            signer,
        }
    }

    /// Whether this statement covers `request` in `workspace`, and if not,
    /// the first part that differs.
    ///
    /// # Errors
    ///
    /// The [`StatementMismatch`] naming that part.
    pub fn covers(
        &self,
        workspace: &str,
        request: &InvokeRequest,
    ) -> Result<(), StatementMismatch> {
        if self.workspace != workspace {
            return Err(StatementMismatch::Workspace);
        }
        if self.invocation != request.invocation {
            return Err(StatementMismatch::Invocation);
        }
        if self.target != request.target {
            return Err(StatementMismatch::Target);
        }
        if self.arguments != argument_hash(&request.argument) {
            return Err(StatementMismatch::Arguments);
        }
        if self.on_behalf_of != request.on_behalf_of {
            return Err(StatementMismatch::OnBehalfOf);
        }
        Ok(())
    }

    /// The bytes the signer's key signs: the domain separator and the BLAKE3
    /// hash of the statement's exact encoding.
    #[must_use]
    pub fn signing_message(&self) -> Vec<u8> {
        let mut message = INVOCATION_SIGNATURE_DOMAIN.to_vec();
        message.extend_from_slice(blake3::hash(&self.encode()).as_bytes());
        message
    }

    /// Whether the issue time is within [`INVOCATION_SIGNATURE_TOLERANCE_NS`]
    /// of `now_ns`.
    #[must_use]
    pub const fn fresh_at(&self, now_ns: u64) -> bool {
        self.issued_at_ns.abs_diff(now_ns) <= INVOCATION_SIGNATURE_TOLERANCE_NS
    }

    /// The statement's one encoding: every field in order, each text length
    /// prefixed, so two statements encode alike only when they are alike.
    fn encode(&self) -> Vec<u8> {
        fn text(out: &mut Vec<u8>, text: &str) {
            out.extend_from_slice(&u64::try_from(text.len()).unwrap_or(u64::MAX).to_le_bytes());
            out.extend_from_slice(text.as_bytes());
        }
        let mut out = Vec::with_capacity(256);
        text(&mut out, &self.workspace);
        out.extend_from_slice(self.invocation.0.as_bytes());
        text(&mut out, &self.target.provider);
        text(&mut out, &self.target.address.resource);
        text(&mut out, &self.target.address.action);
        out.extend_from_slice(&self.arguments);
        match self.on_behalf_of {
            None => out.push(0),
            Some(original) => {
                out.push(1);
                out.extend_from_slice(original.0.as_bytes());
            }
        }
        out.extend_from_slice(&self.issued_at_ns.to_le_bytes());
        out.extend_from_slice(&self.signer);
        out
    }
}

/// The BLAKE3 hash of an invocation's argument in its canonical form: a tag
/// for which form it takes, then the text as is, or the argument object as JSON with
/// every object's keys sorted, so the same arguments hash alike however a
/// transport ordered them.
#[must_use]
pub fn argument_hash(argument: &InvokeArgument) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tilo-invocation-arguments-v1\0");
    match argument {
        InvokeArgument::None => {
            hasher.update(&[0]);
        }
        InvokeArgument::Text(text) => {
            hasher.update(&[1]);
            hasher.update(text.as_bytes());
        }
        InvokeArgument::Json(object) => {
            hasher.update(&[2]);
            tilo_canonical_json::write_object_bytes(object, |chunk| {
                hasher.update(chunk);
            });
        }
    }
    *hasher.finalize().as_bytes()
}

/// An invocation's [`InvocationStatement`], the signer's signature over it,
/// and the signer's certificate. Decoded, it is only what it claims: nothing
/// in it is trusted until the identity layer verified the signature, the
/// certificate, its revocation and its freshness.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "SignedWire", into = "SignedWire")]
pub struct SignedInvocation {
    statement: InvocationStatement,
    signature: [u8; 64],
    signer_cert: Vec<u8>,
}

impl SignedInvocation {
    /// `statement` with the `signature` its signer made over
    /// [`InvocationStatement::signing_message`], and `signer_cert`, the
    /// signer's certificate in its binary encoding.
    #[must_use]
    pub const fn new(
        statement: InvocationStatement,
        signature: [u8; 64],
        signer_cert: Vec<u8>,
    ) -> Self {
        Self {
            statement,
            signature,
            signer_cert,
        }
    }

    /// What the signer claims to have asked. Trust it only once verified.
    #[must_use]
    pub const fn statement(&self) -> &InvocationStatement {
        &self.statement
    }

    #[must_use]
    pub const fn signature(&self) -> &[u8; 64] {
        &self.signature
    }

    /// The signer's certificate in its binary encoding, its issuer's within.
    #[must_use]
    pub fn signer_cert(&self) -> &[u8] {
        &self.signer_cert
    }
}

/// The most of an invocation's argument a change keeps beside its signed
/// invocation, encoded: what the change already documents of its action
/// ([`crate::context::WHOLE_ACTION_BUDGET`]). A larger argument is left out,
/// and the change still says who asked for what.
pub const MAX_RECORDED_ARGUMENT_BYTES: usize = crate::context::WHOLE_ACTION_BUDGET;

/// An invocation as a change a host made for it keeps it: the caller's
/// [`SignedInvocation`] and the argument it signed. The host signs the change
/// as the one who ran it; this is what says who asked for it. A peer that
/// reads the change checks the signature and the certificate, and the
/// argument against the hash under the signature ([`Self::argument_signed`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedCall {
    pub signed: SignedInvocation,
    /// The argument as the caller sent it. `None` when it was larger than
    /// [`MAX_RECORDED_ARGUMENT_BYTES`]: the change then says who asked for
    /// which action, not with what.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argument: Option<InvokeArgument>,
}

impl SignedCall {
    /// What a change made for `request` keeps of it: `None` for a request
    /// nobody signed.
    #[must_use]
    pub fn of(request: &InvokeRequest) -> Option<Self> {
        let signed = request.signed.as_deref()?.clone();
        let argument = serde_json::to_vec(&request.argument)
            .is_ok_and(|encoded| encoded.len() <= MAX_RECORDED_ARGUMENT_BYTES)
            .then(|| request.argument.clone());
        Some(Self { signed, argument })
    }

    /// Whether the kept argument is the one signed: `None` when the change
    /// keeps none to compare.
    #[must_use]
    pub fn argument_signed(&self) -> Option<bool> {
        self.argument
            .as_ref()
            .map(|argument| argument_hash(argument) == self.signed.statement.arguments)
    }
}

/// The envelope on the wire: the statement's fields, the signature and the
/// certificate, the byte strings as lower-case hex so the envelope reads the
/// same in every encoding a frame travels in.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedWire {
    workspace: String,
    invocation: InvocationId,
    target: RemoteActionTarget,
    arguments: String,
    on_behalf_of: Option<InvocationId>,
    issued_at_ns: u64,
    signer: String,
    signature: String,
    signer_cert: String,
}

impl From<SignedInvocation> for SignedWire {
    fn from(signed: SignedInvocation) -> Self {
        let InvocationStatement {
            workspace,
            invocation,
            target,
            arguments,
            on_behalf_of,
            issued_at_ns,
            signer,
        } = signed.statement;
        Self {
            workspace,
            invocation,
            target,
            arguments: hex::encode(arguments),
            on_behalf_of,
            issued_at_ns,
            signer: hex::encode(signer),
            signature: hex::encode(signed.signature),
            signer_cert: hex::encode(signed.signer_cert),
        }
    }
}

impl TryFrom<SignedWire> for SignedInvocation {
    type Error = String;

    fn try_from(wire: SignedWire) -> Result<Self, Self::Error> {
        fn bytes<const N: usize>(text: &str, what: &str) -> Result<[u8; N], String> {
            let mut out = [0; N];
            hex::decode_to_slice(text, &mut out)
                .map_err(|_| format!("a signed invocation's {what} is not {N} hex bytes"))?;
            Ok(out)
        }
        if wire.signer_cert.len() > 2 * MAX_SIGNER_CERT_BYTES {
            return Err("a signed invocation's certificate is too large".into());
        }
        let signer_cert = hex::decode(&wire.signer_cert)
            .map_err(|_| "a signed invocation's certificate is not hex".to_owned())?;
        Ok(Self {
            statement: InvocationStatement {
                workspace: wire.workspace,
                invocation: wire.invocation,
                target: wire.target,
                arguments: bytes(&wire.arguments, "argument hash")?,
                on_behalf_of: wire.on_behalf_of,
                issued_at_ns: wire.issued_at_ns,
                signer: bytes(&wire.signer, "signer")?,
            },
            signature: bytes(&wire.signature, "signature")?,
            signer_cert,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ActionAddress;

    fn request(argument: InvokeArgument) -> InvokeRequest {
        InvokeRequest {
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: "so.tilo.notes".into(),
                address: ActionAddress::new("note:1", "rename"),
            },
            argument,
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }
    }

    fn object(json: &str) -> InvokeArgument {
        match serde_json::from_str(json).unwrap() {
            serde_json::Value::Object(object) => InvokeArgument::Json(object),
            _ => unreachable!(),
        }
    }

    #[test]
    fn arguments_hash_alike_in_any_key_order_and_unlike_once_a_value_changes() {
        let one = object(r#"{"title":"Plan","tags":["a",{"z":1,"y":2.5}]}"#);
        let reordered = object(r#"{"tags":["a",{"y":2.5,"z":1}],"title":"Plan"}"#);
        let changed = object(r#"{"title":"Plan!","tags":["a",{"z":1,"y":2.5}]}"#);
        assert_eq!(argument_hash(&one), argument_hash(&reordered));
        assert_ne!(argument_hash(&one), argument_hash(&changed));
        // A text and an object with the same characters are not one argument.
        assert_ne!(
            argument_hash(&InvokeArgument::Text("{}".into())),
            argument_hash(&object("{}"))
        );
        assert_ne!(
            argument_hash(&InvokeArgument::None),
            argument_hash(&InvokeArgument::Text(String::new()))
        );
    }

    #[test]
    fn a_statement_covers_its_request_and_names_the_first_part_another_changes() {
        let asked = request(object(r#"{"title":"Plan"}"#));
        let statement = InvocationStatement::of("w1", &asked, 7, [3; 32]);
        assert_eq!(statement.covers("w1", &asked), Ok(()));
        assert_eq!(
            statement.covers("w2", &asked),
            Err(StatementMismatch::Workspace)
        );
        let mut other = asked.clone();
        other.target.address.action = "delete".into();
        assert_eq!(
            statement.covers("w1", &other),
            Err(StatementMismatch::Target)
        );
        let mut other = asked.clone();
        other.argument = object(r#"{"title":"Other"}"#);
        assert_eq!(
            statement.covers("w1", &other),
            Err(StatementMismatch::Arguments)
        );
        let mut other = asked;
        other.on_behalf_of = Some(InvocationId::new());
        assert_eq!(
            statement.covers("w1", &other),
            Err(StatementMismatch::OnBehalfOf)
        );
        // Each field reaches the signed bytes.
        let mut later = statement.clone();
        later.issued_at_ns += 1;
        assert_ne!(statement.signing_message(), later.signing_message());
    }

    #[test]
    fn the_envelope_reads_back_the_same_from_json_and_postcard() {
        let statement = InvocationStatement::of("w1", &request(InvokeArgument::None), 9, [5; 32]);
        let signed = SignedInvocation::new(statement, [7; 64], vec![1, 2, 3]);
        let json = serde_json::to_string(&signed).unwrap();
        assert_eq!(
            serde_json::from_str::<SignedInvocation>(&json).unwrap(),
            signed
        );
        let binary = postcard::to_allocvec(&signed).unwrap();
        assert_eq!(
            postcard::from_bytes::<SignedInvocation>(&binary).unwrap(),
            signed
        );
        let short = json.replace(&hex::encode([7u8; 64]), "07");
        assert!(serde_json::from_str::<SignedInvocation>(&short).is_err());
    }

    const CANONICAL_JSON_CORPUS: &[(&str, &str, &[u8])] = &[
        (
            "nested_unsorted",
            r#"{"z":{"b":2,"a":1},"a":{"d":{"y":true,"x":false},"c":[3,2,1]},"m":0}"#,
            br#"{"a":{"c":[3,2,1],"d":{"x":false,"y":true}},"m":0,"z":{"a":1,"b":2}}"#,
        ),
        (
            "utf8_vs_utf16_key_order",
            r#"{"\uD83D\uDE00":4,"\uE000":3,"é":2,"z":1}"#,
            b"{\"z\":1,\"\xc3\xa9\":2,\"\xee\x80\x80\":3,\"\xf0\x9f\x98\x80\":4}",
        ),
        (
            "non_bmp_vs_bmp_high_key_order",
            r#"{"\uD83D\uDE80":1,"\uFFFF":2,"\uE000":3}"#,
            b"{\"\xee\x80\x80\":3,\"\xef\xbf\xbf\":2,\"\xf0\x9f\x9a\x80\":1}",
        ),
        (
            "ascii_empty_prefix_key_order",
            r#"{"b":1,"":2,"a":3,"ab":4,"B":5,"A":6}"#,
            br#"{"":2,"A":6,"B":5,"a":3,"ab":4,"b":1}"#,
        ),
        (
            "nested_ten_keys",
            r#"{"k9":9,"k8":8,"k7":{"j9":9,"j8":8,"j7":7,"j6":6,"j5":5,"j4":4,"j3":3,"j2":2,"j1":1,"j0":0},"k6":6,"k5":5,"k4":4,"k3":3,"k2":2,"k1":1,"k0":0}"#,
            br#"{"k0":0,"k1":1,"k2":2,"k3":3,"k4":4,"k5":5,"k6":6,"k7":{"j0":0,"j1":1,"j2":2,"j3":3,"j4":4,"j5":5,"j6":6,"j7":7,"j8":8,"j9":9},"k8":8,"k9":9}"#,
        ),
        ("empty_object", "{}", b"{}"),
        ("empty_array", "[]", b"[]"),
        ("empty_string", r#""""#, br#""""#),
        (
            "line_seps_fffd_and_solidus",
            r#""\u2028\u2029\ufffd/""#,
            b"\"\xe2\x80\xa8\xe2\x80\xa9\xef\xbf\xbd/\"",
        ),
        (
            "all_32_control_bytes_value",
            r#""\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\u0008\u0009\u000a\u000b\u000c\u000d\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f""#,
            br#""\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\b\t\n\u000b\f\r\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f""#,
        ),
        (
            "all_32_control_bytes_key",
            r#"{"a":2,"\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\u0008\u0009\u000a\u000b\u000c\u000d\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f":1}"#,
            br#"{"\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\b\t\n\u000b\f\r\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f":1,"a":2}"#,
        ),
        (
            "escaped_and_unicode_string",
            r#""q\"b\\c\u0000\u0001\u0007\u0008\u0009\u000a\u000b\u000c\u000d\u000e\u001f\u007f café 日本語 🚀""#,
            b"\"q\\\"b\\\\c\\u0000\\u0001\\u0007\\b\\t\\n\\u000b\\f\\r\\u000e\\u001f\x7f caf\xc3\xa9 \xe6\x97\xa5\xe6\x9c\xac\xe8\xaa\x9e \xf0\x9f\x9a\x80\"",
        ),
        (
            "escaped_keys_object",
            r#"{"q\"b\\c\u0000\b\t\n\f\r\u001f\u007f":2,"a":1}"#,
            b"{\"a\":1,\"q\\\"b\\\\c\\u0000\\b\\t\\n\\f\\r\\u001f\x7f\":2}",
        ),
        ("i64_min", "-9223372036854775808", b"-9223372036854775808"),
        ("i64_max", "9223372036854775807", b"9223372036854775807"),
        (
            "u64_above_i64_max",
            "9223372036854775808",
            b"9223372036854775808",
        ),
        ("u64_max", "18446744073709551615", b"18446744073709551615"),
        ("zero", "0", b"0"),
        ("negative_zero", "-0.0", b"-0.0"),
        ("float_whole", "1.0", b"1.0"),
        ("float_tenth", "0.1", b"0.1"),
        ("float_1e20", "1e20", b"1e+20"),
        ("float_1e21", "1e21", b"1e+21"),
        ("float_1e_neg5", "1e-5", b"0.00001"),
        ("float_1e_neg7", "1e-7", b"1e-7"),
        ("float_min_subnormal", "5e-324", b"5e-324"),
        (
            "float_max_finite",
            "1.7976931348623157e308",
            b"1.7976931348623157e+308",
        ),
        ("float_1e15", "1e15", b"1000000000000000.0"),
        ("float_0_3_ulp", "0.30000000000000004", b"0.30000000000000004"),
        ("bool_true", "true", b"true"),
        ("bool_false", "false", b"false"),
        ("null", "null", b"null"),
        (
            "mixed_array",
            r#"[null,true,false,0,-1,1.0,0.1,-0.0,"a\"b",[],{},{"b":2,"a":1}]"#,
            br#"[null,true,false,0,-1,1.0,0.1,-0.0,"a\"b",[],{},{"a":1,"b":2}]"#,
        ),
    ];

    #[test]
    fn argument_hash_pins_canonical_bytes_for_shared_corpus() {
        const PINNED_NESTED_HASH: [u8; 32] = [
            0xda, 0x27, 0x10, 0xfd, 0x5e, 0x3c, 0xe4, 0x49, 0x6e, 0xd8, 0xa3, 0x94, 0x16, 0x6e,
            0x6d, 0x76, 0x7f, 0x43, 0x1b, 0xcd, 0x08, 0x53, 0xc1, 0xb7, 0x1a, 0x7e, 0x10, 0xa9,
            0x59, 0x37, 0x22, 0x44,
        ];

        for &(name, input, expected) in CANONICAL_JSON_CORPUS {
            let value: serde_json::Value = serde_json::from_str(input).unwrap();
            let (map, expected_bytes) = match value {
                serde_json::Value::Object(map) => (map, expected.to_vec()),
                other => {
                    let mut map = serde_json::Map::new();
                    map.insert("v".to_owned(), other);
                    let mut bytes = Vec::with_capacity(expected.len().saturating_add(6));
                    bytes.extend_from_slice(br#"{"v":"#);
                    bytes.extend_from_slice(expected);
                    bytes.push(b'}');
                    (map, bytes)
                }
            };
            let mut hasher = blake3::Hasher::new();
            hasher.update(b"tilo-invocation-arguments-v1\0");
            hasher.update(&[2]);
            hasher.update(&expected_bytes);
            assert_eq!(
                argument_hash(&InvokeArgument::Json(map)),
                *hasher.finalize().as_bytes(),
                "row {name}"
            );
        }
        assert_eq!(
            argument_hash(&object(CANONICAL_JSON_CORPUS[0].1)),
            PINNED_NESTED_HASH
        );
    }
}
