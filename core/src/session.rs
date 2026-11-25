use crate::{Camera, FramePlan, Mode, Point, Size};

/// IDs are opaque to the core and must be assigned by the platform backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureSource {
    Window(String),
    Display(String),
}

#[derive(Clone, Copy, Debug)]
pub struct SourceGeometry<'a> {
    pub source: &'a CaptureSource,
    pub size: Size,
    pub pointer: Option<Point>,
}

/// Owns the immutable capture selection for one recording.
pub struct FramingSession {
    source: CaptureSource,
    camera: Camera,
}

impl FramingSession {
    pub fn new(source: CaptureSource, output: Size, mode: Mode) -> Option<Self> {
        Some(Self {
            source,
            camera: Camera::new(output, mode)?,
        })
    }

    pub fn source(&self) -> &CaptureSource {
        &self.source
    }

    /// Use geometry from the same verified capture surface as the pixels to be
    /// rendered. Missing or mismatched surfaces are rendered as background.
    pub fn frame(&mut self, geometry: Option<SourceGeometry<'_>>, elapsed: f64) -> FramePlan {
        match geometry.filter(|frame| frame.source == &self.source) {
            Some(frame) => self.camera.frame(Some(frame.size), frame.pointer, elapsed),
            None => self.camera.frame(None, None, elapsed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_other_windows_and_whole_display_for_a_window_session() {
        let window = CaptureSource::Window("selected".into());
        let output = Size {
            width: 720,
            height: 1280,
        };
        let mut session = FramingSession::new(window.clone(), output, Mode::Fit).unwrap();
        for (source, expected) in [
            (window, true),
            (CaptureSource::Window("other".into()), false),
            (CaptureSource::Display("selected".into()), false),
        ] {
            let frame = SourceGeometry {
                source: &source,
                size: output,
                pointer: None,
            };
            assert_eq!(session.frame(Some(frame), 0.0).blit.is_some(), expected);
        }
    }
}
