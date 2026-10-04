#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

//! Calculator — thin binary entry point. All logic lives in the library; the
//! framework runs the live window and the standard `--shots`, `--headless`
//! and `--native-audit` modes.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::android_main!();

/// Run the calculator.
pub fn main() {
    #[cfg(not(target_arch = "wasm32"))]
    ui::framework::run::<calculator::CalculatorShell>();
}
