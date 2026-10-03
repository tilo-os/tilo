//! The `GStreamer` features select native backends. They compile to nothing on
//! the web, so code gates on these cfgs rather than on the features.

fn main() {
    let web = std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32");
    println!("cargo::rustc-check-cfg=cfg(web_page)");
    if web && std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("unknown") {
        println!("cargo::rustc-cfg=web_page");
    }
    for (feature, cfg) in [
        ("DECODE", "gst_decode"),
        ("ENCODE", "gst_encode"),
        ("REALTIME", "gst_realtime"),
    ] {
        println!("cargo::rustc-check-cfg=cfg({cfg})");
        if !web && std::env::var_os(format!("CARGO_FEATURE_{feature}")).is_some() {
            println!("cargo::rustc-cfg={cfg}");
        }
    }
}
