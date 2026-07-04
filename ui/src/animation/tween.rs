pub use crate::tokens::Ease;

#[derive(Clone, Debug)]
pub struct Tween { start: f32, end: f32, duration: f32, elapsed: f32, ease: Ease }

impl Tween {
    pub fn new(start: f32, end: f32, duration: f32, ease: Ease) -> Self { Self { start, end, duration: duration.max(0.0001), elapsed: 0.0, ease } }
    pub fn advance(&mut self, dt: f32) { self.elapsed = (self.elapsed + dt).min(self.duration); }
    pub fn done(&self) -> bool { self.elapsed >= self.duration }
    pub fn progress(&self) -> f32 { (self.elapsed / self.duration).clamp(0.0, 1.0) }
    pub fn value(&self) -> f32 {
        let p = self.progress();
        let eased = match self.ease {
            Ease::Linear => p,
            Ease::Standard => {
                // cubic-bezier(0.2, 0, 0, 1) approx
                let t = p; t * t * (3.0 - 2.0 * t)
            }
            Ease::Emphasized => {
                let t = p; 1.0 - (1.0 - t).powi(3)
            }
        };
        self.start + (self.end - self.start) * eased
    }
}
