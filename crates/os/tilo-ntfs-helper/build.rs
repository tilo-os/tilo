use std::{env, path::PathBuf, process::Command};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if env::var("CARGO_CFG_TARGET_FAMILY")? != "unix" {
        return Ok(());
    }
    let compiler = env::var("TILO_NTFS_CC").unwrap_or_else(|_| "cc".into());
    let archiver = env::var("TILO_NTFS_AR").unwrap_or_else(|_| "ar".into());
    let out = PathBuf::from(env::var("OUT_DIR")?);
    let status = Command::new("python3")
        .args(["build_native.py", "--output"])
        .arg(&out)
        .status()?;
    if !status.success() {
        return Err("pinned native build failed".into());
    }
    let src = out.join("ntfs-3g-d327833ec1d5eb1358b6f2c37139f10a3460944d");
    for source in ["ntfs_reader", "strict_logfile"] {
        let status = Command::new(&compiler)
            .args([
                "-std=c11",
                "-D_DARWIN_C_SOURCE",
                "-D_GNU_SOURCE",
                "-DHAVE_CONFIG_H",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-fPIC",
                "-c",
                &format!("native/{source}.c"),
                "-o",
            ])
            .arg(out.join(format!("{source}.o")))
            .arg("-I")
            .arg(&src)
            .arg("-I")
            .arg(src.join("include/ntfs-3g"))
            .status()?;
        if !status.success() {
            return Err("reader C compilation failed".into());
        }
    }
    if !Command::new(&compiler)
        .args([
            "-std=c11",
            "-D_DARWIN_C_SOURCE",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-fPIC",
            "-c",
            "native/sandbox.c",
            "-o",
        ])
        .arg(out.join("sandbox.o"))
        .status()?
        .success()
    {
        return Err("sandbox compilation failed".into());
    }
    if !Command::new(&archiver)
        .arg("rcs")
        .arg(out.join("libtilo_ntfs_reader.a"))
        .arg(out.join("ntfs_reader.o"))
        .arg(out.join("strict_logfile.o"))
        .arg(out.join("sandbox.o"))
        .status()?
        .success()
    {
        return Err("reader archive failed".into());
    }
    println!("cargo:rustc-link-search=native={}", out.display());
    println!(
        "cargo:rustc-link-search=native={}",
        src.join("libntfs-3g/.libs").display()
    );
    println!("cargo:rustc-link-lib=static=tilo_ntfs_reader");
    println!("cargo:rustc-link-lib=static=ntfs-3g");
    if env::var("CARGO_CFG_TARGET_OS")? == "macos" {
        println!("cargo:rustc-link-lib=iconv");
    }
    for name in [
        "TILO_NTFS_CC",
        "TILO_NTFS_AR",
        "TILO_NTFS_HOST",
        "TILO_NTFS_NM",
        "TILO_NTFS_RANLIB",
        "TILO_NTFS_SOURCE_CACHE",
        "TILO_NTFS_SOURCE_OFFLINE",
        "CARGO_NET_OFFLINE",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    println!("cargo:rerun-if-changed=native");
    println!("cargo:rerun-if-changed=vendor");
    println!("cargo:rerun-if-changed=build_native.py");
    println!("cargo:rerun-if-changed=source_artifact.py");
    Ok(())
}
