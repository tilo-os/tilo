use vello::peniko::Color;

#[derive(Clone, Debug)]
pub struct Shadow { pub offset_y: f32, pub blur: f32, pub color: Color }

#[derive(Clone, Debug)]
pub struct Elevation { pub e0: Shadow, pub e1: Shadow, pub e2: Shadow, pub e3: Shadow, pub e4: Shadow, pub glass_inner: Shadow, pub glass_outer: Shadow }

impl Default for Elevation {
    fn default() -> Self {
        let c = |a: u8| Color::from_rgba8(0, 0, 0, a);
        Self {
            e0: Shadow { offset_y: 0.0, blur: 0.0, color: c(0) },
            e1: Shadow { offset_y: 1.0, blur: 2.0, color: c(40) },
            e2: Shadow { offset_y: 2.0, blur: 6.0, color: c(60) },
            e3: Shadow { offset_y: 4.0, blur: 12.0, color: c(80) },
            e4: Shadow { offset_y: 8.0, blur: 24.0, color: c(110) },
            glass_inner: Shadow { offset_y: 0.0, blur: 1.0, color: Color::from_rgba8(255, 255, 255, 20) },
            glass_outer: Shadow { offset_y: 12.0, blur: 32.0, color: c(120) },
        }
    }
}
