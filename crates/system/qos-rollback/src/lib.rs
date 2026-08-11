//! Bounded rollback protection for the sealed root filesystem.
//!
//! The last few OS generations may unseal the disk. Older ones may not. That is
//! the whole feature, and the interesting part is why it takes two independent
//! mechanisms rather than one.
//!
//! # The naive design, and why it is not built here
//!
//! The obvious shape is: seal the unlock secret to a policy requiring
//! `NV_counter >= G`, and bump the counter to `N` when installing generation
//! `N`. That is not bounded rollback, it is *zero* rollback — the moment the
//! counter reaches `N`, generation `N-1`'s secret can no longer be released,
//! so a broken update leaves nothing to fall back to. The very first thing you
//! need after a bad update is the thing that design destroys.
//!
//! # The shape that works: the counter holds the FLOOR
//!
//! * Each image carries its own generation number `G`, on the UKI command line
//!   and therefore inside the image signature and the boot measurements. An
//!   attacker cannot relabel an old image with a higher generation, because the
//!   command line is not an independently editable file (see [`generation`]).
//! * A TPM2 NV **monotonic counter** holds the *oldest permitted* generation —
//!   the floor, not the ceiling.
//! * The unseal policy requires `floor <= G` via `TPM2_PolicyNV`, with `G`
//!   fixed inside the policy digest at seal time, so each generation's sealed
//!   object carries its own bound.
//! * Confirming generation `N` raises the floor to `N - retained + 1`, leaving
//!   the newest `retained` generations able to unseal. NV counters only ever
//!   increment, so the floor can never move backwards — not by software, not by
//!   `TPM2_NV_UndefineSpace` followed by a redefine (a newly defined counter is
//!   initialised to the highest value any counter on that TPM has ever held,
//!   precisely to stop that trick), and not by restoring an old disk image.
//!
//! ## When the floor advances is the load-bearing decision
//!
//! **Not at install time. At confirmation time.** Installing generation `N` and
//! immediately raising the floor would destroy the fallback before anyone knows
//! whether `N` boots. The sequence is: stage `N` → boot `N` → health check
//! passes → *then* raise the floor. Until that last step the previous
//! generation is still fully able to unseal, which is what makes an automatic
//! rollback survivable.
//!
//! An update may also declare a floor *higher* than `N - retained + 1`, so a
//! security update can jump the floor past a known-vulnerable generation
//! instead of leaving it reachable inside the window. The effective floor is
//! the maximum of the window floor, the declared floor, and the current floor —
//! and it is refused outright if it would exceed the generation that is being
//! confirmed, because that would lock out the machine that is running.
//!
//! # Second, independent mechanism: retained keyslots
//!
//! We keep `retained` sealed LUKS2 tokens, one per retained generation, and drop
//! the oldest as new ones arrive (see [`retention`]).
//!
//! On its own this is weak. The tokens live in the LUKS2 header, which is
//! plaintext metadata on the disk: an attacker with physical access can
//! `luksHeaderBackup` before an update and `luksHeaderRestore` afterwards,
//! resurrecting every token that was ever pruned. Verified — see
//! `tests/mechanism.rs`, which performs exactly that attack.
//!
//! The NV counter defeats it, because the counter lives in the TPM and no
//! amount of disk-header surgery moves it.
//!
//! Conversely the counter alone is not enough either: it bounds *how far back*
//! a generation may be, but something still has to exist for a genuine
//! rollback to unseal with, and pruning is what keeps the header from
//! accumulating one keyslot per update forever (LUKS2 has 32 slots).
//!
//! **So both are required.** Unlock succeeds only when the token for the
//! running generation is present in the header *and* the NV policy passes. A
//! header-restore attack reinstates the token and still cannot unseal below the
//! floor. A missing token cannot be conjured from a passing NV policy.
//!
//! # Why the generation has to appear in the policy reference
//!
//! This is the subtle part, and getting it wrong silently voids the whole
//! feature.
//!
//! The seal is bound to *signed* PCR policy rather than to literal measurement
//! values, so that a legitimate firmware or kernel update ships a new signature
//! and re-satisfies the existing policy instead of breaking it.
//! `TPM2_PolicyAuthorize` implements that, but it **discards** the
//! session digest it validated and replaces it with
//! `H(0 || TPM_CC_PolicyAuthorize || keyName || policyRef)` — a value that does
//! not depend on the PCR values at all.
//!
//! So if `policyRef` were empty, a sealed object would accept *any* prediction
//! the authority ever signed, for any generation. Booting a five-generations-old
//! image would satisfy `PolicyAuthorize` using that old image's own published
//! prediction, and then satisfy `PolicyNV(floor <= G)` using the *newest*
//! retained token's `G`. Rollback protection: gone, while every individual
//! mechanism still looks correct in isolation.
//!
//! The fix is to put the generation in `policyRef`: the authority signs
//! `H(approvedPolicy || G)` for generation `G`'s predicted measurements, and
//! the token for `G` carries `PolicyAuthorize(keyName, policyRef = G)`. Token
//! `G` then unseals only while generation `G` is actually running, and only
//! while `floor <= G`. That is what makes the two mechanisms compose into
//! "the last `retained` generations, and nothing older".
//!
//! # Where a mistake costs a user their data
//!
//! Marked in the code with `SAFETY-CRITICAL`. The invariants:
//!
//! 1. The recovery key is a working, independent unlock path at every step.
//!    Nothing here ever touches its keyslot, and every operation that mutates
//!    the header re-verifies it afterwards.
//! 2. No keyslot is killed until the replacement slot has been proven to unlock
//!    the device ([`retention::prune_is_safe`] refuses to prune without a
//!    verified survivor and a verified recovery key).
//! 3. The floor is never raised until a token at or above the target floor has
//!    been proven to unseal *now*.
//! 4. A declared floor above the running generation is refused, not clamped
//!    silently, because silently clamping hides an authoring mistake that would
//!    otherwise be caught before it ships.
//! 5. The counter cannot be incremented by anything that does not hold a signed,
//!    single-use approval bound to one specific target value ([`approval`]). An
//!    unreachable authority freezes the floor; it never stops the disk unlocking.

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
    /// How many generations keep a sealed keyslot, counting the current one.
    /// 3 means "this generation plus the two before it".
    ///
    /// Raising this costs **disk on the ESP**, not just keyslots: every
    /// generation in the window has to stay bootable, and on the sealed layout
    /// each one's kernel is a ~134 MB Unified Kernel Image on the ESP —
    /// `retained_generations` of them plus up to
    /// [`retention::RETAINED_NEWER_GENERATIONS`] staged ahead. The default 2 GiB
    /// ESP (`ESP_SIZE` in `os/dev/disk-image.sh`) holds a window of 10; past that
    /// an update would fail for want of space on a machine already in the field.
    /// `os/tests/esp_capacity.sh` asserts the two agree, and the disk-image step
    /// refuses a combination that does not fit.
    pub retained_generations: u32,
    /// PCR bank and selection the prediction authority signs over.
    pub pcr_bank: String,
    pub pcrs: Vec<u32>,
    /// Public half of the PCR-prediction signing key.
    pub pcr_public_key: String,
    /// Public half of the key that authorises floor advances.
    ///
    /// Defaults to the PCR-prediction key because both are the same trust domain —
    /// the authority that publishes an image. Production custody should separate
    /// them: a prediction signature only says "these are the expected
    /// measurements", while a floor approval permanently narrows what a machine
    /// will boot, so they warrant different handling.
    pub floor_authority_key: String,
    /// Where the device keeps the signed floor-advance approvals it holds.
    pub approval_dir: String,
    /// Override the key-derivation cost of the keyslots this crate creates.
    ///
    /// `None` in production, which means cryptsetup's own defaults (argon2id,
    /// benchmarked). It exists because the mechanism tests add and remove a
    /// dozen keyslots and argon2id at default cost turns a five-second test into
    /// a minute of CPU and a gigabyte of RAM. Setting it weakens the keyslot, so
    /// nothing but a test should ever set it.
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
            // 7  — Secure Boot state and the enrolled keys.
            // 11 — systemd-stub's UKI measurements (kernel, initrd, cmdline,
            //      os-release), which is what makes the generation number and
            //      the root digest transitively measured.
            pcrs: vec![7, 11],
            pcr_public_key: "/usr/share/qos/tpm2-pcr-signing.public.pem".into(),
            floor_authority_key: "/usr/share/qos/tpm2-pcr-signing.public.pem".into(),
            approval_dir: approval::DEFAULT_DIR.into(),
            pbkdf: None,
        }
    }
}
