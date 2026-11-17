//! Displays: the app keeps its `wl_output`s across a restart of the desktop. Each one is matched
//! to the desktop's by name and updated in place; one the desktop no longer has goes, but never
//! the last (an app whose last output disappears may crash).

use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::reexports::wayland_server::protocol::wl_output::Transform as WlTransform;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::Transform;
use wayland_client::protocol as c;
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1;

use crate::state::State;

/// One of the app's outputs.
pub(crate) struct KeptOutput {
    pub output: Output,
    global: GlobalId,
    /// The desktop generation that last described it.
    seen: u64,
}

/// What a desktop says of one of its outputs.
#[derive(Clone, Default)]
struct Info {
    make: String,
    model: String,
    physical: (i32, i32),
    subpixel: u32,
    transform: u32,
    mode: Option<(i32, i32, i32)>,
    scale: i32,
    position: (i32, i32),
    /// What the desktop calls the output, when it says more than make, model and name.
    description: Option<String>,
}

impl Info {
    /// The monitor as `wl_output` describes it.
    fn physical(&self) -> PhysicalProperties {
        PhysicalProperties {
            size: self.physical.into(),
            subpixel: subpixel(self.subpixel),
            make: self.make.clone(),
            model: self.model.clone(),
        }
    }
}

/// One of the desktop's outputs.
pub(crate) struct UpOutput {
    pub global: u32,
    pub wl: c::wl_output::WlOutput,
    pub xdg: Option<zxdg_output_v1::ZxdgOutputV1>,
    pub name: Option<String>,
    info: Info,
}

impl UpOutput {
    pub(crate) fn new(
        global: u32,
        wl: c::wl_output::WlOutput,
        xdg: Option<zxdg_output_v1::ZxdgOutputV1>,
    ) -> Self {
        Self {
            global,
            wl,
            xdg,
            name: None,
            info: Info {
                scale: 1,
                ..Info::default()
            },
        }
    }
}

/// Tell the app the description the desktop gave `output`, when it is not what the output derives
/// from make, model and name.
fn describe(output: &Output, description: Option<&str>) {
    if let Some(description) = description
        && output.description() != description
    {
        output.set_description(description);
    }
}

fn subpixel(raw: u32) -> Subpixel {
    match raw {
        1 => Subpixel::None,
        2 => Subpixel::HorizontalRgb,
        3 => Subpixel::HorizontalBgr,
        4 => Subpixel::VerticalRgb,
        5 => Subpixel::VerticalBgr,
        _ => Subpixel::Unknown,
    }
}

impl State {
    /// The desktop finished describing its output `global`: update the app's output of the same
    /// name, or make one.
    fn desktop_output_done(&mut self, global: u32) {
        let generation = self.generation;
        let Some(up) = self.up.as_ref() else {
            return;
        };
        let Some(output) = up.outputs.iter().find(|output| output.global == global) else {
            return;
        };
        let Some(name) = output.name.clone() else {
            return;
        };
        let info = output.info.clone();
        let mode = info.mode.map(|(w, h, refresh)| Mode {
            size: (w, h).into(),
            refresh,
        });
        let transform =
            WlTransform::try_from(info.transform).map_or(Transform::Normal, Transform::from);
        let position = Some(info.position.into());
        let scale = Some(Scale::Integer(info.scale.max(1)));
        let physical = info.physical();
        let at = self
            .outputs
            .iter()
            .position(|kept| kept.output.name() == name);
        if let Some(at) = at {
            // The monitor on that connector changed: make, model or size are another monitor
            // now, and `wl_output` has no event for them, so the app is shown a new output of
            // the same name where the old one stood.
            let Some(known) = self
                .outputs
                .get(at)
                .map(|kept| kept.output.physical_properties())
            else {
                return;
            };
            let same_monitor = known.make == physical.make
                && known.model == physical.model
                && known.size == physical.size
                && known.subpixel == physical.subpixel;
            if !same_monitor {
                let old = self.outputs.remove(at);
                self.dh.remove_global::<Self>(old.global);
                let output = Output::new(name, physical);
                if let Some(mode) = mode {
                    output.add_mode(mode);
                    output.set_preferred(mode);
                }
                output.change_current_state(mode, Some(transform), scale, position);
                describe(&output, info.description.as_deref());
                let global = output.create_global::<Self>(&self.dh);
                self.outputs.insert(
                    at,
                    KeptOutput {
                        output,
                        global,
                        seen: generation,
                    },
                );
                return;
            }
            let Some(kept) = self.outputs.get_mut(at) else {
                return;
            };
            if let Some(mode) = mode {
                kept.output.add_mode(mode);
                kept.output.set_preferred(mode);
            }
            kept.output
                .change_current_state(mode, Some(transform), scale, position);
            describe(&kept.output, info.description.as_deref());
            kept.seen = generation;
            return;
        }
        let output = Output::new(name, physical);
        if let Some(mode) = mode {
            output.add_mode(mode);
            output.set_preferred(mode);
        }
        output.change_current_state(mode, Some(transform), scale, position);
        describe(&output, info.description.as_deref());
        let global = output.create_global::<Self>(&self.dh);
        self.outputs.push(KeptOutput {
            output,
            global,
            seen: generation,
        });
    }

    /// The desktop removed an output while attached.
    pub(crate) fn desktop_output_removed(&mut self, global: u32) {
        let Some(up) = self.up.as_mut() else {
            return;
        };
        let Some(at) = up.outputs.iter().position(|output| output.global == global) else {
            return;
        };
        let removed = up.outputs.remove(at);
        removed.wl.release();
        if let Some(name) = removed.name {
            self.remove_outputs(|kept| kept.output.name() == name);
        }
    }

    /// The first state of a fresh desktop arrived: outputs it does not have go.
    pub(crate) fn drop_unseen_outputs(&mut self) {
        let generation = self.generation;
        if self.outputs.iter().any(|kept| kept.seen == generation) {
            self.remove_outputs(|kept| kept.seen != generation);
        }
    }

    /// Remove the outputs `which` names, keeping at least one.
    fn remove_outputs(&mut self, which: impl Fn(&KeptOutput) -> bool) {
        let mut index = 0;
        while index < self.outputs.len() {
            let remove = self.outputs.len() > 1 && self.outputs.get(index).is_some_and(&which);
            if remove {
                let kept = self.outputs.remove(index);
                self.dh.remove_global::<Self>(kept.global);
            } else {
                index = index.saturating_add(1);
            }
        }
    }

    /// The desktop says a surface entered or left its output `global`.
    pub(crate) fn surface_on_output(
        &self,
        surface: &WlSurface,
        global: Option<u32>,
        entered: bool,
    ) {
        let Some(up) = self.up.as_ref() else {
            return;
        };
        let Some(name) = up
            .outputs
            .iter()
            .find(|output| Some(output.global) == global)
            .and_then(|output| output.name.as_ref())
        else {
            return;
        };
        if let Some(kept) = self.outputs.iter().find(|kept| kept.output.name() == *name) {
            if entered {
                kept.output.enter(surface);
            } else {
                kept.output.leave(surface);
            }
        }
    }

    fn up_output(&mut self, global: u32) -> Option<&mut UpOutput> {
        self.up
            .as_mut()?
            .outputs
            .iter_mut()
            .find(|output| output.global == global)
    }
}

impl Dispatch<c::wl_output::WlOutput, u32> for State {
    fn event(
        state: &mut Self,
        _: &c::wl_output::WlOutput,
        event: c::wl_output::Event,
        global: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, c::wl_output::Event::Done) {
            state.desktop_output_done(*global);
            return;
        }
        let Some(output) = state.up_output(*global) else {
            return;
        };
        match event {
            c::wl_output::Event::Geometry {
                physical_width,
                physical_height,
                subpixel,
                make,
                model,
                transform,
                ..
            } => {
                output.info.make = make;
                output.info.model = model;
                output.info.physical = (physical_width, physical_height);
                if let WEnum::Value(subpixel) = subpixel {
                    output.info.subpixel = subpixel.into();
                }
                if let WEnum::Value(transform) = transform {
                    output.info.transform = transform.into();
                }
            }
            c::wl_output::Event::Mode {
                flags: WEnum::Value(flags),
                width,
                height,
                refresh,
            } if flags.contains(c::wl_output::Mode::Current) => {
                output.info.mode = Some((width, height, refresh));
            }
            c::wl_output::Event::Scale { factor } => output.info.scale = factor,
            c::wl_output::Event::Name { name } => output.name = Some(name),
            _ => {}
        }
    }
}

impl Dispatch<zxdg_output_v1::ZxdgOutputV1, u32> for State {
    fn event(
        state: &mut Self,
        xdg: &zxdg_output_v1::ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        global: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => {
                if let Some(output) = state.up_output(*global) {
                    output.info.position = (x, y);
                }
            }
            zxdg_output_v1::Event::Name { name } => {
                if let Some(output) = state.up_output(*global) {
                    output.name = Some(name);
                }
            }
            zxdg_output_v1::Event::Description { description } => {
                if let Some(output) = state.up_output(*global) {
                    output.info.description = Some(description);
                }
            }
            // Before version 3 the xdg-output's own done ends its description.
            zxdg_output_v1::Event::Done if wayland_client::Proxy::version(xdg) < 3 => {
                state.desktop_output_done(*global);
            }
            _ => {}
        }
    }
}
