//! Whether a keyboard is attached, for the sign-in screen's on-screen
//! keyboard under the kiosk compositor.
//!
//! The kiosk's seat says it has a keyboard as soon as any device sends keys,
//! and a power button or a lid switch does. So the screen asks the kernel
//! what each input device can type instead: a keyboard has letter keys. The
//! KMS host in the initramfs applies the same rule to the devices it reads
//! (`ui_kms`).

use std::path::Path;

/// `/sys/class/input`, where each device lists the keys it has.
pub const INPUT_CLASS: &str = "/sys/class/input";
/// `/dev/input`, where a device appears and goes as it is plugged.
pub const DEV_INPUT: &str = "/dev/input";
/// The external event that says the attached devices changed.
pub const KEYBOARDS_WAKE: &str = "tilo-greeter-keyboards";

/// evdev codes from linux/input-event-codes.h.
const KEY_A: usize = 30;
const KEY_Z: usize = 44;

/// Whether a device under `class` has the letter keys.
pub fn present(class: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(class) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        std::fs::read_to_string(entry.path().join("capabilities/key"))
            .is_ok_and(|bitmap| has_key(&bitmap, KEY_A) && has_key(&bitmap, KEY_Z))
    })
}

/// Whether `code` is set in a sysfs key bitmap: words of the kernel's long
/// in hex, most significant first.
fn has_key(bitmap: &str, code: usize) -> bool {
    const BITS: usize = usize::BITS as usize;
    let words: Vec<&str> = bitmap.split_whitespace().collect();
    let Some(index) = words
        .len()
        .checked_sub(code / BITS)
        .and_then(|after| after.checked_sub(1))
    else {
        return false;
    };
    let bit = u32::try_from(code % BITS).unwrap_or_default();
    words
        .get(index)
        .and_then(|word| usize::from_str_radix(word, 16).ok())
        .and_then(|word| word.checked_shr(bit))
        .is_some_and(|word| word & 1 == 1)
}

/// Call `changed` whenever a device appears in or leaves `dev_input`.
pub fn watch(dev_input: &Path, changed: impl Fn() + Send + 'static) {
    use rustix::fs::inotify;
    let watch = inotify::init(inotify::CreateFlags::CLOEXEC).and_then(|fd| {
        inotify::add_watch(
            &fd,
            dev_input,
            inotify::WatchFlags::CREATE | inotify::WatchFlags::DELETE,
        )?;
        Ok(fd)
    });
    let fd = match watch {
        Ok(fd) => fd,
        Err(error) => {
            tracing::warn!(%error, "the sign-in screen cannot follow keyboards being plugged");
            return;
        }
    };
    let spawned = std::thread::Builder::new()
        .name("keyboards".into())
        .spawn(move || {
            let mut buffer = [std::mem::MaybeUninit::<u8>::uninit(); 4096];
            let mut reader = inotify::Reader::new(&fd, &mut buffer);
            while reader.next().is_ok() {
                changed();
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "the sign-in screen cannot follow keyboards being plugged");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letter_keys_make_a_keyboard_and_a_power_button_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let device = |name: &str, bitmap: &str| {
            let path = dir.path().join(name).join("capabilities");
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("key"), bitmap).unwrap();
        };
        // KEY_POWER (116) alone.
        device("input0", "10000000000000 0\n");
        assert!(!present(dir.path()));
        // A keyboard: every key from 1 to 63 at least.
        device("input1", "fffffffffffffffe\n");
        assert!(present(dir.path()));
        assert!(!has_key("", KEY_A));
        assert!(!present(&dir.path().join("missing")));
    }
}
