//! Remote connections — standard Tilo application entry point.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    ui::launch::run_or_exit(&remote::launch_spec());
}
