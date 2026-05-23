#[derive(Clone, Copy, Debug)]
pub struct SpringParams { pub stiffness: f32, pub damping: f32, pub mass: f32 }

#[derive(Clone, Debug)]
pub struct SpringPresets { pub subtle: SpringParams, pub snappy: SpringParams, pub bouncy: SpringParams }

impl Default for SpringPresets {
    fn default() -> Self {
        Self {
            subtle: SpringParams { stiffness: 120.0, damping: 22.0, mass: 1.0 },
            snappy: SpringParams { stiffness: 260.0, damping: 28.0, mass: 1.0 },
            bouncy: SpringParams { stiffness: 180.0, damping: 12.0, mass: 1.0 },
        }
    }
}

#[derive(Clone, Debug)]
pub struct Durations { pub fast_ms: u32, pub base_ms: u32, pub slow_ms: u32 }

impl Default for Durations { fn default() -> Self { Self { fast_ms: 120, base_ms: 220, slow_ms: 400 } } }

#[derive(Clone, Copy, Debug)]
pub enum Ease { Linear, Standard, Emphasized }

#[derive(Clone, Debug)]
pub struct Motion { pub springs: SpringPresets, pub durations: Durations }

impl Default for Motion { fn default() -> Self { Self { springs: SpringPresets::default(), durations: Durations::default() } } }
