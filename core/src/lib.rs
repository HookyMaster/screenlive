//! Platform-independent framing. This crate never acquires screen pixels.

mod director;
mod session;
pub use director::Director;
pub use session::{CaptureSource, FramingSession, SourceGeometry};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Follow,
    Fit,
}

/// A renderer clears the entire output to the virtual background first, then
/// draws only this source rectangle into its matching output rectangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Blit {
    pub source: Rect,
    pub destination: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FramePlan {
    pub output: Size,
    /// None means background only, including after a source is lost.
    pub blit: Option<Blit>,
}

pub struct Camera {
    output: Size,
    mode: Mode,
    source: Option<Size>,
    center: Point,
    zoom_out: f64,
    previous_pointer: Option<Point>,
}

impl Camera {
    pub fn new(output: Size, mode: Mode) -> Option<Self> {
        (output.width > 0 && output.height > 0).then_some(Self {
            output,
            mode,
            source: None,
            center: Point { x: 0.0, y: 0.0 },
            zoom_out: 0.0,
            previous_pointer: None,
        })
    }

    /// Pointer coordinates are physical pixels relative to the selected source.
    /// A missing/invalid size represents a lost source, never an alternate source.
    pub fn frame(
        &mut self,
        source: Option<Size>,
        pointer: Option<Point>,
        elapsed_seconds: f64,
    ) -> FramePlan {
        let source = source.filter(|s| s.width > 0 && s.height > 0);
        let empty = FramePlan {
            output: self.output,
            blit: None,
        };
        let Some(size) = source else {
            self.source = None;
            self.previous_pointer = None;
            return empty;
        };
        let (w, h) = (f64::from(size.width), f64::from(size.height));
        let (ow, oh) = (f64::from(self.output.width), f64::from(self.output.height));
        let ratio = ow / oh;
        let midpoint = Point {
            x: w / 2.0,
            y: h / 2.0,
        };
        if self.source != source {
            self.source = source;
            self.center = midpoint;
            self.previous_pointer = None;
            self.zoom_out = 0.0;
        }

        // Interpolate between the largest inscribed crop and the smallest
        // same-aspect viewport that contains the entire source.
        let crop_height = h.min(w / ratio);
        let fit_height = h.max(w / ratio);
        let height = if self.mode == Mode::Fit {
            fit_height
        } else {
            let pointer = pointer.filter(|p| p.x >= 0.0 && p.x < w && p.y >= 0.0 && p.y < h);
            if let Some(p) = pointer {
                let dt = if elapsed_seconds.is_finite() {
                    elapsed_seconds.clamp(0.0, 0.1)
                } else {
                    0.0
                };
                let motion = self.previous_pointer.map_or(0.0, |last| {
                    ((p.x - last.x) / (crop_height * ratio)).hypot((p.y - last.y) / crop_height)
                });
                let zoom_target = if dt > 0.0 {
                    ((motion / dt - 1.0) / 4.0).clamp(0.0, 1.0)
                } else {
                    self.zoom_out
                };
                let response = if zoom_target > self.zoom_out {
                    14.0
                } else {
                    3.0
                };
                self.zoom_out = smooth(self.zoom_out, zoom_target, response, dt);
                let target_x = smooth(p.x, midpoint.x, 1.0, self.zoom_out);
                let target_y = smooth(p.y, midpoint.y, 1.0, self.zoom_out);
                let speed = (7.0 / ratio.sqrt()).clamp(3.0, 12.0);
                self.center.x = smooth(self.center.x, target_x, speed, dt);
                self.center.y = smooth(self.center.y, target_y, speed, dt);
            }
            self.previous_pointer = pointer;
            crop_height + (fit_height - crop_height) * self.zoom_out
        };
        let width = height * ratio;
        let x = viewport_start(self.center.x, width, w);
        let y = viewport_start(self.center.y, height, h);
        let left = x.max(0.0);
        let top = y.max(0.0);
        let right = (x + width).min(w);
        let bottom = (y + height).min(h);
        let scale = oh / height;
        FramePlan {
            output: self.output,
            blit: Some(Blit {
                source: Rect {
                    x: left,
                    y: top,
                    width: right - left,
                    height: bottom - top,
                },
                destination: Rect {
                    x: (left - x) * scale,
                    y: (top - y) * scale,
                    width: (right - left) * scale,
                    height: (bottom - top) * scale,
                },
            }),
        }
    }
}

fn smooth(current: f64, target: f64, response: f64, dt: f64) -> f64 {
    current + (target - current) * (1.0 - (-response * dt).exp())
}

fn viewport_start(center: f64, span: f64, source: f64) -> f64 {
    if span >= source {
        (source - span) / 2.0
    } else {
        (center - span / 2.0).clamp(0.0, source - span)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESKTOP: Size = Size {
        width: 1920,
        height: 1080,
    };
    const PORTRAIT: Size = Size {
        width: 1080,
        height: 1920,
    };

    #[test]
    fn fit_preserves_source_and_centers_padding_in_either_orientation() {
        for (source, output, pad_x) in [(DESKTOP, PORTRAIT, false), (PORTRAIT, DESKTOP, true)] {
            let mut camera = Camera::new(output, Mode::Fit).unwrap();
            let blit = camera.frame(Some(source), None, 0.0).blit.unwrap();
            assert!((blit.source.width - f64::from(source.width)).abs() < 1e-6);
            assert!((blit.source.height - f64::from(source.height)).abs() < 1e-6);
            let dest = blit.destination;
            assert_eq!(dest.x > 0.0, pad_x);
            assert_eq!(dest.y > 0.0, !pad_x);
            assert!((2.0 * dest.x + dest.width - f64::from(output.width)).abs() < 1e-6);
            assert!((2.0 * dest.y + dest.height - f64::from(output.height)).abs() < 1e-6);
        }
    }

    #[test]
    fn large_move_pulls_back_without_sampling_outside_source() {
        let mut camera = Camera::new(PORTRAIT, Mode::Follow).unwrap();
        let dt = 1.0 / 30.0;
        let initial = camera.frame(Some(DESKTOP), Some(Point { x: 12.0, y: 500.0 }), dt);
        let pointer = Some(Point {
            x: 1900.0,
            y: 500.0,
        });
        let expanded = camera.frame(Some(DESKTOP), pointer, dt);
        assert!(expanded.blit.unwrap().source.width > initial.blit.unwrap().source.width);
        let outside = camera.frame(Some(DESKTOP), Some(Point { x: -5.0, y: 500.0 }), dt);
        assert_eq!(expanded, outside);
        let mut settled = expanded;
        for _ in 0..120 {
            settled = camera.frame(Some(DESKTOP), pointer, dt);
            let rect = settled.blit.unwrap().source;
            assert!(rect.x >= 0.0 && rect.x + rect.width <= 1920.0 + 1e-6);
            assert!(rect.y >= 0.0 && rect.y + rect.height <= 1080.0 + 1e-6);
        }
        assert!(settled.blit.unwrap().source.width < expanded.blit.unwrap().source.width);
    }

    #[test]
    fn lost_source_returns_background_even_after_a_valid_frame() {
        let mut camera = Camera::new(PORTRAIT, Mode::Follow).unwrap();
        assert!(camera.frame(Some(DESKTOP), None, 0.0).blit.is_some());
        assert_eq!(
            camera.frame(None, None, 0.0),
            FramePlan {
                output: PORTRAIT,
                blit: None
            }
        );
    }
}
