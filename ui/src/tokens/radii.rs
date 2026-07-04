#[derive(Clone, Debug)]
pub struct Radii { pub sm: f32, pub md: f32, pub lg: f32, pub xl: f32, pub full: f32 }

// Defaults mapped onto Tailwind's radius scale as used by the nirios concept
// (`docs/nirios/concept-features.md`): `rounded` (4), `rounded-lg` (8),
// `rounded-xl` (12), `rounded-3xl` (24), `rounded-full` (≈9999). The 2xl step
// (16 px) is intentionally absent — there are only five slots and the concept
// leans on 8/12/24 for cards vs. pills vs. panels, with the window card's
// `rounded-3xl` (24) being the visual anchor for `radii.xl`.
impl Default for Radii { fn default() -> Self { Self { sm: 4.0, md: 8.0, lg: 12.0, xl: 24.0, full: 9999.0 } } }
