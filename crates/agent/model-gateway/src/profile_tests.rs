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
) -> Result<ResolvedProfile, ModelError> {
    cache.resolve(("p", "m"), stamp, now, || {
        let mut profile = ProviderProfile::local_vllm();
        profile.id = "p".into();
        Ok(((profile, Some(Zeroizing::new(key.to_owned()))), None))
    })
}

/// Resolve `("p", "m")` through `cache`, a read answering a minted `token`
/// usable until `until`.
fn minted(
    cache: &ProfileCache,
    now: Instant,
    token: &str,
    until: Instant,
) -> Result<ResolvedProfile, ModelError> {
    cache.resolve(("p", "m"), Some(stamp(1)), now, || {
        let mut profile = ProviderProfile::google_vertex("p-1", "global", "m");
        profile.id = "p".into();
        Ok((
            (profile, Some(Zeroizing::new(token.to_owned()))),
            Some(until),
        ))
    })
}

/// A minted access token is reused only until shortly before it expires,
/// however recently it was read; a sweep lets go of an expired one.
#[test]
fn a_minted_token_is_reused_only_while_it_is_valid() {
    let start = Instant::now();
    let cache = ProfileCache::new();
    // Expires in 120 s: reused for the 30 s before the margin.
    let until = reuse_until(1_120, 1_000, start);
    assert_eq!(until, at(start, 30));
    let first = minted(&cache, start, "first", until).unwrap();
    let later = minted(&cache, at(start, 29), "later", until).unwrap();
    assert_eq!(key_of(&first), Some("first"));
    assert_eq!(key_of(&later), Some("first"));
    assert_eq!(reads(&cache), 1);
    let renewed = minted(&cache, at(start, 30), "renewed", at(start, 3000)).unwrap();
    assert_eq!(key_of(&renewed), Some("renewed"));
    assert_eq!(reads(&cache), 2);
    // Still bounded by the reuse age, not only the token's lifetime.
    let aged = minted(&cache, at(start, 91), "aged", at(start, 3000)).unwrap();
    assert_eq!(key_of(&aged), Some("aged"));
    // A token already inside the margin is used once and never kept.
    let cache = ProfileCache::new();
    let due = reuse_until(1_050, 1_000, start);
    assert_eq!(due, start);
    minted(&cache, start, "due", due).unwrap();
    assert_eq!(kept(&cache), 0);
    // A sweep drops a token whose time is up even within the reuse age.
    minted(&cache, start, "short", at(start, 10)).unwrap();
    cache.sweep(at(start, 9));
    assert_eq!(kept(&cache), 1);
    cache.sweep(at(start, 10));
    assert_eq!(kept(&cache), 0);
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
        cache
            .resolve(("p", "other"), Some(stamp(1)), start, || {
                Ok(((ProviderProfile::local_vllm(), None), None))
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
                    (
                        ProviderProfile::local_vllm(),
                        Some(Zeroizing::new("old".into())),
                    ),
                    None,
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
            Err(resolution_error(
                ErrorKind::InvalidRequest,
                "provider is not enabled",
            ))
        });
        assert_eq!(
            failed.unwrap_err(),
            resolution_error(ErrorKind::InvalidRequest, "provider is not enabled")
        );
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
        ..Default::default()
    }
    .save()
    .unwrap();
}

fn planned_key() -> Result<String, String> {
    plan_profile("test-provider", "test-model")
        .map(|(_, key)| key.map(|key| key.as_str().to_owned()).unwrap_or_default())
        .map_err(|error| error.message)
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
fn complete_error(profile_id: &str, model_id: &str) -> ModelError {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime
        .block_on(rig_adapter::complete(
            &provider_client().unwrap(),
            &PlanCall {
                profile_id,
                model_id,
                timeout: tilo_test_env::hang_bound(),
                stream: false,
                deltas: false,
                trace: None,
            },
            agent_provider_config::protocol::Conversation::new("system", "hello")
                .request(Vec::new(), 64),
            &mut |_| true,
        ))
        .unwrap_err()
}

fn child_follows_settings_and_vault() {
    let reads = || PLAN_PROFILES.reads.load(Ordering::Relaxed);
    save_profile(true);
    assert_eq!(
        plan_profile("missing", "test-model").unwrap_err().kind,
        ErrorKind::InvalidRequest
    );
    assert_eq!(
        plan_profile("test-provider", "missing").unwrap_err().kind,
        ErrorKind::InvalidRequest
    );
    assert_eq!(
        complete_error("missing", "test-model").kind,
        ErrorKind::InvalidRequest
    );
    assert_eq!(
        complete_error("test-provider", "missing").kind,
        ErrorKind::InvalidRequest
    );
    // Establish local test custody explicitly, never use an ambient keyring.
    let root = PathBuf::from(std::env::var("XDG_CONFIG_HOME").unwrap()).join("tilo");
    let profile = configured_profile("test-provider").unwrap();
    let (reference, authorization) = credential_binding(&profile).unwrap();
    ServiceCredentials::open_in(&root)
        .store(&reference, &authorization, "seed-key")
        .unwrap();
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
    assert_eq!(
        plan_profile("test-provider", "test-model")
            .unwrap_err()
            .kind,
        ErrorKind::Authentication
    );
    assert!(!holds("test-provider"));

    assert_eq!(
        complete_error("test-provider", "test-model").kind,
        ErrorKind::Authentication
    );

    // Phase: a vault another writer rewrote is read again.
    set_key("test-provider", "third-key".into()).unwrap();
    assert_eq!(planned_key().unwrap(), "third-key");
    let profile = credential_profile("test-provider").unwrap();
    let (reference, authorization) = credential_binding(&profile).unwrap();
    ServiceCredentials::open()
        .store(&reference, &authorization, "fourth-key")
        .unwrap();
    assert_eq!(planned_key().unwrap(), "fourth-key");

    // Phase: a settings change reaches the next turn. `ProviderConfig`
    // writes the very document this gateway stamps, or the reuse above
    // (nothing stamped) or this (a stale reading) fails.
    save_profile(false);
    assert_eq!(planned_key().unwrap_err(), "provider is not enabled");
    assert_eq!(
        plan_profile("test-provider", "test-model")
            .unwrap_err()
            .kind,
        ErrorKind::InvalidRequest
    );
    assert!(!holds("test-provider"));
    save_profile(true);
    fs::write(
        accounts::vault_dir_in(&root).join("vault.enc"),
        b"invalid encrypted vault",
    )
    .unwrap();
    assert_eq!(
        complete_error("test-provider", "test-model").kind,
        ErrorKind::Unavailable
    );
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
                "service::profile_tests::plan_profiles_follow_the_settings_document_and_vault_on_disk",
                "--nocapture",
            ])
            // No ambient credentials, settings path, or secret-tool.
            .env_clear()
            .env("PATH", root.path())
            .env("XDG_CONFIG_HOME", root.path())
            .env("PROFILE_TEST_CHILD", "1");
        if session_document {
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

fn seal_grant(profile_id: &str, refresh_token: &str) {
    let tokens = account_custody::TokenSet {
        access_token: "minted-access-token".into(),
        scope: Some("https://www.googleapis.com/auth/cloud-platform".into()),
        refresh_token: refresh_token.into(),
        expires_in: 3600,
        obtained_at: tilo_clock::unix_now(),
    };
    Custodian::live()
        .seal_oauth(&grant_id(profile_id).unwrap(), &tokens)
        .unwrap();
}

/// The production path for a Google sign-in over real files: the gateway
/// takes the access token custody holds, Settings and the agent see it as
/// configured, and once the grant is revoked every one of them says to
/// sign in again; the profile's API-key slot is never consulted.
fn child_mints_from_custody() {
    let mut profile = ProviderProfile::google_vertex("tilo-509618", "global", "gemini-4-argon");
    profile.id = "vertex-provider".into();
    let model = profile.default_model.clone().unwrap();
    ProviderConfig {
        profiles: vec![profile],
        default_profile: None,
        ..Default::default()
    }
    .save()
    .unwrap();
    let signed_in = |expected: bool| {
        assert_eq!(secret_status("vertex-provider"), Ok(expected));
        let selection = selection_configured(
            "vertex-provider",
            &model,
            ProviderProtocol::OpenAiCompatible,
        );
        assert_eq!(selection.is_ok(), expected, "{selection:?}");
    };

    // Phase: never signed in on this device.
    signed_in(false);
    assert_eq!(
        plan_profile("vertex-provider", &model).unwrap_err(),
        resolution_error(ErrorKind::Authentication, SIGN_IN_AGAIN)
    );

    // Phase: a sealed grant yields its access token, kept between turns.
    seal_grant("vertex-provider", "refresh-token");
    signed_in(true);
    let (profile, token) = plan_profile("vertex-provider", &model).unwrap();
    assert_eq!(
        token.as_deref().map(String::as_str),
        Some("minted-access-token")
    );
    assert!(profile.signs_in_with_google());
    assert!(holds("vertex-provider"));
    let store = ServiceCredentials::open();
    let (reference, authorization) = credential_binding(&profile).unwrap();
    assert!(
        !store.status(&reference, &authorization).unwrap(),
        "a sign-in never writes the bound API-key slot"
    );
    assert!(
        !store.legacy_agent_provider_status(&profile.id).unwrap(),
        "a sign-in never writes the legacy API-key slot"
    );

    // Phase: a revoked grant (custody keeps it empty) means sign in again.
    seal_grant("vertex-provider", "");
    signed_in(false);
    assert_eq!(
        plan_profile("vertex-provider", &model).unwrap_err(),
        resolution_error(ErrorKind::Authentication, SIGN_IN_AGAIN)
    );
    assert!(!holds("vertex-provider"));
}

#[test]
fn a_google_sign_in_is_minted_by_custody_and_reports_sign_in_again() {
    if std::env::var_os("PROFILE_TEST_CHILD").is_some() {
        child_mints_from_custody();
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "service::profile_tests::a_google_sign_in_is_minted_by_custody_and_reports_sign_in_again",
            "--nocapture",
        ])
        // No ambient credentials, settings path, or secret-tool.
        .env_clear()
        .env("PATH", root.path())
        .env("XDG_CONFIG_HOME", root.path())
        // Use an isolated credential backend rather than the login keyring.
        .env("TILO_SECRETS_DIR", root.path().join("secrets"))
        .env("PROFILE_TEST_CHILD", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
