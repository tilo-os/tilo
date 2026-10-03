//! `tilo-files` in a real browser: the conformance cases against OPFS and
//! Web Locks, plus what only a page can show (contents surviving a reload,
//! locks between tabs, a lock the page never released).
//!
//! Built for `wasm32-unknown-unknown` and driven by `web/run.mjs`, which
//! loads it in headless Chrome and calls the exports below. Natively this is
//! an empty library.

#![cfg_attr(
    all(target_arch = "wasm32", target_os = "unknown"),
    allow(
        clippy::future_not_send,
        reason = "browser storage handles live on the page's one thread"
    )
)]
#![allow(
    clippy::indexing_slicing,
    clippy::needless_pass_by_value,
    reason = "a test probe: indexing is the assertion, and wasm-bindgen exports take owned values"
)]

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod probe {
    use std::future::Future;
    use std::pin::pin;
    use std::task::Poll;

    use js_sys::{Array, Function, Object, Promise, Reflect};
    use tilo_files::{
        Dir, Entry, Feature, FsError, Kind, Location, LockMode, VPath, VirtualRoot, blocking,
    };
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::JsFuture;

    type Outcome = Result<(), String>;

    macro_rules! check {
        ($cond:expr) => {
            if !$cond {
                return Err(format!("{}:{}: {}", file!(), line!(), stringify!($cond)));
            }
        };
    }

    macro_rules! check_eq {
        ($left:expr, $right:expr) => {{
            let (left, right) = (&$left, &$right);
            if left != right {
                return Err(format!(
                    "{}:{}: {} == {}: {:?} != {:?}",
                    file!(),
                    line!(),
                    stringify!($left),
                    stringify!($right),
                    left,
                    right
                ));
            }
        }};
    }

    fn p(path: &str) -> VPath {
        VPath::new(path).unwrap_or_else(|_| VPath::root())
    }

    fn open(profile: &str, prefix: &str) -> Result<Dir, String> {
        Dir::open(Location::Virtual(
            VirtualRoot::Opfs {
                profile: profile.to_owned(),
            },
            p(prefix),
        ))
        .map_err(|error| format!("open {profile}/{prefix}: {error}"))
    }

    fn fail(error: FsError) -> String {
        format!("{error:?}")
    }

    pub(super) async fn sleep(ms: u32) {
        let promise = Promise::new(&mut |resolve, _| {
            let set_timeout: Function = Reflect::get(&js_sys::global(), &"setTimeout".into())
                .map_or_else(|_| Function::new_no_args(""), Into::into);
            let _ = set_timeout.call2(&JsValue::NULL, &resolve, &JsValue::from(ms));
        });
        let _ = JsFuture::from(promise).await;
    }

    /// `future`'s output if it finishes within `ms`.
    async fn within<F: Future>(ms: u32, future: F) -> Option<F::Output> {
        let mut future = pin!(future);
        let mut timer = pin!(sleep(ms));
        std::future::poll_fn(|cx| {
            if let Poll::Ready(output) = future.as_mut().poll(cx) {
                return Poll::Ready(Some(output));
            }
            if timer.as_mut().poll(cx).is_ready() {
                return Poll::Ready(None);
            }
            Poll::Pending
        })
        .await
    }

    // ---- the conformance cases, as in tests/conformance.rs ----

    async fn writes_replace_whole_contents_and_create_parents(dir: &Dir) -> Outcome {
        let file = p("notes/2026/a.md");
        check_eq!(dir.read(&file).await, Err(FsError::NotFound));
        dir.write_atomic(&file, b"a long first draft")
            .await
            .map_err(fail)?;
        check_eq!(dir.read(&file).await.map_err(fail)?, b"a long first draft");
        dir.write_atomic(&file, b"short").await.map_err(fail)?;
        check_eq!(dir.read(&file).await.map_err(fail)?, b"short");
        dir.write_atomic(&file, b"").await.map_err(fail)?;
        check_eq!(dir.read(&file).await.map_err(fail)?, b"");
        Ok(())
    }

    async fn kinds_in_the_way_are_reported_alike(dir: &Dir) -> Outcome {
        dir.create_dir_all(&p("folder")).await.map_err(fail)?;
        dir.create_dir_all(&p("folder")).await.map_err(fail)?;
        check_eq!(dir.read(&p("folder")).await, Err(FsError::IsADirectory));
        check_eq!(
            dir.write_atomic(&p("folder"), b"x").await,
            Err(FsError::IsADirectory)
        );
        dir.write_atomic(&p("file"), b"x").await.map_err(fail)?;
        check_eq!(
            dir.write_atomic(&p("file/child"), b"y").await,
            Err(FsError::NotADirectory)
        );
        check_eq!(
            dir.read(&p("file/child")).await,
            Err(FsError::NotADirectory)
        );
        check_eq!(dir.list(&p("file")).await, Err(FsError::NotADirectory));
        check_eq!(dir.read(&p("file")).await.map_err(fail)?, b"x");
        Ok(())
    }

    async fn creation_never_replaces(dir: &Dir) -> Outcome {
        let file = p("ids/broker");
        check!(dir.create_new(&file, b"first").await.map_err(fail)?);
        check!(!dir.create_new(&file, b"second").await.map_err(fail)?);
        check_eq!(dir.read(&file).await.map_err(fail)?, b"first");
        dir.create_dir_all(&p("ids/taken")).await.map_err(fail)?;
        check!(!dir.create_new(&p("ids/taken"), b"x").await.map_err(fail)?);
        check_eq!(
            dir.list(&p("ids")).await.map_err(fail)?,
            vec![
                Entry {
                    name: "broker".into(),
                    kind: Kind::File
                },
                Entry {
                    name: "taken".into(),
                    kind: Kind::Dir
                },
            ]
        );
        Ok(())
    }

    async fn renames_never_replace(dir: &Dir) -> Outcome {
        dir.write_atomic(&p("a"), b"a").await.map_err(fail)?;
        dir.write_atomic(&p("b"), b"b").await.map_err(fail)?;
        check_eq!(
            dir.rename_noreplace(&p("a"), &p("b")).await,
            Err(FsError::AlreadyExists)
        );
        check_eq!(dir.read(&p("a")).await.map_err(fail)?, b"a");
        check_eq!(dir.read(&p("b")).await.map_err(fail)?, b"b");
        dir.rename_noreplace(&p("a"), &p("moved/a"))
            .await
            .map_err(fail)?;
        check_eq!(dir.read(&p("a")).await, Err(FsError::NotFound));
        check_eq!(dir.read(&p("moved/a")).await.map_err(fail)?, b"a");
        check_eq!(
            dir.rename_noreplace(&p("a"), &p("c")).await,
            Err(FsError::NotFound)
        );
        dir.write_atomic(&p("tree/sub/leaf"), b"leaf")
            .await
            .map_err(fail)?;
        dir.rename_noreplace(&p("tree"), &p("grown"))
            .await
            .map_err(fail)?;
        check_eq!(dir.read(&p("grown/sub/leaf")).await.map_err(fail)?, b"leaf");
        check_eq!(dir.list(&p("tree")).await, Err(FsError::NotFound));
        check!(matches!(
            dir.rename_noreplace(&p("grown"), &p("grown/inner")).await,
            Err(FsError::InvalidPath(_))
        ));
        check_eq!(
            dir.rename_noreplace(&p("grown"), &p("grown")).await,
            Err(FsError::AlreadyExists)
        );
        Ok(())
    }

    async fn removal_takes_files_and_empty_directories(dir: &Dir) -> Outcome {
        dir.write_atomic(&p("d/f"), b"x").await.map_err(fail)?;
        check_eq!(dir.remove(&p("d")).await, Err(FsError::DirectoryNotEmpty));
        dir.remove(&p("d/f")).await.map_err(fail)?;
        check_eq!(dir.remove(&p("d/f")).await, Err(FsError::NotFound));
        dir.remove(&p("d")).await.map_err(fail)?;
        check_eq!(dir.list(&p("d")).await, Err(FsError::NotFound));
        Ok(())
    }

    async fn listings_are_sorted_with_kinds(dir: &Dir) -> Outcome {
        for name in ["b", "a b", "c/d", "a"] {
            dir.write_atomic(&p(&format!("l/{name}")), b"")
                .await
                .map_err(fail)?;
        }
        let names: Vec<(String, Kind)> = dir
            .list(&p("l"))
            .await
            .map_err(fail)?
            .into_iter()
            .map(|entry| (entry.name, entry.kind))
            .collect();
        check_eq!(
            names,
            [
                ("a".to_owned(), Kind::File),
                ("a b".to_owned(), Kind::File),
                ("b".to_owned(), Kind::File),
                ("c".to_owned(), Kind::Dir),
            ]
        );
        check!(
            dir.list(&VPath::root())
                .await
                .map_err(fail)?
                .iter()
                .any(|entry| entry.name == "l" && entry.kind == Kind::Dir)
        );
        check_eq!(dir.list(&p("missing")).await, Err(FsError::NotFound));
        Ok(())
    }

    async fn metadata_reports_size_kind_and_time(dir: &Dir) -> Outcome {
        let before = tilo_clock::wall()
            .checked_sub(tilo_clock::Duration::from_secs(5))
            .ok_or("clock")?;
        dir.write_atomic(&p("m/file"), b"12345")
            .await
            .map_err(fail)?;
        let file = dir.metadata(&p("m/file")).await.map_err(fail)?;
        check_eq!((file.len, file.kind, file.symlink), (5, Kind::File, false));
        check!(file.modified >= before);
        let folder = dir.metadata(&p("m")).await.map_err(fail)?;
        check_eq!(folder.kind, Kind::Dir);
        check_eq!(dir.metadata(&p("m/none")).await, Err(FsError::NotFound));
        Ok(())
    }

    async fn the_root_is_not_a_file(dir: &Dir) -> Outcome {
        let root = VPath::root();
        for result in [
            dir.write_atomic(&root, b"x").await,
            dir.create_new(&root, b"x").await.map(drop),
            dir.remove(&root).await,
            dir.rename_noreplace(&root, &p("x")).await,
            dir.lock(&root, LockMode::Exclusive).await.map(drop),
        ] {
            check!(matches!(result, Err(FsError::InvalidPath(_))));
        }
        dir.write_atomic(&p("seed"), b"").await.map_err(fail)?;
        check_eq!(dir.metadata(&root).await.map_err(fail)?.kind, Kind::Dir);
        check!(matches!(
            dir.trash(&p("x")).await,
            Err(FsError::Unsupported {
                feature: Feature::Trash,
                ..
            })
        ));
        Ok(())
    }

    async fn locks_exclude_as_their_mode_says(dir: &Dir) -> Outcome {
        let path = p("locks/owner.lock");
        let exclusive = dir.lock(&path, LockMode::Exclusive).await.map_err(fail)?;
        check_eq!(exclusive.mode(), LockMode::Exclusive);
        check_eq!(dir.read(&path).await.map_err(fail)?, b"");
        check!(
            within(200, dir.lock(&path, LockMode::Shared))
                .await
                .is_none()
        );
        drop(exclusive);

        let first = dir.lock(&path, LockMode::Shared).await.map_err(fail)?;
        let second = within(500, dir.lock(&path, LockMode::Shared))
            .await
            .ok_or("shared holders must coexist")?
            .map_err(fail)?;
        check!(
            within(200, dir.lock(&path, LockMode::Exclusive))
                .await
                .is_none()
        );
        drop((first, second));
        check!(
            within(500, dir.lock(&path, LockMode::Exclusive))
                .await
                .is_some_and(|lock| lock.is_ok())
        );
        Ok(())
    }

    async fn an_async_lock_waits_for_release(dir: &Dir) -> Outcome {
        let path = p("wait.lock");
        let held = dir.lock(&path, LockMode::Exclusive).await.map_err(fail)?;
        let waiter = dir.lock(&path, LockMode::Exclusive);
        let mut waiter = pin!(waiter);
        check!(within(150, waiter.as_mut()).await.is_none());
        drop(held);
        let granted = within(1000, waiter)
            .await
            .ok_or("not granted after release")?;
        drop(granted.map_err(fail)?);
        Ok(())
    }

    async fn a_dropped_wait_never_holds_the_lock(dir: &Dir) -> Outcome {
        let path = p("abandon.lock");
        let held = dir.lock(&path, LockMode::Exclusive).await.map_err(fail)?;
        check!(
            within(100, dir.lock(&path, LockMode::Exclusive))
                .await
                .is_none()
        );
        drop(held);
        // The abandoned request is granted and released at once.
        check!(
            within(1000, dir.lock(&path, LockMode::Exclusive))
                .await
                .is_some()
        );
        Ok(())
    }

    async fn blocking_calls_are_unsupported_off_the_mirror(dir: &Dir) -> Outcome {
        let result = blocking::read(dir, &p("x"));
        check!(matches!(
            result,
            Err(FsError::Unsupported {
                feature: Feature::Blocking,
                ..
            })
        ));
        check!(dir.native_path(&p("x")).is_err());
        Ok(())
    }

    async fn large_and_binary_contents_round_trip(dir: &Dir) -> Outcome {
        let bytes: Vec<u8> = (0..(3 * 1024 * 1024 + 17))
            .map(|i: u32| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
            .collect();
        dir.write_atomic(&p("big.bin"), &bytes)
            .await
            .map_err(fail)?;
        check!(dir.read(&p("big.bin")).await.map_err(fail)? == bytes);
        check_eq!(
            dir.metadata(&p("big.bin")).await.map_err(fail)?.len,
            bytes.len() as u64
        );
        Ok(())
    }

    async fn names_beyond_ascii_are_kept(dir: &Dir) -> Outcome {
        let name = "Grüße – 日本語 🗂.md";
        dir.write_atomic(&p(&format!("u/{name}")), "ü".as_bytes())
            .await
            .map_err(fail)?;
        let listed = dir.list(&p("u")).await.map_err(fail)?;
        check_eq!(listed.len(), 1);
        check_eq!(listed[0].name.as_str(), name);
        Ok(())
    }

    async fn profiles_and_prefixes_are_isolated(profile: &str) -> Outcome {
        let one = open(profile, "app/data")?;
        let same = open(profile, "app/data")?;
        let parent = open(profile, "app")?;
        let other = open(profile, "other/data")?;
        let elsewhere = open(&format!("{profile}-b"), "app/data")?;
        one.write_atomic(&p("state.json"), b"{}")
            .await
            .map_err(fail)?;
        check_eq!(same.read(&p("state.json")).await.map_err(fail)?, b"{}");
        check_eq!(
            parent.read(&p("data/state.json")).await.map_err(fail)?,
            b"{}"
        );
        check_eq!(other.read(&p("state.json")).await, Err(FsError::NotFound));
        check_eq!(
            elsewhere.read(&p("state.json")).await,
            Err(FsError::NotFound)
        );
        let _ = remove_profile(&format!("{profile}-b")).await;
        Ok(())
    }

    async fn concurrent_writers_leave_one_whole_value(dir: &Dir) -> Outcome {
        let path = p("race");
        let a = vec![b'a'; 64 * 1024];
        let b = vec![b'b'; 32 * 1024];
        let (first, second) = (dir.write_atomic(&path, &a), dir.write_atomic(&path, &b));
        let mut first = pin!(first);
        let mut second = pin!(second);
        let (mut one, mut two) = (None, None);
        std::future::poll_fn(|cx| {
            if one.is_none()
                && let Poll::Ready(r) = first.as_mut().poll(cx)
            {
                one = Some(r);
            }
            if two.is_none()
                && let Poll::Ready(r) = second.as_mut().poll(cx)
            {
                two = Some(r);
            }
            if one.is_some() && two.is_some() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;
        one.ok_or("first")?.map_err(fail)?;
        two.ok_or("second")?.map_err(fail)?;
        let read = dir.read(&path).await.map_err(fail)?;
        check!(read == a || read == b);
        Ok(())
    }

    pub(super) async fn remove_profile(profile: &str) -> Result<(), JsValue> {
        let storage = Reflect::get(&js_sys::global(), &"navigator".into())
            .and_then(|navigator| Reflect::get(&navigator, &"storage".into()))?;
        let get: Function = Reflect::get(&storage, &"getDirectory".into())?.into();
        let root = JsFuture::from(Promise::from(get.call0(&storage)?)).await?;
        let remove: Function = Reflect::get(&root, &"removeEntry".into())?.into();
        let options = Object::new();
        Reflect::set(&options, &"recursive".into(), &true.into())?;
        let removal = remove.call2(&root, &profile.into(), &options)?;
        match JsFuture::from(Promise::from(removal)).await {
            Ok(_) => Ok(()),
            Err(error)
                if Reflect::get(&error, &"name".into())
                    .ok()
                    .and_then(|name| name.as_string())
                    .as_deref()
                    == Some("NotFoundError") =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    macro_rules! suite {
        ($report:ident, $profile:expr, $($case:ident),* $(,)?) => {
            $(
                let profile = format!("{}-{}", $profile, stringify!($case));
                let outcome = match open(&profile, "app/data") {
                    Ok(dir) => $case(&dir).await,
                    Err(error) => Err(error),
                };
                let _ = remove_profile(&profile).await;
                $report.push((stringify!($case), outcome));
            )*
        };
    }

    fn report(results: Vec<(&'static str, Outcome)>) -> JsValue {
        let passed = Array::new();
        let failed = Array::new();
        for (name, outcome) in results {
            match outcome {
                Ok(()) => {
                    passed.push(&name.into());
                }
                Err(error) => {
                    let entry = Object::new();
                    let _ = Reflect::set(&entry, &"name".into(), &name.into());
                    let _ = Reflect::set(&entry, &"error".into(), &error.into());
                    failed.push(&entry);
                }
            }
        }
        let out = Object::new();
        let _ = Reflect::set(&out, &"passed".into(), &passed);
        let _ = Reflect::set(&out, &"failed".into(), &failed);
        out.into()
    }

    /// Every case against a fresh profile each; `{passed, failed}`.
    #[wasm_bindgen]
    pub async fn run_suite(profile: String) -> JsValue {
        let mut results: Vec<(&'static str, Outcome)> = Vec::new();
        suite!(
            results,
            profile,
            writes_replace_whole_contents_and_create_parents,
            kinds_in_the_way_are_reported_alike,
            creation_never_replaces,
            renames_never_replace,
            removal_takes_files_and_empty_directories,
            listings_are_sorted_with_kinds,
            metadata_reports_size_kind_and_time,
            the_root_is_not_a_file,
            locks_exclude_as_their_mode_says,
            an_async_lock_waits_for_release,
            a_dropped_wait_never_holds_the_lock,
            blocking_calls_are_unsupported_off_the_mirror,
            large_and_binary_contents_round_trip,
            names_beyond_ascii_are_kept,
            concurrent_writers_leave_one_whole_value,
        );
        let isolation = format!("{profile}-isolation");
        results.push((
            "profiles_and_prefixes_are_isolated",
            profiles_and_prefixes_are_isolated(&isolation).await,
        ));
        let _ = remove_profile(&isolation).await;
        report(results)
    }

    const PERSISTED: &[(&str, &[u8])] = &[
        ("notes/a.md", b"# first page\n"),
        ("notes/nested/b.md", "zweite Seite – ü".as_bytes()),
        ("state.json", b"{\"open\":[1,2,3]}"),
    ];

    /// Write the files a reload must keep, and leave a lock the page never
    /// releases.
    #[wasm_bindgen]
    pub async fn seed(profile: String) -> JsValue {
        let outcome: Outcome = async {
            let dir = open(&profile, "app/data")?;
            for (path, bytes) in PERSISTED {
                dir.write_atomic(&p(path), b"to be replaced")
                    .await
                    .map_err(fail)?;
                dir.write_atomic(&p(path), bytes).await.map_err(fail)?;
            }
            dir.write_atomic(&p("gone"), b"x").await.map_err(fail)?;
            dir.remove(&p("gone")).await.map_err(fail)?;
            dir.rename_noreplace(&p("notes/nested"), &p("notes/moved"))
                .await
                .map_err(fail)?;
            let lock = dir
                .lock(&p("held.lock"), LockMode::Exclusive)
                .await
                .map_err(fail)?;
            std::mem::forget(lock);
            Ok(())
        }
        .await;
        report(vec![("seed", outcome)])
    }

    /// After a reload: the seeded files are there, and the lock the previous
    /// page held is free.
    #[wasm_bindgen]
    pub async fn verify(profile: String) -> JsValue {
        let outcome: Outcome = async {
            let dir = open(&profile, "app/data")?;
            for (path, bytes) in PERSISTED {
                let path = path.replace("nested", "moved");
                check_eq!(dir.read(&p(&path)).await.map_err(fail)?, bytes.to_vec());
            }
            check_eq!(dir.read(&p("gone")).await, Err(FsError::NotFound));
            check_eq!(dir.list(&p("notes/nested")).await, Err(FsError::NotFound));
            let names: Vec<String> = dir
                .list(&VPath::root())
                .await
                .map_err(fail)?
                .into_iter()
                .map(|entry| entry.name)
                .collect();
            check_eq!(names, ["held.lock", "notes", "state.json"]);
            check!(
                within(2000, dir.lock(&p("held.lock"), LockMode::Exclusive))
                    .await
                    .is_some()
            );
            Ok(())
        }
        .await;
        let _ = remove_profile(&profile).await;
        report(vec![("verify", outcome)])
    }

    thread_local! {
        static HELD: std::cell::RefCell<Vec<(String, tilo_files::Lock)>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    /// Take `path` exclusively and keep it until [`release`], for a second
    /// tab to contend with. Resolves once granted.
    #[wasm_bindgen]
    pub async fn acquire(profile: String, path: String) -> JsValue {
        let outcome: Outcome = async {
            let dir = open(&profile, "app/data")?;
            let lock = dir
                .lock(&p(&path), LockMode::Exclusive)
                .await
                .map_err(fail)?;
            HELD.with_borrow_mut(|held| held.push((path, lock)));
            Ok(())
        }
        .await;
        report(vec![("acquire", outcome)])
    }

    /// Drop the lock [`acquire`] took on `path`.
    #[wasm_bindgen]
    pub fn release(path: &str) -> bool {
        HELD.with_borrow_mut(|held| {
            let before = held.len();
            held.retain(|(held, _)| held != path);
            held.len() != before
        })
    }

    /// Milliseconds until `path` is granted in `mode`, or -1 after `limit`.
    #[wasm_bindgen]
    pub async fn wait_for(profile: String, path: String, shared: bool, limit: u32) -> f64 {
        let Ok(dir) = open(&profile, "app/data") else {
            return -2.0;
        };
        let mode = if shared {
            LockMode::Shared
        } else {
            LockMode::Exclusive
        };
        let start = js_sys::Date::now();
        match within(limit, dir.lock(&p(&path), mode)).await {
            Some(Ok(_lock)) => js_sys::Date::now() - start,
            Some(Err(_)) => -2.0,
            None => -1.0,
        }
    }

    /// Remove a profile's tree.
    #[wasm_bindgen]
    pub async fn cleanup(profile: String) -> Result<(), JsValue> {
        remove_profile(&profile).await
    }

    // ---- the page's mirror and watches ----

    fn changes(watch: &tilo_files::Watch) -> Vec<String> {
        std::iter::from_fn(|| watch.try_next())
            .map(|change| format!("{:?} {}", change.kind, change.path))
            .collect()
    }

    async fn mirror_serves_blocking_calls(profile: &str) -> Outcome {
        let data = open(profile, "app/data")?;
        let cache = open(profile, "app/cache")?;
        data.write_atomic(&p("before/one.txt"), b"one")
            .await
            .map_err(fail)?;
        cache
            .write_atomic(&p("big"), b"cached")
            .await
            .map_err(fail)?;
        check!(blocking::read(&data, &p("before/one.txt")).is_err());
        check!(!tilo_files::web::is_mirrored(profile));

        tilo_files::web::mirror(profile).await.map_err(fail)?;
        check!(tilo_files::web::is_mirrored(profile));
        check_eq!(
            blocking::read(&data, &p("before/one.txt")).map_err(fail)?,
            b"one"
        );
        check!(matches!(
            blocking::read(&cache, &p("big")),
            Err(FsError::Unsupported {
                feature: Feature::Blocking,
                ..
            })
        ));
        check_eq!(cache.read(&p("big")).await.map_err(fail)?, b"cached");
        check!(matches!(
            blocking::try_lock(&data, &p("l"), LockMode::Exclusive),
            Err(FsError::Unsupported { .. })
        ));

        let watch = data.watch(&VPath::root()).map_err(fail)?;
        blocking::write_atomic(&data, &p("a.json"), b"{\"v\":1}").map_err(fail)?;
        blocking::write_atomic(&data, &p("a.json"), b"{\"v\":2}").map_err(fail)?;
        check!(tilo_files::web::pending(profile) > 0);
        check_eq!(
            blocking::read(&data, &p("a.json")).map_err(fail)?,
            b"{\"v\":2}"
        );
        blocking::rename_noreplace(&data, &p("before"), &p("after")).map_err(fail)?;
        blocking::remove(&data, &p("after/one.txt")).map_err(fail)?;
        check!(blocking::create_new(&data, &p("new"), b"n").map_err(fail)?);
        check!(!blocking::create_new(&data, &p("new"), b"x").map_err(fail)?);
        check_eq!(
            changes(&watch),
            [
                "Changed a.json",
                "Removed before",
                "Changed after",
                "Removed after/one.txt",
                "Changed new"
            ]
        );
        tilo_files::web::flush(profile).await.map_err(fail)?;
        check_eq!(tilo_files::web::pending(profile), 0);

        // Async calls on a mirrored location resolve once in OPFS.
        data.write_atomic(&p("async"), b"a").await.map_err(fail)?;
        check_eq!(tilo_files::web::pending(profile), 0);
        check_eq!(blocking::read(&data, &p("async")).map_err(fail)?, b"a");
        check_eq!(
            data.list(&p("after")).await.map_err(fail)?,
            Vec::<Entry>::new()
        );

        // Unmirrored locations report their async changes too.
        let cache_watch = cache.watch(&VPath::root()).map_err(fail)?;
        cache
            .write_atomic(&p("big"), b"again")
            .await
            .map_err(fail)?;
        check_eq!(changes(&cache_watch), ["Changed big"]);
        Ok(())
    }

    /// The mirror's own checks, then the OPFS tree it wrote, read without a
    /// mirror: after a reload.
    #[wasm_bindgen]
    pub async fn run_mirror_suite(profile: String) -> JsValue {
        let outcome = mirror_serves_blocking_calls(&profile).await;
        report(vec![("mirror_serves_blocking_calls", outcome)])
    }

    /// Mirror `profile` and write through it synchronously, as an app does.
    #[wasm_bindgen]
    pub async fn seed_mirror(profile: String) -> JsValue {
        let outcome: Outcome = async {
            tilo_files::web::mirror(&profile).await.map_err(fail)?;
            let data = open(&profile, "so.tilo.probe/data")?;
            let state = open(&profile, "so.tilo.probe/state")?;
            blocking::write_atomic(&data, &p("vault.json"), b"{\"accounts\":2}").map_err(fail)?;
            blocking::write_atomic(&state, &p("window/layout"), b"wide").map_err(fail)?;
            blocking::write_atomic(&data, &p("drop"), b"x").map_err(fail)?;
            blocking::remove(&data, &p("drop")).map_err(fail)?;
            tilo_files::web::flush(&profile).await.map_err(fail)?;
            Ok(())
        }
        .await;
        report(vec![("seed_mirror", outcome)])
    }

    /// After a reload: OPFS holds what the mirror wrote, and a new mirror
    /// serves it synchronously.
    #[wasm_bindgen]
    pub async fn verify_mirror(profile: String) -> JsValue {
        let outcome: Outcome = async {
            let data = open(&profile, "so.tilo.probe/data")?;
            check_eq!(
                data.read(&p("vault.json")).await.map_err(fail)?,
                b"{\"accounts\":2}"
            );
            check_eq!(data.read(&p("drop")).await, Err(FsError::NotFound));
            tilo_files::web::mirror(&profile).await.map_err(fail)?;
            let state = open(&profile, "so.tilo.probe/state")?;
            check_eq!(
                blocking::read(&state, &p("window/layout")).map_err(fail)?,
                b"wide"
            );
            Ok(())
        }
        .await;
        report(vec![("verify_mirror", outcome)])
    }

    thread_local! {
        static WATCH: std::cell::RefCell<Option<tilo_files::Watch>> =
            const { std::cell::RefCell::new(None) };
    }

    /// Mirror `profile` and watch its probe app's data.
    #[wasm_bindgen]
    pub async fn watch_start(profile: String) -> JsValue {
        let outcome: Outcome = async {
            tilo_files::web::mirror(&profile).await.map_err(fail)?;
            let data = open(&profile, "so.tilo.probe/data")?;
            let watch = data.watch(&VPath::root()).map_err(fail)?;
            WATCH.with_borrow_mut(|slot| *slot = Some(watch));
            Ok(())
        }
        .await;
        report(vec![("watch_start", outcome)])
    }

    /// Write through this tab's mirror and wait for OPFS.
    #[wasm_bindgen]
    pub async fn write_through(profile: String, path: String, text: String) -> JsValue {
        let outcome: Outcome = async {
            tilo_files::web::mirror(&profile).await.map_err(fail)?;
            let data = open(&profile, "so.tilo.probe/data")?;
            blocking::write_atomic(&data, &p(&path), text.as_bytes()).map_err(fail)?;
            tilo_files::web::flush(&profile).await.map_err(fail)
        }
        .await;
        report(vec![("write_through", outcome)])
    }

    /// The changes the watch saw since the last call.
    #[wasm_bindgen]
    pub fn watch_take() -> Vec<String> {
        WATCH.with_borrow(|watch| watch.as_ref().map(changes).unwrap_or_default())
    }

    /// `path` in the probe app's data, synchronously from this tab's mirror.
    #[wasm_bindgen]
    pub fn read_mirrored(profile: String, path: String) -> Option<String> {
        let data = open(&profile, "so.tilo.probe/data").ok()?;
        blocking::read(&data, &p(&path))
            .ok()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }
}
