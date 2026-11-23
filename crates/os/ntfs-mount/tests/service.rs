#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    clippy::unnecessary_wraps,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use ntfs_mount::grants::GrantStore;
use ntfs_mount::identity::Identity;
use ntfs_mount::policy::DriveFacts;
use ntfs_mount::protocol::{
    self, Consent, Drive, Mode, Placement, ReadOnlyReason, RememberOffer, Request, Response,
};
use ntfs_mount::service::{Backend, Caller, Candidate, Health, MountRequest, Service, server};
use ntfs_mount::topology::{Controller, Link, Topology};

const UID: u32 = 1000;
const OTHER: u32 = 1001;

#[derive(Default)]
struct World {
    drives: Vec<Candidate>,
    health: Option<ReadOnlyReason>,
    mounted: Vec<(String, Mode, bool, PathBuf)>,
    busy: bool,
    logged_out: bool,
    next_grant: u32,
}

struct Fake(Arc<Mutex<World>>);

impl Backend for Fake {
    type Mount = String;

    fn drives(&mut self) -> Result<Vec<Candidate>, String> {
        Ok(self.0.lock().unwrap().drives.clone())
    }

    fn preflight(&mut self, _candidate: &Candidate) -> Result<Health, String> {
        Ok(Health {
            read_only: self.0.lock().unwrap().health,
        })
    }

    fn mount(&mut self, request: &MountRequest<'_>) -> Result<(PathBuf, String), String> {
        let path = PathBuf::from(format!("/run/media/person/{}", request.candidate.label));
        self.0.lock().unwrap().mounted.push((
            request.candidate.id.clone(),
            request.mode,
            request.read_only,
            path.clone(),
        ));
        Ok((path, request.candidate.id.clone()))
    }

    fn unmount(&mut self, path: &Path, mount: &mut String, gone: bool) -> Result<(), String> {
        let mut world = self.0.lock().unwrap();
        if world.busy && !gone {
            return Err("Files on this drive are still open".into());
        }
        world
            .mounted
            .retain(|(id, _, _, at)| id != mount || at != path);
        Ok(())
    }

    fn alive(&mut self, _mount: &mut String) -> bool {
        true
    }

    fn owner_present(&self, _uid: u32) -> bool {
        !self.0.lock().unwrap().logged_out
    }

    fn new_grant_id(&mut self) -> String {
        let mut world = self.0.lock().unwrap();
        world.next_grant += 1;
        format!("grant-{}", world.next_grant)
    }
}

fn windows_partition() -> Candidate {
    Candidate {
        id: "nvme0n1p3:1".into(),
        name: "nvme0n1p3".into(),
        label: "Windows".into(),
        size_bytes: 500 << 30,
        facts: DriveFacts {
            topology: Topology {
                device_path:
                    "devices/pci0000:00/0000:00:1d.0/0000:3d:00.0/nvme/nvme0/nvme0n1/nvme0n1p3"
                        .into(),
                disk: "nvme0n1".into(),
                controller: Some(Controller::NvmeRootPort),
                hotplug_link: None,
                removable: false,
                virtual_device: false,
            },
            identity: Identity {
                drive: Some("wwn:eui.0025385b".into()),
                partition: Some("6a1f0c2e".into()),
                volume: Some(0x1C8A),
            },
            present_at_boot: true,
            on_install_disk: true,
        },
    }
}

fn forged_stick(seq: u32) -> Candidate {
    let mut stick = windows_partition();
    stick.id = format!("sdb1:{seq}");
    stick.name = "sdb1".into();
    stick.label = "Windows".into();
    stick.facts.topology = Topology {
        device_path: "devices/pci0000:00/0000:00:14.0/usb2/2-1/2-1:1.0/host6/target6:0:0/6:0:0:0/block/sdb/sdb1".into(),
        disk: "sdb".into(),
        controller: None,
        hotplug_link: Some(Link::Usb),
        removable: true,
        virtual_device: false,
    };
    stick.facts.present_at_boot = false;
    stick.facts.on_install_disk = false;
    stick
}

struct Running {
    socket: PathBuf,
    world: Arc<Mutex<World>>,
    caller: Arc<Mutex<Caller>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    _owned: Option<tempfile::TempDir>,
}

/// Socket names in one directory, so a restarted service can share it.
static SOCKETS: AtomicU32 = AtomicU32::new(0);

impl Running {
    fn start(drives: Vec<Candidate>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        Self::build(Some(dir), &path, drives)
    }

    /// A server over `drives`, keeping its grants in `dir`: a service that
    /// starts again over the same directory is the same computer.
    fn at(dir: &Path, drives: Vec<Candidate>) -> Self {
        Self::build(None, dir, drives)
    }

    fn build(owned: Option<tempfile::TempDir>, dir: &Path, drives: Vec<Candidate>) -> Self {
        let socket = dir.join(format!(
            "control-{}.sock",
            SOCKETS.fetch_add(1, Ordering::Relaxed)
        ));
        let listener = UnixListener::bind(&socket).unwrap();
        let world = Arc::new(Mutex::new(World {
            drives,
            ..World::default()
        }));
        let caller = Arc::new(Mutex::new(Caller::Shell { uid: UID }));
        let stop = Arc::new(AtomicBool::new(false));
        let mut service = Service::new(
            Fake(Arc::clone(&world)),
            GrantStore::new(dir.join("grants")),
        );
        let (who, halt) = (Arc::clone(&caller), Arc::clone(&stop));
        let thread = std::thread::spawn(move || {
            server::serve(
                &listener,
                &mut service,
                |_| Ok(*who.lock().unwrap()),
                server::Pacing {
                    housekeeping: Duration::from_millis(50),
                    idle_exit: None,
                },
                &halt,
            );
        });
        Self {
            socket,
            world,
            caller,
            stop,
            thread: Some(thread),
            _owned: owned,
        }
    }

    fn as_shell(&self, request: Request) -> Response {
        *self.caller.lock().unwrap() = Caller::Shell { uid: UID };
        protocol::call(&self.socket, request, tilo_test_env::hang_bound()).unwrap()
    }

    fn as_person(&self, request: Request) -> Response {
        *self.caller.lock().unwrap() = Caller::Person { uid: UID };
        protocol::call(&self.socket, request, tilo_test_env::hang_bound()).unwrap()
    }

    fn as_other(&self, request: Request) -> Response {
        *self.caller.lock().unwrap() = Caller::Person { uid: OTHER };
        protocol::call(&self.socket, request, tilo_test_env::hang_bound()).unwrap()
    }

    fn drive(&self, id: &str) -> Drive {
        let Response::Drives { drives } = self.as_person(Request::Drives) else {
            panic!("no drive list");
        };
        drives.into_iter().find(|drive| drive.id == id).unwrap()
    }

    fn open(&self, id: &str, consent: Option<Consent>) -> Response {
        self.as_shell(Request::Open {
            drive: id.into(),
            consent,
        })
    }

    fn remembered(&self) -> Vec<protocol::RememberedDrive> {
        let Response::Remembered { drives } = self.as_person(Request::Remembered) else {
            panic!("no remembered list");
        };
        drives
    }

    fn wait_until(&self, what: &str, done: impl Fn(&World) -> bool) {
        tilo_test_env::wait_until(what, || done(&self.world.lock().unwrap()));
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The Windows partition's facts, changed in one way, under another id.
fn altered(id: &str, change: impl FnOnce(&mut DriveFacts)) -> Candidate {
    let mut drive = windows_partition();
    drive.id = id.into();
    drive.name = id.split(':').next().unwrap().into();
    change(&mut drive.facts);
    drive
}

fn consent(mode: Mode, remember: bool) -> Option<Consent> {
    Some(Consent { mode, remember })
}

tilo_test_env::scenario! {
    /// The service over its socket, each case on its own server, socket and grant store.
    fn consent_guarded_service;

    phase the_windows_partition_is_asked_about_once_then_opens_lazily;
    {
        let service = Running::start(vec![windows_partition()]);
        let id = "nvme0n1p3:1";

        let drive = service.drive(id);
        assert_eq!(drive.placement, Placement::Internal);
        assert!(
            drive.needs_consent,
            "consent is needed even for the internal disk"
        );
        assert_eq!(
            drive.preselect,
            Mode::Open,
            "pre-selected for the install's own disk"
        );
        assert_eq!(drive.can_remember, protocol::RememberOffer::AnyMode);
        assert!(
            service.world.lock().unwrap().mounted.is_empty(),
            "listing mounts nothing"
        );

        service
            .world
            .lock()
            .unwrap()
            .drives
            .push(altered("nvme7n1p1:1", |facts| facts.on_install_disk = false));
        assert_eq!(
            service.drive("nvme7n1p1:1").preselect,
            Mode::OpenSafely,
            "Open is pre-selected only for the install's own disk"
        );

        assert!(matches!(
            service.open(id, None),
            Response::NeedsConsent { .. }
        ));
        assert!(
            matches!(
                service.as_person(Request::Open {
                    drive: id.into(),
                    consent: consent(Mode::Open, true),
                }),
                Response::Refused { .. }
            ),
            "only the shell, which draws the prompt, may open"
        );
        assert!(service.world.lock().unwrap().mounted.is_empty());

        let Response::Opened { mounted } = service.open(id, consent(Mode::Open, true)) else {
            panic!("consented open failed");
        };
        assert_eq!(mounted.mode, Mode::Open);
        assert_eq!(mounted.read_only, None);
        assert_eq!(service.drive(id).mounted.as_ref(), Some(&mounted));
        assert!(matches!(
            service.as_person(Request::Close { drive: id.into() }),
            Response::Done
        ));
        assert!(service.world.lock().unwrap().mounted.is_empty());

        let drive = service.drive(id);
        assert!(!drive.needs_consent, "remembered: no prompt");
        assert!(drive.mounted.is_none(), "remembered does not mean mounted");
        let Response::Opened { mounted } = service.open(id, None) else {
            panic!("remembered drive did not open on demand");
        };
        assert_eq!(mounted.mode, Mode::Open);
        let remembered = service.remembered();
        assert_eq!(remembered.len(), 1);
        assert_eq!(remembered[0].placement, Placement::Internal);
        assert_eq!(
            remembered[0].mounted.as_deref(),
            Some(mounted.path.as_str())
        );

        let grant = remembered[0].grant.clone();
        assert!(matches!(
            service.as_person(Request::SetMode {
                grant: grant.clone(),
                mode: Mode::OpenSafely
            }),
            Response::Done
        ));
        assert_eq!(service.world.lock().unwrap().mounted[0].1, Mode::Open);
        assert!(matches!(
            service.as_person(Request::Close { drive: id.into() }),
            Response::Done
        ));
        let Response::Opened { mounted } = service.open(id, None) else {
            panic!("reopen failed");
        };
        assert_eq!(mounted.mode, Mode::OpenSafely);

        assert!(matches!(
            service.as_person(Request::Forget { grant }),
            Response::Done
        ));
        assert!(service.world.lock().unwrap().mounted.is_empty());
        assert!(service.remembered().is_empty());
        assert!(service.drive(id).needs_consent);
    }

    phase a_stick_that_copies_the_windows_ids_is_still_asked_about;
    {
        let service = Running::start(vec![windows_partition()]);
        assert!(matches!(
            service.open("nvme0n1p3:1", consent(Mode::Open, true)),
            Response::Opened { .. }
        ));

        service.world.lock().unwrap().drives.push(forged_stick(7));
        let stick = service.drive("sdb1:7");
        assert_eq!(stick.placement, Placement::External);
        assert!(stick.hot_added, "the shell asks as it arrives");
        assert!(stick.needs_consent);
        assert_eq!(stick.preselect, Mode::OpenSafely);
        assert!(
            stick.remembered.is_none(),
            "copied IDs do not inherit the internal grant"
        );
        assert!(matches!(
            service.open("sdb1:7", None),
            Response::NeedsConsent { .. }
        ));

        let Response::Opened { mounted } = service.open("sdb1:7", consent(Mode::Open, true)) else {
            panic!("consented open of the stick failed");
        };
        assert_eq!(mounted.mode, Mode::Open);
        let external = service
            .remembered()
            .into_iter()
            .find(|drive| drive.placement == Placement::External)
            .unwrap();
        assert_eq!(external.mode, Mode::OpenSafely);
        assert!(matches!(
            service.as_person(Request::SetMode {
                grant: external.grant,
                mode: Mode::Open
            }),
            Response::Refused { .. }
        ));

        service
            .world
            .lock()
            .unwrap()
            .drives
            .retain(|drive| drive.id != "sdb1:7");
        service.wait_until("the unplugged stick is closed", |world| {
            world.mounted.iter().all(|(id, ..)| id != "sdb1:7")
        });
        service.world.lock().unwrap().drives.push(forged_stick(8));
        let again = service.drive("sdb1:8");
        assert!(again.needs_consent);
        assert_eq!(again.preselect, Mode::OpenSafely);
        assert!(
            again.remembered.is_some(),
            "remembered, so it pre-selects Open safely"
        );

        let mut another = forged_stick(9);
        another.facts.identity.volume = Some(99);
        service.world.lock().unwrap().drives.push(another);
        let stranger = service.drive("sdb1:9");
        assert!(stranger.needs_consent);
        assert!(
            stranger.remembered.is_none(),
            "only for the stick with the same IDs"
        );
    }

    phase nothing_but_the_motherboard_and_its_own_disk_earns_open;
    {
        let service = Running::start(vec![windows_partition()]);
        assert!(matches!(
            service.open("nvme0n1p3:1", consent(Mode::Open, true)),
            Response::Opened { .. }
        ));

        // The same identifiers at another place, or on other hardware: each
        // asks again, and each is treated as a drive nobody has seen.
        type Change = fn(&mut DriveFacts);
        let cases: [(&str, Placement, Change); 6] = [
            (
                "nvme1n1p3:1",
                Placement::Internal,
                |facts| {
                    facts.topology.device_path =
                        "devices/pci0000:00/0000:00:1c.4/0000:3e:00.0/nvme/nvme1/nvme1n1/nvme1n1p3"
                            .into();
                },
            ),
            ("nvme0n1p4:1", Placement::External, |facts| {
                facts.topology.controller = Some(Controller::NvmeBehindBridge);
            }),
            ("sdb1:1", Placement::External, |facts| {
                facts.topology.hotplug_link = Some(Link::ExternalPci);
            }),
            ("sda2:1", Placement::External, |facts| {
                facts.topology.removable = true;
            }),
            ("sdc1:1", Placement::External, |facts| {
                facts.present_at_boot = false;
            }),
            ("loop0p1:1", Placement::External, |facts| {
                facts.topology.virtual_device = true;
            }),
        ];
        let mut world = service.world.lock().unwrap();
        for (id, _, change) in cases {
            world.drives.push(altered(id, change));
        }
        drop(world);

        for (id, placement, _) in cases {
            let drive = service.drive(id);
            assert_eq!(drive.placement, placement, "{id}");
            assert!(drive.needs_consent, "{id}: must ask again");
            assert_eq!(drive.preselect, Mode::OpenSafely, "{id}: treated as new");
            assert!(drive.remembered.is_none(), "{id}");
        }

        // Each identifier alone voids the memory of the drive in this place.
        let swaps: [Change; 4] = [
            |facts| facts.identity.drive = Some("wwn:other".into()),
            |facts| facts.identity.partition = Some("guid-2".into()),
            |facts| facts.identity.volume = Some(1),
            |facts| facts.identity.volume = None,
        ];
        let mut world = service.world.lock().unwrap();
        for (at, swap) in swaps.into_iter().enumerate() {
            world.drives.push(altered(&format!("nvme9n1p{}:1", at + 1), swap));
        }
        drop(world);
        for at in 1..=4 {
            let drive = service.drive(&format!("nvme9n1p{at}:1"));
            assert!(drive.needs_consent, "swapped identifier {at}");
            assert_eq!(drive.preselect, Mode::OpenSafely);
        }

        // What a drive may keep: nothing for one with no identifier at all.
        let mut world = service.world.lock().unwrap();
        world.drives.push(altered("nvme0n1p5:1", |facts| {
            facts.identity.partition = None;
        }));
        world.drives.push(altered("nvme0n1p6:1", |facts| {
            facts.identity = Identity::default();
        }));
        drop(world);
        assert_eq!(
            service.drive("nvme0n1p5:1").can_remember,
            RememberOffer::OpenSafely,
            "missing an identifier, Open cannot be kept"
        );
        assert_eq!(
            service.drive("nvme0n1p6:1").can_remember,
            RememberOffer::Nothing
        );
    }

    phase a_remembered_drive_survives_a_restart_and_belongs_to_one_person;
    {
        let dir = tempfile::tempdir().unwrap();
        let first = Running::at(dir.path(), vec![windows_partition()]);
        assert!(matches!(
            first.open("nvme0n1p3:1", consent(Mode::OpenSafely, true)),
            Response::Opened { .. }
        ));
        assert_eq!(first.remembered().len(), 1);
        drop(first);

        let restarted = Running::at(dir.path(), vec![windows_partition()]);
        let drive = restarted.drive("nvme0n1p3:1");
        assert!(!drive.needs_consent, "the choice outlives the service");
        assert_eq!(drive.preselect, Mode::OpenSafely);
        assert!(drive.mounted.is_none(), "a choice is not a mount");
        assert_eq!(restarted.remembered().len(), 1);
        assert!(
            matches!(
                restarted.as_other(Request::Remembered),
                Response::Remembered { drives } if drives.is_empty()
            ),
            "another person on this computer sees none of them"
        );
        let Response::Drives { drives } = restarted.as_other(Request::Drives) else {
            panic!("no drive list");
        };
        assert!(
            drives.iter().all(|drive| drive.needs_consent),
            "and is asked about every drive"
        );

        let grant = restarted.remembered()[0].grant.clone();
        assert!(matches!(
            restarted.as_person(Request::Forget { grant }),
            Response::Done
        ));
        drop(restarted);
        let after = Running::at(dir.path(), vec![windows_partition()]);
        assert!(after.remembered().is_empty(), "forgetting outlives it too");
        assert!(after.drive("nvme0n1p3:1").needs_consent);
    }

    phase an_unclean_volume_opens_read_only_in_either_mode;
    {
        let service = Running::start(vec![windows_partition()]);
        service.world.lock().unwrap().health = Some(ReadOnlyReason::WindowsHibernated);
        for mode in [Mode::Open, Mode::OpenSafely] {
            let Response::Opened { mounted } = service.open("nvme0n1p3:1", consent(mode, false)) else {
                panic!("open failed");
            };
            assert_eq!(mounted.read_only, Some(ReadOnlyReason::WindowsHibernated));
            assert!(
                service.world.lock().unwrap().mounted[0].2,
                "mounted read-only"
            );
            assert!(matches!(
                service.as_person(Request::Close {
                    drive: "nvme0n1p3:1".into()
                }),
                Response::Done
            ));
        }
    }

    phase mounts_end_with_the_session_and_busy_drives_stay_open;
    {
        let service = Running::start(vec![windows_partition()]);
        assert!(matches!(
            service.open("nvme0n1p3:1", consent(Mode::Open, true)),
            Response::Opened { .. }
        ));
        service.world.lock().unwrap().busy = true;
        assert!(matches!(
            service.as_person(Request::Close {
                drive: "nvme0n1p3:1".into()
            }),
            Response::Failed { .. }
        ));
        let grant = service.remembered()[0].grant.clone();
        let Response::Failed { message } = service.as_person(Request::Forget { grant }) else {
            panic!("forgetting a busy drive must say it is still open");
        };
        assert!(message.contains("still open"));
        assert!(
            service.remembered().is_empty(),
            "the grant is gone regardless"
        );

        let mut world = service.world.lock().unwrap();
        world.busy = false;
        world.logged_out = true;
        drop(world);
        service.wait_until("the logged-out person's drive is closed", |world| {
            world.mounted.is_empty()
        });
    }

    phase housekeeping_runs_once_at_start_without_waiting_an_interval;
    {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("control.sock");
        let listener = tilo_files::native::socket::bind(&socket).unwrap();
        let world = Arc::new(Mutex::new(World {
            drives: vec![windows_partition()],
            ..World::default()
        }));
        let mut service = Service::new(
            Fake(Arc::clone(&world)),
            GrantStore::new(dir.path().join("grants")),
        );
        assert!(matches!(
            service.handle(
                Caller::Shell { uid: UID },
                Request::Open {
                    drive: "nvme0n1p3:1".into(),
                    consent: consent(Mode::Open, false),
                },
            ),
            Response::Opened { .. }
        ));
        assert_eq!(world.lock().unwrap().mounted.len(), 1);
        world.lock().unwrap().logged_out = true;

        // Even with a long housekeeping interval and stop already set, startup
        // housekeeping runs once before the poll loop and unmounts the stale
        // drive immediately.
        let stop = AtomicBool::new(true);
        server::serve(
            &listener,
            &mut service,
            |_| Ok(Caller::Shell { uid: UID }),
            server::Pacing {
                housekeeping: Duration::from_secs(60),
                idle_exit: None,
            },
            &stop,
        );
        assert!(
            world.lock().unwrap().mounted.is_empty(),
            "startup housekeeping must close the logged-out person's drive at once"
        );
    }
}
