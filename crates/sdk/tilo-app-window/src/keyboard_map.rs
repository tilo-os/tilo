//! [`KeyboardMap`] as a described component: a physical keyboard model drawn with the legends of
//! an XKB layout, compiled once per layout and model.
//!
//! The drawing is a native fact — the installed layouts and the geometry of the PC models — so the
//! window compiles it, off the frame, and keeps the last few drawings: every view of `de` on a
//! `pc105` shares the one. A drawing not compiled yet shows nothing and starts compiling; when it
//! lands the window is woken and the map is drawn.

use std::collections::VecDeque;
use std::sync::{Arc, LazyLock, Mutex};

use ui::component::{Component, Described};
use ui::widget::{Decorator, KeyboardMap, KeyboardMapKey, Widget};
use ui::{Keycaps, WakeHandle, lock::MutexRecover as _};

/// A physical keyboard model with the legends of one layout.
type Drawing = (Vec<KeyboardMapKey>, ui::taffy::Size<f64>);

/// How many drawings are kept: the layouts a device offers, and a couple of models.
const KEPT: usize = 8;

/// The geometry's margin, in model units, so a key's outline never touches the edge.
const ROOM: f64 = 1.5;
/// How far the laptop drawing's function row sits below the model's top, in model units.
const LAPTOP_FUNCTION_ROW_DROP: u8 = 20;

/// A physical keyboard with an XKB layout's legends.
#[derive(Component, Default)]
#[component(name = "KeyboardMap", role = "Image", label = "label")]
pub struct KeyboardMapProps {
    /// The XKB layout, or `layout:variant` (`de`, `us:intl`).
    pub layout: String,
    /// The physical model: `pc104`, `pc105`, `jp106` or `kr106`.
    pub model: String,
    /// The XKB options the drawing is compiled with (`compose:caps`).
    pub options: Option<String>,
    /// Draw only the main block, as a laptop's keyboard, without the navigation cluster or the
    /// numpad.
    pub compact: bool,
    /// Its spoken name.
    pub label: Option<String>,
    /// Paint only the legend of this modifier level, counting from zero, on each key.
    pub level: Option<i64>,
    /// Show the focused keyboard's real symbols, rather than the drawn layout's.
    pub live: bool,
    /// The physical key chosen for inspection, by its XKB name (`AD01`).
    pub inspected: Option<String>,
}

/// One layout on one model, as a drawing is asked for and kept.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Spec {
    layout: String,
    variant: String,
    model: String,
    options: String,
    compact: bool,
}

impl Spec {
    fn of(props: &KeyboardMapProps) -> Self {
        let (layout, variant) = parse_source(&props.layout);
        Self {
            layout,
            variant,
            model: props.model.clone(),
            options: props.options.clone().unwrap_or_default(),
            compact: props.compact,
        }
    }

    /// The layout as XKB names it, `layout:variant` when it has one.
    fn source(&self) -> String {
        if self.variant.is_empty() {
            self.layout.clone()
        } else {
            format!("{}:{}", self.layout, self.variant)
        }
    }

    /// The drawing, once it is compiled; never compiles it.
    fn compiled(&self) -> Option<Arc<Result<Drawing, String>>> {
        DRAWINGS.lock_recover()
            .done
            .iter()
            .find(|(spec, _)| spec == self)
            .map(|(_, drawn)| Arc::clone(drawn))
    }

    /// Compile the drawing here and keep it. Reads the installed XKB data, so never on the
    /// window's thread.
    fn compile(&self) {
        ui::rt::off_ui_thread();
        let drawn = Arc::new(drawing(self));
        let mut kept = DRAWINGS.lock_recover();
        kept.compiling.retain(|spec| spec != self);
        kept.done.retain(|(spec, _)| spec != self);
        if kept.done.len() >= KEPT {
            kept.done.pop_front();
        }
        kept.done.push_back((self.clone(), drawn));
    }

    /// Compile the drawing in the background unless it is compiled or being compiled, waking
    /// `wake` when it lands.
    fn compile_in_background(&self, wake: &WakeHandle) {
        {
            let mut kept = DRAWINGS.lock_recover();
            if kept.done.iter().any(|(spec, _)| spec == self)
                || kept.compiling.contains(self)
            {
                return;
            }
            kept.compiling.push(self.clone());
        }
        let spec = self.clone();
        let woken = wake.clone();
        let spawned = std::thread::Builder::new()
            .name("tilo-keyboard-drawing".into())
            .spawn(move || {
                spec.compile();
                woken.wake();
            });
        if spawned.is_err() {
            let mut kept = DRAWINGS.lock_recover();
            kept.compiling.retain(|spec| spec != self);
            kept.done
                .push_back((self.clone(), Arc::new(Err("the keyboard could not be drawn".into()))));
        }
    }
}

#[derive(Default)]
struct Drawings {
    done: VecDeque<(Spec, Arc<Result<Drawing, String>>)>,
    compiling: Vec<Spec>,
}

static DRAWINGS: LazyLock<Mutex<Drawings>> = LazyLock::new(|| Mutex::new(Drawings::default()));

/// A keyboard map drawn from an XKB layout and a PC model: what the Displays and Keyboard panes
/// of Settings show.
pub struct DescribedKeyboardMap {
    map: KeyboardMap,
    spec: Spec,
    label: String,
    level: Option<i64>,
    live: bool,
    inspected: Option<String>,
    wake: WakeHandle,
    /// The drawing is in the map, or has failed: it is not asked for again.
    drawn: bool,
}

impl DescribedKeyboardMap {
    /// Put the compiled drawing in the map once it is there, or start compiling it. `true` when
    /// the map was replaced.
    fn ensure_drawn(&mut self) -> bool {
        if self.drawn {
            return false;
        }
        let Some(drawn) = self.spec.compiled() else {
            self.spec.compile_in_background(&self.wake);
            return false;
        };
        self.drawn = true;
        let Ok((keys, size)) = drawn.as_ref() else {
            return false;
        };
        let mut map = KeyboardMap::new(keys.clone(), *size, self.label.clone());
        if let Some(level) = self.level {
            map = map.with_single_level(usize::try_from(level).unwrap_or(0));
        }
        map = map
            .with_live_layout(self.live)
            .with_inspected_key(self.inspected.as_deref().unwrap_or_default())
            .with_configuration(self.spec.source(), self.spec.model.clone());
        self.map = map;
        true
    }
}

impl Described for DescribedKeyboardMap {
    type Props = KeyboardMapProps;

    fn from_props(_key: &str, props: KeyboardMapProps) -> Self {
        let spec = Spec::of(&props);
        let label = props.label.unwrap_or_default();
        Self {
            map: KeyboardMap::new(
                Vec::new(),
                ui::taffy::Size {
                    width: 1.0,
                    height: 1.0,
                },
                label.clone(),
            ),
            spec,
            label,
            level: props.level,
            live: props.live,
            inspected: props.inspected,
            wake: WakeHandle::new(),
            drawn: false,
        }
    }

    fn set_props(&mut self, props: KeyboardMapProps) -> bool {
        let _ = props;
        false
    }
}

impl Decorator for DescribedKeyboardMap {
    type Inner = KeyboardMap;

    fn inner(&self) -> &KeyboardMap {
        &self.map
    }

    fn inner_mut(&mut self) -> &mut KeyboardMap {
        &mut self.map
    }

    fn build_layout(
        &mut self,
        taffy: &mut ui::taffy::TaffyTree<()>,
        theme: &ui::theme::Theme,
        node_map: &mut std::collections::HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        self.ensure_drawn();
        Widget::build_layout(&mut self.map, taffy, theme, node_map)
    }

    fn draw(&mut self, ctx: &mut ui::draw::DrawCtx<'_>) {
        self.ensure_drawn();
        Widget::draw(&mut self.map, ctx);
    }
}

/// `layout:variant` as the two, when both are names an XKB layout may have.
fn parse_source(source: &str) -> (String, String) {
    let mut parts = source.splitn(2, ':');
    let layout = parts.next().unwrap_or_default().trim();
    let variant = parts.next().unwrap_or_default().trim();
    let valid = |name: &str, empty: bool| {
        (empty || !name.is_empty())
            && name.len() <= 32
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    };
    if valid(layout, false) && valid(variant, true) {
        (layout.to_owned(), variant.to_owned())
    } else {
        (String::new(), String::new())
    }
}

/// The keys of `spec`'s model with its layout's legends, and the model's size.
fn drawing(spec: &Spec) -> Result<Drawing, String> {
    if spec.layout.is_empty() {
        return Err("the layout is not an XKB layout".into());
    }
    let geometries = pc_keyboard_geometry::models()?;
    let geometry = geometries
        .get(spec.model.as_str())
        .ok_or("unknown physical keyboard model")?;
    let keys = if spec.compact {
        compact_geometry(geometry).1
    } else {
        geometry.keys.clone()
    };
    let (size, keys) = tight(keys);
    let keys = compile(spec, &keys)?;
    Ok((keys, size))
}

/// Cut the drawing to its keys, keeping `ROOM` around them.
fn tight(mut keys: Vec<pc_keyboard_geometry::Key>) -> (ui::taffy::Size<f64>, Vec<pc_keyboard_geometry::Key>) {
    let points = || keys.iter().flat_map(|key| key.outline.iter());
    let (x0, y0) = points().fold((f64::MAX, f64::MAX), |(x, y), [px, py]| {
        (x.min(*px), y.min(*py))
    });
    let (x1, y1) = points().fold((f64::MIN, f64::MIN), |(x, y), [px, py]| {
        (x.max(*px), y.max(*py))
    });
    if x0 > x1 || y0 > y1 {
        return (
            ui::taffy::Size {
                width: 1.0,
                height: 1.0,
            },
            keys,
        );
    }
    for key in &mut keys {
        for point in &mut key.outline {
            *point = [point[0] - x0 + ROOM, point[1] - y0 + ROOM];
        }
    }
    (
        ui::taffy::Size {
            width: x1 - x0 + 2.0 * ROOM,
            height: y1 - y0 + 2.0 * ROOM,
        },
        keys,
    )
}

/// The main block of a keyboard, with the arrows in one row: a laptop's drawing.
fn compact_geometry(
    geometry: &pc_keyboard_geometry::Geometry,
) -> (f64, Vec<pc_keyboard_geometry::Key>) {
    const DROPPED: [&str; 12] = [
        "PRSC", "SCLK", "PAUS", "INS", "HOME", "PGUP", "DELE", "END", "PGDN", "NMLK", "RWIN",
        "MENU",
    ];
    let arrows = [
        ("LEFT", 230.0),
        ("UP", 249.0),
        ("DOWN", 268.0),
        ("RGHT", 287.0),
    ];
    let mut keys = Vec::new();
    for key in &geometry.keys {
        if DROPPED.contains(&key.name.as_str())
            || key.name.starts_with("KP")
            || key.name == "RCTL"
            || arrows.iter().any(|(name, _)| *name == key.name)
        {
            continue;
        }
        let mut key = key.clone();
        if key.name == "ESC" || key.name.starts_with("FK") {
            for point in &mut key.outline {
                point[1] += f64::from(LAPTOP_FUNCTION_ROW_DROP);
            }
        }
        keys.push(key);
    }
    for (name, x0) in arrows {
        keys.push(pc_keyboard_geometry::Key {
            name: name.to_owned(),
            outline: vec![
                [x0, 138.0],
                [x0 + 19.0, 138.0],
                [x0 + 19.0, 156.0],
                [x0, 156.0],
            ],
        });
    }
    (327.0, keys)
}

/// The legends of `spec`'s layout on `geometry`, each key with its outline.
fn compile(
    spec: &Spec,
    geometry: &[pc_keyboard_geometry::Key],
) -> Result<Vec<KeyboardMapKey>, String> {
    ui::rt::off_ui_thread();
    let names = geometry
        .iter()
        .map(|key| key.name.clone())
        .collect::<Vec<_>>();
    let native = match tilo_keyboard_map::compile(
        &spec.layout,
        &spec.variant,
        &spec.model,
        &spec.options,
        &names,
    ) {
        Ok(keys) => keys,
        Err(error) if error.is_unsupported() => {
            // No installed keyboard data on this host: the drawing's shape is in the geometry,
            // so a key can still be read; what it prints is the layout's to say.
            let levels = tilo_keyboard_map::unlabelled_levels(&names);
            return Ok(finish(geometry, &levels, spec));
        }
        Err(error) => return Err(error.to_string()),
    };
    Ok(finish(geometry, &native, spec))
}

fn finish(
    geometry: &[pc_keyboard_geometry::Key],
    native: &[tilo_keyboard_map::Key],
    spec: &Spec,
) -> Vec<KeyboardMapKey> {
    let caps = Keycaps::from_xkb(&spec.layout, &spec.variant);
    let english = Keycaps::from_xkb("us", "");
    native
        .iter()
        .zip(geometry)
        .map(|(key, outline)| {
            let levels = key
                .levels
                .iter()
                .map(|symbols| {
                    symbols
                        .iter()
                        .map(|symbol| {
                            let printed = caps.native_symbol(&symbol.name, &symbol.text);
                            if printed == english.native_symbol(&symbol.name, &symbol.text) {
                                short_legend(&symbol.name).map_or(printed, str::to_owned)
                            } else {
                                printed
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect();
            KeyboardMapKey {
                name: key.name.clone(),
                physical: key.physical,
                outline: outline
                    .outline
                    .iter()
                    .map(|&[x, y]| ui::kurbo::Point::new(x, y))
                    .collect(),
                levels,
            }
        })
        .collect()
}

/// The printed name of a navigation key, whose keysym is too long for a keycap.
fn short_legend(keysym: &str) -> Option<&'static str> {
    Some(match keysym {
        "Print" | "Sys_Req" => "PrtSc",
        "Scroll_Lock" => "ScrLk",
        "Pause" | "Break" => "Pause",
        "Num_Lock" => "Num",
        "Prior" | "Page_Up" => "PgUp",
        "Next" | "Page_Down" => "PgDn",
        "Insert" => "Ins",
        "Delete" => "Del",
        "Caps_Lock" => "Caps",
        "BackSpace" => "Bksp",
        "Escape" => "Esc",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_names_a_layout_and_its_variant_alone() {
        assert_eq!(parse_source("de"), ("de".to_owned(), String::new()));
        assert_eq!(parse_source(" us : intl "), ("us".to_owned(), "intl".to_owned()));
        assert_eq!(parse_source(""), (String::new(), String::new()));
        assert_eq!(
            parse_source("de;rm -rf /"),
            (String::new(), String::new()),
            "a source is a layout, never a path or a command"
        );
    }
}
