use super::*;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

#[test]
fn retained_policy_time_resamples_grants_and_cannot_be_overridden_by_numeric_pin() {
    let time = Arc::new(AtomicU64::new(99));
    let clock = time.clone();
    let context = PermissionContext::from_capabilities(
        Actor::person(None, "member"),
        vec![
            CapOverride::grant(
                Subject::person("member"),
                Scope::App("fixture".into()),
                Verb::Write,
            )
            .with_expiry(100),
        ],
    )
    .at_unix_time(99)
    .with_policy_time_sampler(Arc::new(move || Ok(clock.load(Ordering::Acquire))));
    let scope = PermissionScope::app("fixture", Verb::Write);
    assert!(context.check(&scope));
    let delayed = context.clone().at_unix_time(1);
    time.store(100, Ordering::Release);
    assert!(
        !delayed.check(&scope),
        "the same retained grant expires at execution"
    );
}

#[test]
fn retained_policy_time_resamples_restriction_expiry_without_permanent_denial() {
    let time = Arc::new(AtomicU64::new(99));
    let clock = time.clone();
    let context = PermissionContext::from_capabilities(
        Actor::person(Some(Role::Editor), "member"),
        vec![
            CapOverride::new(
                Subject::person("member"),
                Scope::App("fixture".into()),
                Effect::Restrict,
                Verb::Read,
            )
            .with_expiry(100),
        ],
    )
    .at_unix_time(99)
    .with_policy_time_sampler(Arc::new(move || Ok(clock.load(Ordering::Acquire))));
    let scope = PermissionScope::app("fixture", Verb::Write);
    assert!(!context.check(&scope));
    time.store(100, Ordering::Release);
    assert!(
        context.check(&scope),
        "an expired restriction no longer caps the role"
    );
}

#[test]
fn retained_policy_time_failure_denies_and_context_identity_does_not_sample() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let context = PermissionContext::permissive().with_policy_time_sampler(Arc::new(move || {
        observed.fetch_add(1, Ordering::Relaxed);
        Err("owner origin retired".into())
    }));
    assert_eq!(context, context.clone());
    assert_ne!(
        context,
        PermissionContext::permissive()
            .with_policy_time_sampler(Arc::new(|| Err("owner origin retired".into())))
    );
    let _ = format!("{context:?}");
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert!(
        !context
            .at_unix_time(1)
            .check(&PermissionScope::workspace(Verb::Write))
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}
