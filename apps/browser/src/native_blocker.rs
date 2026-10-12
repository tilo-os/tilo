//! Native Chromium filter-list loading, separate from `WebKit` JSON conversion.

use std::path::Path;
use std::sync::Arc;
use ui::webview::{SharedEngine, WebViewCommand};

// Small first-run safety net authored for Tilo. Full maintained lists are
// loaded from cache and refreshed below; this is not advertised as a full list.
const BASELINE: &str = "! Tilo first-run baseline\n||doubleclick.net^\n||googlesyndication.com^\n||googleadservices.com^\n||adnxs.com^\n||adsrvr.org^\n||scorecardresearch.com^\n||outbrain.com^\n||taboola.com^\n";
const MAX_LIST: u64 = 8 * 1024 * 1024;
const MAX_TOTAL: usize = 30 * 1024 * 1024;

fn append_bounded(rules: &mut String, text: &str) {
    if rules.len().saturating_add(text.len()).saturating_add(1) <= MAX_TOTAL {
        rules.push('\n');
        rules.push_str(text);
    }
}

fn cached_rules(directory: &Path) -> String {
    let mut rules = BASELINE.to_string();
    for (name, _) in crate::blocker::LISTS {
        let path = directory.join(name);
        if std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() <= MAX_LIST)
            && let Some(text) = crate::blocker::read_cached_list(&path)
        {
            append_bounded(&mut rules, &text);
        }
    }
    rules
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Status {
    pub busy: bool,
    pub message: String,
    pub updated: Option<u64>,
}
impl Default for Status {
    fn default() -> Self {
        Self {
            busy: false,
            message: "Using the built-in filter list".into(),
            updated: None,
        }
    }
}
fn state() -> &'static std::sync::Mutex<Status> {
    static STATE: std::sync::OnceLock<std::sync::Mutex<Status>> = std::sync::OnceLock::new();
    STATE.get_or_init(Default::default)
}
pub(crate) fn status() -> Status {
    state()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}
fn fetch_rules(
    directory: &Path,
    lists: &[(String, String)],
    force: bool,
) -> Result<(String, bool), String> {
    use std::io::Read;
    let client = tilo_net::http::blocking_client()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let mut rules = BASELINE.to_string();
    let mut downloads = Vec::new();
    for (name, url) in lists {
        let path = directory.join(name);
        let fresh = !force
            && std::fs::metadata(&path).ok().is_some_and(|m| {
                m.len() <= MAX_LIST
                    && m.modified()
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|a| a.as_secs() < 604_800)
            });
        let text = if fresh {
            crate::blocker::read_cached_list(&path).ok_or_else(|| format!("Cannot read {name}"))?
        } else {
            let response = client
                .get(url)
                .send()
                .and_then(reqwest::blocking::Response::error_for_status)
                .map_err(|_| format!("Could not download {name}"))?;
            let mut text = String::new();
            response
                .take(MAX_LIST + 1)
                .read_to_string(&mut text)
                .map_err(|_| format!("Invalid {name}"))?;
            if text.len() > MAX_LIST as usize
                || text.trim().is_empty()
                || text.trim_start().starts_with('<')
            {
                return Err(format!("Invalid {name}"));
            }
            downloads.push((path, text.clone()));
            text
        };
        if rules.len() + text.len() + 1 > MAX_TOTAL {
            return Err("Combined lists exceed limit".into());
        }
        append_bounded(&mut rules, &text);
    }
    // No cache is replaced until the whole requested set downloaded correctly.
    if !downloads.is_empty() {
        let _private_write = crate::storage::private_root_write_guard(directory)?;
        std::fs::create_dir_all(directory).map_err(|_| "Cannot save filter cache")?;
        for (path, text) in &downloads {
            tilo_files::native::atomic::write(path, text.as_bytes())
                .map_err(|_| "Cannot save filter cache")?;
        }
        let updated = ui::time::try_unix_now();
        let _ = tilo_files::native::atomic::write(
            &directory.join("native-filter-updated.json"),
            &serde_json::to_vec(&updated).unwrap_or_default(),
        );
    }
    Ok((rules, !downloads.is_empty()))
}
fn begin_refresh(status: &std::sync::Mutex<Status>) -> bool {
    let mut status = status
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if status.busy {
        return false;
    }
    status.busy = true;
    status.message = "Updating filter lists…".into();
    true
}
fn last_successful_update(directory: &Path) -> Option<u64> {
    let path = directory.join("native-filter-updated.json");
    std::fs::metadata(&path)
        .ok()
        .filter(|m| m.len() <= 256)
        .and_then(|_| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}
pub(crate) fn refresh(engine: &SharedEngine, force: bool) {
    if !begin_refresh(state()) {
        return;
    }
    // Restore independently of cache freshness or network success. A failed
    // startup refresh must not erase the previous successful download time.
    if let Some(directory) = crate::blocker::cache_dir() {
        let mut status = state()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        status.updated = status
            .updated
            .or_else(|| last_successful_update(&directory));
    }
    let engine = Arc::downgrade(engine);
    let wake = ui::WakeHandle::new();
    let worker = std::thread::Builder::new()
        .name("chromium-filter-loader".into())
        .spawn(move || {
            let directory = crate::blocker::cache_dir();
            // Test endpoints are opt-in and restricted to explicitly isolated app roots.
            let endpoint = crate::storage::explicit_root()
                .and_then(|_| std::env::var("TILO_BROWSER_FILTER_ENDPOINT").ok())
                .filter(|s| {
                    url::Url::parse(s).is_ok_and(|u| {
                        matches!(u.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
                    })
                });
            let lists = crate::blocker::LISTS
                .iter()
                .map(|(name, url)| {
                    (
                        name.to_string(),
                        endpoint.clone().unwrap_or_else(|| url.to_string()),
                    )
                })
                .collect::<Vec<_>>();
            let result = directory
                .as_ref()
                .ok_or_else(|| "Filter cache unavailable".to_string())
                .and_then(|d| fetch_rules(d, &lists, force));
            let mut status = state()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            status.busy = false;
            match result {
                Ok((rules, downloaded)) => {
                    if let Some(engine) = engine.upgrade() {
                        engine.send(WebViewCommand::SetNativeFilterRules {
                            rules: Arc::new(rules),
                            enabled: None,
                        });
                    }
                    if downloaded {
                        status.updated = ui::time::try_unix_now();
                    }
                    status.message = if downloaded {
                        "Filter lists updated"
                    } else {
                        "Using cached filter lists"
                    }
                    .into();
                }
                Err(error) => {
                    let error = error.chars().take(512).collect::<String>();
                    status.message = format!("Update failed: {error}. Previous rules retained.");
                }
            }
            wake.wake();
        });
    if worker.is_err() {
        let mut status = state()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        status.busy = false;
        status.message = "Update failed: worker unavailable. Previous rules retained.".into();
    }
}
/// Enqueue cached rules synchronously, before initial navigation.
#[allow(
    clippy::needless_pass_by_value,
    reason = "matches the shared blocker startup interface"
)]
pub(crate) fn start(engine: SharedEngine) {
    let initial = crate::blocker::cache_dir().map_or_else(
        || BASELINE.to_string(),
        |directory| cached_rules(&directory),
    );
    engine.send(WebViewCommand::SetNativeFilterRules {
        rules: Arc::new(initial),
        enabled: Some(true),
    });
    refresh(&engine, false);
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    #[ignore = "child entry point for private refresh/root lifecycle regression"]
    #[cfg(unix)]
    fn private_refresh_does_not_recreate_root_after_helper_exit_child() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::path::PathBuf::from(std::env::var_os("TILO_TEST_PRIVATE_ROOT").unwrap());
        crate::storage::configure(root.to_str()).unwrap();
        let fixture = tempfile::tempdir().unwrap();
        let trigger = fixture.path().join("exit-helper");
        let host = fixture.path().join("private-helper");
        let ready = serde_json::to_vec(&serde_json::json!({
            "event": "ready",
            "cef_version": "152",
            "chromium_version": "152"
        }))
        .unwrap();
        let mut framed = Vec::with_capacity(ready.len() + 5);
        framed.extend_from_slice(&u32::try_from(ready.len() + 1).unwrap().to_le_bytes());
        framed.push(1);
        framed.extend_from_slice(&ready);
        let mut encoded = String::new();
        for byte in framed {
            std::fmt::Write::write_fmt(&mut encoded, format_args!("\\{byte:03o}")).unwrap();
        }
        std::fs::write(
            &host,
            format!(
                "#!/bin/sh\nprintf '{encoded}'\nwhile [ ! -e '{}' ]; do sleep 0.01; done\nexit 9\n",
                trigger.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o755)).unwrap();
        let engine = ui::webview::chromium::ChromiumEngine::with_private_host(&host, &root).unwrap()
            as SharedEngine;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/filters", listener.local_addr().unwrap());
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_BROWSER_FILTER_ENDPOINT", endpoint);
        let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            for index in 0..crate::blocker::LISTS.len() {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 4096];
                let _ = stream.read(&mut request);
                if index == 0 {
                    accepted_tx.send(()).unwrap();
                    release_rx
                        .recv_timeout(tilo_test_env::hang_bound())
                        .unwrap();
                }
                let body = b"||private-refresh.fixture^\n";
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(body).unwrap();
            }
        });

        refresh(&engine, true);
        accepted_rx
            .recv_timeout(tilo_test_env::hang_bound())
            .unwrap();
        std::fs::write(&trigger, b"exit").unwrap();
        let removal_deadline = tilo_test_env::hang_deadline();
        while root.exists() && ui::time::now() < removal_deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!root.exists(), "helper exit must remove the private root");
        release_tx.send(()).unwrap();
        let worker_deadline = tilo_test_env::hang_deadline();
        while status().busy && ui::time::now() < worker_deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!status().busy, "delayed refresh worker did not finish");
        server.join().unwrap();
        assert!(
            !root.exists(),
            "completed refresh must not recreate a cleaned private root"
        );
        drop(engine);
    }

    /// Baseline protection, cached lists, refresh failures, concurrency and private root cleanup.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn native_rules_survive_cache_failures_and_admit_one_refresh() {
        // Phase `private_refresh_does_not_recreate_root_after_helper_exit`.
        #[cfg(unix)]
        {
            let root = crate::storage::allocate_private_root().unwrap();
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "native_blocker::tests::private_refresh_does_not_recreate_root_after_helper_exit_child",
                    "--ignored",
                    "--nocapture",
                ])
                .env("TILO_TEST_PRIVATE_ROOT", &root)
                .status()
                .unwrap();
            let remained = root.exists();
            if remained {
                std::fs::remove_dir_all(&root).unwrap();
            }
            assert!(result.success());
            assert!(!remained, "child left or recreated its private root");
        }

        // Phase `baseline_protection_exists_without_network_or_cache`.
        {
            let dir = tempfile::tempdir().unwrap();
            let rules = cached_rules(dir.path());
            assert!(rules.contains("||doubleclick.net^"));
            assert!(rules.contains("||googlesyndication.com^"));
        }

        // Phase `reads_known_cached_lists_and_ignores_other_files`.
        {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("easylist.txt"), "||ads.fixture.test^").unwrap();
            std::fs::write(dir.path().join("unrelated.txt"), "must-not-load").unwrap();
            let rules = cached_rules(dir.path());
            assert!(rules.contains("||ads.fixture.test^"));
            assert!(!rules.contains("must-not-load"));
        }

        // Phase `refresh_cache_failure_and_retry_preserve_valid_rules`.
        {
            let directory = tempfile::tempdir().unwrap();
            std::fs::write(directory.path().join("fixture.txt"), "||cached.test^").unwrap();
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/list", listener.local_addr().unwrap());
            let lists = vec![("fixture.txt".into(), url)];
            let cached = fetch_rules(directory.path(), &lists, false).unwrap();
            assert!(!cached.1);
            assert!(cached.0.contains("cached.test"));
            let server = std::thread::spawn(move || {
                for response in [
                    "HTTP/1.1 503 Failed\r\nContent-Length: 0\r\n\r\n",
                    "HTTP/1.1 200 OK\r\nContent-Length: 13\r\n\r\n||fresh.test^",
                ] {
                    let (mut stream, _) = listener.accept().unwrap();
                    let mut bytes = [0; 4096];
                    let _ = stream.read(&mut bytes);
                    stream.write_all(response.as_bytes()).unwrap();
                }
            });
            assert!(fetch_rules(directory.path(), &lists, true).is_err());
            assert_eq!(
                std::fs::read_to_string(directory.path().join("fixture.txt")).unwrap(),
                "||cached.test^"
            );
            let refreshed = fetch_rules(directory.path(), &lists, true).unwrap();
            assert!(refreshed.1);
            assert!(refreshed.0.contains("fresh.test"));
            server.join().unwrap();
        }

        // Phase `concurrent_refresh_requests_admit_only_one_worker`.
        {
            let state = Arc::new(std::sync::Mutex::new(Status::default()));
            #[allow(
                clippy::needless_collect,
                reason = "spawn all workers before joining any, preserving the concurrent admission race"
            )]
            let threads = (0..16)
                .map(|_| {
                    let state = state.clone();
                    std::thread::spawn(move || begin_refresh(&state))
                })
                .collect::<Vec<_>>();
            assert_eq!(
                threads
                    .into_iter()
                    .map(|t| usize::from(t.join().unwrap()))
                    .sum::<usize>(),
                1
            );
        }
    }

    #[test]
    fn offline_restart_restores_last_successful_update() {
        const LAST_SUCCESS: u64 = 1_700_000_123;
        if std::env::var_os("TILO_TEST_FILTER_RESTART_CHILD").is_some() {
            crate::storage::configure(None).unwrap();
            let engine: SharedEngine = ui::webview::test_engine::TestEngine::new();
            start(engine.clone());
            let deadline = tilo_test_env::hang_deadline();
            while status().busy && ui::time::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let result = status();
            assert!(!result.busy);
            assert!(result.message.starts_with("Update failed:"), "{result:?}");
            assert_eq!(
                result.updated,
                Some(LAST_SUCCESS),
                "offline restart must retain persisted success time"
            );
            let directory = crate::blocker::cache_dir().unwrap();
            assert!(cached_rules(&directory).contains("cached.fixture.test"));
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("cache/filters");
        std::fs::create_dir_all(&directory).unwrap();
        for (name, _) in crate::blocker::LISTS {
            let path = directory.join(name);
            std::fs::write(&path, "||cached.fixture.test^").unwrap();
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(ui::time::UNIX_EPOCH))
                .unwrap();
        }
        std::fs::write(
            directory.join("native-filter-updated.json"),
            LAST_SUCCESS.to_string(),
        )
        .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/offline", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request);
            stream
                .write_all(b"HTTP/1.1 503 Offline\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        });
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native_blocker::tests::offline_restart_restores_last_successful_update",
                "--nocapture",
            ])
            .env("TILO_TEST_FILTER_RESTART_CHILD", "1")
            .env("TILO_BROWSER_APP_ROOT", root.path())
            .env("TILO_BROWSER_FILTER_ENDPOINT", endpoint)
            .output()
            .unwrap();
        server.join().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        // libtest exits 0 when `--exact` selects nothing; require the child ran.
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed"),
            "the child ran no test: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}
