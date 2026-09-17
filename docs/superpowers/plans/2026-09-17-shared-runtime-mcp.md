# Shared runtime MCP implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Execute sequential implementation tasks with independent reviews.

**Goal:** A single production-runtime MCP endpoint controls framework apps and proves a real square-crop image handoff into Writer.
**Architecture:** Existing runtime_host::Session, production AppActionBroker, app pull directories, invocation lifecycle and shared blob store remain authoritative. Add a platform-neutral JSON-RPC adapter and thin stdio/lifecycle executable.
**Tech Stack:** Rust, serde_json, existing Tokio/broker APIs, Python native proof client.
**Spec:** docs/superpowers/specs/2026-09-17-shared-runtime-mcp.md

## Global Constraints
- YAGNI; maximize shared code; platform code only at edges.
- Reuse Session, AppActionBroker and existing ui::App publication/execution; no second registry, authority system or workflow engine.
- Host fixes actor/workspace; requests cannot choose either or assert trusted broker admission.
- One MCP endpoint, instance-qualified identities, bounded lazy discovery, fresh invocation resolution, unchanged canonical results/content references.
- Accepted is not Succeeded; no automatic mutation replay; confirmations are never fabricated.
- Runtime adapter contains no Photos/Writer-specific behavior.
- Preserve concurrent work; stage only owned files/hunks. No push, merge, GUI activity or broad formatting without controller coordination.
- Existing Unix host is the native validation target; no unsupported Windows claims or speculative remote federation.

### Task 1: Shared broker-backed MCP adapter
**Files:** Create shell/src/runtime_host/mcp.rs and shell/src/runtime_host/mcp_tests.rs; modify only the module export in shell/src/runtime_host.rs. Existing broker API additions only if essential and coordinate first.
**Interfaces:** Consume AppActionBroker::{action_directory_root_page,action_directory_request_queue,statuses,cancel} and queue::{try_query,try_resolve,try_start_resolved}. Produce `pub struct McpServer` with `pub fn new(broker: AppActionBroker, room: String, actor: String) -> Self` and `pub async fn handle(&mut self, request: serde_json::Value) -> Option<serde_json::Value>`; no transport or process spawning here.
- [ ] Write failing tests for initialize/ping/notifications, wrong JSON-RPC/request shapes, tools/resources discovery, exact instance identities, denied/stale invocation and terminal content results. Use actual production broker integration fixtures for authority and routing tests; inspect existing broker directory tests before introducing fixtures.
- [ ] Implement generic tools `qos_list_apps`, `qos_query`, `qos_resolve`, `qos_invoke`, `qos_status`, `qos_cancel`. List/query results retain broker pagination and revision. Query uses canonical budgets capped at 128 results, 64KiB, 512 work, depth16. Tool arguments reject caller actor/workspace and unknown security fields. Bind room/actor from constructor. For status, inspect actor-scoped statuses rather than unscoped status alone.
- [ ] `qos_query` accepts peer_id, revision/cursor, parent_resource and optional needle; returns rows with decoded canonical params and contracts. `qos_resolve`/`qos_invoke` require peer_id/resource/action/revision. `qos_invoke` accepts a JSON arguments object passed through existing typed binding, no confirmation/broker-admitted boolean. Call try_start_resolved atomically; never execute cached descriptors. Preserve invocation id and canonical update/result JSON. Unknown/unavailable/confirmation failures return MCP tool errors.
- [ ] Resources list/read use the same bounded routes; app URI identifies peer instance. Do not flatten every app tree. Scope roots to the host room, recheck live roots and broker authority on read. Do not parse app identity from decorative labels.
- [ ] Negotiate supported MCP protocol versions explicitly; initialize advertises only actual tools/resources capabilities (no listChanged without notifications). JSON-RPC errors and tool failures remain distinct. Notifications get no response. All returned success data corresponds to broker results; no fake acknowledgement.
- [ ] Test malformed arguments, busy/timeout/disconnect handling, stale revisions, same app multiple instances, auxiliary resource identity, denied execution and cancellation ownership. Run focused tests and applicable existing broker tests once; record commands/results. Commit only task-owned changes and self-review.

### Task 2: Native runtime entry point and automatic app publication
**Files:** Create shell/src/bin/qos-runtime-mcp.rs and focused lifecycle tests under shell/tests; update shell/Cargo.toml only if explicit bin declaration is needed, docs/shared-runtime-proof.md and a new docs/runtime-mcp.md. Consume Task1 McpServer.
**Interfaces:** Command `qos-runtime-mcp --profile PATH --apps MANIFEST`. Manifest JSON schema: `{"apps":[{"id":"dev.qos.example","executable":"/absolute/path/app","args":[]}]}`. A zero-app list is allowed, duplicates of app id allowed as distinct instances. Startup config is supplied by the user/client configuration; tool calls cannot select executables.
- [ ] Write failing lifecycle/framing tests proving bounded line parsing, protocol-only stdout, startup validation before children, child cleanup on EOF/failure, profile contention refusal and distinct-instance publication through normal SDK startup.
- [ ] Compose Session::start with McpServer; configure persistent profile identity/blob/config paths before threads using existing runtime proof startup helpers as reference. Launch apps through Session::launch, redirect app stdout away from MCP, retain stderr/logs. No app-specific registration hook or MCP code in individual apps.
- [ ] Serve newline-delimited MCP JSON-RPC over stdin/stdout using a bounded reader (1MiB request cap); handle parse errors with protocol errors, flushing each reply. Handle EOF and failed children honestly. Keep one session for the connection and reap only owned children before dropping services. Existing profile lock prevents a duplicate runtime owning the same profile; report it instead of deleting sockets or locks.
- [ ] Document one MCP client command/configuration, app manifest and automatic publication behavior, scope of local same-user authority, confirmation refusals, lazy discovery and result chaining. Explain that the current Unix host is macOS/Linux and Windows host transport remains unqualified; no client/account changes or installation performed automatically.
- [ ] Run Task1 plus lifecycle tests, legacy Python MCP tests and a smoke initialize/ping with zero apps. Commit only owned changes and self-review.

### Task 3: Real cross-app MCP acceptance proof
**Files:** Create scripts/runtime-mcp-proof.py with protocol tests in scripts/tests/test_runtime_mcp_proof.py; extend docs/runtime-mcp.md with verified evidence. Reuse shell/examples/edited_handoff_proof and existing signed-document reconstruction/verification helpers where possible; factor only necessary shared proof helpers, not production workflow code.
**Interfaces:** Consume built qos-runtime-mcp, Photos and Word Processor binaries plus Task2 manifest. Every crop/export/place-image mutation must be a tools/call request over the external MCP stdio connection, never direct broker/App calls from the proof.
- [ ] Start isolated profile with patterned nonsquare PNG and real app processes. Discover live app instances and actual action descriptors through MCP. Wait for normal publication with a bounded timeout; assert actual child liveness.
- [ ] Crop to square using normalized coordinates computed from the real image dimensions (for 48x32 centered image: x=1/6,y=0,w=2/3,h=1). Invoke crop, wait for terminal result, freshly discover/resolve export-edited-content and invoke. Consume actual ActionOutput content reference; convert through canonical content_hash semantics and invoke freshly resolved Writer place-image. Never use original image hash or invented output.
- [ ] Verify exported image is 32x32 and pixels equal expected centered source crop. Independently verify signed Writer document events or persistent native document evidence reconstruct an image block with the exact exported hash/dimensions. Preserve undo/history boundaries; never patch Writer state in test.
- [ ] Also exercise an unrelated declared operation (e.g. a reversible rotation followed by reset) through identical generic discovery/invoke and verify actual state, plus invalid args/stale instance denial. This is additional coverage, not a hardcoded runtime route.
- [ ] Retain transcript, metadata/binary hashes, logs, input/result images, reconstructed document and explicit failures. Run native proof on macOS after controller GUI coordination, inspect result visually where useful, and document limits. No Windows-native or external ChatGPT/Claude installation claim.
- [ ] Run relevant final suites and independent whole-change review. Fix findings through original implementer; commit owned proof/docs. Completion requires real successful MCP crop→export→Writer proof and no open load-bearing review defects.
