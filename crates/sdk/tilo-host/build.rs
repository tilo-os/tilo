//! `cfg(freedesktop)`: a Unix desktop with a D-Bus session bus and the XDG
//! Desktop Portal. Android is Unix but has neither; its host services come
//! from the activity instead (`native::android`).

fn main() {
    println!("cargo::rustc-check-cfg=cfg(freedesktop)");
    let unix = std::env::var_os("CARGO_CFG_UNIX").is_some();
    let android = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "android");
    if unix && !android {
        println!("cargo::rustc-cfg=freedesktop");
    }
}
