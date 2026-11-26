fn emit_hex_fixture(text: &str, filename: &str) -> Result<(), Box<dyn std::error::Error>> {
    let digits: String = text
        .chars()
        .filter(|value| !value.is_ascii_whitespace())
        .collect();
    let pairs = digits.as_bytes().chunks_exact(2);
    if !pairs.remainder().is_empty() {
        return Err("invalid fixture hex encoding".into());
    }
    let mut bytes = Vec::new();
    for pair in pairs {
        bytes.push(u8::from_str_radix(std::str::from_utf8(pair)?, 16)?);
    }
    let output =
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").ok_or("missing build output")?);
    std::fs::write(output.join(filename), bytes)?;
    Ok(())
}

/// The icon set, generated from its sources: one SVG per icon in `assets/icons`, named by its file.
/// Every stroked icon shares one root (the 24 grid, a 1.5 px round stroke in the ink), so a source
/// drawn on another grid, weight or corner fails the build rather than drawing out of family; a
/// filled mark says so on its root. Its `desc` documents the variant.
fn emit_icons() -> Result<(), Box<dyn std::error::Error>> {
    use std::fmt::Write as _;
    println!("cargo::rerun-if-changed=assets/icons");
    let mut sources = std::fs::read_dir("assets/icons")?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    // The folder's README and licence sit beside the sources.
    sources.retain(|path| path.extension().is_some_and(|ext| ext == "svg"));
    sources.sort();
    let mut table = String::from("icons! {\n");
    for source in sources {
        let fail = |why: &str| format!("{}: {why}", source.display());
        let name = source
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| {
                stem.split('-').all(|word| {
                    !word.is_empty()
                        && word
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
                })
            })
            .ok_or_else(|| fail("an icon's file name is its kebab-case name"))?;
        let variant: String = name
            .split('-')
            .flat_map(|word| {
                let mut chars = word.chars();
                chars
                    .next()
                    .map(|first| first.to_ascii_uppercase())
                    .into_iter()
                    .chain(chars)
            })
            .collect();
        let icon = parse_icon(&std::fs::read_to_string(&source)?).map_err(|why| fail(&why))?;
        if let Some(desc) = icon.desc {
            writeln!(table, "    #[doc = {desc:?}]")?;
        }
        writeln!(
            table,
            "    {variant} => {name:?}, {}, [{}],",
            icon.filled,
            icon.parts.join(", ")
        )?;
    }
    table.push_str("}\n");
    let output =
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").ok_or("missing build output")?);
    std::fs::write(output.join("icons.rs"), table)?;
    Ok(())
}

/// One icon source as the table states it.
struct IconSource {
    desc: Option<String>,
    filled: bool,
    /// Each shape as the `Part` expression that rebuilds it.
    parts: Vec<String>,
}

fn parse_icon(svg: &str) -> Result<IconSource, String> {
    const STROKED: &str = r#"viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round""#;
    const FILLED: [&str; 2] = [
        r#"viewBox="0 0 24 24" fill="currentColor""#,
        r#"viewBox="0 0 24 24" fill="currentColor" fill-rule="evenodd""#,
    ];
    let (root, mut body) = svg
        .strip_prefix(r#"<svg xmlns="http://www.w3.org/2000/svg" "#)
        .and_then(|rest| rest.split_once('>'))
        .ok_or("does not open with the icon root")?;
    let filled = match root {
        STROKED => false,
        other if FILLED.contains(&other) => true,
        _ => return Err("its root is not the icon grid and stroke".into()),
    };
    let mut desc = None;
    if let Some((text, rest)) = body
        .trim_start()
        .strip_prefix("<desc>")
        .and_then(|text| text.split_once("</desc>"))
    {
        desc = Some(
            text.replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&amp;", "&"),
        );
        body = rest;
    }
    let mut parts = Vec::new();
    for element in body.split('<').map(str::trim).filter(|e| !e.is_empty()) {
        if element == "/svg>" {
            break;
        }
        parts.push(shape(element)?);
    }
    Ok(IconSource {
        desc,
        filled,
        parts,
    })
}

/// One `<shape …/>` element as the `Part` expression that rebuilds it.
fn shape(element: &str) -> Result<String, String> {
    let (tag, attrs) = element
        .strip_suffix("/>")
        .and_then(|e| e.split_once(' '))
        .ok_or_else(|| format!("unexpected <{element}"))?;
    let attr = |key: &str| -> Result<String, String> {
        attrs
            .split(&format!(" {key}=\""))
            .nth(1)
            .or_else(|| attrs.strip_prefix(&format!("{key}=\"")))
            .and_then(|v| v.split_once('"'))
            .map(|(value, _)| value.to_owned())
            .ok_or_else(|| format!("<{tag}> without {key}"))
    };
    let num = |key: &str| -> Result<String, String> {
        let value = attr(key)?;
        value
            .parse::<f64>()
            .map(|n| format!("{n:?}"))
            .map_err(|_| format!("<{tag}> {key}={value:?}"))
    };
    let nums = |keys: &[&str]| -> Result<String, String> {
        Ok(keys
            .iter()
            .map(|key| num(key))
            .collect::<Result<Vec<_>, _>>()?
            .join(", "))
    };
    Ok(match tag {
        "path" => format!("Part::Path({:?})", attr("d")?),
        "rect" => format!(
            "Part::Rect({}, {})",
            nums(&["x", "y", "width", "height"])?,
            num("rx").unwrap_or_else(|_| "0.0".into())
        ),
        "circle" => format!("Part::Circle({})", nums(&["cx", "cy", "r"])?),
        "ellipse" => format!("Part::Ellipse({})", nums(&["cx", "cy", "rx", "ry"])?),
        "line" => format!("Part::Line({})", nums(&["x1", "y1", "x2", "y2"])?),
        _ => return Err(format!("<{tag}> is not an icon shape")),
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    emit_icons()?;
    // Without an explicit input list Cargo watches the whole package, so a
    // documentation or test edit needlessly rebuilds the toolkit and relinks
    // every app using it. The font fixture and the icon sources are the script's only inputs.
    println!(
        "cargo::rerun-if-changed=tests/fixtures/fonts/TiloSansFeaturesFixture-Regular.ttf.hex"
    );
    emit_hex_fixture(
        include_str!("tests/fixtures/fonts/TiloSansFeaturesFixture-Regular.ttf.hex"),
        "TiloSansFeaturesFixture-Regular.ttf",
    )?;
    Ok(())
}
