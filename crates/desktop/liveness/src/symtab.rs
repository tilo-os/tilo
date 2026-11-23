//! Names for return addresses, from the executable's own symbol table.
//!
//! A dump has to be written while it matters: by a watchdog about to end a
//! hung compositor, or for a test waiting on a stall. Resolving addresses
//! through the debug information (`backtrace::resolve`) first parses and
//! decompresses it, which in a large debug build takes longer than either
//! can wait. The ELF symbol table names every function without that: its
//! entries are read once per dump, only the ones the captured addresses fall
//! in are kept, and nothing is cached between dumps. Frames outside the
//! executable (the C library, the loader) are named by their object and
//! offset, which `addr2line` resolves afterwards from the report alone.

use std::collections::HashMap;
use std::ffi::{CStr, c_int, c_void};
use std::fmt::Write as _;

use object::{Object as _, ObjectSymbol as _, SymbolKind};

/// One object loaded into the process: where its segments are mapped and the
/// amount its addresses are shifted by from those in its file.
struct Module {
    path: String,
    bias: usize,
    segments: Vec<(usize, usize)>,
    /// Its GNU build-id, in hex: with an `object+offset` frame it finds the
    /// separate debug file that names the frame, for a stripped object.
    build_id: Option<String>,
}

impl Module {
    fn contains(&self, address: usize) -> bool {
        self.segments
            .iter()
            .any(|&(start, end)| (start..end).contains(&address))
    }
}

/// The process's loaded objects, the executable first.
fn modules() -> Vec<Module> {
    unsafe extern "C" fn each(
        info: *mut libc::dl_phdr_info,
        _size: usize,
        data: *mut c_void,
    ) -> c_int {
        // SAFETY: `dl_iterate_phdr` passes a valid entry and the pointer to
        // the vector this module hands it, for the duration of the call.
        let (info, out) = unsafe { (&*info, &mut *data.cast::<Vec<Module>>()) };
        let path = if info.dlpi_name.is_null() {
            String::new()
        } else {
            // SAFETY: a non-null name is a NUL-terminated string the loader owns.
            unsafe { CStr::from_ptr(info.dlpi_name) }
                .to_string_lossy()
                .into_owned()
        };
        let bias = usize::try_from(info.dlpi_addr).unwrap_or(0);
        let headers: &[libc::Elf64_Phdr] = if info.dlpi_phdr.is_null() {
            &[]
        } else {
            // SAFETY: `dlpi_phnum` program headers start at `dlpi_phdr`.
            unsafe { std::slice::from_raw_parts(info.dlpi_phdr, usize::from(info.dlpi_phnum)) }
        };
        let segments = headers
            .iter()
            .filter(|header| header.p_type == libc::PT_LOAD)
            .filter_map(|header| {
                let start = bias.checked_add(usize::try_from(header.p_vaddr).ok()?)?;
                Some((
                    start,
                    start.checked_add(usize::try_from(header.p_memsz).ok()?)?,
                ))
            })
            .collect();
        let build_id = headers
            .iter()
            .filter(|header| header.p_type == libc::PT_NOTE)
            .find_map(|header| {
                let start = bias.checked_add(usize::try_from(header.p_vaddr).ok()?)?;
                let len = usize::try_from(header.p_memsz).ok()?;
                // SAFETY: a loaded note segment is mapped for `p_memsz` bytes
                // at its address for as long as its object stays loaded,
                // which the loader's lock held during this call guarantees.
                let notes = unsafe {
                    std::slice::from_raw_parts(std::ptr::with_exposed_provenance::<u8>(start), len)
                };
                gnu_build_id(notes)
            });
        out.push(Module {
            path,
            bias,
            segments,
            build_id,
        });
        0
    }
    let mut out: Vec<Module> = Vec::new();
    // SAFETY: the callback only reads the entries it is handed and writes
    // into `out`, which outlives the call.
    unsafe { libc::dl_iterate_phdr(Some(each), (&raw mut out).cast::<c_void>()) };
    out
}

/// The executable's function symbols containing any of `addresses` (file
/// addresses), by address.
fn executable_names(addresses: &[u64]) -> HashMap<u64, String> {
    let mut wanted: Vec<u64> = addresses.to_vec();
    wanted.sort_unstable();
    wanted.dedup();
    let mut names = HashMap::new();
    let Ok(file) = std::fs::File::open("/proc/self/exe") else {
        return names;
    };
    let cache = object::ReadCache::new(file);
    let Ok(elf) = object::File::parse(&cache) else {
        return names;
    };
    for symbol in elf.symbols() {
        if symbol.kind() != SymbolKind::Text || symbol.size() == 0 {
            continue;
        }
        let start = symbol.address();
        let end = start.saturating_add(symbol.size());
        let first = wanted.partition_point(|&address| address < start);
        let inside = wanted
            .get(first..)
            .unwrap_or_default()
            .iter()
            .take_while(|&&address| address < end);
        let mut name = None;
        for &address in inside {
            let name = name.get_or_insert_with(|| {
                symbol.name_bytes().map_or_else(
                    |_| "<unnamed>".to_string(),
                    |bytes| {
                        let raw = String::from_utf8_lossy(bytes);
                        format!("{:#}", rustc_demangle::demangle(&raw))
                    },
                )
            });
            names.insert(address, name.clone());
        }
    }
    names
}

/// `  N: name` lines for `stacks` (each innermost frame first), as
/// `std::backtrace` numbers them: names from the executable's symbol table,
/// `object+offset` for frames in other objects.
pub(crate) fn render_all(stacks: &[&[usize]]) -> Vec<String> {
    let modules = modules();
    let executable = modules.first();
    // A return address is the instruction after the call: look the call up.
    let call = |ip: usize| ip.saturating_sub(1);
    let in_executable: Vec<u64> = stacks
        .iter()
        .flat_map(|stack| stack.iter())
        .filter(|&&ip| executable.is_some_and(|module| module.contains(call(ip))))
        .filter_map(|&ip| {
            u64::try_from(call(ip).saturating_sub(executable.map_or(0, |module| module.bias))).ok()
        })
        .collect();
    let names = executable_names(&in_executable);
    let executable_path = std::fs::read_link("/proc/self/exe").map_or_else(
        |_| "<executable>".to_string(),
        |path| path.display().to_string(),
    );
    stacks
        .iter()
        .map(|stack| {
            let mut out = String::new();
            for (index, &ip) in stack.iter().enumerate() {
                let found = modules.iter().position(|module| module.contains(call(ip)));
                let module = found.and_then(|at| modules.get(at));
                let symbol = (found == Some(0))
                    .then(|| module.map_or(0, |module| module.bias))
                    .and_then(|bias| u64::try_from(call(ip).saturating_sub(bias)).ok())
                    .and_then(|address| names.get(&address));
                match (symbol, module) {
                    (Some(name), _) => {
                        let _ = writeln!(out, "{index:>4}: {name}");
                    }
                    (None, Some(module)) => {
                        // The loader names the executable "": name its file,
                        // which `addr2line -e` takes.
                        let path = if module.path.is_empty() {
                            executable_path.as_str()
                        } else {
                            module.path.as_str()
                        };
                        let offset = ip.saturating_sub(module.bias);
                        let _ = match &module.build_id {
                            Some(id) => writeln!(
                                out,
                                "{index:>4}: <unknown> {path}+{offset:#x} (build-id {id})"
                            ),
                            None => writeln!(out, "{index:>4}: <unknown> {path}+{offset:#x}"),
                        };
                    }
                    (None, None) => {
                        let _ = writeln!(out, "{index:>4}: <unknown> {ip:#x}");
                    }
                }
            }
            out
        })
        .collect()
}

/// The GNU build-id in a note segment's bytes (`NT_GNU_BUILD_ID`, owner
/// `GNU`), in hex.
fn gnu_build_id(notes: &[u8]) -> Option<String> {
    const NT_GNU_BUILD_ID: u32 = 3;
    let word = |at: usize| -> Option<u32> {
        Some(u32::from_ne_bytes(
            notes.get(at..at.checked_add(4)?)?.try_into().ok()?,
        ))
    };
    let aligned = |len: usize| len.checked_add(3).map(|len| len & !3);
    let mut at = 0usize;
    while at.checked_add(12)? <= notes.len() {
        let name_len = usize::try_from(word(at)?).ok()?;
        let desc_len = usize::try_from(word(at.checked_add(4)?)?).ok()?;
        let kind = word(at.checked_add(8)?)?;
        let name_at = at.checked_add(12)?;
        let desc_at = name_at.checked_add(aligned(name_len)?)?;
        let next = desc_at.checked_add(aligned(desc_len)?)?;
        if kind == NT_GNU_BUILD_ID
            && notes.get(name_at..name_at.checked_add(name_len)?)? == b"GNU\0"
        {
            let id = notes.get(desc_at..desc_at.checked_add(desc_len)?)?;
            return Some(id.iter().fold(String::new(), |mut hex, byte| {
                let _ = write!(hex, "{byte:02x}");
                hex
            }));
        }
        at = next;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One ELF note: its header, then the name and description, each padded
    /// to four bytes.
    fn note(kind: u32, name: &[u8], desc: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for word in [name.len(), desc.len()] {
            out.extend_from_slice(&u32::try_from(word).unwrap().to_ne_bytes());
        }
        out.extend_from_slice(&kind.to_ne_bytes());
        for part in [name, desc] {
            out.extend_from_slice(part);
            out.resize(out.len().next_multiple_of(4), 0);
        }
        out
    }

    #[test]
    fn build_id_is_read_past_other_notes() {
        let mut notes = note(1, b"GNU\0", &[0; 16]);
        notes.extend(note(3, b"Go\0", &[0xaa; 3]));
        notes.extend(note(3, b"GNU\0", &[0x01, 0xab, 0xff]));
        assert_eq!(gnu_build_id(&notes).as_deref(), Some("01abff"));
    }

    #[test]
    fn truncated_notes_have_no_build_id() {
        let notes = note(3, b"GNU\0", &[0x01, 0xab, 0xff, 0x00]);
        assert_eq!(gnu_build_id(notes.get(..notes.len() - 1).unwrap()), None);
        assert_eq!(gnu_build_id(&[]), None);
    }

    #[test]
    fn frames_in_the_executable_are_named() {
        let ip = (build_id_is_read_past_other_notes as fn() as usize).saturating_add(1);
        let rendered = render_all(&[&[ip]]).pop().unwrap();
        assert!(
            rendered.contains("build_id_is_read_past_other_notes"),
            "{rendered}"
        );
    }
}
