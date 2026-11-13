//! Bounded rollback protection for the sealed root filesystem: the last few OS generations can
//! unseal the disk, older ones cannot, so a broken update keeps the generation it replaced.
//!
//! A TPM NV counter holds the oldest permitted generation (the floor) and [`retention`] keeps
//! one sealed keyslot per retained generation; unlock needs both. Sealing straight to
//! `NV_counter >= G` gives zero rollback instead: once the counter reaches the newest
//! generation, the one before it can no longer unseal.
//!
//! SAFETY-CRITICAL invariants, marked in code:
//! 1. The recovery key always stays a working, independent unlock path; every header mutation
//!    re-verifies it afterwards.
//! 2. No keyslot is killed until its replacement has been proven to unlock the device
//!    ([`retention::prune_is_safe`]).
//! 3. The floor is never raised until a token at or above the target has been proven to unseal.
//! 4. A declared floor above the running generation is refused, not silently clamped.
//! 5. The counter only moves with a signed, single-use approval bound to one target value
//!    ([`approval`]); an unreachable authority freezes the floor, not the disk.
//!
//! Token `G` carries `PolicyAuthorize(policyRef = G)`, which is what stops an older image from
//! presenting its own stale prediction and then unsealing against a newer floor.

pub mod approval;
pub mod certify;
pub mod confirm;
pub mod counter;
pub mod dual_boot;
pub mod exec;
pub mod generation;
pub mod keyslots;
pub mod lockout;
pub mod policy;
pub mod retention;
pub mod token;
pub mod tpm_policy;
pub mod verified;

/// Deployment policy, not a compiled-in constant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// NV index holding the rollback floor.
    pub nv_index: u32,
    /// How many generations keep a sealed keyslot, counting the current one (3 = this
    /// generation plus the two before it). Each generation's kernel is a ~134 MB UKI on the
    /// ESP, and the default 2 GiB image (`ESP_SIZE` in `os/dev/disk-image.sh`) holds a window
    /// of 10 (`retained_generations` plus [`retention::RETAINED_NEWER_GENERATIONS`] staged
    /// ahead); past that an update fails for want of space. `os/tests/esp_capacity.sh`
    /// asserts the two agree.
    pub retained_generations: u32,
    /// Bank the prediction authority signs over.
    pub pcr_bank: String,
    pub pcrs: Vec<u32>,
    /// PCRs applied after `PolicyAuthorize`; their values depend on the machine, so a release
    /// prediction cannot cover them.
    pub platform_pcrs: Vec<u32>,
    pub pcr_public_key: String,
    /// Public half of the key that authorises floor advances. Defaults to the prediction
    /// key; production custody should separate them, since a floor approval permanently
    /// narrows what a machine boots.
    /// narrows what a machine will boot.
    pub floor_authority_key: String,
    pub approval_dir: String,
    /// Override the key-derivation cost of keyslots this crate creates. `None` in production
    /// (cryptsetup's benchmarked argon2id defaults); only tests set it, to keep a dozen
    /// keyslots from costing a minute of CPU. Setting it weakens the keyslot.
    pub pbkdf: Option<Pbkdf>,
}

/// Deliberately weak key derivation. Tests only.
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
