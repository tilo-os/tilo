//! Which backend serves each capability. The features select native
//! `GStreamer` backends and compile to nothing on the web, where the page's
//! media devices serve every capability; code gates on these cfgs.

fn main() {
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let web = arch == "wasm32";
    let feature = |name: &str| std::env::var_os(format!("CARGO_FEATURE_{name}")).is_some();
    let cfgs = [
        ("web_page", web && os == "unknown"),
        ("gst_camera", !web && feature("CAMERA")),
        ("gst_microphone", !web && feature("MICROPHONE")),
        ("gst_screen", !web && feature("SCREEN")),
        ("portal_screen", os == "linux" && feature("SCREEN")),
        ("sane_scanner", os == "linux" && feature("SCANNER")),
    ];
    for (cfg, on) in cfgs {
        println!("cargo::rustc-check-cfg=cfg({cfg})");
        if on {
            println!("cargo::rustc-cfg={cfg}");
        }
    }
    if cfgs.iter().skip(1).take(3).any(|(_, on)| *on) {
        println!("cargo::rustc-cfg=gst");
    }
    println!("cargo::rustc-check-cfg=cfg(gst)");
}
