use tilo_install_artifacts::{Client, PreferenceState};
#[test]
fn preference_surface_keeps_opaque_bytes_and_resume_separate() {
    let _: fn(&Client) -> PreferenceState = Client::preference;
    let _: fn(
        &Client,
        tilo_install_artifacts::ReleaseSelection,
        Vec<u8>,
    ) -> Result<tilo_install_artifacts::Token, tilo_install_artifacts::Error> = Client::start;
}
#[cfg(all(unix, feature = "test-support"))]
mod worker {
    use super::*;
    use std::time::{Duration, Instant};
    use tilo_install_artifacts::{
        Phase, Status,
        test_support::{Server, manifest_bytes, manifest_value, response},
    };
    fn wait(client: &Client, phase: Phase) -> Status {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = client.status() {
                if status.phase == phase {
                    return status;
                }
            }
            assert!(
                Instant::now() < until,
                "waiting for {phase:?}: {:?}",
                client.status()
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    #[test]
    fn preference_commit_survives_early_network_failure() {
        let bytes = manifest_bytes(&manifest_value());
        let server = Server::new(vec![response(404, "", b"")]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("selection.json");
        let client = server.client(&dir.path().join("cache"), &path);
        client
            .start(server.selection(&bytes), b"advisory".to_vec())
            .unwrap();
        let failed = wait(&client, Phase::Failed);
        assert!(
            failed.preference_commit.is_some(),
            "save acknowledgement must survive later failure"
        );
        assert!(path.exists());
    }
    fn loaded(client: &Client) -> PreferenceState {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let state = client.preference();
            if state != PreferenceState::Loading {
                return state;
            }
            assert!(Instant::now() < until, "preference load did not settle");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    #[test]
    fn faults_retain_exact_knowledge_and_prevent_network_and_cache_effects() {
        use tilo_install_artifacts::{
            PreferenceNamespace as N, PreferencePublication as P, test_support::PreferenceFault,
        };
        for (fault, publication) in [
            (PreferenceFault::BeforePublish, P::NotPublished),
            (PreferenceFault::BeforeCommit, P::PublishedDurabilityUnknown),
        ] {
            let bytes = manifest_bytes(&manifest_value());
            let server = Server::new(vec![]);
            let dir = tempfile::tempdir().unwrap();
            let cache = dir.path().join("cache");
            let path = dir.path().join("selection.json");
            std::fs::write(&path, b"old").unwrap();
            let (client, observed) = server.client_with_preference_fault(&cache, &path, fault);
            assert_eq!(
                loaded(&client),
                PreferenceState::Loaded(Some(b"old".to_vec()))
            );
            let token = client
                .start(server.selection(&bytes), b"new".to_vec())
                .unwrap();
            assert_eq!(
                observed.recv_timeout(Duration::from_secs(5)).unwrap(),
                fault
            );
            let status = wait(&client, Phase::Failed);
            assert_eq!(status.token, token);
            assert!(status.preference_commit.is_none());
            assert_eq!(status.uncertain, fault == PreferenceFault::BeforeCommit);
            let PreferenceState::Failed(failure) = client.preference() else {
                panic!("missing save failure")
            };
            assert_eq!(failure.publication, publication);
            assert_eq!(failure.namespace, N::Durable);
            assert_eq!(status.failure_kind, Some(failure.kind));
            assert!(server.requests().is_empty());
            assert!(!cache.exists());
            let later = server.client(&cache, &path);
            let expected = if fault == PreferenceFault::BeforePublish {
                b"old"
            } else {
                b"new"
            };
            assert_eq!(
                loaded(&later),
                PreferenceState::Loaded(Some(expected.to_vec()))
            );
        }
    }
    #[test]
    fn cancellation_at_commit_keeps_commit_and_stops_before_authentication() {
        let bytes = manifest_bytes(&manifest_value());
        let server = Server::new(vec![]);
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache");
        let path = dir.path().join("selection.json");
        let (client, observed, release) = server.client_with_preference_hold(&cache, &path);
        let token = client
            .start(server.selection(&bytes), b"new".to_vec())
            .unwrap();
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(client.status().unwrap().phase, Phase::SavingPreference);
        client.cancel(&token).unwrap();
        release.send(()).unwrap();
        let status = wait(&client, Phase::Cancelled);
        assert_eq!(status.token, token);
        assert!(status.preference_commit.is_some());
        assert_eq!(std::fs::read(path).unwrap(), b"new");
        assert!(server.requests().is_empty());
        assert!(!cache.exists());
    }
    #[test]
    fn oversized_admission_has_no_effect_and_resume_never_saves() {
        let bytes = manifest_bytes(&manifest_value());
        let server = Server::new(vec![response(404, "", b"")]);
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache");
        let path = dir.path().join("missing/selection.json");
        let (client, observed) = server.client_with_preference_fault(
            &cache,
            &path,
            tilo_install_artifacts::test_support::PreferenceFault::BeforePublish,
        );
        assert_eq!(loaded(&client), PreferenceState::Loaded(None));
        assert_eq!(
            client
                .start(server.selection(&bytes), vec![0; 4097])
                .unwrap_err()
                .kind(),
            tilo_install_artifacts::ErrorKind::InvalidInput
        );
        assert!(client.status().is_none());
        assert!(!path.parent().unwrap().exists());
        assert!(!cache.exists());
        assert!(server.requests().is_empty());
        client.resume(server.selection(&bytes)).unwrap();
        assert!(wait(&client, Phase::Failed).preference_commit.is_none());
        assert!(!path.parent().unwrap().exists());
        assert!(observed.try_recv().is_err());
        assert_eq!(server.requests().len(), 1);
    }
    #[test]
    fn load_runs_once_without_creating_and_malformed_bytes_remain_opaque() {
        let server = Server::new(vec![]);
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache");
        let path = dir.path().join("selection.json");
        std::fs::write(&path, b"malformed JSON").unwrap();
        let client = server.client(&cache, &path);
        assert_eq!(
            loaded(&client),
            PreferenceState::Loaded(Some(b"malformed JSON".to_vec()))
        );
        std::fs::write(&path, b"changed after startup").unwrap();
        assert_eq!(
            client.preference(),
            PreferenceState::Loaded(Some(b"malformed JSON".to_vec()))
        );
        assert!(!cache.exists());
        assert!(server.requests().is_empty());
    }
}
