use crate::tokens::SpringParams;

#[derive(Clone, Debug)]
pub struct Spring { value: f32, velocity: f32, target: f32, params: SpringParams }

impl Spring {
    pub fn new(value: f32, params: SpringParams) -> Self { Self { value, velocity: 0.0, target: value, params } }
    pub fn set_target(&mut self, t: f32) { self.target = t; }
    pub fn snap(&mut self, v: f32) { self.value = v; self.velocity = 0.0; self.target = v; }
    pub fn value(&self) -> f32 { self.value }
    pub fn velocity(&self) -> f32 { self.velocity }
    pub fn target(&self) -> f32 { self.target }
    pub fn is_settled(&self) -> bool {
        (self.value - self.target).abs() < 0.1 && self.velocity.abs() < 0.1
    }
    pub fn step(&mut self, dt: f32) {
        // Semi-implicit Euler for stability.
        let k = self.params.stiffness;
        let c = self.params.damping;
        let m = self.params.mass.max(0.0001);
        let force = -k * (self.value - self.target) - c * self.velocity;
        let accel = force / m;
        self.velocity += accel * dt;
        self.value += self.velocity * dt;
    }
}
