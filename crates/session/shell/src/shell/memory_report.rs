//! What the compositor's own memory is, by kind of mapping.
//!
//! Tilo's kernel is built without the page monitor
//! (`CONFIG_PROC_PAGE_MONITOR=n`, a required hardening option), so there is no
//! `/proc/PID/smaps`, no PSS and no per-mapping RSS for anyone to read from
//! outside. `/proc/self/maps` still lists the mappings, and `mincore(2)` tells
//! a process which pages of its own mappings are in memory. This module
//! combines the two into a breakdown the vm-acceptance R01 check records:
//! heap (glibc's main heap and its per-thread arenas, with glibc's own
//! in-use/free split), thread stacks, anonymous mappings (large `malloc`
//! blocks, JIT code), the GPU driver's device memory (lavapipe's memfd),
//! fonts, the binary, libraries and other files.
//!
//! For a file mapping `mincore` reports the page cache, so a page counts as
//! resident if it is cached even when this process never touched it; for an
//! anonymous mapping it is exactly the process's own pages. It runs only when
//! asked (`tilo-control raw '{"op":"process_memory"}'`) and takes a few
//! milliseconds.

use std::collections::BTreeMap;

use serde_json::{Value, json};

/// glibc places each non-main arena's heap at a multiple of `HEAP_MAX_SIZE`
/// (2 × `DEFAULT_MMAP_THRESHOLD_MAX`, 64 MiB on 64-bit).
const ARENA_ALIGN: u64 = 64 << 20;
/// Largest guard below a thread stack counted as one: glibc's default is one
/// page; Rust's `std::thread` asks for the default too.
const MAX_GUARD: u64 = 64 << 10;
/// Pages asked of `mincore` at once, so the vector stays small.
const CHUNK_PAGES: u64 = 16 * 1024;
/// Mappings listed one by one, largest resident first.
const TOP: usize = 40;
/// Substrings of a library path that name the GPU driver stack.
const GPU_LIBS: [&str; 12] = [
    "libvulkan",
    "libllvm",
    "libgallium",
    "mesa",
    "/dri/",
    "libdrm",
    "libgbm",
    "libegl",
    "libgl.",
    "libglx",
    "libglapi",
    "lvp",
];

/// One line of `/proc/self/maps`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Mapping {
    start: u64,
    end: u64,
    perms: String,
    path: String,
}

impl Mapping {
    const fn len(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    /// Address space only: `---p`, a guard or an arena's reserved tail.
    fn reserved(&self) -> bool {
        self.perms.starts_with("---")
    }
}

/// `/proc/self/maps` text → its mappings, in address order.
fn parse_maps(text: &str) -> Vec<Mapping> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_ascii_whitespace();
            let range = fields.next()?;
            let perms = fields.next()?.to_owned();
            // offset, device, inode
            fields.next()?;
            fields.next()?;
            fields.next()?;
            let path = fields.collect::<Vec<_>>().join(" ");
            let (start, end) = range.split_once('-')?;
            Some(Mapping {
                start: u64::from_str_radix(start, 16).ok()?,
                end: u64::from_str_radix(end, 16).ok()?,
                perms,
                path,
            })
        })
        .collect()
}

/// The kind of memory an anonymous mapping holds; `previous` is the mapping
/// just below it.
fn anon_class(mapping: &Mapping, previous: Option<&Mapping>) -> &'static str {
    let guard_below = previous.is_some_and(|p| {
        p.end == mapping.start && p.path.is_empty() && p.reserved() && p.len() <= MAX_GUARD
    });
    if mapping.reserved() {
        "reserved"
    } else if mapping.perms.as_bytes().get(2) == Some(&b'x') {
        "anon_exec"
    } else if mapping.start.is_multiple_of(ARENA_ALIGN) && !guard_below {
        "heap_arenas"
    } else if guard_below {
        "thread_stacks"
    } else {
        "anon_other"
    }
}

/// The kind of memory a mapping holds. `previous` is the mapping just below
/// it, `exe` the running binary's path.
fn class_of(mapping: &Mapping, previous: Option<&Mapping>, exe: &str) -> &'static str {
    let path = mapping.path.as_str();
    match path {
        "" => return anon_class(mapping, previous),
        "[heap]" => return "heap_main",
        "[stack]" => return "thread_stacks",
        _ if path.starts_with('[') => return "kernel_pages",
        // Mesa lavapipe's VkDeviceMemory: every texture and buffer.
        _ if path.starts_with("/memfd:allocation fd") => return "gpu_memfd",
        _ if ["/memfd:", "/dev/shm/", "/SYSV"]
            .iter()
            .any(|prefix| path.starts_with(prefix)) =>
        {
            return "shmem_other";
        }
        _ if path.starts_with("/dev/dri/") => return "drm",
        _ => {}
    }
    let lower = path.to_ascii_lowercase();
    if [".ttf", ".otf", ".ttc", ".otc", ".woff", ".woff2"]
        .iter()
        .any(|ext| lower.ends_with(ext))
        || lower.contains("/fonts/")
    {
        "fonts"
    } else if !exe.is_empty() && path.trim_end_matches(" (deleted)") == exe {
        "binary"
    } else if lower.contains(".so") {
        if GPU_LIBS.iter().any(|needle| lower.contains(needle)) {
            "gpu_driver_libs"
        } else {
            "libraries"
        }
    } else {
        "files_other"
    }
}

/// Pages of `mapping` in memory, by `mincore`; `None` when the kernel refused
/// (the mapping went away while it was read).
#[cfg(target_os = "linux")]
fn resident_pages(mapping: &Mapping, page: u64, scratch: &mut Vec<u8>) -> Option<u64> {
    let pages = mapping.len().checked_div(page)?;
    let mut resident = 0u64;
    let mut done = 0u64;
    while done < pages {
        let n = pages.saturating_sub(done).min(CHUNK_PAGES);
        scratch.clear();
        scratch.resize(usize::try_from(n).ok()?, 0);
        let addr = usize::try_from(mapping.start.checked_add(done.checked_mul(page)?)?).ok()?;
        let len = usize::try_from(n.checked_mul(page)?).ok()?;
        // SAFETY: `mincore` only writes one byte per page of the range into
        // `scratch`, which holds `n` bytes for the `n` pages asked. The range
        // is a mapping `/proc/self/maps` listed; if it was unmapped since,
        // the call fails with ENOMEM instead of touching it.
        let rc = unsafe {
            libc::mincore(
                std::ptr::without_provenance_mut(addr),
                len,
                scratch.as_mut_ptr(),
            )
        };
        if rc != 0 {
            return None;
        }
        let in_core = scratch.iter().filter(|b| **b & 1 == 1).count();
        resident = resident.saturating_add(u64::try_from(in_core).ok()?);
        done = done.saturating_add(n);
    }
    Some(resident)
}

#[cfg(not(target_os = "linux"))]
const fn resident_pages(_mapping: &Mapping, _page: u64, _scratch: &mut Vec<u8>) -> Option<u64> {
    None
}

#[derive(Debug, Default, Clone, Copy)]
struct ClassTotal {
    mappings: u64,
    virtual_kib: u64,
    resident_kib: u64,
}

/// `/proc/self/status` memory lines, in KiB, and the thread count.
fn status() -> BTreeMap<String, u64> {
    let text = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    text.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            let wanted = key.starts_with("Vm") || key.starts_with("Rss") || key == "Threads";
            let number = value.trim().trim_end_matches("kB").trim().parse().ok()?;
            wanted.then(|| (key.to_owned(), number))
        })
        .collect()
}

/// Threads by name (`/proc/self/task/*/comm`), most numerous first.
fn threads() -> Vec<(String, u64)> {
    let mut by_name: BTreeMap<String, u64> = BTreeMap::new();
    if let Ok(dir) = std::fs::read_dir("/proc/self/task") {
        for task in dir.flatten() {
            let name = std::fs::read_to_string(task.path().join("comm")).unwrap_or_default();
            let count = by_name.entry(name.trim().to_owned()).or_default();
            *count = count.saturating_add(1);
        }
    }
    let mut rows: Vec<_> = by_name.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    rows
}

/// glibc's own account of its heap, in KiB: what it holds in arenas and in
/// separately mmapped blocks, of which in use and free.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn malloc_stats() -> Value {
    // SAFETY: `mallinfo2` only reads allocator statistics; it takes no
    // arguments and returns a plain struct by value.
    let info = unsafe { libc::mallinfo2() };
    json!({
        "arena_kib": info.arena / 1024,
        "mmapped_kib": info.hblkhd / 1024,
        "mmapped_blocks": info.hblks,
        "in_use_kib": info.uordblks / 1024,
        "free_kib": info.fordblks / 1024,
        "top_releasable_kib": info.keepcost / 1024,
    })
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
const fn malloc_stats() -> Value {
    Value::Null
}

/// Classify and measure `maps`, with pages of `page` bytes.
fn breakdown(maps: &[Mapping], exe: &str, page: u64) -> Value {
    let page_kib = page / 1024;
    let mut classes: BTreeMap<&'static str, ClassTotal> = BTreeMap::new();
    let mut rows = Vec::new();
    let mut unreadable = 0u64;
    let mut scratch = Vec::new();
    let mut previous: Option<&Mapping> = None;
    for mapping in maps {
        let class = class_of(mapping, previous, exe);
        previous = Some(mapping);
        let total = classes.entry(class).or_default();
        total.mappings = total.mappings.saturating_add(1);
        total.virtual_kib = total.virtual_kib.saturating_add(mapping.len() / 1024);
        if matches!(class, "reserved" | "kernel_pages") {
            continue;
        }
        let Some(pages) = resident_pages(mapping, page, &mut scratch) else {
            unreadable = unreadable.saturating_add(1);
            continue;
        };
        let kib = pages.saturating_mul(page_kib);
        total.resident_kib = total.resident_kib.saturating_add(kib);
        if kib > 0 {
            rows.push((kib, class, mapping));
        }
    }
    rows.sort_by_key(|row| std::cmp::Reverse(row.0));
    let top: Vec<Value> = rows
        .iter()
        .take(TOP)
        .map(|(kib, class, m)| {
            json!({
                "resident_kib": kib,
                "virtual_kib": m.len() / 1024,
                "class": class,
                "perms": m.perms,
                "start": format!("{:x}", m.start),
                "path": m.path,
            })
        })
        .collect();
    let classes: serde_json::Map<String, Value> = classes
        .iter()
        .map(|(name, t)| {
            (
                (*name).to_owned(),
                json!({"resident_kib": t.resident_kib, "virtual_kib": t.virtual_kib, "mappings": t.mappings}),
            )
        })
        .collect();
    let mut files: BTreeMap<&str, u64> = BTreeMap::new();
    for (kib, class, m) in &rows {
        if matches!(
            *class,
            "fonts" | "files_other" | "libraries" | "gpu_driver_libs" | "binary"
        ) {
            let total = files.entry(m.path.as_str()).or_default();
            *total = total.saturating_add(*kib);
        }
    }
    let mut files: Vec<_> = files.into_iter().collect();
    files.sort_by_key(|file| std::cmp::Reverse(file.1));
    let files: Vec<Value> = files
        .into_iter()
        .take(TOP)
        .map(|(path, kib)| json!({"path": path, "resident_kib": kib}))
        .collect();
    json!({
        "classes": classes,
        "top_mappings": top,
        "top_files": files,
        "unreadable_mappings": unreadable,
    })
}

/// The system's page size.
fn page_size() -> u64 {
    #[cfg(unix)]
    {
        // SAFETY: `sysconf` only reads a constant of the running system.
        let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        u64::try_from(size)
            .ok()
            .filter(|s| *s >= 1024)
            .unwrap_or(4096)
    }
    #[cfg(not(unix))]
    {
        4096
    }
}

/// The whole report: status, glibc's heap, threads by name and the mapping
/// breakdown, every size in KiB.
#[must_use]
pub fn report() -> Value {
    let maps = parse_maps(&std::fs::read_to_string("/proc/self/maps").unwrap_or_default());
    let exe = std::fs::read_link("/proc/self/exe")
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let threads: Vec<Value> = threads()
        .into_iter()
        .map(|(name, count)| json!({"name": name, "count": count}))
        .collect();
    json!({
        "pid": std::process::id(),
        "status_kib": status(),
        "malloc": malloc_stats(),
        "threads": threads,
        "mappings": breakdown(&maps, &exe, page_size()),
        "startup_trimmed": crate::shell::heap_trim::startup_trimmed(),
        "page_monitor": std::path::Path::new("/proc/self/smaps_rollup").exists(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAPS: &str = "\
55d0c0000000-55d0c1000000 r-xp 00000000 fd:00 123 /usr/bin/shell
55d0c2000000-55d0c3000000 rw-p 00000000 00:00 0 [heap]
7f0000000000-7f0000400000 rw-p 00000000 00:00 0
7f0000400000-7f0004000000 ---p 00000000 00:00 0
7f1000000000-7f1000001000 ---p 00000000 00:00 0
7f1000001000-7f1000200000 rw-p 00000000 00:00 0
7f2000000000-7f2000100000 rw-s 00000000 00:01 9 /memfd:allocation fd (deleted)
7f2000100000-7f2000200000 r--p 00000000 fd:00 77 /usr/share/fonts/inter/Inter.ttf
7f2000200000-7f2000300000 r-xp 00000000 fd:00 78 /usr/lib64/libvulkan_lvp.so
7f2000300000-7f2000400000 r-xp 00000000 fd:00 79 /usr/lib64/libglib-2.0.so.0
7f2000400000-7f2000500000 rwxp 00000000 00:00 0
7f2000500000-7f2000600000 rw-p 00000000 00:00 0
7ffd00000000-7ffd00021000 rw-p 00000000 00:00 0                          [stack]
ffffffffff600000-ffffffffff601000 --xp 00000000 00:00 0 [vsyscall]
";

    #[test]
    fn mappings_are_classified_by_what_they_hold() {
        let maps = parse_maps(MAPS);
        assert_eq!(maps.len(), 14);
        let classes: Vec<_> = maps
            .iter()
            .enumerate()
            .map(|(i, m)| class_of(m, i.checked_sub(1).map(|j| &maps[j]), "/usr/bin/shell"))
            .collect();
        assert_eq!(
            classes,
            [
                "binary",
                "heap_main",
                "heap_arenas",
                "reserved",
                "reserved",
                "thread_stacks",
                "gpu_memfd",
                "fonts",
                "gpu_driver_libs",
                "libraries",
                "anon_exec",
                "anon_other",
                "thread_stacks",
                "kernel_pages",
            ]
        );
    }

    #[test]
    fn a_path_with_spaces_is_kept_whole() {
        let maps = parse_maps("7f00-7f01 r--p 00000000 fd:00 1 /home/u/My Fonts/a b.otf\n");
        assert_eq!(maps[0].path, "/home/u/My Fonts/a b.otf");
    }

    /// The production path on this process: the report reads its own maps,
    /// counts resident pages and accounts a freshly written block.
    #[cfg(target_os = "linux")]
    #[test]
    fn own_report_counts_a_written_block() {
        let anon = |r: &Value| -> u64 {
            r["mappings"]["classes"]
                .as_object()
                .unwrap()
                .iter()
                .filter(|(k, _)| k.starts_with("anon") || k.starts_with("heap"))
                .map(|(_, v)| v["resident_kib"].as_u64().unwrap())
                .sum()
        };
        let before = report();
        let block = vec![1u8; 64 << 20];
        let after = report();
        assert!(std::hint::black_box(&block).iter().all(|b| *b == 1));
        assert!(
            anon(&after) >= anon(&before).saturating_add(48 * 1024),
            "a 64 MiB block written in full shows as resident: {} -> {} KiB",
            anon(&before),
            anon(&after)
        );
        assert!(after["status_kib"]["RssAnon"].as_u64().unwrap() > 0);
        assert!(after["threads"].as_array().is_some_and(|t| !t.is_empty()));
    }
}
