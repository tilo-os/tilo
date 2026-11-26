use super::*;
use std::io::Read;

use super::super::{Ownership, Provider, initialize_fresh};
use crate::setup::internal_cache::tests::{FreshDisk, fail};
use crate::setup::internal_download::tests::with_checkpoint_interval;
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    fs::{self, File},
    io::{Seek, SeekFrom},
    path::{Path, PathBuf},
};

thread_local! {
    static ROOT: RefCell<PathBuf> = RefCell::new(PathBuf::new());
    static SETUP_HOOK: RefCell<Option<Box<dyn FnMut(usize, &str) -> Result<(), String>>>> = RefCell::new(None);
}
fn setup_event(index: usize, stage: &str) -> Result<(), String> {
    SETUP_HOOK.with(|h| match h.borrow_mut().as_mut() {
        Some(h) => h(index, stage),
        None => Ok(()),
    })
}
fn fixture_worker(
    root: &Path,
    transport: InternalTransport,
    consumer: impl FnMut(
        &SelectedManifest,
        &Cancellation<'_>,
        &mut dyn HeldReadSeek,
        &mut dyn HeldReadSeek,
    ) -> Result<(), String>
    + Send
    + 'static,
) -> Worker {
    fixture_worker_hook(root, transport, consumer, |_| {})
}
impl Worker {
    pub(in crate::setup) fn fixture_for_test(
        root: &Path,
        transport: InternalTransport,
        consumer: impl FnMut(
            &SelectedManifest,
            &Cancellation<'_>,
            &mut dyn HeldReadSeek,
            &mut dyn HeldReadSeek,
        ) -> Result<(), String>
        + Send
        + 'static,
    ) -> Self {
        fixture_worker(root, transport, consumer)
    }
}
fn fixture_worker_hook(
    root: &Path,
    transport: InternalTransport,
    consumer: impl FnMut(
        &SelectedManifest,
        &Cancellation<'_>,
        &mut dyn HeldReadSeek,
        &mut dyn HeldReadSeek,
    ) -> Result<(), String>
    + Send
    + 'static,
    install: impl FnOnce(Arc<Shared>) + Send + 'static,
) -> Worker {
    let root = root.to_owned();
    Worker::spawn_runner(move |shared| {
        ROOT.with(|r| *r.borrow_mut() = root);
        install(shared.clone());
        with_checkpoint_interval(1024, || worker_loop(shared, transport, consumer));
        SETUP_HOOK.with(|h| *h.borrow_mut() = None);
        fail(None);
    })
    .unwrap()
}
pub(in crate::setup::internal_cache::coordinator) mod fixture {
    use super::*;
    pub(in crate::setup::internal_cache::coordinator) struct CacheRoot {
        path: PathBuf,
    }
    pub(in crate::setup::internal_cache::coordinator) struct Partial<'s> {
        pub provider: FreshDisk<'s>,
        pub file: File,
        selected: &'s SelectedManifest,
        index: usize,
        root: PathBuf,
    }
    pub(in crate::setup::internal_cache::coordinator) struct FinalHandle<'s> {
        file: File,
        selected: &'s SelectedManifest,
        index: usize,
    }
    pub(in crate::setup::internal_cache::coordinator) enum Component<'s> {
        Partial(Partial<'s>),
        Final(FinalHandle<'s>),
    }
    fn id(file: &File) -> [u8; 32] {
        use std::os::unix::fs::MetadataExt;
        let m = file.metadata().unwrap();
        let mut h = Sha256::new();
        h.update(m.dev().to_le_bytes());
        h.update(m.ino().to_le_bytes());
        h.finalize().into()
    }
    fn check(cancel: &AtomicBool) -> Result<(), String> {
        if cancel.load(Ordering::Acquire) {
            Err("Internal transfer cancelled".into())
        } else {
            Ok(())
        }
    }
    fn verify(
        file: &mut File,
        selected: &SelectedManifest,
        index: usize,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        check(cancel)?;
        let c = selected.bundle().component(index).unwrap();
        if file.metadata().unwrap().len() != c.byte_size {
            return Err("fixture final size mismatch".into());
        }
        file.rewind().unwrap();
        let mut h = Sha256::new();
        let mut buffer = [0; 8192];
        loop {
            check(cancel)?;
            let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            h.update(&buffer[..n]);
        }
        if hex::encode(h.finalize()) != c.sha256 {
            return Err("fixture final digest mismatch".into());
        }
        check(cancel)
    }
    impl CacheRoot {
        pub(in crate::setup::internal_cache::coordinator) fn acquire(
            _: &SelectedManifest,
        ) -> Result<Self, String> {
            Ok(Self {
                path: ROOT.with(|r| r.borrow().clone()),
            })
        }
        pub(in crate::setup::internal_cache::coordinator) fn bootstrap<'s>(
            self,
            selected: &'s SelectedManifest,
            cancel: &AtomicBool,
        ) -> Result<[Component<'s>; 2], String> {
            let mut components = Vec::new();
            for index in 0..2 {
                check(cancel)?;
                let root = self.path.join(index.to_string());
                if !root.exists() {
                    fs::create_dir(&root).unwrap();
                }
                if root.join("final").exists() {
                    let mut file = File::open(root.join("final")).map_err(|e| e.to_string())?;
                    verify(&mut file, selected, index, cancel)?;
                    components.push(Component::Final(FinalHandle {
                        file,
                        selected,
                        index,
                    }));
                    continue;
                }
                setup_event(index, "before-create")?;
                check(cancel)?;
                let leaf = ".tilo-part-0123456789abcdef0123456789abcdef";
                let (mut provider, mut file) = if root.join(leaf).exists() {
                    if !root.join("journal").exists()
                        || fs::read_dir(root.join("journal")).unwrap().next().is_none()
                    {
                        return Err("unknown fixture residue".into());
                    }
                    let own = Ownership {
                        parent: id(&File::open(&root).unwrap()),
                        file: id(&File::open(root.join(leaf)).unwrap()),
                        leaf: leaf.into(),
                    };
                    FreshDisk::reopen(&root, selected, index, own)
                } else {
                    let (mut provider, mut file) = FreshDisk::create(&root, selected, index);
                    setup_event(index, "after-create")?;
                    initialize_fresh(selected, index, &mut &mut provider, &mut file)?;
                    setup_event(index, "after-initialize")?;
                    check(cancel)?;
                    (provider, file)
                };
                let own = (&mut provider).ownership();
                (&mut provider).verify(&file, &own)?;
                file.rewind().unwrap();
                components.push(Component::Partial(Partial {
                    provider,
                    file,
                    selected,
                    index,
                    root,
                }));
            }
            components
                .try_into()
                .map_err(|_| "fixture component count".into())
        }
    }
    impl<'s> Partial<'s> {
        pub(in crate::setup::internal_cache::coordinator) fn publish(
            mut self,
            cancel: &AtomicBool,
        ) -> Result<FinalHandle<'s>, String> {
            check(cancel)?;
            setup_event(self.index, "publish")?;
            verify(&mut self.file, self.selected, self.index, cancel)?;
            let own = (&mut self.provider).ownership();
            (&mut self.provider).verify(&self.file, &own)?;
            let final_path = self.root.join("final");
            if final_path.exists() {
                return Err("fixture final collision retained".into());
            }
            fs::rename(self.root.join(own.leaf), &final_path).map_err(|e| e.to_string())?;
            drop(self.file);
            let mut file = File::open(final_path).map_err(|e| e.to_string())?;
            verify(&mut file, self.selected, self.index, cancel)?;
            Ok(FinalHandle {
                file,
                selected: self.selected,
                index: self.index,
            })
        }
        pub(in crate::setup::internal_cache::coordinator) fn discard(
            mut self,
            operation: u64,
            cancel: &AtomicBool,
        ) -> Result<(), String> {
            {
                let mut s = Session::new(self.selected, self.index, operation, &mut self.provider)?;
                drop(s.prepare(self.selected, &mut self.file, cancel)?);
            }
            check(cancel)?;
            let own = (&mut self.provider).ownership();
            (&mut self.provider).verify(&self.file, &own)?;
            fs::remove_file(self.root.join(own.leaf)).map_err(|e| e.to_string())?;
            (&mut self.provider).retain(&[])
        }
    }
    impl FinalHandle<'_> {
        pub(in crate::setup::internal_cache::coordinator) fn has_retained_residue(&self) -> bool {
            true
        } // journal retained by this fixture
        pub(in crate::setup::internal_cache::coordinator) fn with_verified_reader<T>(
            &mut self,
            cancel: &AtomicBool,
            consume: impl FnOnce(&mut dyn HeldReadSeek) -> Result<T, String>,
        ) -> Result<T, String> {
            let pos = self.file.stream_position().unwrap();
            let result = (|| {
                verify(&mut self.file, self.selected, self.index, cancel)?;
                self.file.rewind().unwrap();
                consume(&mut self.file)
            })();
            let restored = self
                .file
                .seek(SeekFrom::Start(pos))
                .map_err(|e| e.to_string())
                .and_then(|position| setup_event(self.index, "reader-restored").map(|_| position));
            let held = setup_event(self.index, "reader-held");
            let root = setup_event(self.index, "reader-root");
            let stopped = check(cancel);
            super::super::super::finish_held_read(result, restored, held, root, stopped)
        }
    }
}
