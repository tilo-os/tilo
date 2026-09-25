//! `tilo-pdf-helper` — the sandboxed PDF engine process. See
//! [`tilo_pdf::helper`]; it is started by `tilo_pdf::Engine`, never by hand.
//!
//! The fonts appearance text is set in are compiled in here, from Tilo's
//! pinned font catalog: exports then never depend on the fonts a host has
//! installed, and the sandbox needs no access to them for it. They live in
//! this binary rather than the library so that every other process linking
//! `tilo-pdf` stays small.

macro_rules! catalog {
    ($file:literal) => {
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../sdk/ui-core/assets/fonts/",
            $file
        ))
    };
}

/// Preference order: the first face covering a character sets it.
fn faces() -> Vec<tilo_pdf::BundledFace> {
    let face = |data: &'static [u8], index: u32, family: &'static str| tilo_pdf::BundledFace {
        data,
        index,
        family,
    };
    vec![
        face(catalog!("NotoSans[wght].ttf"), 0, "Noto Sans"),
        face(catalog!("NotoSansArabic[wght].ttf"), 0, "Noto Sans Arabic"),
        face(catalog!("NotoSansHebrew[wght].ttf"), 0, "Noto Sans Hebrew"),
        face(
            catalog!("NotoSansDevanagari[wght].ttf"),
            0,
            "Noto Sans Devanagari",
        ),
        face(
            catalog!("NotoSansBengali[wght].ttf"),
            0,
            "Noto Sans Bengali",
        ),
        face(catalog!("NotoSansThai[wght].ttf"), 0, "Noto Sans Thai"),
        // Face 2 of the collection is Noto Sans CJK SC: Han in its
        // simplified-Chinese forms, with kana and hangul.
        face(catalog!("NotoSansCJK-VF.ttc"), 2, "Noto Sans CJK SC"),
        face(
            catalog!("NotoSansSymbols[wght].ttf"),
            0,
            "Noto Sans Symbols",
        ),
        face(
            catalog!("NotoSansSymbols2-Regular.ttf"),
            0,
            "Noto Sans Symbols 2",
        ),
        face(catalog!("NotoSansMath-Regular.ttf"), 0, "Noto Sans Math"),
    ]
}

fn main() {
    tilo_pdf::install_appearance_faces(faces());
    std::process::exit(tilo_pdf::helper::run());
}
