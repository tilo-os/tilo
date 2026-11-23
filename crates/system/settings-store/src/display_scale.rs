//! Per-output display scale, derived from the panel's physical size (which DRM reports per
//! connector, in millimetres) against its resolution: the only signal that separates a 1080p 24"
//! monitor (≈92 dpi, wants 1.0) from a 1080p 14" panel (≈157 dpi, wants 1.5). A person's setting
//! overrides the derivation outright.

pub const REFERENCE_DPI: f64 = 110.0;

pub const SCALE_STEP: f64 = 0.25;

pub const MIN_SCALE: f64 = 1.0;

pub const MAX_SCALE: f64 = 2.0;

pub const MIN_LOGICAL_WIDTH: f64 = 1024.0;

pub fn dpi_from_physical(px: (u32, u32), mm: (u32, u32)) -> Option<f64> {
    if px.0 == 0 || px.1 == 0 || mm.0 == 0 || mm.1 == 0 {
        return None;
    }
    let diag_px = (f64::from(px.0).powi(2) + f64::from(px.1).powi(2)).sqrt();
    let diag_mm = (f64::from(mm.0).powi(2) + f64::from(mm.1).powi(2)).sqrt();
    let diag_in = diag_mm / 25.4;
    if diag_in <= 0.0 {
        return None;
    }
    Some(diag_px / diag_in)
}

pub fn snap_scale(raw: f64) -> f64 {
    let stepped = (raw / SCALE_STEP).round() * SCALE_STEP;
    stepped.clamp(MIN_SCALE, MAX_SCALE)
}

pub fn derive_scale(px: (u32, u32), mm: Option<(u32, u32)>) -> f64 {
    let Some(mm) = mm else {
        return MIN_SCALE;
    };
    let Some(dpi) = dpi_from_physical(px, mm) else {
        return MIN_SCALE;
    };
    let mut scale = snap_scale(dpi / REFERENCE_DPI);
    while scale > MIN_SCALE && (f64::from(px.0) / scale) < MIN_LOGICAL_WIDTH {
        scale -= SCALE_STEP;
    }
    scale.max(MIN_SCALE)
}

pub fn user_scale(display: &serde_json::Map<String, serde_json::Value>) -> Option<f64> {
    display
        .get("scale")
        .and_then(serde_json::Value::as_f64)
        .filter(|scale| (0.5..=3.0).contains(scale))
}

pub fn load_user_scale() -> Option<f64> {
    user_scale(&crate::load_section(crate::hardware::DISPLAY_SECTION))
}

pub fn resolve_scale(px: (u32, u32), mm: Option<(u32, u32)>, override_scale: Option<f64>) -> f64 {
    match override_scale {
        Some(s) if s > 0.0 => s,
        _ => derive_scale(px, mm),
    }
}
