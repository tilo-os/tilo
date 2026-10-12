#![cfg(target_os = "linux")]

use std::error::Error;
use std::fs;
use std::path::Path;
use std::process::Command;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn require(condition: bool, message: &str) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[test]
fn production_native_archives_do_not_embed_build_timestamps() -> TestResult {
    let directory = Path::new(env!("OUT_DIR")).join("speech-install/lib");
    for name in ["whisper", "ggml", "ggml-base", "ggml-cpu"] {
        let bytes = fs::read(directory.join(format!("lib{name}.a")))?;
        require(
            bytes.get(..8) == Some(b"!<arch>\n".as_slice()),
            "invalid archive magic",
        )?;
        let mut offset: usize = 8;
        let mut members = 0;
        while offset < bytes.len() {
            let end = offset.checked_add(60).ok_or("archive header overflow")?;
            let header = bytes.get(offset..end).ok_or("truncated archive header")?;
            let timestamp = std::str::from_utf8(header.get(16..28).ok_or("missing timestamp")?)?;
            // GNU long-name tables have no timestamp field. Object members
            // and the symbol index must carry the deterministic zero value.
            let expected = if header.get(..16) == Some(b"//              ".as_slice()) {
                ""
            } else {
                "0"
            };
            require(
                timestamp.trim() == expected,
                &format!("{name} has a volatile archive member: {timestamp:?}"),
            )?;
            let size = std::str::from_utf8(header.get(48..58).ok_or("missing size")?)?
                .trim()
                .parse::<usize>()?;
            offset = end
                .checked_add(size)
                .and_then(|v| v.checked_add(size % 2))
                .ok_or("archive member overflow")?;
            members += 1;
        }
        require(
            offset == bytes.len(),
            "archive member extends beyond the archive",
        )?;
        require(
            members > 1,
            &format!("{name} must contain actual native object files"),
        )?;
    }
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> TestResult {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let output = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &output)?;
        } else {
            fs::copy(entry.path(), output)?;
        }
    }
    Ok(())
}

fn checked(command: &mut Command) -> TestResult<String> {
    let output = command.output()?;
    require(
        output.status.success(),
        &String::from_utf8_lossy(&output.stderr),
    )?;
    Ok(String::from_utf8(output.stdout)?)
}

fn configure(source: &Path, build: &Path, git: &Path) -> TestResult<String> {
    let output = checked(
        Command::new("cmake")
            .arg("-S")
            .arg(source)
            .arg("-B")
            .arg(build)
            .arg(format!(
                "-DTILO_WHISPER_ROOT={}",
                source.join("upstream/whisper.cpp").display()
            ))
            .arg("-DCMAKE_SYSTEM_NAME=Linux")
            .arg(format!(
                "-DCMAKE_SYSTEM_PROCESSOR={}",
                std::env::consts::ARCH
            ))
            .env("GIT_DIR", git)
            .env("GIT_WORK_TREE", source),
    )?;
    let identity = output
        .lines()
        .find_map(|line| line.strip_prefix("-- ggml commit:  "))
        .ok_or("production CMake did not report a native source identity")?;
    require(
        identity.starts_with("sha256:"),
        "native identity is not source-addressed",
    )?;
    Ok(identity.to_owned())
}

#[test]
fn real_cmake_uses_native_source_identity_across_parent_git_heads() -> TestResult {
    let temporary = tempfile::tempdir()?;
    let source = temporary.path().join("native");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("native"),
        &source,
    )?;
    let git = temporary.path().join("parent.git");
    checked(Command::new("git").arg("init").arg("--bare").arg(&git))?;
    let command = |args: &[&str]| {
        let mut command = Command::new("git");
        command
            .args(args)
            .env("GIT_DIR", &git)
            .env("GIT_WORK_TREE", &source)
            .current_dir(&source);
        command
    };
    checked(&mut command(&["config", "user.name", "Native build test"]))?;
    checked(&mut command(&[
        "config",
        "user.email",
        "native-build@example.invalid",
    ]))?;
    checked(&mut command(&["config", "commit.gpgsign", "false"]))?;
    checked(&mut command(&["add", "--all"]))?;
    checked(&mut command(&["commit", "-m", "parent one"]))?;
    let build = temporary.path().join("build");
    let first = configure(&source, &build, &git)?;
    checked(&mut command(&[
        "commit",
        "--allow-empty",
        "-m",
        "parent two",
    ]))?;
    require(
        first == configure(&source, &build, &git)?,
        "unrelated parent HEAD invalidated native identity",
    )?;
    let input = source.join("upstream/whisper.cpp/include/whisper.h");
    let mut changed = fs::read(&input)?;
    changed.extend_from_slice(b"\n/* native source identity test */\n");
    fs::write(input, changed)?;
    require(
        first != configure(&source, &build, &git)?,
        "changed native source did not invalidate its identity",
    )?;
    Ok(())
}
