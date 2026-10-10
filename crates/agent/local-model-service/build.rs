use std::env;
use std::io;
use std::path::PathBuf;
use std::process::Command;

fn checked(command: &mut Command) -> io::Result<()> {
    // CMake answers its own parallelism (`--parallel`); a jobserver handed
    // down from Cargo is not one CMake can use, and some hosts pass
    // descriptors its configure then reads forever.
    let status = command
        .env_remove("MAKEFLAGS")
        .env_remove("MFLAGS")
        .env_remove("CARGO_MAKEFLAGS")
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "speech CPU build failed: {status}"
        )))
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=native");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return Ok(());
    }
    if env::var_os("CARGO_FEATURE_WHISPER").is_none() {
        return Ok(());
    }
    let root = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").ok_or("Cargo manifest directory is missing")?,
    );
    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("Cargo output directory is missing")?);
    let build = output.join("speech-cpu");
    let destination = output.join("speech-install");
    let source = root.join("native/upstream/whisper.cpp");
    let compiler = cc::Build::new().cpp(true).get_compiler();
    let c_compiler = cc::Build::new().get_compiler();
    let flags = |tool: &cc::Tool| {
        tool.args()
            .iter()
            .map(|arg| format!("\"{}\"", arg.to_string_lossy().replace('"', "\\\"")))
            .collect::<Vec<_>>()
            .join(" ")
    };
    checked(
        Command::new("cmake")
            .arg("-S")
            .arg(root.join("native"))
            .arg("-B")
            .arg(&build)
            .arg("-DCMAKE_BUILD_TYPE=Release")
            .arg("-DCMAKE_POSITION_INDEPENDENT_CODE=ON")
            .arg("-DCMAKE_SYSTEM_NAME=Linux")
            .arg(format!(
                "-DCMAKE_SYSTEM_PROCESSOR={}",
                env::var("CARGO_CFG_TARGET_ARCH")?
            ))
            .arg(format!(
                "-DCMAKE_CXX_COMPILER={}",
                compiler.path().display()
            ))
            .arg(format!(
                "-DCMAKE_C_COMPILER={}",
                c_compiler.path().display()
            ))
            .arg(format!("-DCMAKE_CXX_FLAGS={}", flags(&compiler)))
            .arg(format!("-DCMAKE_C_FLAGS={}", flags(&c_compiler)))
            .arg(format!("-DTILO_WHISPER_ROOT={}", source.display()))
            .arg(format!("-DCMAKE_INSTALL_PREFIX={}", destination.display())),
    )?;
    // Native kernels are one owned heavy build; respect the Cargo job budget
    // and leave CPU headroom for the desktop and other sessions.
    let jobs = env::var("NUM_JOBS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, 2);
    checked(
        Command::new("cmake")
            .arg("--build")
            .arg(&build)
            .arg("--parallel")
            .arg(jobs.to_string()),
    )?;
    checked(Command::new("cmake").arg("--install").arg(&build))?;
    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .include(source.join("include"))
        .include(source.join("ggml/include"))
        .file(root.join("native/speech.cpp"))
        .compile("tilo-speech-bridge");
    println!(
        "cargo:rustc-link-search=native={}",
        destination.join("lib").display()
    );
    for library in ["whisper", "ggml", "ggml-base", "ggml-cpu"] {
        println!("cargo:rustc-link-lib=static={library}");
    }
    Ok(())
}
