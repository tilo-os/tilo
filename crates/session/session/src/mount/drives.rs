//! The drives a person plugs in: removable media are announced with what they
//! hold, and the notice opens or ejects them.
//!
//! udisks2 owns the machine's block devices. Its object manager says when one
//! appears or goes, and its properties say which are media a person carries:
//! a block device on a drive that reports itself removable, holding a
//! filesystem. The machine's own disks are not announced, and neither is a
//! medium udisks could not mount.
//!
//! An announcement is an ordinary notice ([`crate::notices`]) naming the
//! medium and how much it holds, offering to open it in Files on its mount
//! point and to eject it. Both run here, on the session's pass, when the
//! person acts on the notification: Files is a launch like any other, and an
//! eject is the unmount and power-off udisks is asked for.
//!
//! Nothing polls: one `GetManagedObjects` reads what is attached when the
//! session starts, and the object manager's signals carry what changes after.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};

use tilo_app_launch::{Launch, Open};
use tilo_notifications_service::{Notifications, Taken};

use crate::mount::apps::Apps;
use crate::notices;

/// A medium a person plugged in, as udisks describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Medium {
    /// Its block device's udisks object path: what an eject is asked of.
    pub device: String,
    /// The label its filesystem carries, when it has one.
    pub label: Option<String>,
    /// How much it holds, in bytes.
    pub size: u64,
    /// Where it is mounted, which is where Files opens it.
    pub mount: PathBuf,
}

/// Where the media a person plugs in come from: udisks on a running session,
/// a plain struct in a test.
pub trait Media {
    /// The media that appeared since the last call, oldest first.
    fn appeared(&self) -> Vec<Medium>;

    /// Eject `medium`: take it off the machine's filesystem tree and power
    /// its drive down, so it can be pulled out.
    fn eject(&self, medium: &Medium);
}

/// The removable media this session announces, and what the person asked of
/// their notices: called once per pass of the session's loop.
pub struct Drives {
    media: std::rc::Rc<dyn Media>,
    notifications: Notifications,
    /// The medium each live notice is about, by notification id.
    told: RefCell<BTreeMap<u32, Medium>>,
    /// The notification the person acted on and how, from the service's
    /// taps, which run wherever it publishes.
    asked: Receiver<(u32, String)>,
    /// The media the person asked to open, which Files starts on this pass.
    opening: RefCell<Vec<Medium>>,
}

impl Drives {
    /// Announce what `media` reports, through `notifications`, which the
    /// person's answers come back through.
    #[must_use]
    pub fn new(media: std::rc::Rc<dyn Media>, notifications: Notifications) -> Self {
        let (asked, actions) = mpsc::channel();
        notifications.on_taken(move |id, taken| {
            if let Taken::Action { key, .. } = taken {
                let _ = asked.send((id, key.to_owned()));
            }
        });
        Self {
            media,
            notifications,
            told: RefCell::new(BTreeMap::new()),
            asked: actions,
            opening: RefCell::new(Vec::new()),
        }
    }

    /// Announce the media that appeared since the last pass, and carry out
    /// what the person asked of a notice: open it in `apps`' Files, or eject
    /// it. Called once per pass of the session's loop.
    pub fn follow(&self, apps: Option<&Apps>) {
        for medium in self.media.appeared() {
            let id = notices::announce_medium(&self.notifications, &medium);
            self.told.borrow_mut().insert(id, medium);
        }
        self.act_on_notices(apps);
        self.forget_closed_notices();
    }

    /// What the person asked of a notice: the eject goes to the media, and
    /// the open waits for the pass that starts Files.
    fn act_on_notices(&self, apps: Option<&Apps>) {
        let asked: Vec<(u32, String)> = self.asked.try_iter().collect();
        for (id, key) in asked {
            let Some(medium) = self.told.borrow().get(&id).cloned() else {
                // Not a drive's notice, or one already acted on.
                continue;
            };
            match key.as_str() {
                notices::OPEN_MEDIUM_ACTION => {
                    self.told.borrow_mut().remove(&id);
                    self.opening.borrow_mut().push(medium);
                }
                notices::EJECT_MEDIUM_ACTION => {
                    self.told.borrow_mut().remove(&id);
                    self.media.eject(&medium);
                }
                _ => {}
            }
        }
        let Some(apps) = apps else {
            return;
        };
        for medium in std::mem::take(&mut *self.opening.borrow_mut()) {
            if let Err(error) = apps.open(&files_on(&medium.mount)) {
                tracing::warn!(
                    %error,
                    mount = %medium.mount.display(),
                    "a drive did not open"
                );
            }
        }
    }

    /// A notice the person closed or that expired is about nothing any more.
    fn forget_closed_notices(&self) {
        if self.told.borrow().is_empty() {
            return;
        }
        let live: std::collections::BTreeSet<u32> = self
            .notifications
            .live()
            .iter()
            .map(|notification| notification.id)
            .collect();
        self.told.borrow_mut().retain(|id, _| live.contains(id));
    }
}

/// Files opening `mount`, which is where a medium the person plugged in is:
/// the folder is what they want to see, so it is opened by the app that
/// shows folders rather than routed by its type.
fn files_on(mount: &std::path::Path) -> Launch {
    Launch::Package {
        app: tilo_desktop_model::app::AppKind::FileExplorer.app_id(),
        open: Some(Open::File(mount.to_path_buf())),
    }
}

/// The media udisks reports on `connection`: the session's own system bus
/// connection, which the desktop services already hold.
///
/// The follower runs on a thread of its own, so the session's loop never
/// waits on the bus.
#[must_use]
pub fn udisks(connection: zbus::Connection) -> std::rc::Rc<dyn Media> {
    let (appearing, fresh) = mpsc::channel();
    let (ejections, ejected) = tokio::sync::mpsc::unbounded_channel();
    let started = std::thread::Builder::new()
        .name("drive-watch".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    tracing::warn!(%error, "removable drives are not watched");
                    return;
                }
            };
            runtime.block_on(follow(connection, appearing, ejected));
        });
    if let Err(error) = started {
        tracing::warn!(%error, "removable drives are not watched");
    }
    std::rc::Rc::new(Udisks { fresh, ejections })
}

/// The udisks side of [`Media`]: what its follower thread reported, and what
/// it is asked to eject.
struct Udisks {
    fresh: Receiver<Medium>,
    ejections: tokio::sync::mpsc::UnboundedSender<Medium>,
}

impl Media for Udisks {
    fn appeared(&self) -> Vec<Medium> {
        self.fresh.try_iter().collect()
    }

    fn eject(&self, medium: &Medium) {
        if let Err(error) = self.ejections.send(medium.clone()) {
            tracing::warn!(
                %error,
                mount = %medium.mount.display(),
                "a drive was not ejected"
            );
        }
    }
}

/// The object manager at `UDisks2`'s root, which knows every block device.
const OBJECT_MANAGER: &str = "/org/freedesktop/UDisks2";
/// The object manager interface, whose signals are what changes.
const MANAGER_IFACE: &str = "org.freedesktop.DBus.ObjectManager";
/// Signals buffered per stream before the oldest is dropped: a dropped
/// `InterfacesAdded` loses a drive's notice, so the buffer is wide.
const SIGNAL_QUEUE: usize = 64;

/// An interface's properties.
type Properties = HashMap<String, zbus::zvariant::OwnedValue>;

/// What udisks knows of the block devices, `a{oa{sa{sv}}}`: an object's
/// interfaces, each with its properties.
type ManagedObjects = HashMap<zbus::zvariant::OwnedObjectPath, HashMap<String, Properties>>;

/// Announce the media udisks reports on `connection` as they appear, until
/// the bus or the loop goes away; eject what the session asks it to.
async fn follow(
    connection: zbus::Connection,
    appearing: Sender<Medium>,
    mut ejections: tokio::sync::mpsc::UnboundedReceiver<Medium>,
) {
    use futures_util::StreamExt as _;

    let added = signal(&connection, "InterfacesAdded").await;
    let removed = signal(&connection, "InterfacesRemoved").await;
    let (Ok(mut added), Ok(mut removed)) = (added, removed) else {
        tracing::warn!("removable drives are not watched");
        return;
    };
    // What is attached now, read once: a medium plugged in while the session
    // started is announced like one plugged in after. Subscribed first, so a
    // medium that arrives during the read is seen either way; `seen` keeps
    // the sweep and the signal from telling twice.
    let mut seen = BTreeMap::new();
    for device in attached(&connection).await {
        announce(&connection, &device, &mut seen, &appearing).await;
    }
    loop {
        tokio::select! {
            signal = added.next() => match signal {
                Some(Ok(message)) => {
                    if let Some((device, interfaces)) = added_at(&message) {
                        // A filesystem or a drive appearing changes what a
                        // block device is; its `Block` alone is a partition
                        // the kernel read, which is nothing to carry.
                        if interfaces.iter().any(|name| {
                            name == block_devices::udisks::IFACE_FILESYSTEM
                                || name == block_devices::udisks::IFACE_DRIVE
                        }) {
                            announce(&connection, &device, &mut seen, &appearing).await;
                        }
                    }
                }
                Some(Err(_)) => {}
                None => return,
            },
            signal = removed.next() => match signal {
                Some(Ok(message)) => {
                    if let Some(device) = removed_at(&message) {
                        // Gone: plugging it back in is a new announcement.
                        seen.remove(&device);
                    }
                }
                Some(Err(_)) => {}
                None => return,
            },
            Some(medium) = ejections.recv() => eject(&connection, &medium).await,
        }
    }
}

/// A stream of `member`'s signals from udisks.
async fn signal(connection: &zbus::Connection, member: &str) -> zbus::Result<zbus::MessageStream> {
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(block_devices::udisks::SERVICE)?
        .path(OBJECT_MANAGER)?
        .interface(MANAGER_IFACE)?
        .member(member)?
        .build();
    zbus::MessageStream::for_match_rule(rule, connection, Some(SIGNAL_QUEUE)).await
}

/// The block devices udisks knows now.
async fn attached(connection: &zbus::Connection) -> Vec<String> {
    let manager = zbus::Proxy::new(
        connection,
        block_devices::udisks::SERVICE,
        OBJECT_MANAGER,
        MANAGER_IFACE,
    )
    .await;
    let Ok(manager) = manager else {
        tracing::warn!("removable drives are not listed");
        return Vec::new();
    };
    let reply = manager.call_method("GetManagedObjects", &()).await;
    let Ok(reply) = reply else {
        tracing::warn!("removable drives are not listed");
        return Vec::new();
    };
    let objects: ManagedObjects = match reply.body().deserialize() {
        Ok(objects) => objects,
        Err(error) => {
            tracing::warn!(%error, "udisks listed no block devices this session can read");
            return Vec::new();
        }
    };
    objects
        .into_iter()
        .filter(|(_, interfaces)| interfaces.contains_key(block_devices::udisks::IFACE_FILESYSTEM))
        .map(|(device, _)| device.to_string())
        .collect()
}

/// The object an `InterfacesAdded` is about and the interfaces it gained.
fn added_at(message: &zbus::Message) -> Option<(String, Vec<String>)> {
    use zbus::zvariant::OwnedValue;
    let interfaces: (
        zbus::zvariant::OwnedObjectPath,
        std::collections::HashMap<String, std::collections::HashMap<String, OwnedValue>>,
    ) = match message.body().deserialize() {
        Ok(added) => added,
        Err(_) => return None,
    };
    let (device, interfaces) = interfaces;
    Some((device.to_string(), interfaces.into_keys().collect()))
}

/// The object an `InterfacesRemoved` is about.
fn removed_at(message: &zbus::Message) -> Option<String> {
    let (device, _interfaces): (zbus::zvariant::OwnedObjectPath, Vec<String>) =
        match message.body().deserialize() {
            Ok(removed) => removed,
            Err(_) => return None,
        };
    Some(device.to_string())
}

/// Announce the medium on `device` when it is one a person plugged in and it
/// has not been announced before; `seen` is what has.
async fn announce(
    connection: &zbus::Connection,
    device: &str,
    seen: &mut BTreeMap<String, Medium>,
    appearing: &Sender<Medium>,
) {
    let Some(medium) = medium_of(connection, device).await else {
        return;
    };
    if seen.get(device) == Some(&medium) {
        return;
    }
    if appearing.send(medium.clone()).is_err() {
        return;
    }
    seen.insert(device.to_owned(), medium);
}

/// The medium on `device`, when it is removable and mounted, mounting it
/// first when udisks holds a filesystem nobody mounted yet.
async fn medium_of(connection: &zbus::Connection, device: &str) -> Option<Medium> {
    use block_devices::udisks::{IFACE_BLOCK, IFACE_DRIVE, IFACE_FILESYSTEM, SERVICE};

    let block = zbus::Proxy::new(connection, SERVICE, device, IFACE_BLOCK)
        .await
        .ok()?;
    let size: u64 = block.get_property("Size").await.ok()?;
    if size == 0 {
        return None;
    }
    let drive: zbus::zvariant::OwnedObjectPath = block.get_property("Drive").await.ok()?;
    if drive.as_str() == "/" {
        // A loop device or similar: nothing a person pulls out.
        return None;
    }
    let drive = zbus::Proxy::new(connection, SERVICE, drive.as_str(), IFACE_DRIVE)
        .await
        .ok()?;
    if !drive
        .get_property::<bool>("Removable")
        .await
        .unwrap_or(false)
    {
        return None;
    }
    // An unlabelled medium is one the person still recognises by its size.
    let label: String = block.get_property("IdLabel").await.unwrap_or_default();
    let filesystem = zbus::Proxy::new(connection, SERVICE, device, IFACE_FILESYSTEM)
        .await
        .ok()?;
    let mount = match mounted_at(&filesystem).await {
        Some(mount) => mount,
        None => mount_now(&filesystem).await?,
    };
    Some(Medium {
        device: device.to_owned(),
        label: (!label.is_empty()).then_some(label),
        size,
        mount,
    })
}

/// Where `filesystem` is mounted, when it is.
async fn mounted_at(filesystem: &zbus::Proxy<'_>) -> Option<PathBuf> {
    let points: Vec<Vec<u8>> = filesystem.get_property("MountPoints").await.ok()?;
    let point = points.first()?;
    Some(PathBuf::from(String::from_utf8_lossy(point).into_owned()))
}

/// `filesystem` mounted by udisks, at the mount point it chose.
async fn mount_now(filesystem: &zbus::Proxy<'_>) -> Option<PathBuf> {
    let mounted: String = filesystem
        .call("Mount", &(no_options(),))
        .await
        .inspect_err(|error| tracing::warn!(%error, "a drive could not be mounted"))
        .ok()?;
    Some(PathBuf::from(mounted))
}

/// Take `medium` off the filesystem tree and power its drive down, so it can
/// be pulled out.
async fn eject(connection: &zbus::Connection, medium: &Medium) {
    use block_devices::udisks::{IFACE_BLOCK, IFACE_DRIVE, IFACE_FILESYSTEM, SERVICE};

    let filesystem = zbus::Proxy::new(
        connection,
        SERVICE,
        medium.device.as_str(),
        IFACE_FILESYSTEM,
    )
    .await;
    if let Ok(filesystem) = filesystem {
        let unmounted: zbus::Result<()> = filesystem.call("Unmount", &(no_options(),)).await;
        if let Err(error) = unmounted {
            tracing::warn!(
                %error,
                mount = %medium.mount.display(),
                "a drive was not unmounted"
            );
        }
    }
    let Ok(block) =
        zbus::Proxy::new(connection, SERVICE, medium.device.as_str(), IFACE_BLOCK).await
    else {
        return;
    };
    let Ok(drive) = block
        .get_property::<zbus::zvariant::OwnedObjectPath>("Drive")
        .await
    else {
        return;
    };
    if drive.as_str() == "/" {
        return;
    }
    let Ok(powered) = zbus::Proxy::new(connection, SERVICE, drive.as_str(), IFACE_DRIVE).await
    else {
        return;
    };
    let powered_off: zbus::Result<()> = powered.call("PowerOff", &(no_options(),)).await;
    if let Err(error) = powered_off {
        tracing::warn!(
            %error,
            mount = %medium.mount.display(),
            "a drive was not powered down"
        );
    }
}

/// Empty `a{sv}` options: udisks picks its defaults (automatic mount point,
/// the unmount it can do).
fn no_options() -> std::collections::HashMap<&'static str, zbus::zvariant::Value<'static>> {
    std::collections::HashMap::new()
}
