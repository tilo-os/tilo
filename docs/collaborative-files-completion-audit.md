# Collaborative Files — requirement evidence audit

This audit maps every completion requirement in
[`collaborative-files-product.md`](collaborative-files-product.md) to direct
evidence. **Green** means the current checkout has direct implementation and
local test evidence. **Partial** means a lower layer exists but the promised
product workflow is not complete. **External** means the source-side gate
exists but its required release environment has not produced evidence yet.

## Secure record replication and sharing

| Requirement | Status | Direct evidence or missing work |
|---|---|---|
| Stable per-record replication; no unauthorized names, metadata, history, hashes, thumbnails, or changes | Green | `record_replication::tests::wrong_record_is_rejected_and_cannot_disclose_siblings`, `main::tests::shared_snapshot_rejects_cross_record_metadata_and_history`, `main::tests::shared_namespace_refuses_any_extra_visible_node`, and `sync::tests::record_session_guards_manifest_chunks_thumbnails_and_get_many_live` exercise exact scope and mixed-batch refusal. |
| Certified, unrevoked device authorship and record-scoped read/write authorization | Green | `record_replication` signature/certification tests, `authenticated_transport::tests::effective_revocation_rejects_otherwise_valid_peer`, and the real-process reader in `collab_release_matrix` prove the boundary through transport and kernel write denial. |
| Live-reachability authorization for blobs and thumbnails; mixed batches fail closed | Green | `sync::tests::blob_authority_is_scoped_to_live_readable_paths` and `sync::tests::record_session_guards_manifest_chunks_thumbnails_and_get_many_live`. |
| Files Share UI supports named or link subjects, view/edit, expiry, optional one use, copy, revoke, and audit | Green | The localized composer exposes named/link subjects, View/Edit, 24-hour/7-day/30-day expiry, no-expiry where valid, and one-use or explicitly bounded reusable links. It sends governed root/device keys rather than labels, performs service I/O off the UI thread, reports Pending/Delivered honestly, and retains copy, revoke, and audit controls. |
| “Shared with” uses governed human identity, device detail on demand, and fingerprint fallback | Green | Room-governed endpoint names are joined only to root-signed device certificates matching the transport-authenticated Iroh endpoint. `TrustedIdentityProjectionV2` carries the governed human root/name and certified device rows; Files reads V1/V2, shows device labels and on-demand fingerprints/capabilities/issue/expiry detail, and still submits only root keys. `network_announcement_governs_and_republishes_files_projection` proves the actual announcement → governor → live launcher projection path and cross-endpoint refusal. `root_signed_room_revocation_is_additive_and_prevents_rename_resurrection`, `revocation_source_is_reloaded_and_cross_root_rows_fail_closed`, and `running_files_reloads_new_renamed_removed_and_expired_governed_devices` prove additive root-signed revocation plus live removal, rename, new-device and expiry behavior; guest governance edits fail closed. |

## Availability, offline use, and cache lifecycle

| Requirement | Status | Direct evidence or missing work |
|---|---|---|
| Versioned availability with residency, transfer, policy, replicas, and freshness | Green | `availability` schema/validation tests and the live Files/FUSE projection gate in `apps/file-explorer/tests/live_collaborative_mount.rs`. |
| Live Files controls for pin, unpin, evict, and retry; future descendants inherit directory pins | Green | Files multi-selection tests, `availability::tests::directory_pin_applies_to_future_descendants_and_survives_move`, and the live mount gate. Collaborative metadata refresh now preserves stable-record selection and scroll. |
| Persistent pins and safe eviction protecting transfers, history, and shared chunks | Green | `availability` and `eviction` refusal matrices plus `sync::tests::three_live_peers_require_a_fresh_exact_replica_before_eviction_and_rehydrate_after_gc`. When safe eviction refuses for lack of a replica, Files offers a separate modal, explicitly destructive action; merely opening the warning cannot write `evict-destructive`. Retained history remains protected even under that override. |
| Byte-bounded observable cache accounting and owned-root garbage collection | Green | `eviction` accounting tests and `store` owned-tag/backend-GC tests. |
| Bounded, cancellable, ACL/network/power/storage-aware predictive hydration | Green | `smart` policy/budget tests, `fs::predictive_tests::prediction_never_includes_a_sibling_hidden_by_the_acl`, and `smart::tests::open_prediction_hydrates_a_real_sibling_and_drains_accounting`. The shipped monitor now samples NetworkManager connectivity/metering, sysfs AC/battery state and cache filesystem reserve before mount and every 15 seconds, fails closed on unavailable signals, and cancels work through the same policy transition. |

## Metadata, thumbnails, history, and conflicts

| Requirement | Status | Direct evidence or missing work |
|---|---|---|
| Bounded versioned metadata independent of payload bytes | Green | `FileMetadataV1` v2 adds strictly bounded dimensions, duration, page count, codec and container while decoding legacy v1. Extraction scans at most 16 MiB of bytes that are already local, binds facts to the current content hash, replicates them through signed metadata, and exposes a bounded `user.qos.metadata` projection; Files renders them without opening payload content. Protocol migration, extraction, replication, projection and localized Properties tests are green. |
| No UI-thread network/decode work; explicit thumbnail placeholder/failure/retry; content-hash cache keys | Green | Files thumbnail view tests, including `a_projected_file_is_never_hydrated_just_to_preview_it` and `workspace_thumbnail_keys_follow_content_across_renames`. |
| Durable replicated versions, actors, parents, provenance, restore-as-new-version, reachable-history retention | Green | `history` graph/restart tests, `record_materializer` replay tests, `fs::history_integration_tests`, and eviction retained-version refusal tests. |
| Atomic signed replacement rename and crash recovery | Green | Namespace transaction tests, transaction journal/coordinator tests, and both-order remote materializer coverage. |
| First-class conflicts and convergent, idempotent, undoable resolution | Green | Conflict/history/record-replication matrices, atomic Keep Both, codec-aware text/qdoc Merge, and the deterministic structural partition matrix. |

## Files application quality

| Requirement | Status | Direct evidence or missing work |
|---|---|---|
| Meaningful multi-selection, keyboard/accessibility equivalents, stable selection/scroll, actionable errors | Green | Files interaction/unit coverage plus the real-mount multi-selection pin/unpin test; AT-SPI runtime smoke has passed locally. |
| Wide/narrow properties, sharing, availability, history, conflicts, honest degraded states, localization | Green | Files layout/semantics tests and exact English/German Fluent parity with ICU formatting. |
| Search and room record index use stable record identity and provenance | Green | The mount emits a bounded ACL-filtered root catalog; `record_catalog_contains_nested_records_but_not_acl_hidden_siblings` proves hidden siblings do not enter metadata. A separate provider caps at 256 records, rejects noncanonical/escaping/oversized projections, uses `record:<UUID>` action identities, and resolves the latest live path on activation. The ordinary Places provider remains unchanged. |

## Required release evidence

| Requirement | Status | Direct evidence or missing work |
|---|---|---|
| Deterministic multi-device partition/reconnect/restart/clock/source-loss harnesses | Green | `fs/tests/collab_release_matrix.rs` and `fs/tests/exact_share_process_fuse.rs`; all use explicit gates/state barriers. The shipped process clock defaults to `SystemTime`; its explicit owner-only release-test file is inode-pinned and revalidated per decision, with malformed/replaced state failing closed. The real daemon test moves backward/forward and across the exact deadline without an expiry sleep and proves receipt timestamps use that clock. |
| Mandatory real-FUSE CI where missing FUSE fails | External | `.forgejo/workflows/fs-fuse.yml` fails closed, but this checkout has no evidence from the actual Forgejo runner yet. |
| Full-stack named/link sharing, attacks, offline conflicts, restore, eviction/source loss, predictive hydration, and Files/live-mount interactions | Green | Named grants use a secretless endpoint-bound envelope, mutually certified device delivery, a bounded durable retry outbox and recipient inbox, and isolated exact-record mounts. Direct tests cover authenticated delivery, wrong-human/bearer refusal, replay, offline retry, and restart. Link lifecycle, attacks, partitions, source loss, prediction and Files/live-mount paths also have direct tests. |
| Security, corruption/migration, accessibility, localization, performance, and installed-OS gates | External | Local corruption/migration, localization, performance, session-lifecycle, and live AT-SPI evidence are green. The shipped greetd entries now use `qos-session`: an incomplete identity opens the existing human acknowledgement ceremony inside a temporary single-client Cage session, a read-only cryptographic verifier binds the personal root to the persisted root/device secrets and signed certificate, and only then does a qOS-owned target activate `graphical-session-pre.target` and `graphical-session.target`. Cancellation, corruption, mismatch, expiry and failed enrollment never start the compositor; exit and signals stop the target. The installed-image release VM uses graphical/virgl mode, drives that real keyboard-accessible first-login ceremony without a test minting bypass, and fails closed unless the qOS compositor publishes a live Wayland socket and the integrity-verified Files package payload exports its accessibility tree. No run of that updated external gate is recorded. Targeted production Linux FS/Files scans have no vulnerability or unsound advisories. A compatible `chacha20` patch update removes that yanked release, and a narrow `genawaiter-proc-macro` compatibility fork removes `proc-macro-error`; strict policy still reports upstream-unmaintained `paste`, Files' `ttf-parser`, and yanked `spin` 0.9/0.10 transitives with no compatible upstream release. The all-workspace `cargo deny` scan is not currently a usable release gate: `sources` passes, while `licenses` rejects unlicensed first-party workspace crates and licenses including AGPL/WTFPL/CDLA, `bans` rejects the repository's unversioned path dependencies, and `advisories` also includes unrelated application/dev stacks. |

## Remaining product work

1. Obtain actual Forgejo `/dev/fuse` and installed bootc VM evidence, including
   the now-mandatory installed-session identity and AT-SPI smoke.
2. Establish a repository-wide licensing decision, version the first-party path
   dependencies (or scope that lint intentionally), and separately migrate the
   unrelated application stacks that keep the all-workspace advisory gate red.
