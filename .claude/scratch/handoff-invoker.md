# Handoff: the 14af24e2a0f invoker reds

Worktree: `/srv/rcargo/t3-builder-home/work/tilo/.claude/worktrees/invoker-reds`, branch `invoker-reds`, forked from `8c1c0726ef6`.
The format-version change lands on main separately. Everything below is the WIP commit on `invoker-reds`.

## Written, needs a proof run

1. **Vault writes name their invoker** (`crates/server/tilo-workspace-service/src/workspaces/vault.rs`).
   - `write_vault` and `changing_vault` take `Option<&Acting>`; `VaultAt` carries it and `Workspaces::vault_for` sets it; `VaultOf` forwards it.
   - Where no caller is known the write names the serving device's person (the change's own signer), never nobody.
   - `Replica::as_invoker_acting` (`crates/sdk/tilo-resource/src/replica.rs`) names a full acting without taking authorship.
   - `Reach` (`workspaces/workspace_stores.rs`) carries `acting: Acting`; person settings, models, the vault handler and account logins pass the caller. Other call sites pass `None` (serving device's person).
   - Not proven: no rerun after this change. The two `hosted_settings_from_page` tests still failed with "a change names no invoker" on a **different** writer (see 4).

2. **The session's settings write names the person** (`crates/session/session/src/settings_sync.rs`).
   - `Record` carries `person`; `Record::write` and `offer` commit with `commit_edit_as` naming `PersonId::from_root(person)`.
   - New test: `settings_sync_tests::a_settings_write_names_the_person_that_set_it`. Never run.

3. **Model/property fixtures name an invoker** (`crates/collab/collab-types/src/claim.rs`).
   - `CommitNamed` gained `commit_edit_as(kind, field, invoker: Option<&ActionInvoker>)`; `commit_edit` names the person of a 32-byte document actor (`PersonId::from_root`), else names none as before.
   - Meant to clear the 79 `tilo-peer-state` "a change names no invoker" failures in fixtures. Never rerun; `r58ffa758ed` (before this change) had 79 failures.

4. **Broker carries the caller's signed invocation** (`crates/server/tilo-workspace-service/src/apps/collab_app_broker/invoker.rs`, `invocation/admit.rs`).
   - `host_set_invoker` takes the original request; `signed_by_request` copies `SignedCall::of(request)` and `source_invocation` into the host-set context.
   - This is the **workspace-service** broker path only. The agent-session host (`workspace_actions`) is a different host and does not get it.

## Failing, with cause

- `hosted_settings_from_page::a_person_with_only_a_page_manages_models_mail_and_vault_on_the_host` and `...a_page_following_the_accounts...` (`r0fabdbcfad`, `rb5156060a0`):
  route `app=so.tilo.settings record=derived:tilo.accounts@tab:...`, message `tilo.entry/3` with app/label but `invoker: None`. A `tilo-resource` Replica was built with `.in_app("so.tilo.settings", ...)` and no author/invoker (`replica.rs::invoke` builds an app-only ambient from `ambient.unwrap_or_default().with_app(...)`). Find that writer (the Settings app's derived `tilo.accounts` chain, server-side) and name the caller. This is also the laptop's own Settings record path the coordinator saw.
- `hosted_agent_questions::a_question_taken_back_or_settled...`, `governed_agent`, and the other "agent acts here without its own certified key" refusals:
  `workspace_actions/publish.rs:446 sign_for` → `grants.rs:235 sign_as` signs with the session's `signer`, a person's device cert; `crates/server/tilo-server-protocol/src/invocation.rs:365 agent_signed` wants an agent-grant cert. The host path must sign through `agents/agent_keys.rs` (`AgentKeys::grant`/`sign`, today only reached by `EngineActions::Device` in `agents/engine_actions.rs:96`, not `::Host`).
- Handed agents ("a change made for a caller carries no invocation that caller signed"):
  `crates/agent/agent-session/src/apps.rs:2499` (`replica_access`) sets `invoker.device` while the request carries no signed call. The handed-agent path must carry a signed call (or `invoker.device` must not be set where the signer is the person's device).
- `member_own::a_member_links_their_discord_account_through_the_host`:
  "the author is not granted Undoable ... action:set-sign-in" from `crates/collab/tilo-peer-state/src/state/authority.rs:1361` (`ChangeAuthor::answer` with the bot's acting). Separate grant decision for bot-authored app records; not the invoker rule.
- `governed_agent::an_existing_roster_agent_joins_after_one_approval_without_a_task_runner`:
  retained edit envelope claims 0 where 1 expected (`governed_agent.rs:1026`). Likely the same host-signing change; not investigated.
- `hosted_consent_notification::a_use_on_a_server_asks_the_person...` (second use under a standing "Always"): needs the use path to name its invoker and carry the person-signed invocation. The vault change gives the invoker; the signed invocation is not threaded into `carrying_vault` yet.
- `hosted_agent_terminal`, `meeting_rundown::connect` (2), `told_as::what_breaks_do_not_disturb`, `mail_connect::what_she_types...`: added after the last run; not investigated.

## Runs

- `r58ffa758ed` — `t cargo test -p tilo-peer-state`: 79 failures, all fixture NoInvoker (before the `CommitNamed` change).
- `r0fabdbcfad` — the 10-test agent set with every change above: 9 failures as listed. 37 min.
- `rb5156060a0` — one settings test with a route print: the Settings derived-chain writer above. 12m41.
- `python3 tools/policy/check_format_migrations.py check` and `against origin/main`: green (the format commit).
- No clippy run for the WIP changes; no aarch64 run of anything.

## Uninvestigated

- Whether the `CommitNamed` change clears the 79 (rerun `t test test/tilo-peer-state --test <module>` uncached on both arches).
- The `in_a_carrier` question tests that also fail in `r0fabdbcfad` (4 tests) — not in the coordinator's list.
- `EngineActions::Host` signing through `agent_keys`, and the handed-agent signed call, per the settled design.
