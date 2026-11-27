use crate::{Blit, Mode, Point, Rect, Size};

pub struct Director {
    output: Size,
    mode: Mode,
    close_fraction: f64,
    follow_fraction: f64,
    bounds: Option<Rect>,
    view: Rect,
    origin: Rect,
    anchor: Option<Point>,
    previous: Option<Point>,
    focused: bool,
    idle: f64,
    elapsed: f64,
}

impl Director {
    pub fn set_output_size(&mut self, size: Size) {
        debug_assert_eq!(
            u64::from(size.width) * u64::from(self.output.height),
            u64::from(size.height) * u64::from(self.output.width)
        );
        self.output = size;
    }

    pub fn new(output: Size, mode: Mode, close_percent: u32, follow_percent: u32) -> Self {
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
            follow_fraction: f64::from(follow_percent.clamp(25, 100)) / 100.,
            bounds: None,
            view: empty,
            origin: empty,
            anchor: None,
            previous: None,
            focused: false,
            idle: 0.,
            elapsed: 0.,
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
        self.update_with_buttons(bounds, pointer, active, dt, false);
    }

    /// Both preview and recording call this with the same desktop input.
    pub fn update_with_buttons(&mut self, bounds: Option<Rect>, pointer: Option<Point>,
        active: Option<Rect>, dt: f64, button_action: bool) {
        let Some(bounds) = bounds else { self.bounds = None; return; };
        let dt = if dt.is_finite() { dt.max(0.) } else { 0. };
        let ratio = f64::from(self.output.width) / f64::from(self.output.height);
        let mid = Point { x: bounds.x + bounds.width / 2., y: bounds.y + bounds.height / 2. };
        let overview = centered_view(mid, bounds.height.max(bounds.width / ratio), ratio);
        let source = active.unwrap_or(bounds);
        let height = self.minimum_height(source, ratio);
        if self.bounds != Some(bounds) {
            self.bounds = Some(bounds);
            self.view = overview;
            self.origin = overview;
            self.focused = false;
            self.anchor = pointer;
            self.previous = None;
            self.idle = 1.;
            self.elapsed = 1.;
        }
        if self.mode == Mode::Fit { self.view = overview; return; }
        self.idle += dt;
        let moved = pointer.is_some() && self.previous.is_some() && pointer != self.previous;
        if let Some(p) = pointer {
            if self.previous.is_none() || self.idle >= 1. { self.anchor = Some(self.previous.unwrap_or(p)); }
            if moved || button_action { self.idle = 0.; }
        }
        let crossed = pointer.zip(self.anchor).is_some_and(|(p, anchor)| {
            let r = self.follow_range(anchor, source);
            p.x < r.x || p.x > r.x+r.width || p.y < r.y || p.y > r.y+r.height
        });
        let mut focused = self.focused || (pointer.is_some() && (button_action || (moved && crossed)));
        if self.idle >= 1. { focused = false; self.anchor = pointer; }
        if focused != self.focused {
            self.origin = self.view;
            self.focused = focused;
            self.elapsed = 0.;
        } else { self.elapsed += dt; }
        let target = if focused { centered_view(pointer.or(self.previous).unwrap_or(mid), height, ratio) } else { overview };
        let t = (self.elapsed / if focused { 0.60 } else { 0.42 }).min(1.);
        let k = t*t*(3.-2.*t);
        let mix = |a:f64,b:f64| a+(b-a)*k;
        self.view = Rect { x:mix(self.origin.x,target.x),y:mix(self.origin.y,target.y),
            width:mix(self.origin.width,target.width),height:mix(self.origin.height,target.height) };
        self.previous = pointer.or(if focused { self.previous } else { None });
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

    fn follow_range(&self, center: Point, source: Rect) -> Rect {
        let width = source.width * self.follow_fraction;
        let height = source.height * self.follow_fraction;
        Rect {
            x: center.x - width / 2.,
            y: center.y - height / 2.,
            width,
            height,
        }
    }

    /// The preview guide shares the source-relative movement threshold.
    pub fn follow_guide(&self, pointer: Point, active: Rect) -> Option<Rect> {
        self.bounds?;
        if self.mode != Mode::Follow {
            return None;
        }
        let scale = f64::from(self.output.height) / self.view.height;
        let pointer = if self.focused { pointer } else { self.anchor.unwrap_or(pointer) };
        let range = self.follow_range(pointer, active);
        Some(Rect {
            x: (range.x - self.view.x) * scale,
            y: (range.y - self.view.y) * scale,
            width: range.width * scale,
            height: range.height * scale,
        })
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

pub fn contains(rect: Rect, point: Point) -> bool {
    point.x >= rect.x
        && point.x < rect.x + rect.width
        && point.y >= rect.y
        && point.y < rect.y + rect.height
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_hold_and_idle_use_mac_timings() {
        let bounds = Rect { x:0., y:0., width:1600., height:900. };
        let p = Some(Point {x:800.,y:450.});
        let mut d = Director::new(Size {width:100,height:100},Mode::Follow,25,70);
        d.update(Some(bounds),p,0.);
        assert_eq!(d.view.height,1600.);
        d.update_with_buttons(Some(bounds),p,None,0.,true);
        d.update(Some(bounds),p,0.30);
        assert_eq!(d.view.height,912.5);
        d.update(Some(bounds),p,0.30);
        assert_eq!(d.view.height,225.);
        d.update_with_buttons(Some(bounds),p,None,2.,true);
        d.update(Some(bounds),p,0.99);
        assert_eq!(d.view.height,225.);
        d.update(Some(bounds),p,0.02);
        d.update(Some(bounds),p,0.42);
        assert_eq!(d.view.height,1600.);
    }

    #[test]
    fn guide_stays_at_rest_anchor_until_crossed() {
        let bounds = Rect {x:0.,y:0.,width:1600.,height:900.};
        let mut d=Director::new(Size{width:900,height:900},Mode::Follow,25,50);
        let p=|x| Some(Point{x,y:450.});
        d.update(Some(bounds),p(800.),0.);
        let guide=d.follow_guide(p(800.).unwrap(),bounds);
        let rect = guide.unwrap();
        assert_eq!(rect.width / rect.height, 16. / 9.);
        d.update(Some(bounds),p(1200.),0.05);
        assert!(!d.focused);
        assert_eq!(guide,d.follow_guide(p(1200.).unwrap(),bounds));
        d.update(Some(bounds),p(1201.),0.05);
        assert!(d.focused);
        d.update_with_buttons(Some(bounds),p(1201.),None,0.60,true);
        assert_eq!(d.view.height,225.);
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
