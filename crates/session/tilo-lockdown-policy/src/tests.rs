use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

#[test]
fn observer_remains_attached_when_missing_parents_under_symlink_are_created() {
    let directory = tempfile::tempdir().unwrap();
    let real = directory.path().join("real-state");
    fs::create_dir(&real).unwrap();
    let alias = directory.path().join("state-alias");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let path = alias.join("tilo/portal/lockdown.json");
    assert!(!path.parent().unwrap().exists());
    let observed = Arc::new(Mutex::new(Vec::new()));
    let checked = observed.clone();
    let listener: Arc<PolicyListener> = Arc::new(move |policy| {
        checked.lock().unwrap().push(policy.disable_microphone);
    });
    watch_policy_at(path.clone(), &listener).unwrap();
    PolicyStore::at_path(&path)
        .set_policy(LockdownPolicy::default())
        .unwrap();
    assert!(path.parent().unwrap().exists());
    observed.lock().unwrap().clear();
    PolicyStore::at_path(&path)
        .set_policy(LockdownPolicy {
            disable_microphone: true,
            ..Default::default()
        })
        .unwrap();
    assert!(
        serde_json::from_slice::<LockdownPolicy>(&fs::read(&path).unwrap())
            .unwrap()
            .disable_microphone
    );
    assert_eq!(
        *observed.lock().unwrap(),
        vec![true],
        "a later durable restriction must still reach the startup observer"
    );
    PolicyStore::at_path(real.join("tilo/portal/lockdown.json"))
        .set_policy(LockdownPolicy::default())
        .unwrap();
    assert_eq!(
        *observed.lock().unwrap(),
        vec![true, false],
        "the real and aliased parent must share the same ordered observer lane"
    );
}

#[test]
fn committed_restriction_is_delivered_before_later_relaxation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("policy.json");
    let observed = Arc::new(Mutex::new(Vec::new()));
    let checked = observed.clone();
    let listener: Arc<PolicyListener> = Arc::new(move |policy| {
        checked.lock().unwrap().push(policy.disable_microphone);
    });
    watch_policy_at(path.clone(), &listener).unwrap();
    observed.lock().unwrap().clear();
    let old = PolicyStore::at_path(&path);
    let (committed, at_commit) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let delayed = std::thread::spawn(move || {
        old.set_policy_with_hook(
            LockdownPolicy {
                disable_microphone: true,
                ..Default::default()
            },
            || {
                committed.send(()).unwrap();
                wait.recv().unwrap();
            },
        )
        .unwrap();
    });
    at_commit.recv().unwrap();
    PolicyStore::at_path(&path)
        .set_policy(LockdownPolicy::default())
        .unwrap();
    release.send(()).unwrap();
    delayed.join().unwrap();
    assert_eq!(
        *observed.lock().unwrap(),
        vec![true, false],
        "a relaxation cannot erase the committed capture-stop obligation"
    );
}

#[test]
fn delayed_relaxation_notification_cannot_overtake_newer_durable_restriction() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("policy.json");
    let observed = Arc::new(AtomicBool::new(false));
    let checked = observed.clone();
    let listener: Arc<PolicyListener> = Arc::new(move |policy| {
        checked.store(policy.disable_microphone, Ordering::SeqCst);
    });
    watch_policy_at(path.clone(), &listener).unwrap();
    let old = PolicyStore::at_path(&path);
    let (committed, at_commit) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let delayed = std::thread::spawn(move || {
        old.set_policy_with_hook(LockdownPolicy::default(), || {
            committed.send(()).unwrap();
            wait.recv().unwrap();
        })
        .unwrap();
    });
    at_commit.recv().unwrap();
    let newest = PolicyStore::at_path(&path);
    newest
        .set_policy(LockdownPolicy {
            disable_microphone: true,
            disable_sound_output: true,
            ..Default::default()
        })
        .unwrap();
    assert!(observed.load(Ordering::SeqCst));
    release.send(()).unwrap();
    delayed.join().unwrap();
    assert!(PolicyStore::at_path(&path).policy().disable_microphone);
    assert!(
        observed.load(Ordering::SeqCst),
        "older post-commit notification must not relax the newer durable restriction"
    );
}

#[test]
fn delayed_initial_observation_cannot_overtake_committed_restriction() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("policy.json");
    let observed = Arc::new(AtomicBool::new(false));
    let checked = observed.clone();
    let listener: Arc<PolicyListener> = Arc::new(move |policy| {
        checked.store(policy.disable_microphone, Ordering::SeqCst);
    });
    let (registered, at_registration) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let initial_path = path.clone();
    let delayed = std::thread::spawn(move || {
        watch_policy_with_hook(initial_path, &listener, || {
            registered.send(()).unwrap();
            wait.recv().unwrap();
        })
        .unwrap();
        listener
    });
    at_registration.recv().unwrap();
    PolicyStore::at_path(&path)
        .set_policy(LockdownPolicy {
            disable_microphone: true,
            ..Default::default()
        })
        .unwrap();
    assert!(observed.load(Ordering::SeqCst));
    release.send(()).unwrap();
    let _listener = delayed.join().unwrap();
    assert!(
        observed.load(Ordering::SeqCst),
        "initial snapshot must not undo the already observed durable restriction"
    );
}

#[test]
fn callbacks_can_inspect_current_policy_but_reentrant_mutation_is_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("policy.json");
    let store = PolicyStore::at_path(&path);
    let callback_store = store.clone();
    let callback_path = path.clone();
    let listener: Arc<PolicyListener> = Arc::new(move |policy| {
        assert_eq!(callback_store.policy(), *policy);
        assert!(
            callback_store
                .set_policy(LockdownPolicy::default())
                .is_err()
        );
        let nested: Arc<PolicyListener> =
            Arc::new(|_| panic!("reentrant observer must not be installed"));
        assert!(watch_policy_at(callback_path.clone(), &nested).is_err());
    });
    watch_policy_at(path.clone(), &listener).unwrap();
    store
        .set_policy(LockdownPolicy {
            disable_microphone: true,
            ..Default::default()
        })
        .unwrap();
    assert!(PolicyStore::at_path(path).policy().disable_microphone);
}
