//! Deterministic, fail-closed content merge codecs.
//!
//! This module is deliberately independent from the signed transaction and
//! namespace layers. It accepts exact immutable version bytes and either
//! returns one deterministic result or refuses the merge. Callers must bind
//! the selected codec and every input version in a signed transaction before
//! publishing the returned bytes.

use automerge::Automerge;
use collab_types::{FileVersionId, FileVersionV1};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

/// Keep automatic conflict resolution bounded independently of hydration.
pub const MAX_MERGE_INPUT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeterministicMergeCodec {
    QdocAutomergeV1,
    PlainUtf8ThreeWayV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactMergeInput {
    pub version_id: FileVersionId,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactMergeBase {
    pub version_id: FileVersionId,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeterministicMergeResult {
    pub codec: DeterministicMergeCodec,
    pub base_version: Option<FileVersionId>,
    pub bytes: Vec<u8>,
}

/// The result of inspecting two immutable conflict members before publishing a
/// resolution.  A preflight never changes history: callers can show the
/// reason to a person first, and only use [`MergePreflight::Safe`] as the
/// input to a separately signed merge transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergePreflight {
    /// The exact bytes can be merged deterministically.  `result` is the
    /// proposed immutable content, not a committed revision.
    Safe {
        result: DeterministicMergeResult,
        explanation: &'static str,
    },
    /// The conservative codecs could not prove that combining the revisions
    /// preserves both people's work.  Nothing is written in this case.
    NeedsReview {
        error: DeterministicMergeError,
        explanation: &'static str,
    },
}

impl MergePreflight {
    /// True only when it is safe to offer a deliberate "Apply merge" action.
    pub fn is_safe(&self) -> bool {
        matches!(self, Self::Safe { .. })
    }

    /// Short person-facing rationale suitable for a conflict sheet.
    pub const fn explanation(&self) -> &'static str {
        match self {
            Self::Safe { explanation, .. } | Self::NeedsReview { explanation, .. } => explanation,
        }
    }
}

/// Inspect a proposed merge without mutating any state.
///
/// The classification shares the exact codec implementation used at commit
/// time, rather than attempting a second, looser UI heuristic.  This makes a
/// merge safe or review-required independent of arrival order.
pub fn preflight_exact(
    first: ExactMergeInput,
    second: ExactMergeInput,
    base: Option<ExactMergeBase>,
) -> MergePreflight {
    match merge_exact(first, second, base) {
        Ok(result) => {
            let explanation = match result.codec {
                DeterministicMergeCodec::QdocAutomergeV1 => {
                    "Both document changes are preserved by the collaborative document format."
                }
                DeterministicMergeCodec::PlainUtf8ThreeWayV1 => {
                    "The text changes affect separate edges of the shared base."
                }
            };
            MergePreflight::Safe {
                result,
                explanation,
            }
        }
        Err(error) => {
            let explanation = match error {
                DeterministicMergeError::OverlappingTextEdits => {
                    "Both revisions changed the same text region. Review the diff before choosing a version."
                }
                DeterministicMergeError::UnsupportedContent
                | DeterministicMergeError::MalformedQdoc => {
                    "This file format cannot be merged safely here. Keep both versions or choose one."
                }
                DeterministicMergeError::NoCommonBase
                | DeterministicMergeError::AmbiguousCommonBase
                | DeterministicMergeError::IncompleteVersionGraph
                | DeterministicMergeError::InvalidBase => {
                    "The shared starting version is not unambiguous, so this merge needs review."
                }
                DeterministicMergeError::InputTooLarge
                | DeterministicMergeError::OutputTooLarge => {
                    "This merge exceeds the safe preview size. Review the versions before resolving it."
                }
                DeterministicMergeError::DuplicateVersion => {
                    "The selected revisions are not two distinct versions. Refresh the conflict and try again."
                }
            };
            MergePreflight::NeedsReview { error, explanation }
        }
    }
}

/// Select the single causally-nearest common ancestor of two conflict
/// members. Ambiguous criss-cross histories and incomplete graphs are refused;
/// choosing an arbitrary base would make a text merge unsafe.
///
/// # Errors
///
/// Returns an error for duplicate or unknown members, incomplete ancestry, no
/// common ancestor, or more than one causally maximal common ancestor.
pub fn exact_common_base(
    versions: &[FileVersionV1],
    first: FileVersionId,
    second: FileVersionId,
) -> Result<FileVersionId, DeterministicMergeError> {
    if first == second {
        return Err(DeterministicMergeError::DuplicateVersion);
    }
    let graph = versions
        .iter()
        .map(|version| (version.id, version.parents.as_slice()))
        .collect::<BTreeMap<_, _>>();
    if !graph.contains_key(&first) || !graph.contains_key(&second) {
        return Err(DeterministicMergeError::IncompleteVersionGraph);
    }
    let ancestors = |start| -> Result<BTreeSet<FileVersionId>, DeterministicMergeError> {
        let mut found = BTreeSet::new();
        let mut pending = vec![start];
        while let Some(id) = pending.pop() {
            if !found.insert(id) {
                continue;
            }
            let parents = graph
                .get(&id)
                .ok_or(DeterministicMergeError::IncompleteVersionGraph)?;
            pending.extend(parents.iter().copied());
            if found.len() > versions.len() {
                return Err(DeterministicMergeError::IncompleteVersionGraph);
            }
        }
        Ok(found)
    };
    let left = ancestors(first)?;
    let right = ancestors(second)?;
    let common = left.intersection(&right).copied().collect::<BTreeSet<_>>();
    let mut maximal = common
        .iter()
        .copied()
        .filter(|candidate| {
            !common.iter().copied().any(|other| {
                other != *candidate
                    && ancestors(other)
                        .is_ok_and(|other_ancestors| other_ancestors.contains(candidate))
            })
        })
        .collect::<Vec<_>>();
    maximal.sort_unstable();
    match maximal.as_slice() {
        [base] => Ok(*base),
        [] => Err(DeterministicMergeError::NoCommonBase),
        _ => Err(DeterministicMergeError::AmbiguousCommonBase),
    }
}

/// Merge two exact immutable members. The choice is content-driven and
/// fail-closed: canonical qdoc Automerge documents use their change graph;
/// ordinary UTF-8 is accepted only for a provably non-overlapping conservative
/// three-way case against an exact common ancestor.
///
/// # Errors
///
/// Returns an error for duplicate versions, oversized or malformed content,
/// an invalid/missing base, or edits that the deterministic codecs cannot merge.
pub fn merge_exact(
    mut first: ExactMergeInput,
    mut second: ExactMergeInput,
    base: Option<ExactMergeBase>,
) -> Result<DeterministicMergeResult, DeterministicMergeError> {
    if first.version_id == second.version_id {
        return Err(DeterministicMergeError::DuplicateVersion);
    }
    if first.bytes.len() > MAX_MERGE_INPUT_BYTES
        || second.bytes.len() > MAX_MERGE_INPUT_BYTES
        || base
            .as_ref()
            .is_some_and(|value| value.bytes.len() > MAX_MERGE_INPUT_BYTES)
    {
        return Err(DeterministicMergeError::InputTooLarge);
    }
    if second.version_id < first.version_id {
        std::mem::swap(&mut first, &mut second);
    }

    if let Ok(bytes) = merge_qdoc(&first.bytes, &second.bytes) {
        return Ok(DeterministicMergeResult {
            codec: DeterministicMergeCodec::QdocAutomergeV1,
            base_version: None,
            bytes,
        });
    }

    let base = base.ok_or(DeterministicMergeError::UnsupportedContent)?;
    if base.version_id == first.version_id || base.version_id == second.version_id {
        return Err(DeterministicMergeError::InvalidBase);
    }
    let base_text = std::str::from_utf8(&base.bytes)
        .map_err(|_| DeterministicMergeError::UnsupportedContent)?;
    let first_text = std::str::from_utf8(&first.bytes)
        .map_err(|_| DeterministicMergeError::UnsupportedContent)?;
    let second_text = std::str::from_utf8(&second.bytes)
        .map_err(|_| DeterministicMergeError::UnsupportedContent)?;
    let merged = merge_plain_utf8(base_text, first_text, second_text)
        .ok_or(DeterministicMergeError::OverlappingTextEdits)?;
    Ok(DeterministicMergeResult {
        codec: DeterministicMergeCodec::PlainUtf8ThreeWayV1,
        base_version: Some(base.version_id),
        bytes: merged.into_bytes(),
    })
}

fn merge_qdoc(first: &[u8], second: &[u8]) -> Result<Vec<u8>, DeterministicMergeError> {
    // Loading through qdoc first rejects arbitrary Automerge documents that do
    // not implement the canonical document schema.
    tilo_doc::Document::load(first).map_err(|_| DeterministicMergeError::UnsupportedContent)?;
    tilo_doc::Document::load(second).map_err(|_| DeterministicMergeError::UnsupportedContent)?;
    let mut left = Automerge::load(first).map_err(|_| DeterministicMergeError::MalformedQdoc)?;
    let mut right = Automerge::load(second).map_err(|_| DeterministicMergeError::MalformedQdoc)?;
    left.merge(&mut right)
        .map_err(|_| DeterministicMergeError::MalformedQdoc)?;
    let bytes = left.save();
    if bytes.len() > MAX_MERGE_INPUT_BYTES {
        return Err(DeterministicMergeError::OutputTooLarge);
    }
    tilo_doc::Document::load(&bytes).map_err(|_| DeterministicMergeError::MalformedQdoc)?;
    Ok(bytes)
}

/// Deliberately small safe subset of three-way text merge. Besides an
/// unchanged side, only a pure prepend on one side plus a pure append on the
/// other is accepted. Concurrent edits in the same region are refused rather
/// than guessed or decorated with conflict markers.
fn merge_plain_utf8(base: &str, first: &str, second: &str) -> Option<String> {
    if first == second {
        return Some(first.to_string());
    }
    if first == base {
        return Some(second.to_string());
    }
    if second == base {
        return Some(first.to_string());
    }

    let first_prefix = first.strip_suffix(base).filter(|value| !value.is_empty());
    let first_suffix = first.strip_prefix(base).filter(|value| !value.is_empty());
    let second_prefix = second.strip_suffix(base).filter(|value| !value.is_empty());
    let second_suffix = second.strip_prefix(base).filter(|value| !value.is_empty());
    match (first_prefix, first_suffix, second_prefix, second_suffix) {
        (Some(prefix), None, None, Some(suffix)) | (None, Some(suffix), Some(prefix), None) => {
            Some(format!("{prefix}{base}{suffix}"))
        }
        _ => None,
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum DeterministicMergeError {
    #[error("merge members must be distinct versions")]
    DuplicateVersion,
    #[error("merge input exceeds its byte bound")]
    InputTooLarge,
    #[error("merged output exceeds its byte bound")]
    OutputTooLarge,
    #[error("merge base must be distinct from both conflict members")]
    InvalidBase,
    #[error("content has no supported deterministic merge codec")]
    UnsupportedContent,
    #[error("qdoc content is malformed")]
    MalformedQdoc,
    #[error("plain-text edits overlap")]
    OverlappingTextEdits,
    #[error("version graph is incomplete")]
    IncompleteVersionGraph,
    #[error("conflict members have no common base")]
    NoCommonBase,
    #[error("conflict members have multiple incomparable common bases")]
    AmbiguousCommonBase,
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::{ContentHash, FileActor, FileProvenance, FileRecordId};
    use tilo_doc::Document;

    fn version(value: u128) -> FileVersionId {
        FileVersionId(uuid::Uuid::from_u128(value))
    }

    fn graph_version(id: u128, parents: &[u128]) -> FileVersionV1 {
        FileVersionV1 {
            id: version(id),
            record_id: FileRecordId(uuid::Uuid::from_u128(99)),
            parents: parents.iter().copied().map(version).collect(),
            actor: FileActor {
                person: collab_types::Ed25519PublicKey([1; 32]),
                device: collab_types::Ed25519PublicKey([2; 32]),
            },
            timestamp_ms: u64::try_from(id).unwrap(),
            content_hash: Some(ContentHash([u8::try_from(id).unwrap(); 32])),
            metadata_hash: ContentHash([0; 32]),
            provenance: FileProvenance::Edit,
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn plain_text_merges_only_disjoint_edits_from_an_exact_common_base() {
        // Phase `common_base_is_exact_and_criss_cross_is_refused`.
        {
            let versions = vec![
                graph_version(1, &[]),
                graph_version(2, &[1]),
                graph_version(3, &[1]),
                graph_version(4, &[2]),
                graph_version(5, &[3]),
            ];
            assert_eq!(
                exact_common_base(&versions, version(4), version(5)),
                Ok(version(1))
            );

            let criss_cross = vec![
                graph_version(1, &[]),
                graph_version(2, &[1]),
                graph_version(3, &[1]),
                graph_version(4, &[2, 3]),
                graph_version(5, &[2, 3]),
            ];
            assert_eq!(
                exact_common_base(&criss_cross, version(4), version(5)),
                Err(DeterministicMergeError::AmbiguousCommonBase)
            );
        }
        // Phase `plain_text_merges_only_disjoint_prepend_and_append`.
        {
            let result = merge_exact(
                ExactMergeInput {
                    version_id: version(2),
                    bytes: b"base\ntail\n".to_vec(),
                },
                ExactMergeInput {
                    version_id: version(3),
                    bytes: b"head\nbase\n".to_vec(),
                },
                Some(ExactMergeBase {
                    version_id: version(1),
                    bytes: b"base\n".to_vec(),
                }),
            )
            .unwrap();
            assert_eq!(result.codec, DeterministicMergeCodec::PlainUtf8ThreeWayV1);
            assert_eq!(result.bytes, b"head\nbase\ntail\n");
        }
        // Phase `plain_text_preflight_is_safe_and_order_independent_for_arriving_edits`.
        {
            let input = |first_id: u128, first: &[u8], second_id: u128, second: &[u8]| {
                preflight_exact(
                    ExactMergeInput {
                        version_id: version(first_id),
                        bytes: first.to_vec(),
                    },
                    ExactMergeInput {
                        version_id: version(second_id),
                        bytes: second.to_vec(),
                    },
                    Some(ExactMergeBase {
                        version_id: version(1),
                        bytes: b"base\n".to_vec(),
                    }),
                )
            };
            let first_arrival = input(2, b"base\ntail\n", 3, b"head\nbase\n");
            let second_arrival = input(3, b"head\nbase\n", 2, b"base\ntail\n");

            assert_eq!(first_arrival, second_arrival);
            let MergePreflight::Safe {
                result,
                explanation,
            } = first_arrival
            else {
                panic!("disjoint text edits should produce a safe preflight");
            };
            assert_eq!(result.bytes, b"head\nbase\ntail\n");
            assert!(explanation.contains("separate"));
        }
        // Phase `overlapping_text_arrival_orders_need_review_without_a_result`.
        {
            let input = |first_id: u128, first: &[u8], second_id: u128, second: &[u8]| {
                preflight_exact(
                    ExactMergeInput {
                        version_id: version(first_id),
                        bytes: first.to_vec(),
                    },
                    ExactMergeInput {
                        version_id: version(second_id),
                        bytes: second.to_vec(),
                    },
                    Some(ExactMergeBase {
                        version_id: version(1),
                        bytes: b"agenda\n".to_vec(),
                    }),
                )
            };
            let first_arrival = input(2, b"agenda: Ada\n", 3, b"agenda: Lin\n");
            let second_arrival = input(3, b"agenda: Lin\n", 2, b"agenda: Ada\n");

            assert_eq!(first_arrival, second_arrival);
            assert!(!first_arrival.is_safe());
            assert!(first_arrival.explanation().contains("same text region"));
            assert!(matches!(
                first_arrival,
                MergePreflight::NeedsReview {
                    error: DeterministicMergeError::OverlappingTextEdits,
                    ..
                }
            ));
        }
        // Phase `overlapping_or_invalid_utf8_content_is_refused`.
        {
            let overlapping = merge_exact(
                ExactMergeInput {
                    version_id: version(2),
                    bytes: b"first".to_vec(),
                },
                ExactMergeInput {
                    version_id: version(3),
                    bytes: b"second".to_vec(),
                },
                Some(ExactMergeBase {
                    version_id: version(1),
                    bytes: b"base".to_vec(),
                }),
            );
            assert_eq!(
                overlapping,
                Err(DeterministicMergeError::OverlappingTextEdits)
            );

            let binary = merge_exact(
                ExactMergeInput {
                    version_id: version(2),
                    bytes: vec![0xff],
                },
                ExactMergeInput {
                    version_id: version(3),
                    bytes: vec![0xfe],
                },
                Some(ExactMergeBase {
                    version_id: version(1),
                    bytes: Vec::new(),
                }),
            );
            assert_eq!(binary, Err(DeterministicMergeError::UnsupportedContent));
        }
    }

    #[test]
    fn qdoc_merges_are_order_independent_and_deterministic() {
        // Phase `qdoc_merge_is_input_order_independent_and_valid`.
        {
            let mut base = Document::new();
            let block = base.insert_block(None, "paragraph");
            base.splice_text(&block, 0, 0, "base");
            let mut left = base.fork();
            let mut right = base.fork();
            left.splice_text(&block, 0, 0, "L");
            right.splice_text(&block, 4, 0, "R");
            let left = left.save();
            let right = right.save();

            let merge = |a: (FileVersionId, Vec<u8>), b: (FileVersionId, Vec<u8>)| {
                merge_exact(
                    ExactMergeInput {
                        version_id: a.0,
                        bytes: a.1,
                    },
                    ExactMergeInput {
                        version_id: b.0,
                        bytes: b.1,
                    },
                    None,
                )
                .unwrap()
            };
            let forward = merge((version(2), left.clone()), (version(3), right.clone()));
            let reverse = merge((version(3), right), (version(2), left));
            assert_eq!(forward, reverse);
            assert_eq!(forward.codec, DeterministicMergeCodec::QdocAutomergeV1);
            Document::load(&forward.bytes).unwrap();

            assert_eq!(
                merge_exact(
                    ExactMergeInput {
                        version_id: version(4),
                        bytes: b"not a qdoc".to_vec(),
                    },
                    ExactMergeInput {
                        version_id: version(5),
                        bytes: b"also not a qdoc".to_vec(),
                    },
                    None,
                ),
                Err(DeterministicMergeError::UnsupportedContent)
            );
        }
        // Phase `identical_retry_is_deterministic`.
        {
            let input = || {
                (
                    ExactMergeInput {
                        version_id: version(2),
                        bytes: b"base+".to_vec(),
                    },
                    ExactMergeInput {
                        version_id: version(3),
                        bytes: b"-base".to_vec(),
                    },
                    Some(ExactMergeBase {
                        version_id: version(1),
                        bytes: b"base".to_vec(),
                    }),
                )
            };
            let (a, b, base) = input();
            let first = merge_exact(a, b, base).unwrap();
            let (a, b, base) = input();
            assert_eq!(first, merge_exact(a, b, base).unwrap());
        }
    }
}
