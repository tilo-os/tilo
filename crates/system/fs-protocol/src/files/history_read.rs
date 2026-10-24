//! Bounded, read-only navigation of immutable file history. Cursors grant no authority.
use super::{FileRecordId, FileVersionId, FileVersionV1};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const HISTORY_READ_XATTR: &str = "user.tilo.hread.v1";
pub const MAX_HISTORY_READ_BYTES: usize = 60 * 1024;
pub const MAX_HISTORY_PAGE_VERSIONS: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileHistoryReadScopeV1 {
    pub workspace_id: Uuid,
    pub mount_lifetime: Uuid,
    pub record_id: FileRecordId,
    pub frontier: [u8; 32],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileHistoryReadQueryV1 {
    Page {
        cursor: Option<([u8; 32], FileVersionId)>,
    },
    Version {
        frontier: [u8; 32],
        version_id: FileVersionId,
    },
    Preview {
        frontier: [u8; 32],
        version_id: FileVersionId,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileHistoryPageV1 {
    pub scope: FileHistoryReadScopeV1,
    pub versions: Vec<FileVersionV1>,
    pub total_versions: u64,
    pub heads: Vec<FileVersionId>,
    pub current_revision: Option<super::ContentRevisionV1>,
    pub next_after: Option<FileVersionId>,
    /// The agents behind those of [`Self::versions`] an agent caused, as
    /// each version's own signing device recorded it. Absent from every
    /// page without one, so a reader that predates it reads the page as
    /// before.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attributions: Vec<super::FileVersionAttributionV1>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileHistoryReadErrorV1 {
    Stale,
    NotVisible,
    MissingVersion,
    InvalidRequest,
    Unavailable,
    TooLarge,
    NoContent,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum FileHistoryReadValueV1 {
    Page(FileHistoryPageV1),
    Version {
        scope: FileHistoryReadScopeV1,
        version: FileVersionV1,
        /// The agent behind this version, when an agent caused it, as its
        /// own signing device recorded it. Absent otherwise, so a reader
        /// that predates it reads the version as before.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attribution: Option<super::FileAttributionV1>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileHistoryReadResponseV1 {
    pub schema_version: u16,
    pub result: Result<FileHistoryReadValueV1, FileHistoryReadErrorV1>,
}
impl FileHistoryReadQueryV1 {
    /// The full digest binds actual workspace/mount/record/history. Compact
    /// names fit macOS `XATTR_MAXNAMELEN=127` as well as Linux's 255-byte limit.
    pub fn xattr_name(self) -> String {
        let (kind, frontier, id) = match self {
            Self::Page { cursor: None } => return HISTORY_READ_XATTR.into(),
            Self::Page {
                cursor: Some((frontier, id)),
            } => ("p", frontier, id),
            Self::Version {
                frontier,
                version_id,
            } => ("v", frontier, version_id),
            Self::Preview {
                frontier,
                version_id,
            } => ("b", frontier, version_id),
        };
        let digest = hex::encode(frontier);
        format!("{HISTORY_READ_XATTR}.{kind}.{digest}.{}", id.0.simple())
    }
    /// Parse one canonical versioned query name.
    /// # Errors
    /// Rejects unsupported versions, malformed fields and overlong names.
    pub fn parse_xattr(name: &str) -> Result<Self, FileHistoryReadErrorV1> {
        use FileHistoryReadErrorV1::InvalidRequest;
        if name == HISTORY_READ_XATTR {
            return Ok(Self::Page { cursor: None });
        }
        if name.len() > 127 {
            return Err(InvalidRequest);
        }
        let rest = name
            .strip_prefix(HISTORY_READ_XATTR)
            .and_then(|s| s.strip_prefix('.'))
            .ok_or(InvalidRequest)?;
        let mut fields = rest.split('.');
        let kind = fields.next().ok_or(InvalidRequest)?;
        let parse_id = |s: Option<&str>| -> Result<Uuid, FileHistoryReadErrorV1> {
            let s = s.ok_or(InvalidRequest)?;
            if s.len() != 32
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(InvalidRequest);
            }
            let id = Uuid::parse_str(s).map_err(|_| InvalidRequest)?;
            if id.is_nil() {
                return Err(InvalidRequest);
            }
            Ok(id)
        };
        let digest = fields.next().ok_or(InvalidRequest)?;
        if digest.len() != 64 {
            return Err(InvalidRequest);
        }
        let mut frontier = [0; 32];
        for (target, pair) in frontier.iter_mut().zip(digest.as_bytes().chunks_exact(2)) {
            let pair = std::str::from_utf8(pair).map_err(|_| InvalidRequest)?;
            if !pair
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(InvalidRequest);
            }
            *target = u8::from_str_radix(pair, 16).map_err(|_| InvalidRequest)?;
        }
        let version_id = FileVersionId(parse_id(fields.next())?);
        if fields.next().is_some() {
            return Err(InvalidRequest);
        }
        match kind {
            "p" => Ok(Self::Page {
                cursor: Some((frontier, version_id)),
            }),
            "v" => Ok(Self::Version {
                frontier,
                version_id,
            }),
            "b" => Ok(Self::Preview {
                frontier,
                version_id,
            }),
            _ => Err(InvalidRequest),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_read_names_are_bounded_exact_and_strict() {
        let scope = FileHistoryReadScopeV1 {
            workspace_id: Uuid::from_u128(1),
            mount_lifetime: Uuid::from_u128(2),
            record_id: FileRecordId(Uuid::from_u128(3)),
            frontier: [0xab; 32],
        };
        for query in [
            FileHistoryReadQueryV1::Page {
                cursor: Some((scope.frontier, FileVersionId(Uuid::from_u128(4)))),
            },
            FileHistoryReadQueryV1::Version {
                frontier: scope.frontier,
                version_id: FileVersionId(Uuid::from_u128(5)),
            },
            FileHistoryReadQueryV1::Preview {
                frontier: scope.frontier,
                version_id: FileVersionId(Uuid::from_u128(6)),
            },
        ] {
            let name = query.xattr_name();
            assert!(
                name.len() <= 127,
                "macOS xattr names must fit XATTR_MAXNAMELEN=127"
            );
            assert_eq!(FileHistoryReadQueryV1::parse_xattr(&name), Ok(query));
            assert_eq!(
                FileHistoryReadQueryV1::parse_xattr(&(name.clone() + ".extra")),
                Err(FileHistoryReadErrorV1::InvalidRequest)
            );
            assert!(FileHistoryReadQueryV1::parse_xattr(&name.replace("abab", "zzzz")).is_err());
            assert!(FileHistoryReadQueryV1::parse_xattr(&name.replace("v1", "v2")).is_err());
            assert!(
                FileHistoryReadQueryV1::parse_xattr(
                    &name
                        .rsplit_once('.')
                        .map(|(prefix, _)| format!("{prefix}.00000000000000000000000000000000"))
                        .unwrap()
                )
                .is_err()
            );
        }
    }

    /// A single-version read carries the agent behind it only when there is
    /// one, so a reader that predates it reads every version as before.
    #[test]
    fn version_reads_carry_their_agent_additively() {
        use crate::files::{ContentHash, FileActor, FileAttributionV1, FileProvenance};
        use collab_types::Ed25519PublicKey;
        let version = FileVersionV1 {
            id: FileVersionId(Uuid::from_u128(4)),
            record_id: FileRecordId(Uuid::from_u128(3)),
            parents: vec![],
            actor: FileActor {
                person: Ed25519PublicKey([1; 32]),
                device: Ed25519PublicKey([2; 32]),
            },
            timestamp_ms: 1,
            content_hash: None,
            metadata_hash: ContentHash([3; 32]),
            provenance: FileProvenance::Create,
        };
        let plain = FileHistoryReadValueV1::Version {
            scope: FileHistoryReadScopeV1 {
                workspace_id: Uuid::from_u128(1),
                mount_lifetime: Uuid::from_u128(2),
                record_id: FileRecordId(Uuid::from_u128(3)),
                frontier: [0xab; 32],
            },
            version,
            attribution: None,
        };
        let bytes = serde_json::to_string(&plain).unwrap();
        assert!(!bytes.contains("attribution"), "{bytes}");
        assert_eq!(
            serde_json::from_str::<FileHistoryReadValueV1>(&bytes).unwrap(),
            plain
        );
        let FileHistoryReadValueV1::Version { scope, version, .. } = plain else {
            unreachable!()
        };
        let attributed = FileHistoryReadValueV1::Version {
            scope,
            version,
            attribution: Some(FileAttributionV1 {
                actor: "agent:scribe".into(),
                label: "Scribe".into(),
                color: [10, 20, 30],
            }),
        };
        let bytes = serde_json::to_vec(&attributed).unwrap();
        assert_eq!(
            serde_json::from_slice::<FileHistoryReadValueV1>(&bytes).unwrap(),
            attributed
        );
    }

    /// A page carries the agents behind its versions only when there are
    /// some, so a page without any reads exactly as before; an attribution
    /// names an agent, never a person or an unpresentable actor.
    #[test]
    fn pages_carry_agent_attributions_additively() {
        use crate::files::{FileAttributionV1, FileVersionAttributionV1};
        let page = FileHistoryPageV1 {
            scope: FileHistoryReadScopeV1 {
                workspace_id: Uuid::from_u128(1),
                mount_lifetime: Uuid::from_u128(2),
                record_id: FileRecordId(Uuid::from_u128(3)),
                frontier: [0xab; 32],
            },
            versions: vec![],
            total_versions: 0,
            heads: vec![],
            current_revision: None,
            next_after: None,
            attributions: vec![],
        };
        let plain = serde_json::to_string(&page).unwrap();
        assert!(!plain.contains("attributions"), "{plain}");
        assert_eq!(
            serde_json::from_str::<FileHistoryPageV1>(&plain).unwrap(),
            page
        );

        let scribe = FileAttributionV1 {
            actor: "agent:scribe".into(),
            label: "Scribe".into(),
            color: [10, 20, 30],
        };
        assert_eq!(scribe.validate(), Ok(()));
        assert_eq!(scribe.agent_id(), Some("scribe"));
        let attributed = FileHistoryPageV1 {
            attributions: vec![FileVersionAttributionV1 {
                version_id: FileVersionId(Uuid::from_u128(4)),
                attribution: scribe.clone(),
            }],
            ..page
        };
        let bytes = serde_json::to_vec(&attributed).unwrap();
        assert_eq!(
            serde_json::from_slice::<FileHistoryPageV1>(&bytes).unwrap(),
            attributed
        );

        for actor in ["ada", "agent:", "agent: scribe", "agent:sc\u{7}ribe"] {
            let forged = FileAttributionV1 {
                actor: actor.into(),
                ..scribe.clone()
            };
            assert_eq!(
                forged.validate(),
                Err(crate::FileRecordError::InvalidAttribution),
                "{actor:?}"
            );
        }
        let unnamed = FileAttributionV1 {
            label: " ".into(),
            ..scribe.clone()
        };
        assert!(unnamed.validate().is_err());
        let long = FileAttributionV1 {
            label: "x".repeat(crate::files::MAX_ATTRIBUTION_TEXT_BYTES + 1),
            ..scribe
        };
        assert!(long.validate().is_err());
    }
}
