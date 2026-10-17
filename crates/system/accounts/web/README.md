# Browser account vault storage

Account inventory belongs to the authenticated profile's `Dir` capability:
`opfs://<user-public-key>@<host>/tilo/config/accounts/vault/vault.enc`.
Existing encrypted bytes remain authoritative. The account vault format and
its platform-held key (`Tilo` / `account-vault`) are unchanged. Saving reloads,
authenticates, compares edits, merges and seals under the profile's OPFS writer
lock. The synchronous caller must retry pending saves; only an acknowledged
commit permits publishing dependent account metadata. A failed write retains
local edits. Edits made during a pending write are rebased on its acknowledged
snapshot and still require their own commit.

## Unassigned inventory recovery

An unreleased implementation wrote an origin-wide inventory into `tilo-secrets`,
app `tilo.accounts`, key `vault.entries`. That record has **no authenticated
profile provenance**. Opening a profile never adopts or deletes it, even if the
profile's vault file is absent. A warning identifies it when present.

If this record contains credentials that need recovery:

1. Preserve the browser origin's storage, including OPFS and IndexedDB. Do not
   clear site data or remove the vault key. Keep a browser-profile backup before
   attempting recovery.
2. In a trusted recovery session, retrieve the record using
   `SecretStore::for_app("tilo.accounts").get("vault.entries")`. Its encrypted
   backend record is in IndexedDB database `tilo-secrets`, store `items`, compound
   key `["tilo.accounts", "vault.entries"]`. Do not log its plaintext.
3. Independently verify ownership of each credential. Re-enter or explicitly
   import only verified entries through that owner's authenticated profile's
   account broker. Do not infer ownership from whichever profile is open.
4. Confirm the profile vault has committed and survives reopening before any
   deliberate removal of the unassigned recovery record. No automatic deletion
   or migration is performed.

## Regression probe

`node crates/system/accounts/web/run.mjs` (or `bun` in place of `node`) builds
and runs the production Rust
account vault in headless Chrome with real WebCrypto, IndexedDB, OPFS and Web
Locks. It covers two authenticated profile scopes in one origin, legacy encrypted
vaults, save/reload isolation, retained unassigned inventory, concurrent tabs,
edits during acknowledgement, write failure/retry and conflicting edits.
`--no-build` reuses the compiled probe; `WASM_BINDGEN` can name the pinned CLI.
The build is `cargo build --locked -p accounts --example account_storage_probe
--target wasm32-unknown-unknown`, followed by `wasm-bindgen --target web` using
the CLI version pinned by the example's `wasm-bindgen` dependency. Set `CHROME`
to a Chromium executable or install Playwright's Chromium. Missing browser,
WASM target, CLI or compiled probe is an error, never a skipped test.

`t test accounts-web-storage` runs the same production probe in CI, after
WASM Clippy of accounts, secret-vault, tilo-files and tilo-secrets with the
standard browser-only lint exceptions. It uses the normal builder image and
x86_64 CPU software rendering; no GPU, KVM or FUSE is required. The check
downloads Chrome for Testing and passes `--no-sandbox` for its root container;
this tests profile capability isolation in one origin, not Chromium's sandbox.
It builds only the probe, not the desktop or OS image.
