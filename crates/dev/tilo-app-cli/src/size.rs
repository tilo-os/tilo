//! Where a package's bytes go, against the `[budget]` in `tilo-app.toml` and
//! the application size limit.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

/// Size ceilings an app sets for itself in `tilo-app.toml`:
///
/// ```toml
/// [budget]
/// package = "1 MiB"   # the signed .tapp
/// binary = "900 KiB"  # each entrypoint binary
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Budget {
    pub package: Option<u64>,
    pub binary: Option<u64>,
}

impl Budget {
    /// Reads `[budget]` from a `tilo-app.toml` source. Unknown keys are refused
    /// so a misspelt ceiling does not silently stop applying.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed TOML, an unknown key or an unreadable size.
    pub fn from_manifest(source: &str) -> Result<Self> {
        let table: toml::Table = source.parse().context("parsing tilo-app.toml")?;
        let Some(budget) = table.get("budget") else {
            return Ok(Self::default());
        };
        let budget = budget
            .as_table()
            .ok_or_else(|| anyhow!("[budget] must be a table"))?;
        let mut parsed = Self::default();
        for (key, value) in budget {
            let bytes = match value {
                toml::Value::Integer(bytes) => u64::try_from(*bytes)
                    .map_err(|_| anyhow!("budget.{key} must not be negative"))?,
                toml::Value::String(text) => {
                    parse_size(text).with_context(|| format!("budget.{key}"))?
                }
                _ => bail!("budget.{key} must be a byte count or a size such as \"1.5 MiB\""),
            };
            match key.as_str() {
                "package" => parsed.package = Some(bytes),
                "binary" => parsed.binary = Some(bytes),
                other => bail!("unknown budget key {other:?}; use package or binary"),
            }
        }
        Ok(parsed)
    }
}

/// Parses `"900 KiB"`, `"1.5 MB"` or `"4096"`. `KB`/`MB` are powers of ten
/// and `KiB`/`MiB` powers of two, as everywhere else.
///
/// # Errors
///
/// Returns an error for a missing number or an unknown unit.
pub fn parse_size(text: &str) -> Result<u64> {
    let text = text.trim();
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: f64 = number
        .parse()
        .map_err(|_| anyhow!("{text:?} does not start with a number"))?;
    let scale: f64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "kb" => 1e3,
        "mb" => 1e6,
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        other => bail!("unknown size unit {other:?}; use B, KB, KiB, MB or MiB"),
    };
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a budget is a small positive size; rounding to whole bytes is the intent"
    )]
    let bytes = (number * scale).round() as u64;
    Ok(bytes)
}

/// How an ELF binary's file bytes divide between what it runs and what the
/// loader needs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sections {
    /// Machine code (`.text` and friends).
    pub code: u64,
    /// Constants, strings and unwind tables.
    pub read_only: u64,
    /// Symbol tables, relocations and the GOT: the cost of dynamic linking.
    pub linking: u64,
    /// Everything else with bytes in the file.
    pub other: u64,
}

/// Section sizes of a little-endian ELF64 file, or `None` for anything else.
#[must_use]
pub fn elf_sections(bytes: &[u8]) -> Option<Sections> {
    const NOBITS: u32 = 8;
    fn field<const N: usize>(bytes: &[u8], base: usize, offset: usize) -> Option<[u8; N]> {
        let start = base.checked_add(offset)?;
        bytes.get(start..start.checked_add(N)?)?.try_into().ok()
    }
    let u16_at = |base, offset| field(bytes, base, offset).map(u16::from_le_bytes);
    let u32_at = |base, offset| field(bytes, base, offset).map(u32::from_le_bytes);
    let u64_at = |base, offset| field(bytes, base, offset).map(u64::from_le_bytes);
    if bytes.get(..6)? != b"\x7fELF\x02\x01" {
        return None;
    }
    let table = usize::try_from(u64_at(0, 0x28)?).ok()?;
    let entry = usize::from(u16_at(0, 0x3A)?);
    let count = usize::from(u16_at(0, 0x3C)?);
    let names_index = usize::from(u16_at(0, 0x3E)?);
    let header = |index: usize| table.checked_add(index.checked_mul(entry)?);
    let names = usize::try_from(u64_at(header(names_index)?, 0x18)?).ok()?;
    let mut sections = Sections::default();
    for index in 1..count {
        let at = header(index)?;
        if u32_at(at, 4)? == NOBITS {
            continue;
        }
        let size = u64_at(at, 0x20)?;
        let name_at = names.checked_add(usize::try_from(u32_at(at, 0)?).ok()?)?;
        let name = bytes.get(name_at..)?;
        let name = name.get(..name.iter().position(|byte| *byte == 0)?)?;
        let bucket = if name.starts_with(b".text")
            || name == b".init"
            || name == b".fini"
            || name.starts_with(b".plt")
        {
            &mut sections.code
        } else if name.starts_with(b".rodata")
            || name.starts_with(b".eh_frame")
            || name == b".gcc_except_table"
        {
            &mut sections.read_only
        } else if name.starts_with(b".dyn")
            || name.starts_with(b".rela")
            || name.starts_with(b".got")
            || name.starts_with(b".gnu.hash")
            || name.starts_with(b".gnu.version")
            || name == b".symtab"
            || name == b".strtab"
            || name == b".hash"
            || name == b".relr.dyn"
        {
            &mut sections.linking
        } else {
            &mut sections.other
        };
        *bucket = bucket.saturating_add(size);
    }
    Some(sections)
}

/// One file in the package payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayloadFile {
    pub path: PathBuf,
    pub bytes: u64,
    pub entrypoint: bool,
    pub sections: Option<Sections>,
}

/// Where a package's bytes go, and whether they fit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SizeReport {
    /// Largest first.
    pub files: Vec<PayloadFile>,
    pub archive: u64,
    pub limit: u64,
    pub budget: Budget,
}

impl SizeReport {
    /// Measures every regular file under `payload`, for a package the registry
    /// accepts up to `limit` bytes of ([`tilo_app_package::platform_size_limit`]).
    ///
    /// # Errors
    ///
    /// Returns an error when the payload cannot be read.
    pub fn measure(
        payload: &Path,
        entrypoints: &[PathBuf],
        archive: u64,
        budget: Budget,
        limit: u64,
    ) -> Result<Self> {
        let mut files = Vec::new();
        let mut pending = vec![PathBuf::new()];
        while let Some(relative) = pending.pop() {
            let dir = payload.join(&relative);
            for entry in std::fs::read_dir(&dir)
                .with_context(|| format!("reading payload {}", dir.display()))?
            {
                let entry = entry?;
                let path = relative.join(entry.file_name());
                let kind = entry.file_type()?;
                if kind.is_dir() {
                    pending.push(path);
                } else if kind.is_file() {
                    let entrypoint = entrypoints.contains(&path);
                    let sections = if entrypoint {
                        elf_sections(&std::fs::read(entry.path())?)
                    } else {
                        None
                    };
                    files.push(PayloadFile {
                        bytes: entry.metadata()?.len(),
                        path,
                        entrypoint,
                        sections,
                    });
                }
            }
        }
        files.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
        Ok(Self {
            files,
            archive,
            limit,
            budget,
        })
    }

    /// Every ceiling the package exceeds, as sentences.
    #[must_use]
    pub fn overruns(&self) -> Vec<String> {
        let mut overruns = Vec::new();
        if let Some(budget) = self.budget.package
            && self.archive > budget
        {
            overruns.push(format!(
                "the package is {} over its budget of {}",
                human(self.archive.saturating_sub(budget)),
                human(budget)
            ));
        }
        if let Some(budget) = self.budget.binary {
            for file in self
                .files
                .iter()
                .filter(|file| file.entrypoint && file.bytes > budget)
            {
                overruns.push(format!(
                    "{} is {} over the binary budget of {}",
                    file.path.display(),
                    human(file.bytes.saturating_sub(budget)),
                    human(budget)
                ));
            }
        }
        overruns
    }

    /// A few lines for the terminal.
    #[must_use]
    pub fn render(&self) -> String {
        let mut text = String::from("[tilo-app] size");
        let width = self
            .files
            .iter()
            .map(|file| file.path.display().to_string().len())
            .max()
            .unwrap_or(0)
            .max("package".len());
        for file in &self.files {
            let _ = write!(
                text,
                "\n  {:<width$}  {:>10}",
                file.path.display().to_string(),
                human(file.bytes)
            );
            if let Some(sections) = file.sections {
                let _ = write!(
                    text,
                    "  code {} · read-only {} · linking {} · other {}",
                    human(sections.code),
                    human(sections.read_only),
                    human(sections.linking),
                    human(sections.other)
                );
            }
        }
        let mut shares = Vec::new();
        if let Some(budget) = self.budget.package {
            shares.push(format!(
                "{}% of the {} budget",
                percent(self.archive, budget),
                human(budget)
            ));
        }
        shares.push(format!(
            "{}% of the {} limit",
            percent(self.archive, self.limit),
            human(self.limit)
        ));
        let _ = write!(
            text,
            "\n  {:<width$}  {:>10}  {}",
            "package",
            human(self.archive),
            shares.join(", ")
        );
        text
    }

    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "package_bytes": self.archive,
            "limit_bytes": self.limit,
            "budget": {"package_bytes": self.budget.package, "binary_bytes": self.budget.binary},
            "over_budget": self.overruns(),
            "files": self.files.iter().map(|file| json!({
                "path": file.path,
                "bytes": file.bytes,
                "entrypoint": file.entrypoint,
                "sections": file.sections.map(|s| json!({
                    "code": s.code,
                    "read_only": s.read_only,
                    "linking": s.linking,
                    "other": s.other,
                })),
            })).collect::<Vec<_>>(),
        })
    }
}

fn percent(part: u64, whole: u64) -> u64 {
    part.saturating_mul(100).checked_div(whole).unwrap_or(0)
}

/// `741.9 KiB`, `1.2 MiB`, `512 B`.
#[must_use]
pub fn human(bytes: u64) -> String {
    #[allow(
        clippy::cast_precision_loss,
        reason = "a display rounding to one decimal place"
    )]
    let value = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", value / 1024.0)
    } else {
        format!("{:.1} MiB", value / (1024.0 * 1024.0))
    }
}
