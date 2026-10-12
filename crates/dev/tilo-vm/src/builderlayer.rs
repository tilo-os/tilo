//! `--builder remote`: build a component's layer on a remote builder of
//! the device's architecture and stream it through this host to the
//! device, without its binaries ever landing on this host's disk.
//!
//! The builder is a cargo drop-in whose `run` runs the program on a machine
//! of that architecture, in this worktree, with its stdout passed through as
//! text lines (`[remote_builders]` in vms.toml names its command line). It
//! runs `tilo-vm layer-pack` there ([`pack`]): the component builds with the
//! same cargo arguments as on every other builder, the manifest installer
//! lays it out, and the layer tar goes to stdout as framed base64 lines.
//! [`Stream`] reads those lines here and hands the decoded bytes straight to
//! tilo-vmd's upload. A plain remote `build` would bring the binaries back
//! into this host's target dir instead.

use std::io::{BufRead, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tilo_vm_protocol::manifest::Component;

use crate::build;
use crate::error::CliError;
use crate::hypervisor::CommandLine;

/// The line before the layer: its [`Packed`] facts as JSON.
pub const BEGIN: &str = "tilo-vm-layer-begin ";
/// One line of layer bytes, base64.
pub const DATA: &str = "tilo-vm-layer-data ";
/// The line after it: `<sha256> <bytes>`.
pub const END: &str = "tilo-vm-layer-end ";

/// Raw bytes per data line: a multiple of 3, so only the last line pads.
const LINE_BYTES: usize = 3 << 18;

/// What the builder says about the layer it built.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Packed {
    pub component: String,
    /// `<arch>-unknown-linux-gnu`, from the built binary.
    pub target: Option<String>,
    /// `rustc <version> (...)`, from the built binary.
    pub toolchain: Option<String>,
    #[serde(default)]
    pub source_digest: Option<String>,
    /// Canonical content identity, not the tar archive digest. Older builders omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer_hash: Option<String>,
    /// An explicit cache result; its framed stream must contain zero archive bytes.
    #[serde(default)]
    pub reused: bool,
    /// Raw bounded chunks instead of base64 data lines.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub binary: bool,
}

/// The remote builder's `run` that builds and streams `component`'s layer:
/// `builder` is its command line up to the cargo subcommand.
pub fn remote_command(builder: &[String], component: &Component, extra: &[String]) -> CommandLine {
    let (program, prefix) = builder
        .split_first()
        .map_or(("cargo", &[][..]), |(program, rest)| {
            (program.as_str(), rest)
        });
    let mut command = CommandLine::new(program, &[]);
    command.args = prefix.to_vec();
    command.args.extend(
        [
            "run",
            "--locked",
            "-p",
            "tilo-vm",
            "--bin",
            "tilo-vm",
            "--",
            "layer-pack",
            "--component",
            &component.name,
            "--profile",
            &component.profile,
        ]
        .map(str::to_owned),
    );
    if !extra.is_empty() {
        command.args.push("--features".into());
        command.args.push(extra.join(","));
    }
    command
}

/// `tilo-vm layer-pack`, on the builder: build `name` from the worktree in
/// the current directory, lay it out, and write the framed layer to stdout.
/// The build's own output goes to stderr.
///
/// # Errors
/// An unknown component, a failed build or installer, I/O.
pub fn pack(name: &str, profile: &str, extra: &[String]) -> Result<(), CliError> {
    pack_with_source(name, profile, extra, None, &[], false)
}

pub(crate) fn pack_with_source(
    name: &str,
    profile: &str,
    extra: &[String],
    inventory: Option<&Path>,
    cached: &[String],
    binary: bool,
) -> Result<(), CliError> {
    if cached.len() > 8 || cached.iter().any(|hash| !valid_hash(hash)) {
        return Err(CliError::InvalidInput(
            "at most eight lowercase SHA-256 cached layer hashes are accepted".into(),
        ));
    }
    let root = std::env::current_dir().map_err(CliError::io("find the worktree"))?;
    let source_digest = inventory
        .map(|path| crate::source_inputs::verify(&root, path))
        .transpose()?;
    let manifest_path = root.join("os/components.toml");
    let manifest = tilo_vm_protocol::Manifest::load(&manifest_path)
        .map_err(|e| CliError::Config {
            path: manifest_path.display().to_string(),
            detail: e.to_string(),
        })?
        .ok_or_else(|| CliError::NoManifest {
            path: manifest_path.display().to_string(),
        })?;
    let component = manifest
        .components
        .get(name)
        .cloned()
        .ok_or_else(|| CliError::UnknownComponent {
            name: name.to_owned(),
            known: manifest.components.keys().cloned().collect(),
        })?
        .with_profile(Some(profile));
    // `cargo run` sets CARGO to the cargo that runs this program.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    if !component.image_assembled {
        run_to_stderr(Command::new(&cargo).args(component.cargo_args(extra)))?;
    } else if component.name != "apps+runtime" {
        return Err(CliError::InvalidInput(
            "no maintained assembler for this image component".into(),
        ));
    }
    let metadata = Command::new(&cargo)
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice(&o.stdout).ok())
        .unwrap_or(serde_json::Value::Null);
    let target = build::target_dir(&metadata);
    let staging_parent = Path::new(&target).join("tilo-vm-layers");
    std::fs::create_dir_all(&staging_parent)
        .map_err(CliError::io(format!("create {}", staging_parent.display())))?;
    let staging = tempfile::Builder::new()
        .prefix("layer-")
        .tempdir_in(&staging_parent)
        .map_err(CliError::io("create layer staging"))?;
    let tree = staging.path().join("tree");
    let from = format!("{target}/{}", component.profile_dir());
    if component.image_assembled {
        run_to_stderr(Command::new("python3").args([
            "os/dev/assemble-app-runtime.py",
            "--repo",
            &root.display().to_string(),
            "--target-dir",
            &target,
            "--profile",
            &component.profile,
            "--features",
            &component.build_features(extra).join(","),
            "--dest",
            &tree.display().to_string(),
        ]))?;
    } else {
        run_to_stderr(
            Command::new("python3")
                .args(component.installer_args(&from, &tree.display().to_string())),
        )?;
    }
    if let Some(inventory) = inventory
        && crate::source_inputs::verify(&root, inventory)?
            != source_digest.clone().unwrap_or_default()
    {
        return Err(CliError::InvalidInput(
            "source changed during component assembly".into(),
        ));
    }
    let facts = build::first_elf(&tree).unwrap_or_default();
    log_native_facts(&component.name, &facts, source_digest.as_deref());
    let packed = Packed {
        component: component.name,
        binary,
        target: facts.target,
        toolchain: facts.toolchain,
        source_digest,
        ..Packed::default()
    };
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    write_layer(&tree, packed, cached, &mut out)
}

fn log_native_facts(component: &str, facts: &build::ElfFacts, source_digest: Option<&str>) {
    eprintln!(
        "native layer facts: component={} target={:?} toolchain={:?} source_digest={:?}",
        component,
        facts.target,
        facts
            .toolchain
            .as_deref()
            .map(|value| value.chars().take(160).collect::<String>()),
        source_digest,
    );
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The canonical assembler output decides reuse, never the source revision alone.
pub(crate) fn write_layer(
    tree: &Path,
    mut packed: Packed,
    cached: &[String],
    out: &mut impl Write,
) -> Result<(), CliError> {
    let hash = tilo_vm_protocol::layer_hash::tree_hash(tree)
        .map_err(CliError::io("hash assembled remote layer"))?;
    packed.reused = cached.contains(&hash);
    packed.layer_hash = Some(hash);
    let header =
        serde_json::to_string(&packed).map_err(|e| CliError::InvalidInput(e.to_string()))?;
    writeln!(out, "{BEGIN}{header}").map_err(CliError::io("write the layer"))?;
    if packed.binary {
        let mut binary = tilo_vm_protocol::binary::Encoder::new(out);
        if !packed.reused {
            build::pack_into(tree, &mut binary)?;
        }
        binary
            .finish()
            .map_err(CliError::io("write binary layer"))?;
        return Ok(());
    }
    let mut lines = Lines::new(out);
    if !packed.reused {
        build::pack_into(tree, &mut lines)?;
    }
    let (sha256, bytes) = lines.finish().map_err(CliError::io("write the layer"))?;
    writeln!(out, "{END}{sha256} {bytes}").map_err(CliError::io("write the layer"))?;
    out.flush().map_err(CliError::io("write the layer"))
}

/// Run `command` with its stdout on our stderr: our stdout is the layer.
pub(crate) fn run_to_stderr(command: &mut Command) -> Result<(), CliError> {
    let shown = format!("{command:?}");
    let stderr = std::os::fd::AsFd::as_fd(&std::io::stderr())
        .try_clone_to_owned()
        .map_err(CliError::io("duplicate stderr"))?;
    let status = command
        .stdout(Stdio::from(stderr))
        .status()
        .map_err(CliError::io(format!("start {shown}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(CliError::CommandFailed {
            command: shown,
            status: status.to_string(),
            stderr: "its output is above".into(),
        })
    }
}

/// Bytes written as [`DATA`] lines, hashed and counted on the way.
pub struct Lines<'a, W: Write> {
    out: &'a mut W,
    pending: Vec<u8>,
    hasher: Sha256,
    bytes: u64,
}

impl<'a, W: Write> Lines<'a, W> {
    #[must_use]
    pub fn new(out: &'a mut W) -> Self {
        Self {
            out,
            pending: Vec::with_capacity(LINE_BYTES),
            hasher: Sha256::new(),
            bytes: 0,
        }
    }

    fn emit(&mut self) -> std::io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        self.out.write_all(DATA.as_bytes())?;
        self.out
            .write_all(STANDARD.encode(&self.pending).as_bytes())?;
        self.out.write_all(b"\n")?;
        self.pending.clear();
        Ok(())
    }

    /// Finish the legacy builder frames with their archive digest.
    /// # Errors
    /// An output write failure.
    pub fn finish(mut self) -> std::io::Result<(String, u64)> {
        self.emit()?;
        Ok((hex::encode(self.hasher.finalize()), self.bytes))
    }
}

impl<W: Write> Write for Lines<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let room = LINE_BYTES.saturating_sub(self.pending.len());
        let taken = buf.get(..room.min(buf.len())).unwrap_or_default();
        self.pending.extend_from_slice(taken);
        self.hasher.update(taken);
        self.bytes = self.bytes.saturating_add(taken.len() as u64);
        if self.pending.len() >= LINE_BYTES {
            self.emit()?;
        }
        Ok(taken.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

/// The layer as it arrives from the builder: [`Self::header`] passes the
/// build's own output on to stderr until the layer begins, then the
/// stream reads as the layer's bytes, checked against the end line.
pub struct Stream<R: BufRead> {
    input: R,
    decoded: Vec<u8>,
    at: usize,
    hasher: Sha256,
    bytes: u64,
    done: bool,
    binary: Option<tilo_vm_protocol::binary::Decoder>,
}

impl<R: BufRead> Stream<R> {
    /// Artifact files must contain precisely one complete stream, with no trailing data.
    pub(crate) fn finish(&mut self) -> std::io::Result<()> {
        if std::io::Read::read(self, &mut [0u8; 1])? != 0 || !self.input.fill_buf()?.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unexpected trailing layer bytes",
            ));
        }
        Ok(())
    }

    pub fn new(input: R) -> Self {
        Self {
            input,
            decoded: Vec::new(),
            at: 0,
            hasher: Sha256::new(),
            bytes: 0,
            done: false,
            binary: None,
        }
    }

    /// Wait for the layer: lines before it are the build's output and go to
    /// `passthrough`.
    ///
    /// # Errors
    /// The stream ended without a layer (the build failed), or I/O.
    pub fn header(&mut self, passthrough: &mut dyn Write) -> std::io::Result<Packed> {
        self.header_as(passthrough)
    }

    pub(crate) fn header_as<T: serde::de::DeserializeOwned>(
        &mut self,
        passthrough: &mut dyn Write,
    ) -> std::io::Result<T> {
        let mut line = String::new();
        loop {
            line.clear();
            if self.input.read_line(&mut line)? == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "the builder sent no layer",
                ));
            }
            if let Some(json) = line.trim_end().strip_prefix(BEGIN) {
                let value: serde_json::Value =
                    serde_json::from_str(json).map_err(std::io::Error::other)?;
                if value.get("binary").and_then(serde_json::Value::as_bool) == Some(true) {
                    self.binary = Some(tilo_vm_protocol::binary::Decoder::default());
                }
                return serde_json::from_value(value).map_err(std::io::Error::other);
            }
            passthrough.write_all(line.as_bytes())?;
        }
    }
}

impl<R: BufRead> Read for Stream<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if let Some(binary) = &mut self.binary {
            return binary.read(&mut self.input, buf);
        }
        if buf.is_empty() {
            return Ok(0);
        }
        while self.at >= self.decoded.len() {
            if self.done {
                return Ok(0);
            }
            let mut line = String::new();
            if self.input.read_line(&mut line)? == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "the layer stream ended before its end line",
                ));
            }
            let line = line.trim_end();
            if let Some(data) = line.strip_prefix(DATA) {
                self.decoded = STANDARD.decode(data).map_err(std::io::Error::other)?;
                self.at = 0;
                self.hasher.update(&self.decoded);
                self.bytes = self.bytes.saturating_add(self.decoded.len() as u64);
            } else if let Some(end) = line.strip_prefix(END) {
                let digest = hex::encode(self.hasher.clone().finalize());
                if end != format!("{digest} {}", self.bytes) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "the layer arrived damaged: {} bytes, sha256 {digest}; the builder sent {end}",
                            self.bytes
                        ),
                    ));
                }
                self.done = true;
            }
            // Anything else between the lines is the program's stray output.
        }
        let rest = self.decoded.get(self.at..).unwrap_or_default();
        let n = rest.len().min(buf.len());
        buf.get_mut(..n)
            .unwrap_or_default()
            .copy_from_slice(rest.get(..n).unwrap_or_default());
        self.at = self.at.saturating_add(n);
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A layer framed on the builder arrives byte for byte, with the build's
    /// output before it passed on; damage is refused.
    #[test]
    fn a_framed_layer_streams_byte_for_byte_and_damage_is_refused() {
        let layer: Vec<u8> = (0..(LINE_BYTES * 2 + 1234))
            .map(|i| u8::try_from(i % 251).unwrap())
            .collect();
        let mut framed = b"   Compiling shell\n".to_vec();
        writeln!(
            framed,
            "{BEGIN}{}",
            serde_json::to_string(&Packed {
                component: "shell".into(),
                target: Some("aarch64-unknown-linux-gnu".into()),
                toolchain: None,
                source_digest: None,
                ..Packed::default()
            })
            .unwrap()
        )
        .unwrap();
        let mut lines = Lines::new(&mut framed);
        lines.write_all(&layer).unwrap();
        let (sha, bytes) = lines.finish().unwrap();
        writeln!(framed, "{END}{sha} {bytes}").unwrap();
        assert_eq!(
            String::from_utf8_lossy(&framed).lines().count(),
            2 + 3 + 1,
            "three data lines"
        );

        let mut stream = Stream::new(&framed[..]);
        let mut passed = Vec::new();
        let packed = stream.header(&mut passed).unwrap();
        assert_eq!(packed.target.as_deref(), Some("aarch64-unknown-linux-gnu"));
        assert_eq!(passed, b"   Compiling shell\n");
        let mut received = Vec::new();
        stream.read_to_end(&mut received).unwrap();
        assert_eq!(received, layer);

        // One flipped byte in a data line, or a cut stream, is an error.
        let text = String::from_utf8(framed.clone()).unwrap();
        let damaged = text.replacen(&format!("{DATA}AAEC"), &format!("{DATA}AAED"), 1);
        assert_ne!(damaged, text);
        let mut stream = Stream::new(damaged.as_bytes());
        stream.header(&mut Vec::new()).unwrap();
        assert!(stream.read_to_end(&mut Vec::new()).is_err());
        let cut = &framed[..framed.len() - 80];
        let mut stream = Stream::new(cut);
        stream.header(&mut Vec::new()).unwrap();
        assert!(stream.read_to_end(&mut Vec::new()).is_err());
        // A build that failed sends no layer.
        assert!(
            Stream::new(&b"error: could not compile\n"[..])
                .header(&mut Vec::new())
                .is_err()
        );
    }

    /// The builder's `run` packs the layer there, after its own arguments.
    #[test]
    fn the_remote_run_packs_on_the_builder() {
        let component = tilo_vm_protocol::Manifest::parse(
            "[[component]]\nname = \"shell\"\npackages = [\"shell\"]\n",
        )
        .unwrap()
        .components
        .remove("shell")
        .unwrap()
        .with_profile(Some("release"));
        let builder = ["env", "BUILDER=arm", "remote-cargo", "--on-builder"].map(str::to_owned);
        let line = remote_command(&builder, &component, &["shell/dev-overrides".into()]);
        assert_eq!(
            line.display(),
            "env BUILDER=arm remote-cargo --on-builder run --locked -p tilo-vm --bin tilo-vm -- layer-pack --component shell --profile release --features shell/dev-overrides"
        );
    }
}

#[cfg(test)]
mod binary_tests {
    use super::*;
    #[test]
    fn binary_builder_footer_is_checked_before_eof() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("usr/bin")).unwrap();
        std::fs::write(dir.path().join("usr/bin/probe"), vec![71; 2 << 20]).unwrap();
        let mut framed = Vec::new();
        write_layer(
            dir.path(),
            Packed {
                binary: true,
                ..Packed::default()
            },
            &[],
            &mut framed,
        )
        .unwrap();
        let mut stream = Stream::new(framed.as_slice());
        assert!(stream.header(&mut Vec::new()).unwrap().binary);
        let mut archive = Vec::new();
        stream.read_to_end(&mut archive).unwrap();
        assert!(archive.len() > 2 << 20);
        *framed.last_mut().unwrap() ^= 1;
        let mut stream = Stream::new(framed.as_slice());
        stream.header(&mut Vec::new()).unwrap();
        assert!(stream.read_to_end(&mut Vec::new()).is_err());
    }
}
