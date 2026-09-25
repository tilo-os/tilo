//! Inert bounded IDB metadata. Only fresh native Renew and signed bytes can
//! authorize reuse; the immutable cut digest remains a cleanup identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Staged {
    pub pin: [u8; 16],
    pub scope: [u8; 32],
    pub cut: [u8; 32],
    pub resume: [u8; 32],
    pub total: u64,
    pub max_item: u64,
}
pub(crate) fn parse(raw: &str) -> Result<[Option<Staged>; 2], String> {
    use serde::{
        Deserialize, Deserializer,
        de::{Error, IgnoredAny, SeqAccess, Visitor},
    };
    if raw.len() > 8192 {
        return Err("staging metadata exceeds bound".into());
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Borrowed<'a> {
        id: &'a str,
        scope: &'a str,
        cut_token: &'a str,
        #[serde(default)]
        resume_digest: Option<&'a str>,
        total_bytes: u64,
        max_item_bytes: u64,
        epoch: &'a str,
        next_offset: u64,
    }
    fn fixed<const N: usize>(text: &str) -> Result<[u8; N], &'static str> {
        if text.len() != 2 * N
            || !text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("invalid stage digest");
        }
        let mut value = [0; N];
        for (out, pair) in value.iter_mut().zip(text.as_bytes().chunks_exact(2)) {
            let nibble = |v: u8| if v <= b'9' { v - b'0' } else { v - b'a' + 10 };
            *out = nibble(pair[0]) * 16 + nibble(pair[1]);
        }
        if value == [0; N] {
            return Err("zero stage identity");
        }
        Ok(value)
    }
    struct List;
    impl<'de> Visitor<'de> for List {
        type Value = [Option<Staged>; 2];
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("at most two bounded staging descriptors")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut entries = [None, None];
            for entry in &mut entries {
                let Some(value) = seq.next_element::<Borrowed<'de>>()? else {
                    return Ok(entries);
                };
                if value.total_bytes == 0
                    || value.total_bytes > 256 * 1024 * 1024
                    || value.max_item_bytes == 0
                    || value.max_item_bytes > value.total_bytes.min(64 * 1024 * 1024)
                    || value.epoch.is_empty()
                    || value.epoch.len() > 4096
                    || value.next_offset > value.total_bytes
                    || (value.next_offset != value.total_bytes
                        && value.next_offset % (240 * 1024) != 0)
                {
                    return Err(A::Error::custom("invalid staging layout"));
                }
                *entry = Some(Staged {
                    pin: fixed(value.id).map_err(A::Error::custom)?,
                    scope: fixed(value.scope).map_err(A::Error::custom)?,
                    cut: fixed(value.cut_token).map_err(A::Error::custom)?,
                    resume: fixed(value.resume_digest.unwrap_or(value.cut_token))
                        .map_err(A::Error::custom)?,
                    total: value.total_bytes,
                    max_item: value.max_item_bytes,
                });
            }
            if seq.next_element::<IgnoredAny>()?.is_some() {
                return Err(A::Error::custom("staging count exceeded"));
            }
            Ok(entries)
        }
    }
    let mut decoder = serde_json::Deserializer::from_str(raw);
    let value = decoder
        .deserialize_seq(List)
        .map_err(|_| "invalid bounded staging metadata")?;
    decoder.end().map_err(|_| "trailing staging metadata")?;
    Ok(value)
}
/// Decode a reset only after the caller has correlated the response to its
/// current authenticated Renew request. This inert codec confers no authority.
pub(crate) fn decode_reset(
    payload: &[u8],
) -> Result<tilo_relay::delivery_v3::state::SnapshotRequired, String> {
    use tilo_relay::delivery_v3::state::{ResetReason, SnapshotRequired};
    let reset = SnapshotRequired::decode(payload).map_err(|_| "invalid snapshot reset")?;
    if !matches!(
        reset.reason(),
        ResetReason::HistoryUnavailable
            | ResetReason::AuthorizationChanged
            | ResetReason::StoreRotated
    ) {
        return Err("unexpected snapshot renewal reset".into());
    }
    Ok(reset)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn own_stage_reset_accepts_only_closed_native_renewal_reasons() {
        use tilo_relay::delivery_v3::state::{ResetReason, SnapshotRequired};
        for reason in [
            ResetReason::HistoryUnavailable,
            ResetReason::AuthorizationChanged,
            ResetReason::StoreRotated,
        ] {
            let bytes = SnapshotRequired::new(reason, [7; 16]).encode().unwrap();
            let reset =
                decode_reset(&bytes).expect("authenticated own-stage reset permits fresh baseline");
            assert_eq!(reset.reason(), reason);
            assert_eq!(reset.store_generation(), [7; 16]);
            let mut extended = bytes;
            extended.push(0);
            assert!(decode_reset(&extended).is_err());
        }
        for reason in [
            ResetReason::CursorUnknown,
            ResetReason::CursorAhead,
            ResetReason::CommitMismatch,
        ] {
            assert!(
                decode_reset(&SnapshotRequired::new(reason, [7; 16]).encode().unwrap()).is_err()
            );
        }
        assert!(decode_reset(b"403").is_err());
        assert!(decode_reset(b"503").is_err());
    }
    fn entry() -> serde_json::Value {
        serde_json::json!({"id":"01".repeat(16),"scope":"02".repeat(32),"cutToken":"03".repeat(32),"resumeDigest":"04".repeat(32),"totalBytes":100,"maxItemBytes":100,"epoch":"lease","nextOffset":0})
    }
    #[test]
    fn metadata_is_borrowed_count_bounded_and_never_replaces_immutable_identity() {
        let good = entry();
        let parsed = parse(&serde_json::json!([good]).to_string()).unwrap();
        let value = parsed[0].as_ref().unwrap();
        assert_eq!(value.cut, [3; 32]);
        assert_eq!(value.resume, [4; 32]);
        assert!(parsed[1].is_none());
        assert!(parse(&serde_json::json!([entry(), entry(), entry()]).to_string()).is_err());
        for (key, value) in [
            ("id", serde_json::json!("00".repeat(16))),
            ("resumeDigest", serde_json::json!("AA".repeat(32))),
            ("totalBytes", serde_json::json!(268435457)),
            ("maxItemBytes", serde_json::json!(101)),
        ] {
            let mut bad = entry();
            bad[key] = value;
            assert!(parse(&serde_json::json!([bad]).to_string()).is_err());
        }
        let mut legacy = entry();
        legacy.as_object_mut().unwrap().remove("resumeDigest");
        let parsed = parse(&serde_json::json!([legacy]).to_string()).unwrap();
        assert_eq!(parsed[0].as_ref().unwrap().resume, [3; 32]);
    }
}
