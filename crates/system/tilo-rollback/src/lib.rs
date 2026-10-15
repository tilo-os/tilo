//! Bounded rollback protection for the sealed root filesystem: the last few OS generations
//! may unseal the disk, older ones may not. Two independent mechanisms enforce this together.
//!
//! # Why not a single "counter >= G" policy
//!
//! Sealing to `NV_counter >= G` and bumping the counter to `N` on install gives *zero*
//! rollback, not bounded rollback: the moment the counter reaches `N`, generation `N-1` can
//! no longer unseal, destroying the fallback a broken update needs.
//!
//! # Mechanism 1: the counter holds the FLOOR
//!
//! * Each image carries its own generation `G` on the UKI command line, inside the image
//!   signature and boot measurements — not independently editable (see [`generation`]).
//! * A TPM2 NV monotonic counter holds the *oldest permitted* generation (the floor, not
//!   the ceiling). The unseal policy requires `floor <= G` via `TPM2_PolicyNV`, with `G`
//!   fixed in each generation's sealed policy digest at seal time.
//! * Confirming generation `N` raises the floor to `N - retained + 1`. NV counters only
//!   increment, so the floor can never move backwards — not by `TPM2_NV_UndefineSpace` plus
//!   redefine (a fresh counter starts at the highest value any counter on that TPM ever
//!   held), and not by restoring an old disk image.
//!
//! The floor advances at *confirmation* time, not install time — stage `N` → boot `N` →
//! health check passes → *then* raise the floor — so the previous generation stays able to
//! unseal until `N` is proven to boot. An update may also declare a floor higher than
//! `N - retained + 1` to jump past a known-vulnerable generation; the effective floor is the
//! max of window/declared/current floor, and is refused if it would exceed the generation
//! being confirmed (that would lock out the running machine).
//!
//! # Mechanism 2: retained keyslots
//!
//! `retained` sealed LUKS2 tokens are kept, one per retained generation, oldest dropped as
//! new ones arrive (see [`retention`]). Alone this is weak: the tokens live in plaintext
//! LUKS2 header metadata, so a `luksHeaderBackup`/`luksHeaderRestore` around an update
//! resurrects every pruned token (verified in `tests/mechanism.rs`). The NV counter defeats
//! that, since it lives in the TPM and header surgery can't touch it. Conversely the counter
//! alone can't unseal anything without a surviving keyslot, and pruning is what keeps the
//! header from growing one slot per update forever (LUKS2 has 32 slots). So both are
//! required: unlock needs the running generation's token present *and* the NV policy to pass.
//!
//! # Why the generation must appear in the policy reference
//!
//! The subtle part, and getting it wrong silently voids the whole feature. The seal binds to
//! *signed* PCR policy (via `TPM2_PolicyAuthorize`) rather than literal measurements, so a
//! legitimate update ships a new signature instead of breaking the policy — but
//! `PolicyAuthorize` discards the PCR-derived digest and replaces it with
//! `H(0 || TPM_CC_PolicyAuthorize || keyName || policyRef)`, independent of PCR values. With
//! an empty `policyRef`, a sealed object would accept *any* prediction the authority ever
//! signed for *any* generation — an old image could satisfy `PolicyAuthorize` with its own
//! stale prediction and then satisfy `PolicyNV` using the newest token's floor, silently
//! defeating rollback protection while every mechanism looks correct in isolation. The fix:
//! the authority signs `H(approvedPolicy || G)`, and token `G` carries
//! `PolicyAuthorize(keyName, policyRef = G)`, so it unseals only while generation `G` is
//! actually running and `floor <= G`.
//!
//! # SAFETY-CRITICAL invariants (marked `SAFETY-CRITICAL` in code)
//!
//! 1. The recovery key is always a working, independent unlock path; nothing here touches
//!    its keyslot, and every header mutation re-verifies it afterwards.
//! 2. No keyslot is killed until the replacement has been proven to unlock the device
//!    ([`retention::prune_is_safe`]).
//! 3. The floor is never raised until a token at or above the target has been proven to
//!    unseal *now*.
//! 4. A declared floor above the running generation is refused, not silently clamped.
//! 5. The counter can only be incremented with a signed, single-use approval bound to one
//!    target value ([`approval`]); an unreachable authority freezes the floor, never the disk.

pub mod approval;
pub mod certify;
pub mod confirm;
pub mod counter;
pub mod exec;
pub mod generation;
pub mod keyslots;
pub mod lockout;
pub mod policy;
pub mod retention;
pub mod token;
pub mod tpm_policy;

/// Everything that is a deployment policy rather than a constant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// NV index holding the rollback floor.
    pub nv_index: u32,
    /// How many generations keep a sealed keyslot, counting the current one (3 = this
    /// generation plus the two before it).
    ///
    /// Costs disk on the ESP, not just keyslots: each generation's kernel is a ~134 MB UKI,
    /// and the default 2 GiB ESP (`ESP_SIZE` in `os/dev/disk-image.sh`) holds a window of 10
    /// (`retained_generations` plus [`retention::RETAINED_NEWER_GENERATIONS`] staged ahead);
    /// past that an update fails for want of space. `os/tests/esp_capacity.sh` asserts the two
    /// agree.
    pub retained_generations: u32,
    /// PCR bank and selection the prediction authority signs over.
    pub pcr_bank: String,
    pub pcrs: Vec<u32>,
    /// Device-local PCRs applied after `PolicyAuthorize`. These cannot be part of
    /// a release prediction because their values depend on the machine.
    pub platform_pcrs: Vec<u32>,
    /// Public half of the PCR-prediction signing key.
    pub pcr_public_key: String,
    /// Public half of the key that authorises floor advances. Defaults to the PCR-prediction
    /// key (same trust domain — the image publisher), but production custody should separate
    /// them: a prediction only claims expected measurements, while a floor approval permanently
    /// narrows what a machine will boot.
    pub floor_authority_key: String,
    /// Where the device keeps the signed floor-advance approvals it holds.
    pub approval_dir: String,
    /// Override the key-derivation cost of keyslots this crate creates. `None` in production
    /// (cryptsetup's benchmarked argon2id defaults); exists because the mechanism tests add and
    /// remove a dozen keyslots, and default-cost argon2id turns a five-second test into a minute
    /// of CPU. Setting it weakens the keyslot — only tests should ever do so.
    pub pbkdf: Option<Pbkdf>,
}

/// A deliberately weakened key-derivation setting. Tests only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pbkdf {
    pub algorithm: String,
    pub iterations: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            nv_index: counter::DEFAULT_NV_INDEX,
            retained_generations: 3,
            pcr_bank: "sha256".into(),
            // 11 — release-predictable systemd-stub UKI measurements.
            pcrs: vec![11],
            // 7 — machine-local Secure Boot state and enrolled keys. This is
            // applied after PolicyAuthorize so the reset cannot erase it.
            platform_pcrs: vec![7],
            pcr_public_key: "/usr/share/tilo/tpm2-pcr-signing.public.pem".into(),
            floor_authority_key: "/usr/share/tilo/tpm2-pcr-signing.public.pem".into(),
            approval_dir: approval::DEFAULT_DIR.into(),
            pbkdf: None,
        }
    }
}
