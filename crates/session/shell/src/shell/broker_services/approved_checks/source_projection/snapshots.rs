//! Complete bounded Unicode source collection from genuine child action terminals.
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use tilo_action_broker::collab_broker_capnp::{
    ApprovedCheckContext, SourceBinding, SourceObservation,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetRecord {
    #[serde(rename = "roomId")]
    room: String,
    #[serde(rename = "appId")]
    app: String,
    #[serde(rename = "recordId")]
    record: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Basis {
    heads: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u8,
    record: TargetRecord,
    basis: Basis,
    content: Content,
    #[serde(rename = "nextCursor")]
    next_cursor: Option<serde_json::Value>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Content {
    file: String,
    position_units: String,
    text_sha256: String,
    total_chars: usize,
    start_chars: usize,
    next_chars: usize,
    eof: bool,
    text: String,
    before_context: String,
    after_context: String,
}
pub(super) struct Capture {
    pub bytes: Vec<u8>,
    pub heads: Vec<String>,
    pub observations: Vec<SourceObservation>,
}
impl Capture {
    pub(super) fn recheck(&self) -> Result<(), String> {
        if self.observations.is_empty() {
            return Err("source capture has no authenticated child observations".into());
        }
        for observed in &self.observations {
            observed.recheck()?;
        }
        Ok(())
    }
}
fn heads_valid(heads: &[String]) -> bool {
    !heads.is_empty()
        && heads.len() <= 64
        && heads.iter().zip(heads.iter().skip(1)).all(|(a, b)| a < b)
        && heads.iter().all(|head| {
            head.len() == 64
                && head
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}
/// Validate actual producer output, without treating a digest as access authority.
fn append(
    capture: &mut Capture,
    observed: SourceObservation,
    binding: &SourceBinding,
    file: &str,
    next: &mut usize,
    identity: &mut Option<(String, usize)>,
) -> Result<bool, String> {
    let eof = append_payload(
        capture,
        observed.output.clone(),
        binding,
        file,
        next,
        identity,
    )?;
    capture.observations.push(observed);
    Ok(eof)
}
fn append_payload(
    capture: &mut Capture,
    output: serde_json::Value,
    binding: &SourceBinding,
    file: &str,
    next: &mut usize,
    identity: &mut Option<(String, usize)>,
) -> Result<bool, String> {
    let value: Snapshot = serde_json::from_value(output)
        .map_err(|_| "source terminal did not match the bounded text snapshot contract")?;
    let chunk = value.content;
    if value.version != 1
        || value.next_cursor.is_some()
        || value.record.app != binding.app_id
        || value.record.room != binding.workspace
        || value.record.record != binding.record
        || !heads_valid(&value.basis.heads)
        || (!capture.heads.is_empty() && value.basis.heads != capture.heads)
        || chunk.file != file
        || chunk.position_units != "unicode_scalar_values"
        || chunk.start_chars != *next
        || chunk.text.chars().count() > 4096
        || chunk.text.len() > 8192
        || chunk.before_context.chars().count() > 32
        || chunk.after_context.chars().count() > 32
        || chunk.next_chars
            != next
                .checked_add(chunk.text.chars().count())
                .ok_or("source range overflow")?
        || chunk.next_chars > chunk.total_chars
        || chunk.eof != (chunk.next_chars == chunk.total_chars)
        || (chunk.text.is_empty() && !chunk.eof)
        || identity
            .as_ref()
            .is_some_and(|(sha, total)| sha != &chunk.text_sha256 || *total != chunk.total_chars)
        || capture.bytes.len().saturating_add(chunk.text.len()) > 2 * 1024 * 1024
    {
        return Err("source snapshot changed identity, heads, range or bounds".into());
    }
    capture.heads = value.basis.heads;
    *identity = Some((chunk.text_sha256, chunk.total_chars));
    *next = chunk.next_chars;
    capture.bytes.extend_from_slice(chunk.text.as_bytes());
    Ok(chunk.eof)
}
pub(super) fn collect(
    context: &ApprovedCheckContext,
    binding: &SourceBinding,
    file: &str,
    expected_heads: Option<&[String]>,
) -> Result<Capture, String> {
    if file.is_empty() || file.len() > 256 {
        return Err("source file identity invalid".into());
    }
    let mut capture = Capture {
        bytes: Vec::new(),
        heads: expected_heads.map_or_else(Vec::new, <[String]>::to_vec),
        observations: Vec::new(),
    };
    let mut next = 0;
    let mut identity = None;
    let mut invocations = HashSet::new();
    let began = std::time::Instant::now();
    loop {
        if began.elapsed() > std::time::Duration::from_secs(600)
            || capture.observations.len() >= 512
        {
            return Err("source collection exceeded its deadline or chunk bound".into());
        }
        let mut arguments = serde_json::Map::from_iter([
            ("start_chars".into(), next.into()),
            ("max_chars".into(), 4096.into()),
        ]);
        if !capture.heads.is_empty() {
            arguments.insert(
                "expected_heads".into(),
                serde_json::to_string(&capture.heads)
                    .map_err(|error| error.to_string())?
                    .into(),
            );
        }
        let observed = context.read_source(binding, &serde_json::Value::Object(arguments))?;
        if !invocations.insert(observed.invocation.clone())
            || observed.parent_invocation != context.invocation()
        {
            return Err("source terminal repeated or changed its parent invocation".into());
        }
        if append(
            &mut capture,
            observed,
            binding,
            file,
            &mut next,
            &mut identity,
        )? {
            break;
        }
    }
    let (expected, _) = identity.ok_or("source snapshot has no whole-file identity")?;
    if hex::encode(Sha256::digest(&capture.bytes)) != expected {
        return Err("source chunks do not match their whole-file digest".into());
    }
    capture.recheck()?;
    Ok(capture)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> (SourceBinding, Capture, serde_json::Value) {
        let binding = SourceBinding {
            app_id: "editor".into(),
            workspace: "room".into(),
            record: "record".into(),
            resource_identity: tilo_actions::ResourceSemanticIdentity {
                kind: "file".into(),
                key: "opaque".into(),
            },
            action: "snapshot".into(),
        };
        let capture = Capture {
            bytes: Vec::new(),
            heads: Vec::new(),
            observations: Vec::new(),
        };
        let text = "α😀x";
        let output = serde_json::json!({"version":1,"nextCursor":null,
            "record":{"appId":"editor","roomId":"room","recordId":"record"},
            "basis":{"heads":["a".repeat(64)]},
            "content":{"file":"file-id","position_units":"unicode_scalar_values",
                "text_sha256":hex::encode(Sha256::digest(text.as_bytes())),"total_chars":3,
                "start_chars":0,"next_chars":3,"eof":true,"text":text,"before_context":"","after_context":""}});
        (binding, capture, output)
    }

    #[test]
    fn source_projection_payload_validation_counts_unicode_but_never_mints_authority() {
        let (binding, mut capture, output) = inputs();
        let mut next = 0;
        assert!(
            append_payload(
                &mut capture,
                output,
                &binding,
                "file-id",
                &mut next,
                &mut None
            )
            .unwrap()
        );
        assert_eq!(next, 3);
        assert_eq!(capture.bytes, "α😀x".as_bytes());
        assert!(
            capture.recheck().is_err(),
            "valid payload bytes are not authenticated observations"
        );
    }

    #[test]
    fn source_projection_payload_refuses_mixed_records_heads_and_byte_positions_atomically() {
        for field in ["record", "heads", "position", "extra"] {
            let (binding, mut capture, mut output) = inputs();
            capture.heads = vec!["a".repeat(64)];
            match field {
                "record" => output["record"]["recordId"] = "foreign".into(),
                "heads" => output["basis"]["heads"] = serde_json::json!(["b".repeat(64)]),
                "position" => output["content"]["next_chars"] = 7.into(),
                _ => output["unreviewed"] = true.into(),
            }
            let mut next = 0;
            let mut identity = None;
            assert!(
                append_payload(
                    &mut capture,
                    output,
                    &binding,
                    "file-id",
                    &mut next,
                    &mut identity
                )
                .is_err()
            );
            assert!(capture.bytes.is_empty());
            assert_eq!(next, 0);
            assert!(identity.is_none());
        }
    }

    #[test]
    fn source_projection_heads_refuse_duplicates_empty_and_noncanonical_values() {
        for heads in [
            vec![],
            vec!["a".repeat(64), "a".repeat(64)],
            vec!["A".repeat(64)],
            vec!["b".repeat(64), "a".repeat(64)],
        ] {
            assert!(!heads_valid(&heads));
        }
    }
}
