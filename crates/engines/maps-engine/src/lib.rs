//! Maps' device work: the areas a person keeps for offline use, on this device.
//!
//! A tilolang app runs sandboxed and keeps no files of its own, so this crate is what
//! `so.tilo.maps` reaches through `engine.call`, `engine.start` and `engine.cancel`. It answers no other app's asks, and every answer is JSON the package reads
//! into its own records.
//!
//! | op | kind | asks | answers |
//! |---|---|---|---|
//! | `save-area` | job | `{id, name, north, south, east, west, least_zoom, most_zoom, basemap}` | the job's path; each move `{done, total}` |
//! | `kept-areas` | call | — | `[{id, name, north, south, east, west, least_zoom, most_zoom, tiles}]` |
//! | `forget-area` | call | `{id}` | `{freed}` |
//! | `clear-saved` | call | — | `{freed}` |
//! | `saved-bytes` | call | — | `{bytes}` |
//! | `locate` | call | — | `{lat, lon, accuracy_m}`, or `{why}` when the device cannot say |
//!
//! The pictures are kept where the app's window keeps the ones it looked at
//! ([`Tiles::of_app`]), so a window that already drew an area saves it without asking the
//! network again, and a picture the person saved is read before the cache and the network.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;
use tilo_tiles::{Area, AreaBounds, Tiles};
use tilolang_engine::{JobOp, JobSink, Needs, Provider, State};

/// The app whose engine this is: the one that asks these ops, and no other.
pub const APP: &str = "so.tilo.maps";

/// The name the app's cache directory is under, as its window keeps its pictures
/// (`tilo_paths::AppDirs::new("maps")`).
const CACHE_APP: &str = "maps";

/// How long a location request may take before the device is said not to answer: the platform
/// asks the person first, and its own fix can take seconds.
const LOCATE_TIMEOUT: Duration = Duration::from_secs(30);

/// Register this crate as Maps' engine: what a session or an app process calls once at its
/// start, before the app asks.
pub fn register() {
    if let Some(tiles) = Tiles::of_app(CACHE_APP) {
        tilolang_engine::register(APP, Arc::new(Maps::with(Arc::new(tiles))));
    }
}

/// An area asked for, as `save-area` names it.
#[derive(Deserialize)]
struct AreaAsk {
    id: String,
    name: String,
    north: f64,
    south: f64,
    east: f64,
    west: f64,
    least_zoom: i64,
    most_zoom: i64,
    basemap: String,
}

#[derive(Deserialize)]
struct AreaId {
    id: String,
}

/// Where the device is, as the platform's own location service says: its latitude, longitude and
/// how many metres the fix is good to.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Fix {
    /// The latitude of the device, in degrees.
    pub latitude: f64,
    /// The longitude of the device, in degrees.
    pub longitude: f64,
    /// How many metres the fix is good to; `0` when the platform does not say.
    pub accuracy_m: f64,
}

/// The device's own location, as an engine asks it. The platform asks the person first — the
/// location portal on Linux, Core Location on macOS, the browser's permission — and answers
/// with a fix or why it cannot.
pub trait Locates: Send + Sync {
    /// Where the device is, waiting for the platform's answer.
    ///
    /// # Errors
    ///
    /// Why the device cannot say where it is.
    fn fix(&self) -> Result<Fix, String>;
}

/// The platform's location service.
struct SystemLocation;

impl Locates for SystemLocation {
    fn fix(&self) -> Result<Fix, String> {
        match tilo_location::system()
            .locate()
            .recv()
            .map_err(|_| "the location service went away".to_owned())?
        {
            tilo_location::LocationOutcome::Located {
                lat,
                lon,
                accuracy_m,
            } => Ok(Fix {
                latitude: lat,
                longitude: lon,
                accuracy_m,
            }),
            tilo_location::LocationOutcome::Unavailable(reason) => Err(reason),
        }
    }
}

/// Maps' engine: the pictures on this device, and the areas being kept.
pub struct Maps {
    tiles: Arc<Tiles>,
    locator: Arc<dyn Locates>,
    /// Each `save-area` under way, by its job: whether the person stopped it.
    running: Arc<Mutex<BTreeMap<String, Arc<AtomicBool>>>>,
}

fn held<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Maps {
    /// The engine over `tiles`: what a test hands a directory of its own.
    #[must_use]
    pub fn with(tiles: Arc<Tiles>) -> Self {
        Self::with_locator(tiles, Arc::new(SystemLocation))
    }

    /// The engine over `tiles`, asking `locator` where the device is: what a test hands a source
    /// of its own.
    #[must_use]
    pub fn with_locator(tiles: Arc<Tiles>, locator: Arc<dyn Locates>) -> Self {
        Self {
            tiles,
            locator,
            running: Arc::default(),
        }
    }

    /// Where the device is, asking the platform on a thread of its own so the caller is not
    /// blocked on the person's answer or on the fix.
    fn locate(&self) -> Result<String, String> {
        let locator = Arc::clone(&self.locator);
        let (tx, rx) = std::sync::mpsc::channel();
        tilo_rt::spawn_thread("maps-locate", move || {
            let _ = tx.send(locator.fix());
        })
        .map_err(|error| format!("the location request starts no thread: {error}"))?;
        let answer = rx.recv_timeout(LOCATE_TIMEOUT).map_err(|_| {
            "The device did not answer where it is. Check that location is allowed for Tilo."
                .to_owned()
        })?;
        match answer {
            Ok(fix) => Ok(json!({
                "lat": fix.latitude,
                "lon": fix.longitude,
                "accuracy_m": fix.accuracy_m,
            })
            .to_string()),
            Err(why) => Ok(json!({ "why": why }).to_string()),
        }
    }

    /// Save `area` as the job `job`, on a thread of its own, telling `moved` how far it has come
    /// and then how it ended.
    fn begin(&self, job: &str, area: Area, moved: JobSink) -> Result<(), String> {
        let stopped = Arc::new(AtomicBool::new(false));
        held(&self.running).insert(job.to_owned(), Arc::clone(&stopped));
        let tiles = Arc::clone(&self.tiles);
        let running = Arc::clone(&self.running);
        let slot = job.to_owned();
        let spawned = std::thread::Builder::new()
            .name(format!("maps-{job}"))
            .spawn(move || {
                let kept = tiles.keep_area(&area, &|| stopped.load(Ordering::Relaxed), |kept| {
                    moved.moved(
                        State::Running,
                        json!({"done": kept.saved, "total": kept.total}),
                    );
                });
                held(&running).remove(&slot);
                let state = if stopped.load(Ordering::Relaxed) {
                    State::Cancelled
                } else {
                    State::Done
                };
                moved.moved(state, json!({"done": kept.saved, "total": kept.total}));
            });
        if let Err(error) = spawned {
            held(&self.running).remove(job);
            return Err(format!("the download starts no thread: {error}"));
        }
        Ok(())
    }
}

impl Provider for Maps {
    fn jobs(&self) -> &'static [JobOp] {
        // The pictures are kept in the app's own cache, fetched from the map's imagery.
        const JOBS: &[JobOp] = &[JobOp::new("save-area", Needs::NETWORK_AND_APP_STATE)];
        JOBS
    }

    fn call(&self, op: &str, input: &str) -> Result<String, String> {
        match op {
            "kept-areas" => {
                let areas: Vec<serde_json::Value> = self
                    .tiles
                    .kept_areas()
                    .iter()
                    .map(|area| {
                        json!({
                            "id": area.id,
                            "name": area.name,
                            "north": area.north,
                            "south": area.south,
                            "east": area.east,
                            "west": area.west,
                            "least_zoom": area.least_zoom,
                            "most_zoom": area.most_zoom,
                            "tiles": area.tile_count(),
                        })
                    })
                    .collect();
                Ok(serde_json::Value::Array(areas).to_string())
            }
            "forget-area" => {
                let ask: AreaId = serde_json::from_str(input)
                    .map_err(|error| format!("the ask does not read: {error}"))?;
                Ok(json!({ "freed": self.tiles.forget_area(&ask.id) }).to_string())
            }
            "clear-saved" => Ok(json!({ "freed": self.tiles.clear_saved() }).to_string()),
            "saved-bytes" => Ok(json!({ "bytes": self.tiles.saved_bytes() }).to_string()),
            "locate" => self.locate(),
            _ => Err(format!("`{op}` is no op this device runs")),
        }
    }

    fn start(&self, job: &str, op: &str, input: &str, moved: JobSink) -> Result<(), String> {
        if op != "save-area" {
            return Err(format!("`{op}` is no op this device runs"));
        }
        let ask: AreaAsk = serde_json::from_str(input)
            .map_err(|error| format!("the ask does not read: {error}"))?;
        if !(ask.north.is_finite()
            && ask.south.is_finite()
            && ask.east.is_finite()
            && ask.west.is_finite())
        {
            return Err(
                "an area's edges are places on Earth, not numbers without an end".to_owned(),
            );
        }
        let area = Area::new(
            ask.id,
            ask.name,
            AreaBounds {
                north: ask.north,
                south: ask.south,
                east: ask.east,
                west: ask.west,
                least_zoom: ask.least_zoom,
                most_zoom: ask.most_zoom,
            },
            ask.basemap,
        );
        self.begin(job, area, moved)
    }

    fn cancel(&self, job: &str) -> bool {
        let Some(stopped) = held(&self.running).get(job).cloned() else {
            return false;
        };
        stopped.store(true, Ordering::Relaxed);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Standing(f64, f64);

    impl Locates for Standing {
        fn fix(&self) -> Result<Fix, String> {
            Ok(Fix {
                latitude: self.0,
                longitude: self.1,
                accuracy_m: 12.0,
            })
        }
    }

    struct Refusing(String);

    impl Locates for Refusing {
        fn fix(&self) -> Result<Fix, String> {
            Err(self.0.clone())
        }
    }

    fn engine(name: &str) -> Maps {
        let dir =
            std::env::temp_dir().join(format!("tilo-maps-engine-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Maps::with_locator(
            Arc::new(Tiles::open(dir)),
            Arc::new(Standing(48.1374, 11.5755)),
        )
    }

    fn ask(name: &str) -> String {
        json!({
            "id": format!("region:{name}"),
            "name": name,
            "north": 48.18,
            "south": 48.10,
            "east": 11.62,
            "west": 11.55,
            "least_zoom": 14,
            "most_zoom": 14,
            "basemap": "http://127.0.0.1:1/%z/%x/%y.png",
        })
        .to_string()
    }

    /// An area the engine keeps is listed with its bounds and levels, and removing it gives up
    /// its place in the list; clearing takes every area away.
    #[test]
    fn an_area_is_kept_listed_and_removed() {
        let engine = engine("listed");
        let (tx, rx) = std::sync::mpsc::channel();
        let moved = JobSink::new(move |state: State, detail: serde_json::Value| {
            if state.ended() {
                let _ = tx.send((state, detail));
            }
        });
        engine
            .start("devices/d/jobs/1", "save-area", &ask("Munich"), moved)
            .expect("it starts");
        // The tile host is not reachable: the job still ends, saying it kept none, and the
        // area is listed because the person asked for it.
        let (state, said) = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the job ends");
        assert_eq!(state, State::Done);
        assert!(said["total"].as_u64().is_some_and(|total| total > 0));
        assert!(
            !engine.cancel("devices/d/jobs/1"),
            "an ended job stops no more"
        );
        let listed: serde_json::Value =
            serde_json::from_str(&engine.call("kept-areas", "").expect("it lists"))
                .expect("a list");
        let areas = listed.as_array().expect("a list of areas");
        assert_eq!(areas.len(), 1);
        assert_eq!(areas[0]["name"], "Munich");
        assert_eq!(areas[0]["least_zoom"], 14);
        let freed: serde_json::Value = serde_json::from_str(
            &engine
                .call("forget-area", &json!({"id": "region:Munich"}).to_string())
                .expect("it forgets"),
        )
        .expect("an answer");
        assert!(freed["freed"].as_u64().is_some());
        let listed: serde_json::Value =
            serde_json::from_str(&engine.call("kept-areas", "").expect("it lists"))
                .expect("a list");
        assert!(listed.as_array().is_some_and(Vec::is_empty));
        assert!(engine.call("clear-saved", "").is_ok());
    }

    /// Where the device is comes from the source the engine asks, and a device that cannot say
    /// answers with why, never with a place it invented.
    #[test]
    fn the_device_says_where_it_is_or_why_it_cannot() {
        let engine = engine("located");
        let said: serde_json::Value =
            serde_json::from_str(&engine.call("locate", "").expect("it answers")).expect("JSON");
        assert_eq!(said["lat"], 48.1374);
        assert_eq!(said["lon"], 11.5755);
        assert_eq!(said["accuracy_m"], 12.0);

        let dir =
            std::env::temp_dir().join(format!("tilo-maps-engine-{}-nowhere", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let nowhere = Maps::with_locator(
            Arc::new(Tiles::open(dir)),
            Arc::new(Refusing("Location is not available here".to_owned())),
        );
        let said: serde_json::Value =
            serde_json::from_str(&nowhere.call("locate", "").expect("it answers")).expect("JSON");
        assert_eq!(said["why"], "Location is not available here");
        assert!(said.get("lat").is_none(), "no place is invented: {said}");
    }

    /// An ask whose edges are not places on Earth is refused before any job exists.
    #[test]
    fn an_area_without_real_edges_is_refused() {
        let engine = engine("refused");
        let mut ask: serde_json::Value = serde_json::from_str(&ask("Nowhere")).unwrap();
        ask["north"] = json!(f64::INFINITY);
        let refused = engine.start(
            "devices/d/jobs/1",
            "save-area",
            &ask.to_string(),
            JobSink::new(|_, _| {}),
        );
        assert!(refused.is_err(), "an area needs real edges: {refused:?}");
    }
}
