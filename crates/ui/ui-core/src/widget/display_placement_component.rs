//! [`DisplayPlacement`] as a described component: the Displays pane's drag-to-arrange canvas.

use crate::LocalState;
use crate::component::{Accepts, Component, Described, Event, PartType, Source};
use crate::widget::display_placement::{
    DisplayPlacement, PlacedDisplay, PlacementGesture, SharedDisplayState, placement::Placement,
};

/// Screens arranged by dragging them, in one layout across the person's devices: the edge a
/// screen is dropped against is the edge the pointer crosses, and a screen dropped apart from
/// the others is drawn out of their reach.
#[derive(Component, Default)]
#[component(name = "PlacementCanvas", role = "Group", label = "name")]
pub struct PlacementCanvasProps {
    /// What the canvas is, for assistive technology.
    pub name: Option<String>,
    /// The screens drawn on it.
    #[component(part(default))]
    pub screens: Vec<PlacementScreenProps>,
    /// The connector of the screen the person chose, drawn chosen; none or empty for none.
    #[component(state)]
    pub selected: Option<String>,
    /// Each screen shows its number, as Identify does.
    pub identify: bool,
    /// A screen was moved: carries its connector and its new top-left in the shared layout, its
    /// x and then its y. What the view does with it is its own.
    #[component(event(value = String, at = Vec<i64>))]
    pub moved: Event,
    /// A screen was chosen: carries its connector.
    #[component(event(value = String))]
    pub chose: Event,
}

/// One screen on the canvas: what it is, where it stands and what it moves with.
#[derive(Component, Default)]
#[component(name = "PlacementScreen", role = "Generic", label = "name")]
pub struct PlacementScreenProps {
    /// The screen's connector, which a move and a choice name it by.
    #[component(key)]
    pub key: String,
    /// Its spoken name, shown on it.
    pub name: Option<String>,
    /// The number Identify shows on it.
    pub number: Option<i64>,
    /// Its top-left in the shared layout, in logical pixels.
    pub x: Option<i64>,
    /// Its top, in logical pixels.
    pub y: Option<i64>,
    /// How wide it stands, in logical pixels.
    pub width: Option<i64>,
    /// How tall it stands, in logical pixels.
    pub height: Option<i64>,
    /// It is the primary screen.
    pub primary: bool,
    /// It can be dragged; a mirrored screen sits on its target and follows it instead.
    pub movable: bool,
    /// The screens that move together with it: a device's own panels.
    pub group: Option<String>,
    /// The device that drives it, when it is another of the person's devices' screen.
    pub device: Option<String>,
    /// Its stable output id on that device.
    pub output: Option<String>,
    /// Whether that device is online; other devices' screens are unless said otherwise.
    pub online: Option<bool>,
    /// Whether the pointer reaches it from the other screens.
    pub reachable: Option<bool>,
}

/// A canvas's screens: [`PlacementScreenProps`] only.
impl PartType for Vec<PlacementScreenProps> {
    const MANY: bool = true;

    fn accepts() -> Accepts {
        Accepts::Component(PlacementScreenProps::NAME.into())
    }

    fn fill(&mut self, source: &mut dyn Source, part: &str) -> Result<(), String> {
        let mut screens = Vec::new();
        source.components(part, &mut |child| {
            screens.push(
                PlacementScreenProps::from_source(child).map_err(|error| error.to_string())?,
            );
            Ok(())
        })?;
        *self = screens;
        Ok(())
    }
}

/// A number a prop gives, within the canvas's own bounds.
fn i32_of(value: i64) -> i32 {
    i32::try_from(value).unwrap_or_else(|_| if value.is_negative() { i32::MIN } else { i32::MAX })
}

fn u32_of(value: i64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn usize_of(value: i64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

fn placed(screen: PlacementScreenProps) -> PlacedDisplay {
    let PlacementScreenProps {
        key: connector,
        name,
        number,
        x,
        y,
        width,
        height,
        primary,
        movable,
        group,
        device,
        output,
        online,
        reachable,
    } = screen;
    let shared = device.map(|device| SharedDisplayState {
        device,
        output: output.unwrap_or_else(|| connector.clone()),
        online: online.unwrap_or(true),
        reachable: reachable.unwrap_or(true),
    });
    PlacedDisplay {
        label: name.unwrap_or_else(|| connector.clone()),
        connector,
        movement_group: group,
        shared,
        number: usize_of(number.unwrap_or(0)),
        placement: Placement::new(
            i32_of(x.unwrap_or(0)),
            i32_of(y.unwrap_or(0)),
            u32_of(width.unwrap_or(0)),
            u32_of(height.unwrap_or(0)),
        ),
        primary,
        movable,
    }
}

impl Described for DisplayPlacement {
    type Props = PlacementCanvasProps;

    fn from_props(_key: &str, props: PlacementCanvasProps) -> Self {
        let PlacementCanvasProps {
            name,
            screens,
            selected,
            identify,
            moved,
            chose,
        } = props;
        let selection = LocalState::new(selected.unwrap_or_default());
        let mut canvas = Self::new(
            screens.into_iter().map(placed).collect(),
            Vec::new(),
            LocalState::new(PlacementGesture::default()),
        )
        .with_selection(selection);
        if let Some(name) = name {
            canvas = canvas.with_accessible_name(name);
        }
        if identify {
            let now = crate::time::Instant::now();
            canvas = canvas.identifying(Some((now, now + crate::time::Duration::from_secs(86_400))));
        }
        canvas.on_move(moved).on_choose(chose)
    }
}
