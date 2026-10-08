use super::{Error, Key, Symbol};
use winit::{keyboard::PhysicalKey, platform::scancode::PhysicalKeyExtScancode};
use xkbcommon::xkb;

const ROOT: &str = "/usr/share/X11/xkb";

pub(super) fn is_installed(layout: &str) -> Result<bool, Error> {
    let root = std::path::Path::new(ROOT).join("symbols");
    if !std::fs::metadata(&root)
        .map_err(|_| Error::Unavailable)?
        .is_dir()
    {
        return Err(Error::Unavailable);
    }
    match std::fs::metadata(root.join(layout)) {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(Error::Unavailable),
    }
}

pub(super) fn compile(
    layout: &str,
    variant: &str,
    model: &str,
    options: &str,
    names: &[String],
) -> Result<Vec<Key>, Error> {
    let mut context =
        xkb::Context::new(xkb::CONTEXT_NO_DEFAULT_INCLUDES | xkb::CONTEXT_NO_ENVIRONMENT_NAMES);
    if !context.include_path_append(std::path::Path::new(ROOT)) {
        return Err(Error::Unavailable);
    }
    let keymap = xkb::Keymap::new_from_names(
        &context,
        "evdev",
        model,
        layout,
        variant,
        Some(options.to_owned()),
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .ok_or(Error::Unavailable)?;
    names
        .iter()
        .map(|name| compile_key(&keymap, name))
        .collect()
}

fn compile_key(keymap: &xkb::Keymap, name: &str) -> Result<Key, Error> {
    let code = keymap.key_by_name(name).ok_or(Error::InvalidMetadata)?;
    let scancode = code.raw().checked_sub(8).ok_or(Error::InvalidMetadata)?;
    let count = keymap.num_levels_for_key(code, 0);
    if count > 32 {
        return Err(Error::InvalidMetadata);
    }
    let levels = (0..count)
        .map(|level| {
            let syms = keymap.key_get_syms_by_level(code, 0, level);
            if syms.len() > 8 {
                return Err(Error::InvalidMetadata);
            }
            syms.iter()
                .map(|sym| {
                    let text = xkb::keysym_to_utf8(*sym);
                    let name = xkb::keysym_get_name(*sym);
                    if text.len() > 32 || name.len() > 64 {
                        return Err(Error::InvalidMetadata);
                    }
                    Ok(Symbol { text, name })
                })
                .collect()
        })
        .collect::<Result<_, _>>()?;
    Ok(Key {
        name: name.to_owned(),
        physical: PhysicalKey::from_scancode(scancode),
        levels,
    })
}
