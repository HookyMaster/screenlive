use crate::{Blit, Mode, Point, Rect, Size};

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Parked,
    Pulling,
    Holding,
    Closing,
}

pub struct Director {
    output: Size,
    mode: Mode,
    close_fraction: f64,
    bounds: Option<Rect>,
    view: Rect,
    origin: Rect,
    target: Rect,
    anchor: Option<Point>,
    quiet_anchor: Option<Point>,
    previous_pointer: Option<Point>,
    pan_idle: f64,
    elapsed: f64,
    phase: Phase,
}

impl Director {
    pub fn set_output_size(&mut self, size: Size) {
        debug_assert_eq!(
            u64::from(size.width) * u64::from(self.output.height),
            u64::from(size.height) * u64::from(self.output.width)
        );
        self.output = size;
    }

    pub fn new(output: Size, mode: Mode, close_percent: u32) -> Self {
        let empty = Rect {
            x: 0.,
            y: 0.,
            width: 0.,
            height: 0.,
        };
        Self {
            output,
            mode,
            close_fraction: f64::from(close_percent.clamp(25, 100)) / 100.,
            bounds: None,
            view: empty,
            origin: empty,
            target: empty,
            anchor: None,
            quiet_anchor: None,
            previous_pointer: None,
            pan_idle: 0.,
            elapsed: 0.,
            phase: Phase::Parked,
        }
    }

    pub fn update(&mut self, bounds: Option<Rect>, pointer: Option<Point>, dt: f64) {
        self.update_for_surface(bounds, pointer, None, dt);
    }

    pub fn update_for_surface(
        &mut self,
        bounds: Option<Rect>,
        pointer: Option<Point>,
        active: Option<Rect>,
        dt: f64,
    ) {
        let Some(bounds) = bounds else {
            self.bounds = None;
            return;
        };
        let ratio = f64::from(self.output.width) / f64::from(self.output.height);
        let mid = Point {
            x: bounds.x + bounds.width / 2.,
            y: bounds.y + bounds.height / 2.,
        };
        let far = viewport(bounds, mid, bounds.height.max(bounds.width / ratio), ratio);
        let target_bounds = active.unwrap_or(bounds);
        let near_height = self.minimum_height(target_bounds, ratio);
        let near = |center| centered_view(center, near_height, ratio);
        let quiet_threshold = (near_height.min(near_height * ratio) * 0.025).max(3.);
        let dt = if dt.is_finite() {
            dt.clamp(0., 0.1)
        } else {
            0.
        };
        if self.bounds != Some(bounds) {
            self.bounds = Some(bounds);
            self.view = if self.mode == Mode::Fit {
                far
            } else {
                near(pointer.unwrap_or(mid))
            };
            self.phase = Phase::Parked;
            self.anchor = pointer;
            self.previous_pointer = pointer;
            self.pan_idle = 0.;
            self.elapsed = 0.;
            self.quiet_anchor = None;
            return;
        }
        if self.mode == Mode::Fit {
            return;
        }
        let previous_pointer = self.previous_pointer;
        if pointer.is_some() {
            self.previous_pointer = pointer;
        }
        match self.phase {
            Phase::Parked => {
                let Some(pointer) = pointer else {
                    return;
                };
                let anchor = *self
                    .anchor
                    .get_or_insert(previous_pointer.unwrap_or(pointer));
                if !contains(inset(self.view, 0.1), pointer) {
                    self.transition(
                        Phase::Pulling,
                        travel_view(anchor, pointer, self.view.height, ratio),
                    );
                } else {
                    let center = centered_view(pointer, self.view.height, ratio);
                    let blend = 1. - (-10. * dt).exp();
                    self.view.x += (center.x - self.view.x) * blend;
                    self.view.y += (center.y - self.view.y) * blend;
                    if previous_pointer.is_some_and(|p| distance(p, pointer) <= 1.) {
                        self.pan_idle += dt;
                        if self.pan_idle >= 0.20 {
                            self.anchor = Some(pointer);
                        }
                    } else {
                        self.pan_idle = 0.;
                    }
                }
            }
            Phase::Pulling => {
                if self.advance(dt, 0.42) {
                    self.phase = Phase::Holding;
                    self.quiet_anchor = pointer;
                    self.elapsed = 0.;
                }
            }
            Phase::Holding => {
                let Some(pointer) = pointer else {
                    self.quiet_anchor = None;
                    self.elapsed = 0.;
                    return;
                };
                if !contains(self.view, pointer) {
                    let start = self.anchor.or(previous_pointer).unwrap_or(pointer);
                    self.transition(
                        Phase::Pulling,
                        travel_view(start, pointer, near_height, ratio),
                    );
                    return;
                }
                if self
                    .quiet_anchor
                    .is_none_or(|p| distance(p, pointer) > quiet_threshold)
                {
                    self.quiet_anchor = Some(pointer);
                    self.elapsed = 0.;
                } else {
                    self.elapsed += dt;
                    if self.elapsed >= 0.60 {
                        self.transition(Phase::Closing, near(pointer));
                    }
                }
            }
            Phase::Closing => {
                let Some(pointer) = pointer else {
                    return;
                };
                if !contains(inset(self.target, 0.1), pointer) {
                    let start = self.quiet_anchor.or(previous_pointer).unwrap_or(pointer);
                    self.anchor = Some(start);
                    self.transition(
                        Phase::Pulling,
                        travel_view(start, pointer, near_height, ratio),
                    );
                } else if self.advance(dt, 0.68) {
                    self.phase = Phase::Parked;
                    self.anchor = Some(pointer);
                    self.pan_idle = 0.;
                }
            }
        }
    }

    fn transition(&mut self, phase: Phase, target: Rect) {
        self.phase = phase;
        self.origin = self.view;
        self.target = target;
        self.elapsed = 0.;
    }

    fn advance(&mut self, dt: f64, duration: f64) -> bool {
        self.elapsed += dt;
        let p = (self.elapsed / duration).min(1.);
        let t = p * p * (3. - 2. * p);
        let lerp = |from: f64, to: f64| from + (to - from) * t;
        self.view = Rect {
            x: lerp(self.origin.x, self.target.x),
            y: lerp(self.origin.y, self.target.y),
            width: lerp(self.origin.width, self.target.width),
            height: lerp(self.origin.height, self.target.height),
        };
        p >= 1.
    }

    pub fn blit(&self, surface: Rect, pixels: Size) -> Option<Blit> {
        self.bounds?;
        let view = self.view;
        let left = surface.x.max(view.x);
        let top = surface.y.max(view.y);
        let right = (surface.x + surface.width).min(view.x + view.width);
        let bottom = (surface.y + surface.height).min(view.y + view.height);
        if right <= left || bottom <= top {
            return None;
        }
        let sx = f64::from(pixels.width) / surface.width;
        let sy = f64::from(pixels.height) / surface.height;
        let scale = f64::from(self.output.height) / view.height;
        Some(Blit {
            source: Rect {
                x: (left - surface.x) * sx,
                y: (top - surface.y) * sy,
                width: (right - left) * sx,
                height: (bottom - top) * sy,
            },
            destination: Rect {
                x: (left - view.x) * scale,
                y: (top - view.y) * scale,
                width: (right - left) * scale,
                height: (bottom - top) * scale,
            },
        })
    }

    pub fn project(&self, p: Point) -> Option<Point> {
        self.bounds?;
        if !contains(self.view, p) {
            return None;
        }
        let scale = f64::from(self.output.height) / self.view.height;
        Some(Point {
            x: (p.x - self.view.x) * scale,
            y: (p.y - self.view.y) * scale,
        })
    }

    fn minimum_height(&self, bounds: Rect, ratio: f64) -> f64 {
        bounds.height.min(bounds.width / ratio) * self.close_fraction
    }

    /// The preview guide follows the pointer independently of the held camera.
    pub fn minimum_guide(&self, pointer: Point, active: Rect) -> Option<Rect> {
        self.bounds?;
        if self.mode != Mode::Follow {
            return None;
        }
        let ratio = f64::from(self.output.width) / f64::from(self.output.height);
        let scale = f64::from(self.output.height) / self.view.height;
        let height = self.minimum_height(active, ratio) * scale;
        let width = height * ratio;
        Some(Rect {
            x: (pointer.x - self.view.x) * scale - width / 2.,
            y: (pointer.y - self.view.y) * scale - height / 2.,
            width,
            height,
        })
    }
}

fn inset(rect: Rect, fraction: f64) -> Rect {
    Rect {
        x: rect.x + rect.width * fraction,
        y: rect.y + rect.height * fraction,
        width: rect.width * (1. - 2. * fraction),
        height: rect.height * (1. - 2. * fraction),
    }
}

fn centered_view(center: Point, height: f64, ratio: f64) -> Rect {
    let width = height * ratio;
    Rect {
        x: center.x - width / 2.,
        y: center.y - height / 2.,
        width,
        height,
    }
}

fn travel_view(start: Point, end: Point, minimum_height: f64, ratio: f64) -> Rect {
    let width = ((end.x - start.x).abs() * 1.2).floor();
    let height = ((end.y - start.y).abs() * 1.2).floor();
    let height = height.max(width / ratio).max(minimum_height);
    centered_view(
        Point {
            x: (start.x + end.x) / 2.,
            y: (start.y + end.y) / 2.,
        },
        height,
        ratio,
    )
}
pub fn contains(rect: Rect, point: Point) -> bool {
    point.x >= rect.x
        && point.x < rect.x + rect.width
        && point.y >= rect.y
        && point.y < rect.y + rect.height
}

fn distance(a: Point, b: Point) -> f64 {
    (a.x - b.x).hypot(a.y - b.y)
}

fn viewport(bounds: Rect, center: Point, height: f64, ratio: f64) -> Rect {
    let width = height * ratio;
    let start = |at: f64, span: f64, min: f64, extent: f64| {
        if span >= extent {
            min + (extent - span) / 2.
        } else {
            (at - span / 2.).clamp(min, min + extent - span)
        }
    };
    Rect {
        x: start(center.x, width, bounds.x, bounds.width),
        y: start(center.y, height, bounds.y, bounds.height),
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guide_centers_on_pointer_while_the_camera_pans_without_zooming() {
        let bounds = Rect {
            x: 0.,
            y: 0.,
            width: 1600.,
            height: 900.,
        };
        let active = Rect {
            x: 300.,
            y: 250.,
            width: 400.,
            height: 400.,
        };
        let mut director = Director::new(
            Size {
                width: 720,
                height: 1280,
            },
            Mode::Follow,
            70,
        );
        let initial = Point { x: 500., y: 450. };
        director.update_for_surface(Some(bounds), Some(initial), Some(active), 0.04);
        let held = director.view;
        let next = Point { x: 520., y: 460. };
        director.update_for_surface(Some(bounds), Some(next), Some(active), 0.04);
        assert_ne!(director.view.x, held.x);
        assert_eq!(director.view.width, held.width);
        assert_eq!(director.view.height, held.height);
        let guide = director.minimum_guide(next, active).unwrap();
        let center = director.project(next).unwrap();
        assert!((guide.x + guide.width / 2. - center.x).abs() < 1e-6);
        assert!((guide.y + guide.height / 2. - center.y).abs() < 1e-6);
    }

    #[test]
    fn crossing_the_eighty_percent_boundary_uses_the_travel_rectangle() {
        let bounds = Rect {
            x: 0.,
            y: 0.,
            width: 1600.,
            height: 900.,
        };
        let mut director = Director::new(
            Size {
                width: 720,
                height: 1280,
            },
            Mode::Follow,
            70,
        );
        let p = |x| Some(Point { x, y: 450. });
        director.update(Some(bounds), p(400.), 0.05);
        let initial = director.view;
        for x in [430., 390., 410., 440.] {
            director.update(Some(bounds), p(x), 0.05);
            assert_eq!(director.view.width, initial.width);
        }
        director.update(Some(bounds), p(1500.), 0.05);
        assert_eq!(director.target.width, 1320.);
        assert_eq!(director.target.x, 290.);
        for _ in 0..15 {
            director.update(Some(bounds), p(1500.), 0.05);
        }
        assert!(director.view.width > initial.width);
        assert!(director.view.width <= bounds.width);
        for _ in 0..40 {
            director.update(Some(bounds), p(1500.), 0.05);
        }
        assert!((director.view.width - initial.width).abs() < 1e-6);
        assert_eq!(director.phase == Phase::Parked, true);
    }

    #[test]
    fn wide_view_can_extend_past_sources_but_only_blits_source_pixels() {
        let source = Rect {
            x: 0.,
            y: 0.,
            width: 1000.,
            height: 500.,
        };
        let output = Size {
            width: 720,
            height: 720,
        };
        let mut director = Director::new(output, Mode::Follow, 70);
        let start = Point { x: 1., y: 250. };
        let end = Point { x: 999., y: 250. };
        director.update(Some(source), Some(start), 0.05);
        director.update(Some(source), Some(end), 0.05);
        assert_eq!(director.target.width, 1197.);
        assert!(director.target.x < 0.);
        for _ in 0..9 {
            director.update(Some(source), Some(end), 0.05);
        }
        let blit = director
            .blit(
                source,
                Size {
                    width: 1000,
                    height: 500,
                },
            )
            .unwrap();
        assert_eq!(blit.source.x, 0.);
        assert_eq!(blit.source.width, 1000.);
        assert!(blit.destination.x > 0.);
    }

    #[test]
    fn separate_surfaces_do_not_claim_the_gap() {
        let mut director = Director::new(
            Size {
                width: 900,
                height: 300,
            },
            Mode::Fit,
            70,
        );
        let left = Rect {
            x: -300.,
            y: 0.,
            width: 300.,
            height: 300.,
        };
        let right = Rect { x: 300., ..left };
        director.update(
            Some(Rect {
                x: -300.,
                y: 0.,
                width: 900.,
                height: 300.,
            }),
            None,
            0.0,
        );
        let pixels = Size {
            width: 300,
            height: 300,
        };
        let a = director.blit(left, pixels).unwrap().destination;
        let b = director.blit(right, pixels).unwrap().destination;
        assert_eq!(b.x - (a.x + a.width), 300.);
    }
}
