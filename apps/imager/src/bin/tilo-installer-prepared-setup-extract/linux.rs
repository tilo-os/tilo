//! Descriptor-relative custody of opaque prepared installer payloads.
//!
//! Authentication and completed-generation authority stay with the native
//! issuer. The whole implementation is one platform boundary: the binary root
//! stays portable and runs nothing else here.

#[path = "fs.rs"]
mod fs;
#[path = "media.rs"]
mod media;
#[path = "payload.rs"]
mod payload;
#[path = "setup.rs"]
mod setup;
#[cfg(test)]
#[path = "tests.rs"]
mod tests;
#[path = "validate.rs"]
mod validate;

pub(super) type Result<T> = std::io::Result<T>;
pub(super) fn invalid() -> std::io::Error {
    std::io::Error::other(
        "prepared setup custody failed; verify the selected medium and installer grants",
    )
}
mod cli {
    use super::{Result, fs, invalid, media, payload, setup};
    use serde_json::Value;
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    const OPERATIONS: [&str; 13] = [
        "retain-authenticated-binding",
        "retain",
        "deliver-user",
        "preflight",
        "preflight-profile",
        "preflight-home",
        "retain-home",
        "deliver-home-user",
        "home-required-bytes",
        "check-home-capacity",
        "preflight-authenticated",
        "preflight-tpm-handoff",
        "selected-generation",
    ];
    struct Args {
        operation: Option<String>,
        values: BTreeMap<String, Vec<String>>,
        encrypted: bool,
    }
    impl Args {
        fn parse() -> Result<Self> {
            let mut arguments = std::env::args().skip(1).peekable();
            let operation = if arguments
                .peek()
                .is_some_and(|v| OPERATIONS.contains(&v.as_str()))
            {
                arguments.next()
            } else {
                None
            };
            let mut args = Self {
                operation,
                values: BTreeMap::new(),
                encrypted: false,
            };
            while let Some(key) = arguments.next() {
                if key == "--encrypted" {
                    if args.encrypted {
                        return Err(invalid());
                    }
                    args.encrypted = true;
                    continue;
                }
                if ![
                    "--source-dir",
                    "--target-var",
                    "--username",
                    "--receipt",
                    "--root-bytes",
                    "--disk-bytes",
                    "--device",
                    "--source-root",
                    "--out-dir",
                    "--sidecar",
                    "--package",
                ]
                .contains(&key.as_str())
                {
                    return Err(invalid());
                }
                let value = arguments.next().ok_or_else(invalid)?;
                let values = args.values.entry(key.clone()).or_default();
                if !values.is_empty() && key != "--device" && key != "--source-root" {
                    return Err(invalid());
                }
                values.push(value);
            }
            Ok(args)
        }
        fn value(&self, key: &str) -> Result<&str> {
            self.values
                .get(key)
                .and_then(|v| v.first())
                .map(String::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(invalid)
        }
        fn list(&self, key: &str) -> Vec<String> {
            self.values.get(key).cloned().unwrap_or_default()
        }
        fn only(&self, allowed: &[&str], required: &[&str], geometry: bool) -> Result<()> {
            if self.values.keys().any(|k| !allowed.contains(&k.as_str()))
                || (!geometry && self.encrypted)
            {
                return Err(invalid());
            }
            for key in required {
                self.value(key)?;
            }
            Ok(())
        }
    }
    fn stdin() -> Result<(Vec<u8>, Value)> {
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(80 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 80 * 1024 {
            return Err(invalid());
        }
        let value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        Ok((bytes, value))
    }
    fn total_required(source: &str) -> Result<u128> {
        let mut total = 0u128;
        for kind in payload::KINDS {
            if let Some(v) = payload::staged(source, kind)? {
                total = total
                    .checked_add(payload::required(
                        v.get("expectation").ok_or_else(invalid)?,
                        kind,
                    )?)
                    .ok_or_else(invalid)?;
            }
        }
        Ok(total)
    }
    fn configured(value: &str) -> Result<u64> {
        let (digits, multiplier) = match value.chars().last() {
            Some('K') => (value.strip_suffix('K').ok_or_else(invalid)?, 1024u64),
            Some('M') => (value.strip_suffix('M').ok_or_else(invalid)?, 1024u64.pow(2)),
            Some('G') => (value.strip_suffix('G').ok_or_else(invalid)?, 1024u64.pow(3)),
            Some('T') => (value.strip_suffix('T').ok_or_else(invalid)?, 1024u64.pow(4)),
            _ => (value, 1),
        };
        let n = number(digits)?
            .checked_mul(multiplier)
            .ok_or_else(invalid)?;
        if n == 0 || n >= 1 << 63 {
            return Err(invalid());
        }
        Ok(n)
    }
    fn number(s: &str) -> Result<u64> {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid());
        }
        s.parse().map_err(|_| invalid())
    }
    fn capacity(args: &Args) -> Result<u128> {
        const UNENCRYPTED_RESERVED: u64 = 4 * 1024u64.pow(3) + 2 * 1024 * 1024;
        const MINIMUM_ROOT: u128 = 32 * 1024u128.pow(3);
        let required = total_required(args.value("--source-dir")?)?;
        if required == 0 {
            return Ok(0);
        }
        let root = args.values.contains_key("--root-bytes");
        let disk = args.values.contains_key("--disk-bytes");
        if root == disk {
            return Err(invalid());
        }
        let raw = number(args.value(if root { "--root-bytes" } else { "--disk-bytes" })?)?;
        if raw == 0 || raw >= 1 << 63 {
            return Err(invalid());
        }
        let size = if disk {
            if args.encrypted {
                let reserved =
                    configured(&std::env::var("TILO_ESP_SIZE").unwrap_or_else(|_| "2G".into()))?
                        .checked_add(configured(
                            &std::env::var("TILO_BOOT_SIZE").unwrap_or_else(|_| "1G".into()),
                        )?)
                        .and_then(|n| n.checked_add(2 * 1024 * 1024))
                        .ok_or_else(invalid)?;
                let helper = std::env::var("TILO_STORE_VOLUME")
                    .unwrap_or_else(|_| "/usr/libexec/tilo-store-volume".into());
                let output = media::command(
                    &helper,
                    &[
                        "plan",
                        "--disk-bytes",
                        &raw.to_string(),
                        "--reserved-bytes",
                        &reserved.to_string(),
                    ],
                    65536,
                    30,
                )?;
                let text = std::str::from_utf8(&output).map_err(|_| invalid())?;
                let roots: Vec<_> = text
                    .lines()
                    .filter_map(|v| v.strip_prefix("ROOT_BYTES="))
                    .collect();
                if roots.len() != 1 {
                    return Err(invalid());
                }
                let size = number(roots.first().ok_or_else(invalid)?)?;
                if size == 0 || size > raw.checked_sub(reserved).ok_or_else(invalid)? {
                    return Err(invalid());
                }
                size
            } else {
                raw.saturating_sub(UNENCRYPTED_RESERVED)
            }
        } else {
            raw
        };
        let usable = u128::from(size.saturating_sub(32 * 1024 * 1024)) * 94 / 100;
        if usable < required.checked_add(MINIMUM_ROOT).ok_or_else(invalid)? {
            return Err(invalid());
        }
        Ok(required)
    }
    fn extract(args: &Args) -> Result<()> {
        args.only(
            &["--device", "--source-root", "--out-dir"],
            &["--out-dir"],
            false,
        )?;
        let devices = args.list("--device");
        let explicit = args.list("--source-root");
        if !devices.is_empty() && !explicit.is_empty() {
            return Err(invalid());
        }
        let mut grants = if devices.is_empty() {
            None
        } else {
            Some(media::Grants::acquire(&devices)?)
        };
        let roots = grants.as_ref().map_or(explicit, |g| g.roots.clone());
        let result = (|| {
            if let Some(receipt) = setup::extract(&roots, args.value("--out-dir")?)? {
                for kind in payload::KINDS {
                    payload::stage(&roots, args.value("--out-dir")?, &receipt, &devices, kind)?;
                }
            }
            Ok(())
        })();
        let cleanup = grants.as_mut().map_or(Ok(()), media::Grants::release);
        result.and(cleanup)
    }
    fn deliver(args: &Args, op: &str) -> Result<()> {
        let account = setup::account(args.value("--username")?)?;
        if op == "deliver-user" {
            let digest = setup::carry(
                "/var/lib/tilo/prepared-setup",
                &account.home,
                &[".local", "state", "tilo", "prepared-setup"],
                account.uid,
                account.gid,
            )?;
            setup::deliver_binding(&account, &digest)?;
        } else {
            for kind in payload::KINDS {
                let (_, _, name) = payload::names(kind)?;
                let source = format!("/var/lib/tilo/{name}");
                match std::fs::symlink_metadata(&source) {
                    Ok(_) => {
                        payload::deliver_to(
                            &source,
                            &account.home,
                            account.uid,
                            account.gid,
                            kind,
                        )?;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(())
    }
    fn preflight_tpm_handoff(args: &Args) -> Result<()> {
        args.only(
            &["--sidecar", "--package"],
            &["--sidecar", "--package"],
            false,
        )?;
        tilo_handoff::prepared::preflight_handoff(
            std::path::Path::new(args.value("--sidecar")?),
            std::path::Path::new(args.value("--package")?),
        )
        .map_err(|_| invalid())
    }

    fn preflight(args: &Args) -> Result<()> {
        args.only(
            &["--source-dir", "--receipt"],
            &["--source-dir", "--receipt"],
            false,
        )?;
        let text = args.value("--receipt")?;
        if text.len() > 1024 {
            return Err(invalid());
        }
        let value = serde_json::from_str(text).map_err(|_| invalid())?;
        setup::preflight(args.value("--source-dir")?, &value)?;
        Ok(())
    }
    pub fn run() -> Result<()> {
        let args = Args::parse()?;
        let Some(op) = args.operation.as_deref() else {
            return extract(&args);
        };
        match op {
            "selected-generation" => {
                args.only(&[], &[], false)?;
                println!("{}", payload::selected()?);
            }
            "preflight" => {
                preflight(&args)?;
            }
            "preflight-profile" => {
                args.only(&["--source-dir"], &["--source-dir"], false)?;
                setup::profile(args.value("--source-dir")?)?;
            }
            "preflight-tpm-handoff" => preflight_tpm_handoff(&args)?,
            "preflight-authenticated" | "retain-authenticated-binding" => {
                let retain = op == "retain-authenticated-binding";
                let keys = if retain {
                    vec!["--source-dir", "--target-var"]
                } else {
                    vec!["--source-dir"]
                };
                args.only(&keys, &keys, false)?;
                let (bytes, value) = stdin()?;
                if retain {
                    setup::retain_binding(
                        args.value("--source-dir")?,
                        args.value("--target-var")?,
                        &value,
                    )?;
                } else {
                    setup::authenticated(args.value("--source-dir")?, &value)?;
                    std::io::stdout().write_all(&bytes)?;
                }
            }
            "retain" => {
                args.only(
                    &["--source-dir", "--target-var", "--receipt"],
                    &["--source-dir", "--target-var"],
                    false,
                )?;
                setup::carry(
                    args.value("--source-dir")?,
                    args.value("--target-var")?,
                    &["lib", "tilo", "prepared-setup"],
                    fs::uid(),
                    fs::gid(),
                )?;
            }
            "deliver-user" | "deliver-home-user" => {
                args.only(&["--username", "--receipt"], &["--username"], false)?;
                if op == "deliver-home-user" && args.values.contains_key("--receipt") {
                    return Err(invalid());
                }
                deliver(&args, op)?;
            }
            "preflight-home" | "retain-home" => {
                let retain = op == "retain-home";
                let keys = if retain {
                    vec!["--source-dir", "--target-var"]
                } else {
                    vec!["--source-dir"]
                };
                args.only(&keys, &keys, false)?;
                for kind in payload::KINDS {
                    if retain {
                        payload::retain(
                            args.value("--source-dir")?,
                            args.value("--target-var")?,
                            kind,
                        )?;
                    } else if let Some(value) = payload::staged(args.value("--source-dir")?, kind)?
                    {
                        payload::input(args.value("--source-dir")?, &value, kind, None)?;
                    }
                }
            }
            "home-required-bytes" => {
                args.only(&["--source-dir"], &["--source-dir"], false)?;
                println!("{}", total_required(args.value("--source-dir")?)?);
            }
            "check-home-capacity" => {
                args.only(
                    &["--source-dir", "--root-bytes", "--disk-bytes"],
                    &["--source-dir"],
                    true,
                )?;
                println!("{}", capacity(&args)?);
            }
            _ => return Err(invalid()),
        }
        Ok(())
    }
}

pub(super) fn run() -> Result<()> {
    cli::run()
}
