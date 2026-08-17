# qpkg sandbox + privileged-capability audit

This is the answer to two questions:

1. **Can every qOS app ship as a sandboxed `.qpkg`?** — yes; all 45 app
   manifests parse and validate, and packing is proven end to end.
2. **What was missing for a privileged app (a firewall changing rules) to
   work inside the sandbox, and what has been done about it?** — the missing
   piece was a *consented, brokered* path for privileged operations. It now
   exists, with firewall wired as the exemplar and the other four privileged
   apps declaring their capabilities.

Everything below is checked by tests, not asserted by prose. The fleet
manifest guard is `crates/packaging/pkg/tests/fleet_manifests.rs`; the broker
decision path is `crates/system/qos-privileged-broker`; the firewall wiring is
`apps/firewall/src/backend.rs`; the compositor consent seam is
`shell/src/sandbox/privileged_broker.rs`.

## What was missing

A qOS app runs in a per-workspace bubblewrap sandbox with **no route to the
kernel's privileged surface**: no `nft`, no block-device node, no
`CAP_NET_ADMIN`. That is the security boundary and it is not weakened. But a
firewall front-end must change firewall rules, a disk tool must write a device,
a VPN app must bring up a tunnel. Before this work there was no supported way
to do any of that:

- **No vocabulary.** The `.qpkg` manifest could declare filesystem, device and
  network needs, but had no way to say "this app performs a privileged system
  operation." The privileged apps' manifests documented "consented
  capabilities" and a "privileged helper" in prose that **did not exist**.
- **No broker.** There was no trusted helper to perform a privileged operation
  on the app's behalf. Firewall's own backend shelled out to `firewall-cmd`
  directly, gated behind a `QOS_FIREWALL_APPLY=1` env flag — which only works
  *outside* the sandbox, because inside it `firewall-cmd` is not reachable.
  That is the honest shape of the gap: the feature worked on a dev host and
  could not work in the product.
- **No consent.** There was therefore no prompt, and no per-workspace grant,
  for "let this app change firewall rules."

## What was fixed

The fix extends the existing capability/sandbox/broker model rather than
bolting on a new one. Deny-by-default at every layer; every privileged
operation is **declared, brokered, consented, and validated**.

### 1. A typed capability vocabulary in the manifest

`crates/packaging/pkg/src/capabilities.rs` adds a `[capabilities]` block so a
package declares exactly the privileged operations it needs:

```toml
[capabilities]
[[capabilities.privileged]]
capability = "firewall-admin"
reason     = "apply the firewall rules the user edits in this app"
```

The vocabulary is a **closed enum** — `firewall-admin`, `block-device`,
`raw-device-write`, `container-runtime`, `vpn-tun`, `packet-capture`. An
unknown capability name fails TOML decode rather than installing as a
silently-ignored no-op, so deny-by-default begins at the parser. A declared
capability maps to one `Scope::Device("privileged:<name>")`, the single bridge
to the runtime capability model — the same `Scope`, `CapOverride` and
`decide_capability` the compositor and filesystem already use. **No second
permission vocabulary.**

The block is a *ceiling*, not a grant. Validation rejects a capability declared
twice; parse + validate run on the real `qos-pkg pack` path.

### 2. A trusted privileged broker

`crates/system/qos-privileged-broker` is a small, auditable helper that runs
**outside** every app sandbox (the compositor hosts it). The app sends a typed
request over a Unix socket the launcher plumbs in — it never touches `nft` /
`firewall-cmd` / the device. Every request passes three gates, deny-by-default
at each:

1. **Declared** — the app's manifest must declare the capability the request
   needs (the install-time ceiling). Otherwise `DenyReason::Undeclared`.
2. **Well-formed** — a shape *allowlist*, not a filter: the firewall applier
   runs **only** `firewall-cmd`, so a plan line naming any other binary is
   `DenyReason::Malformed` and never reaches a shell. A consented app still
   cannot smuggle an arbitrary command.
3. **Consented** — the user must have granted the capability in the workspace,
   evaluated by the same `decide_capability` every other check uses.
   Otherwise `DenyReason::NoConsent`, and **no action**.

Only after all three does the broker apply, through a pluggable applier so the
real `firewall-cmd` boundary is mocked in tests while the authorization path
stays real. The other five capabilities have no request variant yet, so an app
cannot send an operation the broker has no code to perform.

### 3. Consent reuses the compositor's grant flow

`shell/src/shell/state.rs` adds `Capability::Privileged`, so a privileged grant
is stored, prompted, shown in the Permissions tab, persisted and revoked
exactly like a camera or network grant — as a `CapOverride` on
`Scope::Device("privileged:<name>")` with `Verb::Admin`. Granting it adds
**no** bind mount (it falls through `apply_grants` to the no-bind arm): the
broker does the work, gated on the consent. `shell/src/sandbox/privileged_broker.rs`
supplies the broker's consent gate straight from that list, so a grant the
Permissions tab shows and a grant the broker honours cannot drift apart.

The standing-grant path is wired and tested. Raising the *prompt* when no
standing grant exists reuses the existing async prompt pump
(`PromptQueueHandle` enqueuing a `Capability::Privileged`, `insert` on Allow) —
that enqueue-on-miss in the broker accept loop is the one remaining
integration step; until it lands, a privileged op succeeds only after the user
has granted the capability, never without consent.

### 4. Firewall, end to end

`apps/firewall/qos-app.toml` declares `firewall-admin` and depends on the
`privbroker` service. `apps/firewall/src/backend.rs` no longer runs
`firewall-cmd`: it computes its plan and sends it to the broker, which applies
after checking declaration + consent. Live vs dry-run is now decided by whether
a broker is reachable, not an env flag — a sandboxed app with no broker to act
for it applies nothing, the honest posture. The view code is untouched (the
`Backend` API is unchanged).

## Proof

| Claim | Where |
|---|---|
| Manifest parses the new `[capabilities]` block; unknown capability is refused; duplicate is rejected | `pkg/src/capabilities.rs`, `pkg/src/validate.rs` tests |
| Every app manifest parses + validates; the 5 privileged apps declare their capability | `pkg/tests/fleet_manifests.rs` |
| Broker denies an undeclared capability before consent | `qos-privileged-broker` `undeclared_capability_is_denied_before_consent` |
| Broker applies a declared + consented firewall plan via the real `decide_capability` path | `declared_and_consented_firewall_rule_applies_via_the_real_path` |
| Consent-denied ⇒ no action | `consent_denied_means_no_action` |
| A plan naming another binary is refused before exec | `a_plan_naming_another_binary_is_malformed_and_never_runs` |
| Firewall sends its plan through the broker over the real wire; a denial changes nothing | `firewall::backend` `apply_sends_the_plan_through_the_broker`, `apply_surfaces_a_broker_denial_and_changes_nothing` |
| A workspace grant made through the ordinary grant flow is exactly what the broker reads as consent | `shell::sandbox::privileged_broker` tests, `state` `privileged_capability_round_trips_through_the_grant_flow` |
| A real, signed, sandboxed firewall `.qpkg` packs and verifies with the capability intact | `qos-pkg pack apps/firewall/qos-app.toml` → `verify` ok; `[capabilities]` present in the packed manifest |

## Per-app gap table

**Packable** = manifest decodes and passes `validate_manifest` (proven for all
by `fleet_manifests.rs`). **Sandbox** = the egress profile (`[sandbox.net]
mode`); every app is `landlock = "strict"`, `seccomp = "qos-default"`, and
declares no `[sandbox.fs]` (retired — file access is a runtime grant).
**Priv cap** = privileged broker capability the app needs; **Declared** = it is
in the manifest now.

### Privileged system apps

| App | Packable | Sandbox | Priv cap needed | Declared | Status / gap |
|---|---|---|---|---|---|
| firewall | yes | net off | `firewall-admin` | ✅ | **Complete** — broker applier live, wired end to end, real `.qpkg` packed |
| disks | yes | net off | `block-device` | ✅ | Declared; broker applier staged (mount/format via udisks) |
| imager | yes | net off | `raw-device-write` | ✅ | Declared; broker applier staged (image → `/dev/sdX`) |
| containers | yes | net off | `container-runtime` | ✅ | Declared; broker applier staged (rootless podman) |
| vpn | yes | net off | `vpn-tun` | ✅ | Declared; broker applier staged (TUN + routes) |

### Apps that exec host tools (not one of the six broker capabilities)

These are packable and sandboxed today; their host-tool access is a **consented
FS read+exec grant** under the existing capability model, not a privileged
broker op. `settings` is the one candidate for a future broader `system-admin`
broker capability.

| App | Packable | Sandbox | Needs | Gap |
|---|---|---|---|---|
| settings | yes | net off | execs systemctl/localectl/bootc; nmcli for network | Broader than the six; a `system-admin` broker capability is a documented follow-up. Runs today via consented exec grants |
| logs | yes | net off | execs `journalctl -o json` (read-only) | Read-only; consented exec grant, no mutation — no broker capability required |
| git_client | yes | net host | read+exec host bin dirs (credential helpers), `~/.ssh` | Consented FS grant; no broker capability |
| code_editor | yes | net off | read+exec host bin dirs (`$SHELL`) | Consented FS grant; no broker capability |
| terminal | yes | net off | binds host bin dir to exec `$SHELL` (ush/dsh runtime) | Own runtime model; no broker capability |

### Ordinary sandboxed apps

All packable, all `landlock = strict`, no privileged capability. Egress
profile noted:

| App | Sandbox | | App | Sandbox |
|---|---|---|---|---|
| api_tester | net host | | monitor | net off |
| backups | net off | | music | net host |
| browser | net host | | notes | net off |
| calculator | net off | | osk (service) | net off |
| calendar | net host | | pdf_viewer | net off |
| camera | net off | | remote | net off (baseline) |
| chat | net host | | sheets | net off |
| contacts | net off | | slides | net off |
| db_client | net off | | snippets | net off |
| diffmerge | net off | | software_store | net off |
| ebook | net off | | store | net none |
| feeds | net host | | themes | net off |
| file-explorer | net off | | totp | net off |
| hex_editor | net off | | tube | net host |
| mail | net host | | typst_editor | net off |
| maps | net allowlist | | video_editor | net off |
| | | | video-player | net off |
| | | | weather | net allowlist |
| | | | word-processor | net off |

## What is deferred (and why it is safe to defer)

- **Broker appliers for disks / imager / containers / vpn.** The capabilities
  are declared and the broker refuses them (no request variant), so the apps
  are honest: they cannot perform the operation until an applier lands, and
  they cannot reach the device/runtime directly either. Each applier is the
  same shape as `SystemFirewall`: validate the typed op, run the trusted tool.
- **A `system-admin` capability for settings.** systemctl/localectl/bootc is a
  broader surface than any of the six; folding it into one of them would be
  dishonest. It warrants its own capability + applier with a fine-grained
  request type (which unit, which action), which is a design step, not a gap in
  this model.
- **Prompt-on-miss in the broker accept loop.** The consent *decision* and the
  grant *storage* are done and tested; enqueuing the prompt when no standing
  grant exists reuses the existing pump and is the final wiring step. Until
  then, deny-by-default holds: no grant means no action.
