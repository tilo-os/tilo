#![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

use std::sync::Mutex;

use super::*;

const NAME: &str = "gutenberg-catalogue";

/// An origin serving one file that changes when the test says so, and
/// answering "unchanged" to a request that names its current version.
#[derive(Default)]
struct Stand {
    file: Mutex<(Vec<u8>, u32)>,
    /// The validators each request carried, in order.
    asked: Mutex<Vec<Option<Validator>>>,
}

impl Stand {
    fn serving(bytes: &[u8]) -> Arc<Self> {
        let stand = Self::default();
        *stand.file.lock().unwrap() = (bytes.to_vec(), 1);
        Arc::new(stand)
    }

    fn publish(&self, bytes: &[u8]) {
        let mut file = self.file.lock().unwrap();
        file.0 = bytes.to_vec();
        file.1 = file.1.saturating_add(1);
    }

    fn asked(&self) -> Vec<Option<Validator>> {
        self.asked.lock().unwrap().clone()
    }
}

impl Origin for Stand {
    fn fetch(
        &self,
        url: &'static str,
        since: Option<Validator>,
        limit: u64,
    ) -> BlobFuture<Result<Answer, String>> {
        assert_eq!(url, source(NAME).unwrap().url);
        self.asked.lock().unwrap().push(since.clone());
        let (bytes, version) = self.file.lock().unwrap().clone();
        let current = Validator {
            etag: Some(format!("\"v{version}\"")),
            last_modified: None,
        };
        let answer = if since.as_ref() == Some(&current) {
            Ok(Answer::Unchanged)
        } else if bytes.len() as u64 > limit {
            Err("too large".into())
        } else {
            Ok(Answer::Changed {
                bytes,
                validator: current,
            })
        };
        Box::pin(async move { answer })
    }
}

struct Device {
    content: Arc<PublicContent>,
    files: Arc<FileArea>,
    runtime: tokio::runtime::Runtime,
    dir: tempfile::TempDir,
}

fn device(origin: Arc<Stand>, budget: u64) -> Device {
    let dir = tempfile::tempdir().unwrap();
    let files = Arc::new(FileArea::open(&dir.path().join("area"), budget).unwrap());
    let content = PublicContent::open(dir.path(), Arc::clone(&files), origin).unwrap();
    Device {
        content,
        files,
        runtime: tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap(),
        dir,
    }
}

impl Device {
    /// Let the fetch a keep or a read started run to its end. Outside a
    /// runtime they start none, so the test runs it here.
    fn arrive(&self) -> bool {
        self.runtime
            .block_on(self.content.fetch(source(NAME).unwrap(), SystemTime::now()))
    }

    /// A refresh run `days` from now.
    fn refresh_in(&self, days: u64) -> u32 {
        let then = SystemTime::now()
            .checked_add(Duration::from_secs(days.saturating_mul(24 * 60 * 60)))
            .unwrap();
        self.runtime.block_on(self.content.refresh_at(then))
    }

    fn read_all(&self) -> PublicRead {
        self.content.read(NAME, 0, u32::MAX).unwrap()
    }
}

fn hash_of(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[test]
fn a_released_copy_stays_until_the_budget_needs_its_room_and_comes_back_on_the_next_read() {
    let catalogue = b"Text#,Title\n1,The Declaration of Independence\n".to_vec();
    let origin = Stand::serving(&catalogue);
    let device = device(Arc::clone(&origin), u64::MAX);

    assert_eq!(
        device.read_all().outcome,
        ReadOutcome::Unavailable,
        "nobody keeps it yet"
    );
    device.content.keep(NAME, "so.tilo.ebook").unwrap();
    assert_eq!(device.read_all().outcome, ReadOutcome::Pending);
    assert!(device.arrive());
    let read = device.read_all();
    assert_eq!(
        read.outcome,
        ReadOutcome::Ready {
            bytes: catalogue.clone(),
            total_len: catalogue.len() as u64,
        }
    );
    assert_eq!(read.hash.as_deref(), Some(hash_of(&catalogue).as_str()));

    // Another app keeping it shares the copy: nothing is fetched again.
    device.content.keep(NAME, "so.tilo.search").unwrap();
    assert_eq!(origin.asked().len(), 1);
    device.content.release(NAME, "so.tilo.ebook").unwrap();
    assert!(matches!(
        device.read_all().outcome,
        ReadOutcome::Ready { .. }
    ));

    // The last release removes nothing: the copy is read again without a
    // fetch, as when Books closes and opens again.
    device.content.release(NAME, "so.tilo.search").unwrap();
    assert_eq!(
        device.read_all().outcome,
        ReadOutcome::Ready {
            bytes: catalogue.clone(),
            total_len: catalogue.len() as u64,
        }
    );
    assert_eq!(origin.asked().len(), 1);

    // When the budget needs its room it goes like any cached content, and
    // the next read fetches it again.
    device.files.set_budget(0);
    assert_eq!(device.files.evict_to_budget(&[]), catalogue.len() as u64);
    device.files.set_budget(u64::MAX);
    assert_eq!(device.read_all().outcome, ReadOutcome::Pending);
    assert!(!device.arrive(), "the same copy came back");
    assert_eq!(origin.asked().len(), 2);
    assert!(matches!(
        device.read_all().outcome,
        ReadOutcome::Ready { ref bytes, .. } if *bytes == catalogue
    ));
}

#[test]
fn a_refresh_asks_whether_the_copy_changed_and_moves_the_name_only_once_the_new_copy_is_whole() {
    let first = b"Text#,Title\n1,First edition\n".to_vec();
    let origin = Stand::serving(&first);
    let device = device(Arc::clone(&origin), u64::MAX);
    device.content.keep(NAME, "so.tilo.ebook").unwrap();
    assert!(device.arrive());

    // Its origin answered just now: not asked again within the week.
    assert_eq!(device.refresh_in(6), 0);
    assert_eq!(origin.asked().len(), 1);

    // Unchanged at the origin: asked with what it said last, nothing
    // downloaded, the copy kept.
    assert_eq!(device.refresh_in(7), 0);
    assert_eq!(
        origin.asked().last().cloned().flatten(),
        Some(Validator {
            etag: Some("\"v1\"".into()),
            last_modified: None,
        })
    );
    assert_eq!(
        device.read_all().hash.as_deref(),
        Some(hash_of(&first).as_str())
    );

    // Changed: the name moves to the new copy and the old one goes.
    let second = b"Text#,Title\n1,First edition\n2,Second book\n".to_vec();
    origin.publish(&second);
    assert_eq!(
        device.refresh_in(6),
        0,
        "the unchanged answer holds a week too"
    );
    assert_eq!(device.refresh_in(14), 1);
    let read = device.read_all();
    assert_eq!(read.hash.as_deref(), Some(hash_of(&second).as_str()));
    assert!(matches!(read.outcome, ReadOutcome::Ready { ref bytes, .. } if *bytes == second));
    let old = area_key(&hash_of(&first)).unwrap();
    assert!(device.files.entry(&old).is_none(), "the replaced copy went");

    // The pointer survives a restart.
    let reopened = PublicContent::open(
        device.dir.path(),
        Arc::clone(&device.files),
        Arc::clone(&origin) as Arc<dyn Origin>,
    )
    .unwrap();
    assert_eq!(
        reopened.read(NAME, 0, 8).unwrap().hash.as_deref(),
        Some(hash_of(&second).as_str())
    );
}

#[test]
fn an_evicted_copy_reads_as_on_its_way_and_is_fetched_again_from_where_it_came_from() {
    let catalogue = vec![b'x'; 300];
    let origin = Stand::serving(&catalogue);
    let device = device(Arc::clone(&origin), u64::MAX);
    device.content.keep(NAME, "so.tilo.ebook").unwrap();
    assert!(device.arrive());

    // The budget evicts it like any cached content.
    device.files.set_budget(0);
    assert_eq!(device.files.evict_to_budget(&[]), 300);
    let read = device.read_all();
    assert_eq!(read.outcome, ReadOutcome::Pending);
    assert_eq!(
        read.hash.as_deref(),
        Some(hash_of(&catalogue).as_str()),
        "the name still says which copy"
    );

    // Fetched again whole, not conditionally: its bytes are gone.
    device.files.set_budget(u64::MAX);
    device.arrive();
    assert_eq!(origin.asked().last().cloned().flatten(), None);
    assert!(
        matches!(device.read_all().outcome, ReadOutcome::Ready { ref bytes, .. } if *bytes == catalogue)
    );
}

#[test]
fn only_known_public_files_are_kept() {
    let device = device(Stand::serving(b"x"), u64::MAX);
    assert!(
        device
            .content
            .keep("https://example.org/anything", "app")
            .is_err()
    );
    assert!(device.content.read("tiles", 0, 8).is_err());
}
