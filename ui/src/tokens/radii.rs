#[derive(Clone, Debug)]
pub struct Radii {
    pub sm: f32,
    pub md: f32,
    pub lg: f32,
    pub xl: f32,
    pub full: f32,
}

// Mapped onto Tailwind's radius scale per the nirios concept:
// `rounded`(4)/`rounded-lg`(8)/`rounded-xl`(12)/`rounded-3xl`(24)/`rounded-full`(≈9999).
// The 2xl step (16px) is intentionally absent — five slots, and the window
// card's `rounded-3xl` is the visual anchor for `radii.xl`.
impl Default for Radii {
    fn default() -> Self {
        Self {
            sm: 4.0,
            md: 8.0,
            lg: 12.0,
            xl: 24.0,
            full: 9999.0,
        }
    }
}
