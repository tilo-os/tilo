//! The machine the page runs on, as the session's status reads it: whether the browser is offline,
//! and the charge of a pack it tells of. A browser says nothing of its sound, camera or players.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use js_sys::{Function, Promise, Reflect};
use tilo_desktop::chrome::{LinkKind, LinkStatus};
use tilo_session::platform::readings::{Charge, Machine, Reading};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast as _, JsValue};

type Wake = Arc<dyn Fn() + Send + Sync>;

/// What the browser last said of its machine, each change moving the revision.
#[derive(Default)]
pub(crate) struct PageMachine {
    revision: Cell<u64>,
    /// A browser cannot tell a wired link from Wi-Fi, so only offline draws a glyph.
    offline: Cell<bool>,
    battery: Cell<Option<Charge>>,
}

impl Machine for PageMachine {
    fn revision(&self) -> u64 {
        self.revision.get()
    }

    fn read(&self) -> Reading {
        Reading {
            link: self.offline.get().then_some(LinkStatus {
                kind: LinkKind::Offline,
                name: None,
                signal: 0,
            }),
            battery: self.battery.get(),
            ..Reading::default()
        }
    }
}

impl PageMachine {
    /// Follow the browser's machine, drawing the page again through `wake` whenever it changes.
    pub(crate) fn follow(wake: &Wake) -> Rc<Self> {
        let machine = Rc::new(Self::default());
        let global = js_sys::global();
        let Ok(navigator) = Reflect::get(&global, &"navigator".into()) else {
            return machine;
        };
        let offline = {
            let navigator = navigator.clone();
            move || field(&navigator, "onLine").and_then(|on| on.as_bool()) == Some(false)
        };
        machine.offline.set(offline());
        listen(
            &global,
            &["online", "offline"],
            &machine,
            wake,
            move |machine| {
                machine.offline.set(offline());
            },
        );
        let asked = field(&navigator, "getBattery")
            .and_then(|get| get.dyn_into::<Function>().ok())
            .and_then(|get| get.call0(&navigator).ok())
            .and_then(|asked| asked.dyn_into::<Promise>().ok());
        if let Some(asked) = asked {
            let (machine, wake) = (Rc::clone(&machine), Arc::clone(wake));
            wasm_bindgen_futures::spawn_local(async move {
                let Ok(battery) = wasm_bindgen_futures::JsFuture::from(asked).await else {
                    return;
                };
                machine.battery.set(charge(&battery));
                machine.moved(&wake);
                let events = ["levelchange", "chargingchange"];
                listen(&battery.clone(), &events, &machine, &wake, move |machine| {
                    machine.battery.set(charge(&battery));
                });
            });
        }
        machine
    }

    fn moved(&self, wake: &Wake) {
        self.revision.set(self.revision.get().wrapping_add(1));
        wake();
    }
}

/// The pack a `BatteryManager` tells of; `None` for one always full on the mains, which is how a
/// browser describes a machine with no pack.
fn charge(battery: &JsValue) -> Option<Charge> {
    let charging = field(battery, "charging")?.as_bool()?;
    let level = field(battery, "level")?.as_f64()?.clamp(0.0, 1.0);
    let charging_time = field(battery, "chargingTime").and_then(|time| time.as_f64());
    if charging && level >= 1.0 && charging_time == Some(0.0) {
        return None;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a level clamped to 0..=1 is a percentage in 0..=100"
    )]
    let percent = (level * 100.0).round() as u8;
    Some(Charge {
        percent,
        charging,
        draining: !charging,
    })
}

fn field(value: &JsValue, name: &str) -> Option<JsValue> {
    Reflect::get(value, &name.into()).ok()
}

/// Run `read` on `machine` and draw again whenever `target` fires one of `events`, for as long as
/// the page lives.
fn listen(
    target: &JsValue,
    events: &[&str],
    machine: &Rc<PageMachine>,
    wake: &Wake,
    read: impl Fn(&PageMachine) + 'static,
) {
    let (machine, wake) = (Rc::clone(machine), Arc::clone(wake));
    let changed = Closure::<dyn Fn()>::new(move || {
        read(&machine);
        machine.moved(&wake);
    });
    if let Some(add) = field(target, "addEventListener")
        && let Some(add) = add.dyn_ref::<Function>()
    {
        for event in events {
            let _ = add.call2(target, &(*event).into(), changed.as_ref());
        }
    }
    changed.forget();
}
