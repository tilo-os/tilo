//! Plans reuse a resolved profile and key between model turns: what is
//! kept, and everything that lets go of it. The last test drives the real
//! settings document and vault in a disposable config root.
use super::*;
use std::process::Command;
use std::sync::atomic::Ordering;

fn stamp(inode: u64) -> SourceStamp {
    let file = FileStamp {
        device: 1,
        inode,
        length: 1,
        modified: (0, 0),
        changed: (0, 0),
    };
    (file, Some(file))
}

fn at(start: Instant, seconds: u64) -> Instant {
    start.checked_add(Duration::from_secs(seconds)).unwrap()
}

fn key_of(resolved: &ResolvedProfile) -> Option<&str> {
    resolved.1.as_ref().map(|key| key.as_str())
}

fn kept(cache: &ProfileCache) -> usize {
    cache.lock().entries.len()
}

/// Resolve `("p", "m")` through `cache`, a read answering `key`.
fn through(
    cache: &ProfileCache,
    stamp: Option<SourceStamp>,
    now: Instant,
    key: &str,
) -> Result<ResolvedProfile, String> {
    cache.resolve(("p", "m"), stamp, now, || {
        let mut profile = ProviderProfile::local_vllm();
        profile.id = "p".into();
        Ok((profile, Some(Zeroizing::new(key.to_owned()))))
    })
}

fn reads(cache: &ProfileCache) -> u64 {
    cache.reads.load(Ordering::Relaxed)
}

/// A kept reading serves later turns without reading again; a changed
/// source, its age, a forgotten key, an unstamped or failed reading, and a
/// reading overtaken by a forget each lead to reading again, and nothing
/// stale stays in memory.
#[test]
fn a_kept_profile_is_reused_only_while_nothing_says_it_changed() {
    let start = Instant::now();

    // Phase: a later turn reuses the first turn's reading.
    {
        let cache = ProfileCache::new();
        let first = through(&cache, Some(stamp(1)), start, "first").unwrap();
        let later = through(&cache, Some(stamp(1)), at(start, 59), "later").unwrap();
        assert_eq!(reads(&cache), 1);
        assert_eq!(key_of(&first), Some("first"));
        assert_eq!(key_of(&later), Some("first"));
        assert_eq!(later.0.id, "p");
        // Another model of the same provider is its own reading.
        cache
            .resolve(("p", "other"), Some(stamp(1)), start, || {
                Ok((ProviderProfile::local_vllm(), None))
            })
            .unwrap();
        assert_eq!(reads(&cache), 2);
        assert_eq!(kept(&cache), 2);
    }

    // Phase: a changed settings document or vault is read again.
    {
        let cache = ProfileCache::new();
        through(&cache, Some(stamp(1)), start, "first").unwrap();
        let changed = through(&cache, Some(stamp(2)), start, "second").unwrap();
        assert_eq!(key_of(&changed), Some("second"));
        assert_eq!(reads(&cache), 2);
        // The superseded reading is gone, not merely shadowed.
        assert_eq!(kept(&cache), 1);
        let mut vault_only = stamp(2);
        vault_only.1 = None;
        let removed = through(&cache, Some(vault_only), start, "third").unwrap();
        assert_eq!(key_of(&removed), Some("third"));
        assert_eq!(reads(&cache), 3);
    }

    // Phase: a reading is not reused past its age, and an idle sweep drops it.
    {
        let cache = ProfileCache::new();
        through(&cache, Some(stamp(1)), start, "first").unwrap();
        let aged = through(&cache, Some(stamp(1)), at(start, 60), "aged").unwrap();
        assert_eq!(key_of(&aged), Some("aged"));
        assert_eq!(reads(&cache), 2);
        cache.sweep(at(start, 119));
        assert_eq!(kept(&cache), 1);
        cache.sweep(at(start, 120));
        assert_eq!(kept(&cache), 0);
    }

    // Phase: a forgotten key is dropped at once and read again.
    {
        let cache = ProfileCache::new();
        through(&cache, Some(stamp(1)), start, "first").unwrap();
        cache.forget("another-profile");
        assert_eq!(kept(&cache), 1);
        cache.forget("p");
        assert_eq!(kept(&cache), 0);
        let again = through(&cache, Some(stamp(1)), start, "set").unwrap();
        assert_eq!(key_of(&again), Some("set"));
        assert_eq!(reads(&cache), 2);
    }

    // Phase: a reading the key changed under is used once and not kept.
    {
        let cache = ProfileCache::new();
        let raced = cache
            .resolve(("p", "m"), Some(stamp(1)), start, || {
                cache.forget("p");
                Ok((
                    ProviderProfile::local_vllm(),
                    Some(Zeroizing::new("old".into())),
                ))
            })
            .unwrap();
        assert_eq!(key_of(&raced), Some("old"));
        assert_eq!(kept(&cache), 0);
        through(&cache, Some(stamp(1)), start, "new").unwrap();
        assert_eq!(reads(&cache), 2);
    }

    // Phase: an unstamped reading and a failed one are never kept, and an
    // unstamped turn lets go of an earlier reading of the same pair.
    {
        let cache = ProfileCache::new();
        through(&cache, Some(stamp(1)), start, "first").unwrap();
        through(&cache, None, start, "unstamped").unwrap();
        assert_eq!(kept(&cache), 0);
        through(&cache, None, start, "unstamped").unwrap();
        assert_eq!(reads(&cache), 3);
        let failed = cache.resolve(("p", "m"), Some(stamp(1)), start, || {
            Err("provider is not enabled".into())
        });
        assert_eq!(failed.unwrap_err(), "provider is not enabled");
        assert_eq!(kept(&cache), 0);
    }
}

fn save_profile(enabled: bool) {
    let mut profile = ProviderProfile::local_vllm();
    profile.id = "test-provider".into();
    profile.models = vec![agent_provider_config::ModelDescriptor::new("test-model")];
    profile.credential = Some(agent_provider_config::SecretReference::provider_api_key(
        &profile.id,
    ));
    profile.enabled = enabled;
    ProviderConfig {
        profiles: vec![profile],
        default_profile: None,
    }
    .save()
    .unwrap();
}

fn planned_key() -> Result<String, String> {
    plan_profile("test-provider", "test-model")
        .map(|(_, key)| key.map(|key| key.as_str().to_owned()).unwrap_or_default())
}

fn holds(profile_id: &str) -> bool {
    PLAN_PROFILES
        .lock()
        .entries
        .iter()
        .any(|entry| entry.profile_id == profile_id)
}

/// The production path over real files: a later turn reads neither the
/// settings document nor the vault; a key set or removed through this
/// gateway, a vault rewritten by another writer, and a settings change all
/// reach the next turn; a removed key is not held.
fn child_follows_settings_and_vault() {
    let reads = || PLAN_PROFILES.reads.load(Ordering::Relaxed);
    save_profile(true);
    set_key("test-provider", "first-key".into()).unwrap();

    // Phase: the first turn reads; the next reuses without touching a file.
    let before = reads();
    assert_eq!(planned_key().unwrap(), "first-key");
    assert_eq!(reads(), before.saturating_add(1));
    assert!(holds("test-provider"));
    assert_eq!(planned_key().unwrap(), "first-key");
    assert_eq!(
        reads(),
        before.saturating_add(1),
        "a kept reading is reused"
    );

    // Phase: a key set through the gateway reaches the next turn.
    set_key("test-provider", "second-key".into()).unwrap();
    assert!(!holds("test-provider"));
    assert_eq!(planned_key().unwrap(), "second-key");

    // Phase: a removed key is not held, and the next turn has none.
    remove_key("test-provider").unwrap();
    assert!(!holds("test-provider"));
    assert_eq!(planned_key().unwrap_err(), "API key is not configured");
    assert!(!holds("test-provider"));

    // Phase: a vault another writer rewrote is read again.
    set_key("test-provider", "third-key".into()).unwrap();
    assert_eq!(planned_key().unwrap(), "third-key");
    let mut vault = SecretVault::open();
    vault.put(
        &vault_id("test-provider").unwrap(),
        &Secret::password("fourth-key"),
    );
    vault.try_save().unwrap();
    assert_eq!(planned_key().unwrap(), "fourth-key");

    // Phase: a settings change reaches the next turn. `ProviderConfig`
    // writes the very document this gateway stamps, or the reuse above
    // (nothing stamped) or this (a stale reading) fails.
    save_profile(false);
    assert_eq!(planned_key().unwrap_err(), "provider is not enabled");
    assert!(!holds("test-provider"));
}

#[test]
fn plan_profiles_follow_the_settings_document_and_vault_on_disk() {
    if std::env::var_os("PROFILE_TEST_CHILD").is_some() {
        child_follows_settings_and_vault();
        return;
    }
    for session_document in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "profile_tests::plan_profiles_follow_the_settings_document_and_vault_on_disk",
                "--nocapture",
            ])
            // No ambient credentials, settings path, or secret-tool.
            .env_clear()
            .env("PATH", root.path())
            .env("XDG_CONFIG_HOME", root.path())
            .env("PROFILE_TEST_CHILD", "1");
        if session_document {
            // A sandbox's session document, apart from the config root's.
            child.env("TILO_SESSION_SETTINGS", root.path().join("session.json"));
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "child failed (session document: {session_document}): {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            root.path().join("session.json").exists(),
            session_document,
            "the configuration went to the document under test"
        );
    }
}
