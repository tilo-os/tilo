//! The bytes a painter set loaded from wasm exchanges with the toolkit.
//!
//! A call is one request buffer in and one answer buffer out, little-endian,
//! with no pointers and no shared types: the guest decodes the request,
//! runs its [`crate::PartPainter`], and encodes the answer; the host decodes
//! the answer as untrusted input (every length is bounded by the bytes that
//! are there, and a malformed answer is `None`, drawn by the built-in set).
//!
//! Request: `kind: u8` ([`PAINT`] or [`TRANSITION`]), then
//! * paint: `part: u8, width: f64, height: f64, now: u16, was: u16,
//!   progress: f32`, the tokens;
//! * transition: `part: u8, from: u16, to: u16`.
//!
//! Answer: paint is `0` for "not mine" or `1`, the scene and the hit shape;
//! transition is one [`crate::Transition`].

use kurbo::{Affine, BezPath, PathEl, Point, Size};
use peniko::Color;

use crate::{
    Brush, Easing, Lens, Op, PaintedPart, Param, Part, PartImage, PartScene, PartState, PartTokens,
    States, Stop, Transition,
};

/// A paint request.
pub const PAINT: u8 = 1;
/// A transition request.
pub const TRANSITION: u8 = 2;

/// Deepest layer nesting an answer may carry; vello's own bound is far
/// above any part's need.
pub const MAX_LAYER_DEPTH: usize = 16;

/// A paint request, decoded.
#[derive(Clone, Debug, PartialEq)]
pub struct PaintRequest {
    pub part: Part,
    pub size: Size,
    pub state: PartState,
    pub progress: f32,
    pub tokens: PartTokens,
}

/// A request, decoded.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    Paint(PaintRequest),
    Transition {
        part: Part,
        from: States,
        to: States,
    },
}

/// Encode a paint request.
#[must_use]
pub fn paint_request(request: &PaintRequest) -> Vec<u8> {
    let mut out = Writer::default();
    out.u8(PAINT);
    out.u8(request.part.index());
    out.f64(request.size.width);
    out.f64(request.size.height);
    out.u16(request.state.now.bits());
    out.u16(request.state.was.bits());
    out.f32(request.progress);
    out.tokens(&request.tokens);
    out.0
}

/// Encode a transition request.
#[must_use]
pub fn transition_request(part: Part, from: States, to: States) -> Vec<u8> {
    let mut out = Writer::default();
    out.u8(TRANSITION);
    out.u8(part.index());
    out.u16(from.bits());
    out.u16(to.bits());
    out.0
}

/// Decode a request.
#[must_use]
pub fn request(bytes: &[u8]) -> Option<Request> {
    let mut input = Reader(bytes);
    let request = match input.u8()? {
        PAINT => Request::Paint(PaintRequest {
            part: Part::from_index(input.u8()?)?,
            size: Size::new(input.f64()?, input.f64()?),
            state: PartState {
                now: States::from_bits(input.u16()?),
                was: States::from_bits(input.u16()?),
            },
            progress: input.f32()?,
            tokens: input.tokens()?,
        }),
        TRANSITION => Request::Transition {
            part: Part::from_index(input.u8()?)?,
            from: States::from_bits(input.u16()?),
            to: States::from_bits(input.u16()?),
        },
        _ => return None,
    };
    input.0.is_empty().then_some(request)
}

/// Encode a paint answer.
#[must_use]
pub fn paint_answer(painted: Option<&PaintedPart>) -> Vec<u8> {
    let mut out = Writer::default();
    match painted {
        None => out.u8(0),
        Some(painted) => {
            out.u8(1);
            out.scene(&painted.scene);
            match &painted.hit {
                None => out.u8(0),
                Some(path) => {
                    out.u8(1);
                    out.path(path);
                }
            }
        }
    }
    out.0
}

/// Decode a paint answer: `Some(None)` for "not mine", `None` for bytes
/// that are not an answer.
#[must_use]
pub fn paint(bytes: &[u8]) -> Option<Option<PaintedPart>> {
    let mut input = Reader(bytes);
    let answer = match input.u8()? {
        0 => None,
        1 => {
            let scene = input.scene()?;
            let hit = match input.u8()? {
                0 => None,
                1 => Some(input.path()?),
                _ => return None,
            };
            Some(PaintedPart { scene, hit })
        }
        _ => return None,
    };
    input.0.is_empty().then_some(answer)
}

/// Encode a transition answer.
#[must_use]
pub fn transition_answer(transition: Transition) -> Vec<u8> {
    let mut out = Writer::default();
    match transition {
        Transition::Inherit => out.u8(0),
        Transition::None => out.u8(1),
        Transition::Spring { response, damping } => {
            out.u8(2);
            out.f32(response);
            out.f32(damping);
        }
        Transition::Stepped { steps, duration } => {
            out.u8(3);
            out.u16(steps);
            out.f32(duration);
        }
        Transition::Eased { duration, easing } => {
            out.u8(4);
            out.f32(duration);
            out.u8(match easing {
                Easing::Linear => 0,
                Easing::EaseOut => 1,
                Easing::EaseInOut => 2,
            });
        }
    }
    out.0
}

/// Decode a transition answer.
#[must_use]
pub fn transition(bytes: &[u8]) -> Option<Transition> {
    let mut input = Reader(bytes);
    let transition = match input.u8()? {
        0 => Transition::Inherit,
        1 => Transition::None,
        2 => Transition::Spring {
            response: input.f32()?,
            damping: input.f32()?,
        },
        3 => Transition::Stepped {
            steps: input.u16()?,
            duration: input.f32()?,
        },
        4 => Transition::Eased {
            duration: input.f32()?,
            easing: match input.u8()? {
                0 => Easing::Linear,
                1 => Easing::EaseOut,
                2 => Easing::EaseInOut,
                _ => return None,
            },
        },
        _ => return None,
    };
    input.0.is_empty().then_some(transition)
}

#[derive(Default)]
struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, value: u8) {
        self.0.push(value);
    }
    fn u16(&mut self, value: u16) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }
    fn u32(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }
    fn f32(&mut self, value: f32) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }
    fn f64(&mut self, value: f64) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }
    fn len(&mut self, len: usize) {
        self.u32(u32::try_from(len).unwrap_or(u32::MAX));
    }
    fn bytes(&mut self, bytes: &[u8]) {
        self.len(bytes.len());
        self.0.extend_from_slice(bytes);
    }
    fn color(&mut self, color: Color) {
        for channel in color.components {
            self.f32(channel);
        }
    }
    fn point(&mut self, point: Point) {
        self.f64(point.x);
        self.f64(point.y);
    }
    fn affine(&mut self, affine: Affine) {
        for coefficient in affine.as_coeffs() {
            self.f64(coefficient);
        }
    }
    fn path(&mut self, path: &BezPath) {
        self.len(path.elements().len());
        for element in path.elements() {
            match *element {
                PathEl::MoveTo(p) => {
                    self.u8(0);
                    self.point(p);
                }
                PathEl::LineTo(p) => {
                    self.u8(1);
                    self.point(p);
                }
                PathEl::QuadTo(a, b) => {
                    self.u8(2);
                    self.point(a);
                    self.point(b);
                }
                PathEl::CurveTo(a, b, c) => {
                    self.u8(3);
                    self.point(a);
                    self.point(b);
                    self.point(c);
                }
                PathEl::ClosePath => self.u8(4),
            }
        }
    }
    fn stops(&mut self, stops: &[Stop]) {
        self.len(stops.len());
        for stop in stops {
            self.f32(stop.offset);
            self.color(stop.color);
        }
    }
    fn brush(&mut self, brush: &Brush) {
        match brush {
            Brush::Solid(color) => {
                self.u8(0);
                self.color(*color);
            }
            Brush::Linear { start, end, stops } => {
                self.u8(1);
                self.point(*start);
                self.point(*end);
                self.stops(stops);
            }
            Brush::Radial {
                center,
                radius,
                stops,
            } => {
                self.u8(2);
                self.point(*center);
                self.f32(*radius);
                self.stops(stops);
            }
            Brush::Pattern { image, scale } => {
                self.u8(3);
                self.u32(image.width);
                self.u32(image.height);
                self.bytes(&image.rgba);
                self.f32(*scale);
            }
            Brush::Backdrop {
                blur,
                lens,
                fallback,
            } => {
                self.u8(4);
                self.f32(*blur);
                self.f32(lens.refraction);
                self.color(*fallback);
            }
        }
    }
    fn scene(&mut self, scene: &PartScene) {
        self.len(scene.ops.len());
        for op in &scene.ops {
            match op {
                Op::Fill {
                    path,
                    brush,
                    brush_transform,
                } => {
                    self.u8(0);
                    self.path(path);
                    self.brush(brush);
                    self.affine(*brush_transform);
                }
                Op::Stroke { path, width, brush } => {
                    self.u8(1);
                    self.path(path);
                    self.f32(*width);
                    self.brush(brush);
                }
                Op::PushLayer { clip, alpha } => {
                    self.u8(2);
                    self.path(clip);
                    self.f32(*alpha);
                }
                Op::PopLayer => self.u8(3),
            }
        }
    }
    fn tokens(&mut self, tokens: &PartTokens) {
        for color in [
            tokens.fill,
            tokens.ink,
            tokens.edge,
            tokens.ground,
            tokens.raised,
            tokens.accent,
            tokens.border,
        ] {
            self.color(color);
        }
        self.f32(tokens.radius);
        self.f32(tokens.border_width);
        self.f32(tokens.scale);
        self.u8(u8::from(tokens.dark));
        self.u8(u8::from(tokens.high_contrast));
        self.u8(u8::from(tokens.reduce_transparency));
        self.len(tokens.params.len());
        for param in &tokens.params {
            self.bytes(param.name.as_bytes());
            self.f32(param.value);
        }
    }
}

/// Untrusted bytes, read front to back. Every read is bounds-checked; a
/// count is believed only as far as the bytes left can hold it.
struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, rest) = self.0.split_first_chunk::<N>()?;
        self.0 = rest;
        Some(*head)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take::<1>().map(|[byte]| byte)
    }
    fn u16(&mut self) -> Option<u16> {
        self.take().map(u16::from_le_bytes)
    }
    fn u32(&mut self) -> Option<u32> {
        self.take().map(u32::from_le_bytes)
    }
    fn f32(&mut self) -> Option<f32> {
        self.take()
            .map(f32::from_le_bytes)
            .filter(|v| v.is_finite())
    }
    fn f64(&mut self) -> Option<f64> {
        self.take()
            .map(f64::from_le_bytes)
            .filter(|v| v.is_finite())
    }
    /// A count of items at least `min_bytes` each, refused when the bytes
    /// left cannot hold that many.
    fn count(&mut self, min_bytes: usize) -> Option<usize> {
        let count = usize::try_from(self.u32()?).ok()?;
        (count.checked_mul(min_bytes)? <= self.0.len()).then_some(count)
    }
    fn bytes(&mut self) -> Option<&[u8]> {
        let len = self.count(1)?;
        let (head, rest) = self.0.split_at_checked(len)?;
        self.0 = rest;
        Some(head)
    }
    fn color(&mut self) -> Option<Color> {
        Some(Color::new([
            self.f32()?,
            self.f32()?,
            self.f32()?,
            self.f32()?,
        ]))
    }
    fn point(&mut self) -> Option<Point> {
        Some(Point::new(self.f64()?, self.f64()?))
    }
    fn affine(&mut self) -> Option<Affine> {
        Some(Affine::new([
            self.f64()?,
            self.f64()?,
            self.f64()?,
            self.f64()?,
            self.f64()?,
            self.f64()?,
        ]))
    }
    fn path(&mut self) -> Option<BezPath> {
        let count = self.count(1)?;
        let mut path = BezPath::new();
        for _ in 0..count {
            let element = match self.u8()? {
                0 => PathEl::MoveTo(self.point()?),
                1 => PathEl::LineTo(self.point()?),
                2 => PathEl::QuadTo(self.point()?, self.point()?),
                3 => PathEl::CurveTo(self.point()?, self.point()?, self.point()?),
                4 => PathEl::ClosePath,
                _ => return None,
            };
            path.push(element);
        }
        Some(path)
    }
    fn stops(&mut self) -> Option<Vec<Stop>> {
        let count = self.count(20)?;
        (0..count)
            .map(|_| {
                Some(Stop {
                    offset: self.f32()?,
                    color: self.color()?,
                })
            })
            .collect()
    }
    fn brush(&mut self) -> Option<Brush> {
        Some(match self.u8()? {
            0 => Brush::Solid(self.color()?),
            1 => Brush::Linear {
                start: self.point()?,
                end: self.point()?,
                stops: self.stops()?,
            },
            2 => Brush::Radial {
                center: self.point()?,
                radius: self.f32()?,
                stops: self.stops()?,
            },
            3 => {
                let image = PartImage {
                    width: self.u32()?,
                    height: self.u32()?,
                    rgba: self.bytes()?.to_vec(),
                };
                if !image.is_well_formed() {
                    return None;
                }
                Brush::Pattern {
                    image,
                    scale: self.f32()?,
                }
            }
            4 => Brush::Backdrop {
                blur: self.f32()?,
                lens: Lens {
                    refraction: self.f32()?,
                },
                fallback: self.color()?,
            },
            _ => return None,
        })
    }
    fn scene(&mut self) -> Option<PartScene> {
        let count = self.count(1)?;
        let mut ops = Vec::with_capacity(count);
        let mut depth = 0_usize;
        for _ in 0..count {
            let op = match self.u8()? {
                0 => Op::Fill {
                    path: self.path()?,
                    brush: self.brush()?,
                    brush_transform: self.affine()?,
                },
                1 => Op::Stroke {
                    path: self.path()?,
                    width: self.f32()?,
                    brush: self.brush()?,
                },
                2 => {
                    depth = depth.checked_add(1).filter(|d| *d <= MAX_LAYER_DEPTH)?;
                    Op::PushLayer {
                        clip: self.path()?,
                        alpha: self.f32()?,
                    }
                }
                3 => {
                    depth = depth.checked_sub(1)?;
                    Op::PopLayer
                }
                _ => return None,
            };
            ops.push(op);
        }
        // Every layer an answer opens, it closes.
        (depth == 0).then_some(PartScene { ops })
    }
    fn tokens(&mut self) -> Option<PartTokens> {
        let mut colors = [Color::BLACK; 7];
        for slot in &mut colors {
            *slot = self.color()?;
        }
        let [fill, ink, edge, ground, raised, accent, border] = colors;
        Some(PartTokens {
            fill,
            ink,
            edge,
            ground,
            raised,
            accent,
            border,
            radius: self.f32()?,
            border_width: self.f32()?,
            scale: self.f32()?,
            dark: self.u8()? != 0,
            high_contrast: self.u8()? != 0,
            reduce_transparency: self.u8()? != 0,
            params: {
                let count = self.count(8)?;
                (0..count)
                    .map(|_| {
                        Some(Param {
                            name: String::from_utf8(self.bytes()?.to_vec()).ok()?,
                            value: self.f32()?,
                        })
                    })
                    .collect::<Option<Vec<_>>>()?
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kurbo::{Rect, Shape};

    fn every_brush() -> Vec<Brush> {
        let stops = vec![
            Stop {
                offset: 0.0,
                color: Color::BLACK,
            },
            Stop {
                offset: 1.0,
                color: Color::WHITE,
            },
        ];
        vec![
            Brush::Solid(Color::from_rgb8(1, 2, 3)),
            Brush::Linear {
                start: Point::new(0.0, 1.0),
                end: Point::new(2.0, 3.0),
                stops: stops.clone(),
            },
            Brush::Radial {
                center: Point::new(4.0, 5.0),
                radius: 6.0,
                stops,
            },
            Brush::Pattern {
                image: PartImage {
                    width: 2,
                    height: 2,
                    rgba: (0..16).collect(),
                },
                scale: 2.0,
            },
            Brush::Backdrop {
                blur: 12.0,
                lens: Lens { refraction: 3.0 },
                fallback: Color::WHITE,
            },
        ]
    }

    #[test]
    fn a_painted_part_round_trips() {
        let body = Rect::new(0.0, 0.0, 40.0, 20.0).to_rounded_rect(6.0);
        let mut scene = PartScene::new();
        for brush in every_brush() {
            scene.fill_placed(body.to_path(0.1), brush, Affine::translate((1.0, 2.0)));
        }
        scene.stroke(body.to_path(0.1), 2.0, Brush::Solid(Color::BLACK));
        scene.layer(body.to_path(0.1), 0.5, |inner| {
            inner.fill(body.to_path(0.1), Brush::Solid(Color::WHITE));
        });
        let painted = PaintedPart::new(scene).with_hit(body.to_path(0.1));
        assert_eq!(paint(&paint_answer(Some(&painted))), Some(Some(painted)));
        assert_eq!(paint(&paint_answer(None)), Some(None));
    }

    #[test]
    fn requests_and_transitions_round_trip() {
        let request_in = PaintRequest {
            part: Part::ButtonContainer,
            size: Size::new(80.0, 28.0),
            state: PartState {
                now: States::HOVERED | States::PRESSED,
                was: States::HOVERED,
            },
            progress: 0.25,
            tokens: PartTokens {
                params: vec![Param {
                    name: "levels".into(),
                    value: 4.0,
                }],
                ..PartTokens::default()
            },
        };
        assert_eq!(
            request(&paint_request(&request_in)),
            Some(Request::Paint(request_in))
        );
        assert_eq!(
            request(&transition_request(
                Part::ScrollThumb,
                States::REST,
                States::DRAGGING
            )),
            Some(Request::Transition {
                part: Part::ScrollThumb,
                from: States::REST,
                to: States::DRAGGING
            })
        );
        for t in [
            Transition::Inherit,
            Transition::None,
            Transition::Spring {
                response: 0.3,
                damping: 0.6,
            },
            Transition::Stepped {
                steps: 3,
                duration: 0.1,
            },
            Transition::Eased {
                duration: 0.2,
                easing: Easing::EaseInOut,
            },
        ] {
            assert_eq!(transition(&transition_answer(t)), Some(t));
        }
    }

    #[test]
    fn malformed_answers_are_refused_not_believed() {
        // A count far past the bytes that follow.
        assert_eq!(paint(&[1, 0xff, 0xff, 0xff, 0xff]), None);
        // An unknown tag, trailing bytes, a non-finite number.
        assert_eq!(paint(&[7]), None);
        assert_eq!(paint(&[0, 0]), None);
        let mut nan = vec![2];
        nan.extend_from_slice(&f32::NAN.to_le_bytes());
        nan.extend_from_slice(&1.0_f32.to_le_bytes());
        assert_eq!(transition(&nan), None);
        // A layer pushed and never popped.
        let mut scene = PartScene::new();
        scene.ops.push(Op::PushLayer {
            clip: BezPath::new(),
            alpha: 1.0,
        });
        assert_eq!(paint(&paint_answer(Some(&PaintedPart::new(scene)))), None);
        // A pattern whose pixels do not fill its size.
        let mut scene = PartScene::new();
        scene.fill(
            BezPath::new(),
            Brush::Pattern {
                image: PartImage {
                    width: 4,
                    height: 4,
                    rgba: vec![0; 4],
                },
                scale: 1.0,
            },
        );
        assert_eq!(paint(&paint_answer(Some(&PaintedPart::new(scene)))), None);
    }
}
