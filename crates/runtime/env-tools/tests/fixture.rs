//! A fixture runtime: the host's bash and a few coreutils with their
//! libraries, plus a scripted `dnf` and `rpm` that install a two-package
//! "hello" tool and make every kind of mess a real transaction makes.
//!
//! It exercises the real sandbox (bwrap, user namespace, overlay) and the
//! real seal without a network or a Fedora tree.

use std::collections::BTreeMap;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

/// Host programs the scripts use.
const PROGRAMS: &[&str] = &["bash", "mkdir", "chmod", "ln", "cat", "rm"];

pub const HELLO: &str = "hello-1.0-1.fc44.x86_64";
pub const LIBGREET: &str = "libgreet-2.0-1.fc44.x86_64";

const DNF: &str = r#"#!/usr/bin/bash
# Fixture dnf: the two invocations the tool builder makes.
db=/usr/lib/sysimage/rpm/installed
mode= pkgs=() rest=
for a in "$@"; do
  if [[ -n $rest ]]; then pkgs+=("$a"); continue; fi
  case $a in --assumeno) mode=resolve ;; -y) mode=install ;; --) rest=1 ;; esac
done
if [[ $mode == resolve ]]; then
  rows=() missing=()
  for p in "${pkgs[@]}"; do
    case $p in
      hello) rows+=(" hello     x86_64 0:1.0-1.fc44 fixture 1.0 KiB") ;;
      base)  echo "Package \"base-1-1.fc44.noarch\" is already installed." ;;
      *)     missing+=("$p") ;;
    esac
  done
  if (( ${#missing[@]} )); then
    echo "Failed to resolve the transaction:"
    for m in "${missing[@]}"; do echo "No match for argument: $m"; done
    exit 1
  fi
  if (( ${#rows[@]} )); then
    echo "Package   Arch   Version      Repository Size"
    echo "Installing:"
    printf '%s\n' "${rows[@]}"
    echo "Installing dependencies:"
    echo " libgreet  x86_64 0:2.0-1.fc44 fixture 2.0 KiB"
    echo
    echo "Operation aborted by the user."
    exit 1
  fi
  echo "Nothing to do."
  exit 0
fi
for p in "${pkgs[@]}"; do
  case $p in
    hello-1.0-1.fc44.x86_64)
      printf '#!/usr/bin/bash\necho hello\n' > /usr/bin/hello
      chmod 4755 /usr/bin/hello
      mkdir -p /usr/share/hello
      printf 'greeting\n' > /usr/share/hello/greeting.txt
      printf 'colour=yes\n' > /etc/hello.conf
      printf 'colour=no\n' > /etc/hello.conf.rpmnew
      printf '/usr/bin/hello\n' >> /etc/shells
      mkdir -p /usr/share/empty-after-seal
      ;;
    libgreet-2.0-1.fc44.x86_64)
      printf 'ELF-ish\n' > /usr/lib64/libgreet.so.2.0
      ln -s libgreet.so.2.0 /usr/lib64/libgreet.so.2
      ;;
    *) echo "fixture dnf: no package $p" >&2; exit 1 ;;
  esac
  echo "$p" >> "$db"
done
# Package-manager churn the seal must drop.
mkdir -p /usr/lib/sysimage/libdnf5 /var/log /usr/lib/.build-id/ab
printf 'history\n' > /usr/lib/sysimage/libdnf5/transaction_history.sqlite
printf 'log\n' > /var/log/dnf5.log
printf 'cache\n' > /etc/ld.so.cache
ln -s ../../../bin/hello /usr/lib/.build-id/ab/cdef
# A file the transaction removes from the runtime.
rm /usr/share/base/obsolete
echo Complete!
"#;

const RPM: &str = r"#!/usr/bin/bash
# Fixture rpm: `rpm -qa --qf …` only.
cat /usr/lib/sysimage/rpm/installed
";

/// Build the fixture runtime under `root`. `None` where the host's
/// programs or libraries live outside `/usr` (the fixture mirrors a
/// merged-/usr layout).
pub fn runtime(root: &Path) -> Option<PathBuf> {
    let tree = root.join("runtime");
    for dir in [
        "usr/bin",
        "usr/lib64",
        "usr/lib/sysimage/rpm",
        "usr/share/base",
        "etc",
        "var",
    ] {
        std::fs::create_dir_all(tree.join(dir)).ok()?;
    }
    for program in PROGRAMS {
        let host = which(program)?;
        copy(&host, &tree.join("usr/bin").join(program))?;
        for lib in libraries(&host)? {
            let canonical = std::fs::canonicalize(&lib).ok()?;
            // Both the file and the name the binary asks for (the
            // interpreter is named through `/lib64`), with the top-level
            // links folded into `usr/` as the fixture's own links do.
            for path in [canonical.as_path(), lib.as_path()] {
                let dst = tree.join(usr_relative(path)?);
                std::fs::create_dir_all(dst.parent()?).ok()?;
                copy(&canonical, &dst)?;
            }
        }
    }
    for (name, target) in [
        ("bin", "usr/bin"),
        ("sbin", "usr/bin"),
        ("lib", "usr/lib"),
        ("lib64", "usr/lib64"),
    ] {
        std::os::unix::fs::symlink(target, tree.join(name)).ok()?;
    }
    write_exec(&tree.join("usr/bin/dnf"), DNF)?;
    write_exec(&tree.join("usr/bin/rpm"), RPM)?;
    std::fs::write(
        tree.join("usr/lib/sysimage/rpm/installed"),
        "base-1-1.fc44.noarch\n",
    )
    .ok()?;
    std::fs::write(tree.join("usr/share/base/obsolete"), "old\n").ok()?;
    std::fs::write(tree.join("etc/shells"), "/bin/sh\n/bin/bash\n").ok()?;
    // Fedora ships these 0555; the build must still be able to add to them.
    for dir in ["usr/lib64", "usr/bin"] {
        std::fs::set_permissions(tree.join(dir), std::fs::Permissions::from_mode(0o555)).ok()?;
    }
    Some(tree)
}

/// `/lib64/x` → `usr/lib64/x`, `/usr/lib64/x` → `usr/lib64/x`; `None`
/// for anything else outside `/usr`.
fn usr_relative(path: &Path) -> Option<PathBuf> {
    let rel = path.strip_prefix("/").ok()?;
    if rel.starts_with("usr") {
        return Some(rel.to_owned());
    }
    ["lib64", "lib", "bin", "sbin"].iter().find_map(|top| {
        rel.strip_prefix(top)
            .ok()
            .map(|rest| Path::new("usr").join(top).join(rest))
    })
}

fn which(program: &str) -> Option<PathBuf> {
    ["/usr/bin", "/bin", "/usr/sbin"]
        .iter()
        .map(|d| Path::new(d).join(program))
        .find(|p| p.is_file())
}

fn libraries(program: &Path) -> Option<Vec<PathBuf>> {
    let out = std::process::Command::new("ldd")
        .arg(program)
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    Some(
        text.lines()
            .filter_map(|line| {
                let path = line
                    .split_once("=>")
                    .map_or(line, |(_, rhs)| rhs)
                    .split_whitespace()
                    .next()?;
                path.starts_with('/').then(|| PathBuf::from(path))
            })
            .collect(),
    )
}

fn copy(src: &Path, dst: &Path) -> Option<()> {
    if dst.exists() {
        return Some(());
    }
    std::fs::copy(src, dst).ok()?;
    Some(())
}

fn write_exec(path: &Path, text: &str) -> Option<()> {
    std::fs::write(path, text).ok()?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).ok()
}

/// A sealed tree as `(path → kind, mode, mtime, content-or-target)`, for
/// comparing two builds.
pub fn listing(root: &Path) -> BTreeMap<String, (char, u32, i64, Vec<u8>)> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, (char, u32, i64, Vec<u8>)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let (kind, body) = if meta.file_type().is_symlink() {
                (
                    'l',
                    std::fs::read_link(&path)
                        .unwrap()
                        .into_os_string()
                        .into_encoded_bytes(),
                )
            } else if meta.is_dir() {
                ('d', Vec::new())
            } else {
                ('f', std::fs::read(&path).unwrap())
            };
            out.insert(rel, (kind, meta.mode() & 0o7777, meta.mtime(), body));
            if kind == 'd' {
                walk(root, &path, out);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}
