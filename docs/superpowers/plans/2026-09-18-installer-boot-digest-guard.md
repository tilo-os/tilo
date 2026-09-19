# Installer boot-digest guard implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use subagent-driven-development for implementation and independent task review.

**Goal:** Make the existing target UKI promotion guard observe and compare the real kernel/initramfs digest, and refuse any failed or malformed observation before promotion.

**Architecture:** Retain the existing Python PE digest algorithm and installer promotion sequence. Expose the already implemented explicit-path CLI verb, capture each caller result with a checked status, validate its digest shape, then compare. Test the real read-only CLI and the exact caller guard with ordinary nonbootable PE fixtures.

**Tech Stack:** Python standard library, Bash, existing installer source; no dependencies.

**Spec:** `docs/superpowers/specs/2026-09-16-guided-migration-design.md`; confirmed source audit `.superpowers/sdd/2026-09-16-internal-installer-boot-proof/postprotocol-next-source-action-audit.md`.

## Global constraints

- BASE is `47b3b82e087ddb8f938d8ae63166d0d0f368fbca`; preserve all unrelated working changes. No Git/index mutation by the implementer.
- Keep the existing OS, persistent boot default, earlier preflight/authority/composefs/root checks and existing promotion order unchanged. No new staging transition or support claim.
- Internal dual boot remains unavailable, release catalog remains empty, and exporter refusal remains intact. No native/helper/VM/SDK/UI/device/mount/install/boot/service/network execution. No builds of OS images or artifact signing.
- Permitted execution is host unit testing: the Python source CLI's explicit `boot-digest TEMPFILE` read-only path, imported Python parser functions, and an extracted ordinary-file shell guard using private temporary fixtures. Never invoke the installed helper, default `/boot` discovery, complete installer function/backend, provider/receiver, or a native Linux/Windows executable. CLI arity errors may omit/overprovide temporary arguments; they must fail before discovery/access.
- Synthetic fixtures are nonbootable test data. Source/host proof cannot establish native durability, bootc compatibility of an absent artifact, successful installation, or qualification.
- Use production paths and meaningful assertion RED. Do not weaken assertions, ignore failures, raise limits, or duplicate the production comparison in a test-side implementation. Missing extraction markers or an unobserved subject must fail.
- Source ownership is exactly the four files listed below; this plan and task report/evidence are coordinator artifacts. No framework, app, Cargo, package, catalog or lock edits. No subagents and no broad formatter/test sweep.
- Capture each actual command, real exit/timing/raw output, source hashes/snapshots and applicable peer/build inputs before and after. Preserve failed attempts. Do not fabricate retrospective RED metadata.

## Task 1: Restore and enforce the target boot identity comparison

**Files:**
- Modify `os/dev/uki-cmdline.py`.
- Modify `os/usr/libexec/tilo-install`.
- Create `os/tests/test_installer_target_boot_digest.py`.
- Modify `os/tests/installer_target_modes.py` only to keep its existing path/promotion fixture's CLI stub faithful to the distinct `show` and `boot-digest` verbs.

**Interfaces:**
- Existing `boot_digest(data)` remains SHA-256 of the `.linux` virtual bytes followed by the `.initrd` virtual bytes, requiring exactly one nonempty in-bounds section of each kind. Do not rewrite the PE parser or algorithm.
- Existing `main(argv)` admits `boot-digest` with exactly one explicit path; usage documents that explicit path. A successful call prints the lowercase 64-character digest and newline, does not modify input, and does not auto-discover `/boot`.
- `promote_target_uki()` must collect the installed and variant digests into distinct function-local variables. Each failed helper exits through `die` before any `.new` copy/rename. Reject a successful observation that is not exactly one lowercase 64-hex value after shell trailing-newline removal. Only valid identical observations permit the unchanged `install -m 0644` and `mv -f` promotion.
- Do not combine declaration and command substitution into `local digest=$(...)`, which can hide the command status. The shape below is the intended check sequence; retain existing project quoting/format style:

```bash
local installed_boot_digest target_boot_digest
installed_boot_digest="$(/usr/libexec/tilo-uki-cmdline boot-digest "$TARGET_UKI")" \
  || die "cannot read installed UKI boot identity"
target_boot_digest="$(/usr/libexec/tilo-uki-cmdline boot-digest "$TARGET_VARIANT")" \
  || die "cannot read target UKI boot identity"
[[ "$installed_boot_digest" =~ ^[0-9a-f]{64}$ ]] \
  && [[ "$target_boot_digest" =~ ^[0-9a-f]{64}$ ]] \
  || die "UKI boot identity is not a SHA-256 digest"
[ "$installed_boot_digest" = "$target_boot_digest" ] \
  || die "target UKI changes the bootc kernel/initramfs identity"
```

### Step 1: Add real failing tests before changing production

- [ ] Build ordinary bytearray PE fixtures with a real section table, known `.linux`/`.initrd` payloads, differing virtual/raw lengths and padding, and a `.cmdline` section. They are explicitly not bootable. Use a fixed known expected `hashlib.sha256(KERNEL + INITRD).hexdigest()` independently of the production decoder; `KERNEL` and `INITRD` are fixture byte literals.
- [ ] Invoke `[sys.executable, '-B', SOURCE, 'boot-digest', fixture]` with bytecode disabled. Assert exit 0, exact expected stdout and unchanged file bytes. Assert command-line and raw-padding differences preserve the digest; either virtual payload change changes it. Missing/duplicate/zero/virtual-larger-than-raw/out-of-file required sections and absent paths fail without digest output. Arity failures cannot fall through to default UKI discovery.
- [ ] Extract the actual production guard beginning at its unique `# bootc stores SHA256(.linux + .initrd)` comment and ending immediately before the unique following `install -d -m 0755 "$esp/EFI/tilo/pcr-predictions"` statement within `promote_target_uki`. Include the existing ordinary-file `install` and `mv` lines. Assert the function/markers/helper calls/promotion statements exist uniquely; changed/unsupported extraction must fail. Do not execute any earlier or later installer statements.
- [ ] Substitute only the installed Python helper command path with a safely quoted invocation of the real source CLI; supply `die`, `TARGET_UKI`, `TARGET_VARIANT` and private temp paths. Do not mock digest calculation in ordinary success/failure cases. Execute the extracted statements under `bash` with `set -euo pipefail`.
- [ ] Require promotion for equal virtual payloads/different command lines. Require failure with destination and `.new` sentinel bytes unchanged when either virtual payload differs, either input is malformed/absent, and both helper reads fail. Preserve unrelated sentinel files. Explicitly assert no promotion for invalid cases, not only nonzero exit.
- [ ] Separately inject successful-but-empty/nonhex/wrong-length/multiline stdout at the helper boundary solely to prove output-shape refusal. Name it a fault fixture, never an identity calculation or authorization substitute; healthy/different/malformed PE cases continue to run the real source CLI.
- [ ] Run the new test through `env PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest discover -s os/tests -p test_installer_target_boot_digest.py -v`; preserve actual behavioral failures. Current dispatch must fail positive CLI assertions, and current caller must expose false empty equality. Do not edit production until these failures are captured.

### Step 2: Correct the existing guard and narrow fixture compatibility

- [ ] Add `boot-digest` to the existing verb allowlist and usage. Keep the implemented explicit-path branch and algorithm unchanged.
- [ ] Replace the comparison operands with checked local assignments, exact digest-shape checks and the equality guard above, before existing promotion statements. Preserve every other installer action.
- [ ] Update only `fixture_cmdline` in `installer_target_modes.py` to branch on its requested verb: `show` retains its existing command-line text, `boot-digest` emits one fixed valid 64-hex digest for both already synthetic non-PE placeholders, and unsupported verbs fail. Document that this legacy test proves path/mode promotion only; the new test owns real digest observation. Do not remove its assertions or substitute this stub into the new real-CLI tests.

### Step 3: Verify, freeze, report

- [ ] Rerun the new focused suite to GREEN. Run the existing `installer_target_modes.py` focused file once; it uses ordinary-file stubs and must not invoke actual backend/helper/provider commands. Inspect it before running. No broad `os/tests` discovery.
- [ ] Run `python3 -B -m unittest discover -s os/tests -p test_installer_target_cmdline.py -v` once as the relevant real-CLI command-line regression, after inspecting its exact existing host-only seam. It does not qualify boot behavior.
- [ ] Run Python `ast.parse` on the three owned Python files, `bash -n os/usr/libexec/tilo-install`, and scoped diff whitespace checks, including the new untracked test. No shellcheck dependency install.
- [ ] Run required `env -u RUSTC_BOOTSTRAP CARGO_BUILD_JOBS=2 PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig cargo check -p shell --tests --offline --locked` using the existing root target after coordinator release. Never set `CARGO_TARGET_DIR`, resolve dependencies or regenerate Cargo.lock. Capture shared-input drift honestly.
- [ ] Freeze four source files; save complete patch including new file, exact file hashes, every RED/GREEN/final raw log and command/environment/source snapshots. Write report `S/postprotocol-target-boot-digest-fix-report.md`, with S the existing internal-installer-boot-proof evidence directory. Explain any failed invocation, all qualification limits and any peer drift. Return `READY_FOR_REVIEW_NO_COMMIT`, release source/build ownership and list active handles. Root arranges fresh Spec/Quality review and exact local commit.
