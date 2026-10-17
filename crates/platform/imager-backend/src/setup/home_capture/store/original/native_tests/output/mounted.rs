//! Three explicitly owned subjects: primary, participant, and genuine USB output.
use super::*;
use crate::setup::migration_handoff::{self, Categories};

struct Bind(PathBuf);
impl Bind {
    fn new(source: &Path, destination: &Path) -> Self {
        let source = std::ffi::CString::new(source.as_os_str().as_bytes()).unwrap();
        let target = std::ffi::CString::new(destination.as_os_str().as_bytes()).unwrap();
        assert_eq!(
            unsafe {
                libc::mount(
                    source.as_ptr(),
                    target.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND,
                    std::ptr::null(),
                )
            },
            0,
            "{}",
            std::io::Error::last_os_error()
        );
        Self(destination.to_owned())
    }
}
impl Drop for Bind {
    fn drop(&mut self) {
        let target = std::ffi::CString::new(self.0.as_os_str().as_bytes()).unwrap();
        assert_eq!(
            unsafe { libc::umount2(target.as_ptr(), 0) },
            0,
            "exact owned bind retirement: {}",
            std::io::Error::last_os_error()
        );
    }
}
fn owner(path: &Path, uid: u32, gid: u32, mode: u32) {
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(c.as_ptr(), uid, gid) }, 0);
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}
fn subject_disk(path: &Path) -> block_devices::BlockDevice {
    let number = fs::metadata(path).unwrap().dev();
    let number = format!("{}:{}", libc::major(number), libc::minor(number));
    let disks = super::inventory();
    let matches: Vec<_> = disks
        .into_iter()
        .filter(|disk| {
            disk.kind == "disk"
                && disk
                    .children
                    .iter()
                    .any(|part| part.kind == "part" && part.maj_min.as_deref() == Some(&number))
        })
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "actual unique direct GPT subject required"
    );
    let disk = matches.into_iter().next().expect("unique GPT subject");
    assert_eq!(disk.pttype.as_deref(), Some("gpt"));
    assert!(disk.ptuuid.is_some());
    disk
}
pub(super) fn preflight(primary: &Path, target: &crate::imager_model::TargetDrive) {
    let participant = PathBuf::from(
        std::env::var_os("TILO_MOUNTED_PARTICIPANT_GPT_STORE")
            .expect("third genuine owned GPT participant required"),
    );
    marker(
        &participant.join(".tilo-original-source-qualification"),
        b"disposable-original-source-only",
    );
    let primary = subject_disk(primary);
    let participant = subject_disk(&participant);
    assert_ne!(primary.name, participant.name);
    assert_ne!(primary.ptuuid, participant.ptuuid);
    assert_ne!(primary.name, target.dev_path());
    assert_ne!(participant.name, target.dev_path());
    // Genuine USB initializer remains unchanged. Never relabel NBD as USB.
    assert_eq!(target.tran.as_deref(), Some("usb"));
    assert_eq!(target.stable_id.as_deref(), Some("tilo-install-target"));
}
pub(super) struct Subjects {
    directory: PathBuf,
    file: PathBuf,
    binds: Vec<Bind>,
}
impl Subjects {
    pub(super) fn new(namespace: &Path, home: &Path, primary: &Path, output: &Path) -> Self {
        let participant = PathBuf::from(
            std::env::var_os("TILO_MOUNTED_PARTICIPANT_GPT_STORE")
                .expect("third genuine owned GPT participant required"),
        );
        marker(
            &participant.join(".tilo-original-source-qualification"),
            b"disposable-original-source-only",
        );
        let participant_dev = fs::metadata(&participant).unwrap().dev();
        assert_ne!(participant_dev, fs::metadata(primary).unwrap().dev());
        assert_ne!(participant_dev, fs::metadata(output).unwrap().dev());
        let temporary = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o755))
            .tempdir_in(&participant)
            .unwrap();
        let root = temporary.keep();
        let directory = root.join("chosen-directory");
        fs::create_dir(&directory).unwrap();
        owner(&directory, 1003, 1002, 0o750);
        let child = directory.join("group-readable");
        fs::write(&child, b"actual participant directory bytes").unwrap();
        owner(&child, 1003, 1002, 0o640);
        fs::hard_link(&child, directory.join("same-group-readable")).unwrap();
        let file = root.join("chosen-file");
        fs::write(&file, b"actual participant regular-file bytes").unwrap();
        owner(&file, 1003, 1002, 0o640);
        let folder = home.join("mounted-directory");
        fs::create_dir(&folder).unwrap();
        owner(&folder, 1000, 1001, 0o700);
        let leaf = home.join("mounted-file");
        fs::write(
            &leaf,
            b"underlying original file must not replace selected mount",
        )
        .unwrap();
        owner(&leaf, 1000, 1001, 0o600);
        let binds = vec![Bind::new(&directory, &folder), Bind::new(&file, &leaf)];
        assert!(namespace.starts_with(primary));
        Self {
            directory,
            file,
            binds,
        }
    }
    pub(super) fn bind_store(&mut self, store: &Path) {
        let canonical = Path::new("/var/lib/tilo-imager/home-capture");
        assert!(
            !canonical.exists(),
            "qualification namespace must have no ambient source Store"
        );
        fs::create_dir_all(canonical).unwrap();
        fs::set_permissions(canonical, fs::Permissions::from_mode(0o700)).unwrap();
        self.binds.push(Bind::new(store, canonical));
    }
    pub(super) fn geometry(&self, operation: &str) -> Vec<Vec<u8>> {
        // Parent directory geometry also covers the regular-file source disk;
        // exact file inode authority remains in the ACKed mounted witness.
        vec![
            migration_handoff::capture(
                &self.directory,
                operation,
                Categories {
                    home: true,
                    flatpak: false,
                    accounts: false,
                },
            )
            .unwrap()
            .bytes()
            .unwrap(),
        ]
    }
    pub(super) fn partition(&self) -> String {
        let plan = migration_handoff::capture(
            &self.directory,
            &uuid::Uuid::new_v4().to_string(),
            Categories {
                home: true,
                flatpak: false,
                accounts: false,
            },
        )
        .unwrap();
        super::partition(&plan.home.volume.partition_guid)
    }
    pub(super) fn verify_original(&self) {
        assert_eq!(
            fs::read(self.directory.join("group-readable")).unwrap(),
            b"actual participant directory bytes"
        );
        assert_eq!(
            fs::read(&self.file).unwrap(),
            b"actual participant regular-file bytes"
        );
        assert_eq!(fs::metadata(&self.file).unwrap().uid(), 1003);
        assert_eq!(fs::metadata(&self.file).unwrap().gid(), 1002);
        assert_eq!(fs::metadata(&self.file).unwrap().mode() & 0o777, 0o640);
    }
}
impl Drop for Subjects {
    fn drop(&mut self) {
        // Namespace store last, then file and directory. No mount survives the
        // UDisks participant/source unmount or takes authority into export.
        while self.binds.pop().is_some() {}
    }
}
pub(super) fn verify_import(home: &Path) {
    assert_eq!(
        fs::read(home.join("mounted-directory/group-readable")).unwrap(),
        b"actual participant directory bytes"
    );
    assert_eq!(
        fs::read(home.join("mounted-file")).unwrap(),
        b"actual participant regular-file bytes"
    );
    assert_eq!(
        fs::metadata(home.join("mounted-directory/group-readable"))
            .unwrap()
            .ino(),
        fs::metadata(home.join("mounted-directory/same-group-readable"))
            .unwrap()
            .ino()
    );
    assert_eq!(
        fs::metadata(home.join("mounted-file")).unwrap().mode() & 0o777,
        0o640
    );
    assert!(
        fs::symlink_metadata(home.join("mounted-directory"))
            .unwrap()
            .is_dir()
    );
    assert!(
        fs::symlink_metadata(home.join("mounted-file"))
            .unwrap()
            .is_file()
    );
}
