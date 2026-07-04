#[derive(Clone, Debug)]
pub struct Spacing { pub s0: f32, pub s1: f32, pub s2: f32, pub s3: f32, pub s4: f32, pub s6: f32, pub s8: f32, pub s12: f32, pub s16: f32 }

impl Default for Spacing { fn default() -> Self { Self { s0: 0.0, s1: 4.0, s2: 8.0, s3: 12.0, s4: 16.0, s6: 24.0, s8: 32.0, s12: 48.0, s16: 64.0 } } }
