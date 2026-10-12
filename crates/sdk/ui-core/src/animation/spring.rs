use crate::tokens::SpringParams;

#[derive(Clone, Debug)]
pub struct Spring {
    value: f32,
    velocity: f32,
    target: f32,
    params: SpringParams,
}

impl Spring {
    pub fn new(value: f32, params: SpringParams) -> Self {
        Self {
            value,
            velocity: 0.0,
            target: value,
            params,
        }
    }
    pub fn set_target(&mut self, t: f32) {
        self.target = t;
    }
    /// Swap the spring's constants while keeping its current value AND
    /// velocity.
    ///
    /// A gesture is usually not one spring but two phases of the same
    /// moving thing: a stiff follow while the pointer is down, a heavier
    /// settle once it lifts. Rebuilding the spring at the hand-off would
    /// zero the velocity and throw away exactly the momentum the settle
    /// is supposed to carry, so the params are swapped in place instead.
    pub fn set_params(&mut self, params: SpringParams) {
        self.params = params;
    }
    pub fn params(&self) -> SpringParams {
        self.params
    }
    pub fn snap(&mut self, v: f32) {
        self.value = v;
        self.velocity = 0.0;
        self.target = v;
    }
    pub fn value(&self) -> f32 {
        self.value
    }
    pub fn velocity(&self) -> f32 {
        self.velocity
    }
    pub fn target(&self) -> f32 {
        self.target
    }
    pub fn is_settled(&self) -> bool {
        (self.value - self.target).abs() < 0.1 && self.velocity.abs() < 0.1
    }
    /// Advance by `dt` seconds. A long frame is integrated in equal
    /// substeps no longer than [`Self::max_substep`], so a slow host (a
    /// software renderer, a stalled frame) lands where a fast one would
    /// instead of diverging.
    pub fn step(&mut self, dt: f32) {
        if dt <= 0.0 || !dt.is_finite() {
            return;
        }
        let h_max = self.max_substep();
        let n = (dt / h_max).ceil().clamp(1.0, MAX_SUBSTEPS);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "n is a whole number clamped to [1, MAX_SUBSTEPS]"
        )]
        let count = n as u32;
        let h = dt / n;
        for _ in 0..count {
            self.substep(h);
        }
    }

    /// Longest step semi-implicit Euler takes with these params: a frame
    /// at 60 Hz, shortened to stay well inside the integrator's stability
    /// bound (`c·h/m < 2` and `(k/m)·h² + 2·(c/m)·h < 4`) for stiff or
    /// heavily damped springs.
    fn max_substep(&self) -> f32 {
        let m = self.params.mass.max(0.0001);
        let w2 = (self.params.stiffness / m).max(0.0);
        let g = (self.params.damping / m).max(0.0);
        let mut bound = f32::INFINITY;
        if g > 0.0 {
            bound = bound.min(2.0 / g);
        }
        if w2 > 0.0 {
            bound = bound.min(((g * g + 4.0 * w2).sqrt() - g) / w2);
        }
        (0.9 * bound).min(1.0 / 60.0)
    }

    fn substep(&mut self, h: f32) {
        // Semi-implicit Euler.
        let k = self.params.stiffness;
        let c = self.params.damping;
        let m = self.params.mass.max(0.0001);
        let force = -k * (self.value - self.target) - c * self.velocity;
        let accel = force / m;
        self.velocity += accel * h;
        self.value += self.velocity * h;
    }
}

/// Cap on substeps per [`Spring::step`], so a stall of minutes costs a
/// bounded amount of work; a spring that long behind has settled anyway.
const MAX_SUBSTEPS: f32 = 4096.0;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::SpringPresets;

    /// A slow host steps the spring at 50 ms. Plain semi-implicit Euler
    /// is unstable there for the heavy preset (`c·dt/m = 3.2 > 2`), and
    /// the window width it drives grew without bound.
    #[test]
    fn long_frames_settle_instead_of_diverging() {
        let presets = SpringPresets::default();
        for params in [
            presets.subtle,
            presets.snappy,
            presets.bouncy,
            presets.heavy,
            presets.tracking,
        ] {
            for (dt, frames) in [
                (1.0 / 240.0, 960),
                (1.0 / 60.0, 240),
                (1.0 / 20.0, 80),
                (0.25, 16),
            ] {
                let mut spring = Spring::new(0.0, params);
                spring.set_target(664.0);
                let mut peak: f32 = 0.0;
                for _ in 0..frames {
                    spring.step(dt);
                    peak = peak.max(spring.value().abs());
                }
                assert!(
                    peak < 664.0 * 2.0,
                    "{params:?} at dt {dt}: peaked at {peak}"
                );
                assert!(
                    spring.is_settled(),
                    "{params:?} at dt {dt}: ended at {} moving {}",
                    spring.value(),
                    spring.velocity()
                );
            }
        }
    }

    /// One frame at 60 Hz is still a single plain step for the presets.
    #[test]
    fn a_sixty_hertz_frame_is_one_step() {
        let presets = SpringPresets::default();
        for params in [
            presets.subtle,
            presets.snappy,
            presets.bouncy,
            presets.heavy,
            presets.tracking,
        ] {
            let mut stepped = Spring::new(0.0, params);
            stepped.set_target(100.0);
            stepped.step(1.0 / 60.0);
            let mut plain = Spring::new(0.0, params);
            plain.set_target(100.0);
            plain.substep(1.0 / 60.0);
            assert_eq!(
                stepped.value().to_bits(),
                plain.value().to_bits(),
                "{params:?}"
            );
        }
    }
}
