// Fixture for `tests/workspace_confinement.rs`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut out = std::io::stdout();
    match args.first().map(String::as_str) {
        Some("write") => {
            let root = std::env::var("TILO_APP_DIR").unwrap_or_else(|_| "/nowhere".into());
            let path = std::path::Path::new(&root).join(&args[1]);
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match std::fs::write(&path, args[2].as_bytes()) {
                Ok(()) => writeln!(out, "WROTE {}", path.display()).unwrap(),
                Err(e) => writeln!(out, "ERR {} {}", path.display(), e.kind_str()).unwrap(),
            }
        }
        Some("writeabs") => {
            let path = std::path::Path::new(&args[1]);
            match std::fs::write(path, args[2].as_bytes()) {
                Ok(()) => writeln!(out, "WROTE {}", path.display()).unwrap(),
                Err(e) => writeln!(out, "ERR {} {}", path.display(), e.kind_str()).unwrap(),
            }
        }
        Some("read") => match std::fs::read_to_string(&args[1]) {
            Ok(s) => writeln!(out, "OK {} {}", &args[1], s.trim()).unwrap(),
            Err(e) => writeln!(out, "ERR {} {}", &args[1], e.kind_str()).unwrap(),
        },
        Some("elf") => match std::fs::File::open(&args[1]) {
            Ok(mut file) => {
                use std::io::Read;
                let mut magic = [0; 4];
                match file.read_exact(&mut magic) {
                    Ok(()) => writeln!(out, "ELF {} {:?}", &args[1], magic).unwrap(),
                    Err(e) => writeln!(out, "ERR {} {}", &args[1], e.kind_str()).unwrap(),
                }
            }
            Err(e) => writeln!(out, "ERR {} {}", &args[1], e.kind_str()).unwrap(),
        },
        Some("readdir") => match std::fs::read_dir(&args[1]) {
            Ok(entries) => {
                let mut names: Vec<String> = entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect();
                names.sort();
                writeln!(out, "OK {} {}", &args[1], names.join(",")).unwrap()
            }
            Err(e) => writeln!(out, "ERR {} {}", &args[1], e.kind_str()).unwrap(),
        },
        Some("rename") => match std::fs::rename(&args[1], &args[2]) {
            Ok(()) => writeln!(out, "RENAMED {} {}", &args[1], &args[2]).unwrap(),
            Err(e) => writeln!(out, "ERR {} {}", &args[1], e.kind_str()).unwrap(),
        },
        Some("env") => {
            let v = std::env::var(&args[1]).unwrap_or_else(|_| "<unset>".into());
            writeln!(out, "ENV {}={}", &args[1], v).unwrap()
        }
        // Names only, never values: a value could be a socket path or home directory with no business
        // in a test log.
        Some("envall") => {
            let mut names: Vec<String> = std::env::vars_os()
                .map(|(k, _)| k.to_string_lossy().into_owned())
                .collect();
            names.sort();
            writeln!(out, "ENVALL {}", names.join(",")).unwrap()
        }
        Some("mkdir") => match std::fs::create_dir(&args[1]) {
            Ok(()) => writeln!(out, "MADE {}", &args[1]).unwrap(),
            Err(e) => writeln!(out, "ERR {} {}", &args[1], e.kind_str()).unwrap(),
        },
        Some("mount") => {
            unsafe extern "C" {
                fn mount(
                    source: *const std::ffi::c_char,
                    target: *const std::ffi::c_char,
                    fstype: *const std::ffi::c_char,
                    flags: std::ffi::c_ulong,
                    data: *const std::ffi::c_void,
                ) -> std::ffi::c_int;
            }
            let _ = std::fs::create_dir_all(&args[2]);
            let fstype = std::ffi::CString::new(args[1].as_str()).unwrap();
            let target = std::ffi::CString::new(args[2].as_str()).unwrap();
            // SAFETY: NUL-terminated strings that outlive the call.
            let status = unsafe {
                mount(
                    fstype.as_ptr(),
                    target.as_ptr(),
                    fstype.as_ptr(),
                    0,
                    std::ptr::null(),
                )
            };
            if status == 0 {
                writeln!(out, "MOUNTED {}", &args[2]).unwrap()
            } else {
                let e = std::io::Error::last_os_error();
                writeln!(out, "ERR {} {}", &args[2], e.kind_str()).unwrap()
            }
        }
        other => writeln!(out, "ERR unknown-command {other:?}").unwrap(),
    }
}

/// `io::ErrorKind` has no stable string form, and the raw OS message is localised, so map to the errno
/// name to tell "not in my namespace" from "there but denied".
trait KindStr {
    fn kind_str(&self) -> String;
}

impl KindStr for std::io::Error {
    fn kind_str(&self) -> String {
        match self.raw_os_error() {
            Some(libc_enoent) if libc_enoent == 2 => "ENOENT".into(),
            Some(1) => "EPERM".into(),
            Some(13) => "EACCES".into(),
            Some(16) => "EBUSY".into(),
            Some(30) => "EROFS".into(),
            Some(other) => format!("errno{other}"),
            None => format!("{:?}", self.kind()),
        }
    }
}
