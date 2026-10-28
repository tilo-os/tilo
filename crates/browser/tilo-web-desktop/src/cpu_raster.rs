//! The CPU rasterizer: the raster every page draws its frames with when the
//! browser offers no WebGPU (`rendererChoice`), loaded as its own module the
//! first time such a page mounts.
//!
//! Like an app's `definition()` (`crate::split`), the export below is the
//! only way to `tilo_cpu_raster`: the splitter moves the rasterizer, and
//! `vello_cpu` under it, into its own module, which the page fetches through
//! [`load`] before it mounts (`ui::cpu_canvas::load_cpu_raster`). A visitor
//! whose browser has WebGPU never downloads it.

use std::ffi::c_void;
use std::future::Future;
use std::pin::Pin;

use tilo_cpu_raster::{CpuRasterizer, Options, Region, Target};
use ui::cpu_canvas::{MakeRaster, Paint, Raster};

use crate::split::Callback;

/// The raster, drawing on the calling thread: `wasm32-unknown-unknown` builds
/// without threads, so `vello_cpu` can spawn none.
struct Canvas {
    rasterizer: CpuRasterizer,
    regions: Vec<Region>,
}

impl Raster for Canvas {
    fn paint(&mut self, paint: Paint<'_>) {
        self.regions.clear();
        self.regions.extend(
            paint
                .regions
                .iter()
                .map(|region| Region::new(region.x, region.y, region.width, region.height)),
        );
        let Ok(mut target) = Target::new(paint.pixels, paint.width, paint.height, paint.stride)
        else {
            return;
        };
        self.rasterizer.prepare(paint.encoding);
        self.rasterizer.render_prepared(
            &mut target,
            Some(&self.regions),
            tilo_cpu_raster::color::AlphaColor::new(paint.base_color.components),
        );
    }
}

fn make() -> Box<dyn Raster> {
    Box::new(Canvas {
        rasterizer: CpuRasterizer::new(Options {
            threads: 0,
            ..Options::default()
        }),
        regions: Vec::new(),
    })
}

#[link(wasm_import_module = "./__wasm_split.js")]
#[allow(
    improper_ctypes,
    reason = "the function pointer only passes between Rust"
)]
unsafe extern "C" {
    #[link_name = "__wasm_split_load_cpu_raster_9f2b61c07d5ae834_raster"]
    fn load_module(callback: Callback, data: *const c_void);
    #[link_name = "__wasm_split_00___cpu_raster___00_import_9f2b61c07d5ae834_raster"]
    fn raster() -> MakeRaster;
}

#[unsafe(export_name = "__wasm_split_00___cpu_raster___00_export_9f2b61c07d5ae834_raster")]
#[allow(
    improper_ctypes_definitions,
    reason = "the function pointer only passes between Rust"
)]
extern "C" fn export() -> MakeRaster {
    make
}

/// Fetch the rasterizer's module, then hand it to the canvas that asked.
pub(crate) fn load() -> Pin<Box<dyn Future<Output = bool>>> {
    Box::pin(async {
        if !crate::split::load(load_module).await {
            return false;
        }
        // SAFETY: the module that defines `raster` is loaded.
        ui::cpu_canvas::install_cpu_raster(unsafe { raster() });
        true
    })
}

pub(crate) fn register() {
    ui::cpu_canvas::defer_cpu_raster(load);
}
