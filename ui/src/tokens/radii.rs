#[derive(Clone, Debug)]
pub struct Radii { pub sm: f32, pub md: f32, pub lg: f32, pub xl: f32, pub full: f32 }

impl Default for Radii { fn default() -> Self { Self { sm: 2.0, md: 4.0, lg: 8.0, xl: 16.0, full: 9999.0 } } }
