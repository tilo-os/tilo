# Online accounts + consent

External **online accounts** (email, CalDAV, WebDAV, …) are distinct from the
qOS cryptographic **identity**. They are contributed by apps, stored once, gated
by per-app consent, and managed both in Settings and in the owning app. This is
also qOS's first real **per-app permission-prompt** surface.

## One contribution model (shared with search)

Apps extend the system through a single **contribution registry** — the same
mechanism `plan/search.md` uses for searchable **resources**. An app contributes
typed things:

- **Resource types** (searchable) — see `plan/search.md`.
- **Account types** — described here.
- (Later: actions, file-type handlers.)

A contribution is declared in `qos-app.toml` (loaded without running the app) and
registered in-process at startup (for the running app's own use). The registry
mirrors `apps/permissions/src/registry.rs` (`PermissionRegistry`): first-seen
order, no hardcoding, `register`/`get`/`in_scope`. Account types are a sibling of
resource types in this registry, not a separate system.

## Account model — a shared crate

Today there is **no shared account store**: mail, calendar, and settings each
re-declare the same structs and only agree on a JSON path (and calendar's shape
doesn't even match settings' — a live bug). Fix that with one crate,
`crates/system/accounts`:

- `AccountTypeId` (e.g. `"email"`, `"caldav"`), `AccountId`.
- `Account { id, type_id, display_name, config: JsonValue, enabled, tint, owner }`
  — `config` is type-specific, opaque to the store, (de)serialized by the owning
  type. **No secret fields here.**
- `AccountStore` — the canonical multi-account store; one persistence location
  (`$XDG_CONFIG_HOME/qos/accounts/…`), replacing the three divergent copies.
  `list(type)/get/upsert/remove/set_active`.
- Mail and calendar migrate onto this crate; the calendar↔settings format bug is
  resolved by construction.

### Secret vault (the missing piece)

Every credential today is `#[serde(skip)]` and memory-only — which is exactly why
mail can't persist or log in. The crate provides a **real encrypted vault**,
reusing totp's proven pattern (XChaCha20-Poly1305 at rest), keyed by `AccountId`,
with the vault key sealed by the OS (the TPM-sealed device key). Credentials are
written to the vault, never to the JSON store, and released only after consent.

## Account type = a registered descriptor + shared setup logic

An app registers an `AccountType`:

- **descriptor**: `id`, label, icon (`ui::icons`), the connection **field schema**
  (name/type/placeholder per field), the **credential kind** (password / OAuth),
  and which capabilities/services an account of this type lights up.
- **setup logic** (the crucial part): provider **routing** (domain → provider),
  **autoconfig** (autofill server settings), **OAuth** flow, and
  **validate-before-save** (probe the connection; persist only on success). This
  lives in the type's shared piece so **Settings and the app run the identical
  flow** — no logic siloed in one app's UI.

The setup **UI** is a shared, schema-driven form (rendered from the field schema)
plus the type's setup logic; both Settings and the owning app host it. The app may
still add bespoke UI on top, but the canonical add/edit path is shared.

## Consent — extend the compositor grant path

qOS already has the right substrate: the compositor portal
(`shell/src/portal/mod.rs`) issues typed capability grants
(`shell/src/shell/state.rs`: `Capability`, `GrantScope::{Once,Session,Always}`,
per-workspace/per-app `Workspace.grants`, CRDT-synced, owner-gated `Always`) and
renders an inline `ui/src/widget/permission_prompt.rs`. Extend it:

- New `Capability::AccountAccess { type_id, account_id: Option<AccountId>, scope }`.
- When an app requests accounts of a type (or a specific account), the store asks
  the compositor to `resolve`; if not already granted, the user sees the prompt
  ("Mail wants access to your email accounts — Allow once / session / always /
  deny"). Secrets release only on Allow.
- Grants are per-app, Once/Session/Always, revocable in Settings (which reads the
  grant store).

This is **not** the `apps/permissions` RBAC system — that's workspace/collab roles
(members, contexts), a different concern. Accounts use the portal capability path.

## Settings hub — registry-driven

Settings "External Accounts" (`apps/settings/src/panes/accounts.rs`) stops
hardcoding `AccountKind` and instead renders the **registered** account types: per
type, list accounts, add (shared schema form + the type's setup logic), edit,
remove. A second section lists **which apps hold which account grants** and lets
the user revoke them (reading the compositor grant store).

## Mail = the reference consumer (subsumes mail round-2)

Mail registers the `email` account type. Its shared setup logic delivers the
round-2 fixes for everyone: smart routing (outlook.de → Microsoft OAuth
**auto-selected**), working Google/Microsoft OAuth (PKCE; a baked qOS client-id,
like Thunderbird ships — surfaced clearly if unset), autoconfig for real domains,
and **real validate-before-save** (probe IMAP+SMTP / acquire the token before
persisting; block + explain on failure). Status UX: spinner for in-progress,
checkmark only on success, a real error toast with the actual reason; readable
sync status. Multiple accounts already work. Add-account is identical in-app and
in Settings because both run the type's shared flow.

## Phasing

1. **`crates/system/accounts`** — model + `AccountStore` + secret vault; migrate
   mail + calendar onto it; fix the calendar format bug. (No behaviour change yet.)
2. **Account-type registry** — the contribution descriptor + `qos-app.toml`
   `[provides.account]` + in-process registration, sharing the search-resource
   registry. Register `email` (mail) + `caldav` (calendar).
3. **Consent** — `Capability::AccountAccess` + resolve path + prompt reuse +
   revoke-in-Settings. Gate secret release on consent.
4. **Settings hub** — registry-driven add/edit/remove + the grants view.
5. **Mail reference** — the shared `email` setup logic (routing/OAuth/validate/
   status), consumed by both Settings and the app.

## Open questions

- **Baked OAuth client-ids**: registering qOS apps with Google/Microsoft is a
  one-time human step (dev consoles); until done, OAuth needs a user-supplied
  client-id. Ship with a clear "sign-in unavailable until configured" state.
- **Cross-workspace accounts**: grants are per-workspace today; is an account
  visible in every workspace, with per-workspace *consent*? (Proposed: yes —
  accounts are user-global, consent is per-app/per-workspace.)
- **Vault unlock**: at login (session key) vs. per-access. Proposed: unlocked for
  the session by the OS, like the rest of the user's at-rest state.
