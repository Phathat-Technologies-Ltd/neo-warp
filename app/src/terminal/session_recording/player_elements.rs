//! Elements for the recording player: the scrubbable timeline and the scale-to-fit screen.

use std::cell::Cell;
use std::rc::Rc;

use pathfinder_color::ColorU;
use warpui::elements::{CornerRadius, Point, Radius};
use warpui::event::DispatchedEvent;
use warpui::geometry::rect::RectF;
use warpui::geometry::vector::{Vector2F, vec2f};
use warpui::{
    AfterLayoutContext, AppContext, Element, Event, EventContext, LayoutContext, PaintContext,
    SizeConstraint,
};

const TIMELINE_HEIGHT: f32 = 18.;
const TRACK_HEIGHT: f32 = 4.;
const TICK_WIDTH: f32 = 2.;
const TICK_HEIGHT: f32 = 12.;
const THUMB_SIZE: f32 = 12.;
/// A press within this many pixels of a marker tick jumps exactly to the marker.
const TICK_HIT_SLOP: f32 = 5.;

type SeekCallback = Box<dyn Fn(&mut EventContext, f64)>;

#[derive(Clone, Copy)]
pub struct TimelineColors {
    pub track: ColorU,
    pub played: ColorU,
    pub tick: ColorU,
    pub thumb: ColorU,
}

/// A full-width playback timeline. Pressing or dragging on it seeks; marker ticks snap.
pub struct Timeline {
    position: f64,
    duration: f64,
    markers: Vec<f64>,
    colors: TimelineColors,
    /// Whether a drag that started on the timeline is in progress. Shared across renders.
    dragging: Rc<Cell<bool>>,
    on_seek: SeekCallback,
    size: Option<Vector2F>,
    origin: Option<Point>,
}

impl Timeline {
    pub fn new(
        position: f64,
        duration: f64,
        markers: Vec<f64>,
        colors: TimelineColors,
        dragging: Rc<Cell<bool>>,
        on_seek: impl Fn(&mut EventContext, f64) + 'static,
    ) -> Self {
        Self {
            position,
            duration,
            markers,
            colors,
            dragging,
            on_seek: Box::new(on_seek),
            size: None,
            origin: None,
        }
    }

    /// Usable track span in pixels, inset so the thumb stays inside the element at both ends.
    fn track_span(&self) -> Option<(f32, f32)> {
        let origin = self.origin?.xy();
        let size = self.size?;
        let start = origin.x() + THUMB_SIZE / 2.;
        Some((start, (size.x() - THUMB_SIZE).max(1.)))
    }

    fn x_of(&self, time: f64) -> Option<f32> {
        let (start, width) = self.track_span()?;
        Some(start + width * fraction(time, self.duration))
    }

    fn time_at(&self, x: f32) -> Option<f64> {
        let (start, width) = self.track_span()?;
        let fraction = ((x - start) / width).clamp(0., 1.);
        Some(f64::from(fraction) * self.duration)
    }

    /// The marker under `x`, if any, else the time at `x`.
    fn seek_target(&self, x: f32) -> Option<f64> {
        let snapped = self
            .markers
            .iter()
            .filter_map(|time| Some((*time, (self.x_of(*time)? - x).abs())))
            .filter(|(_, distance)| *distance <= TICK_HIT_SLOP)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(time, _)| time);
        snapped.or_else(|| self.time_at(x))
    }
}

fn fraction(time: f64, duration: f64) -> f32 {
    if duration <= 0. {
        0.
    } else {
        (time / duration).clamp(0., 1.) as f32
    }
}

impl Element for Timeline {
    fn layout(
        &mut self,
        constraint: SizeConstraint,
        _ctx: &mut LayoutContext,
        _app: &AppContext,
    ) -> Vector2F {
        let size = vec2f(
            constraint.max.x(),
            TIMELINE_HEIGHT.clamp(constraint.min.y(), constraint.max.y()),
        );
        self.size = Some(size);
        size
    }

    fn after_layout(&mut self, _ctx: &mut AfterLayoutContext, _app: &AppContext) {}

    fn paint(&mut self, origin: Vector2F, ctx: &mut PaintContext, _app: &AppContext) {
        self.origin = Some(Point::from_vec2f(origin, ctx.scene.z_index()));
        let Some(size) = self.size else {
            return;
        };
        ctx.scene
            .draw_rect_with_hit_recording(RectF::new(origin, size));
        let Some((start, width)) = self.track_span() else {
            return;
        };
        let center_y = origin.y() + size.y() / 2.;
        let rounded = CornerRadius::with_all(Radius::Pixels(TRACK_HEIGHT / 2.));
        let track_origin = vec2f(start, center_y - TRACK_HEIGHT / 2.);
        ctx.scene
            .draw_rect_without_hit_recording(RectF::new(track_origin, vec2f(width, TRACK_HEIGHT)))
            .with_background(self.colors.track)
            .with_corner_radius(rounded);
        let played = width * fraction(self.position, self.duration);
        if played > 0. {
            ctx.scene
                .draw_rect_without_hit_recording(RectF::new(
                    track_origin,
                    vec2f(played, TRACK_HEIGHT),
                ))
                .with_background(self.colors.played)
                .with_corner_radius(rounded);
        }
        for time in &self.markers {
            let x = start + width * fraction(*time, self.duration);
            ctx.scene
                .draw_rect_without_hit_recording(RectF::new(
                    vec2f(x - TICK_WIDTH / 2., center_y - TICK_HEIGHT / 2.),
                    vec2f(TICK_WIDTH, TICK_HEIGHT),
                ))
                .with_background(self.colors.tick);
        }
        ctx.scene
            .draw_rect_without_hit_recording(RectF::new(
                vec2f(start + played - THUMB_SIZE / 2., center_y - THUMB_SIZE / 2.),
                vec2f(THUMB_SIZE, THUMB_SIZE),
            ))
            .with_background(self.colors.thumb)
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(THUMB_SIZE / 2.)));
    }

    fn size(&self) -> Option<Vector2F> {
        self.size
    }

    fn origin(&self) -> Option<Point> {
        self.origin
    }

    fn dispatch_event(
        &mut self,
        event: &DispatchedEvent,
        ctx: &mut EventContext,
        _app: &AppContext,
    ) -> bool {
        let Some(z_index) = self.z_index() else {
            return false;
        };
        match event.at_z_index(z_index, ctx) {
            Some(Event::LeftMouseDown { position, .. }) => {
                if !self
                    .bounds()
                    .is_some_and(|bounds| bounds.contains_point(*position))
                {
                    return false;
                }
                if let Some(time) = self.seek_target(position.x()) {
                    self.dragging.set(true);
                    (self.on_seek)(ctx, time);
                    return true;
                }
                false
            }
            Some(Event::LeftMouseDragged { position, .. }) if self.dragging.get() => {
                if let Some(time) = self.time_at(position.x()) {
                    (self.on_seek)(ctx, time);
                }
                true
            }
            Some(Event::LeftMouseUp { .. }) if self.dragging.get() => {
                self.dragging.set(false);
                true
            }
            _ => false,
        }
    }
}

/// Lays out a terminal screen at the font size that fits it in the available space, centered
/// horizontally. The child is built once at a reference size to measure it, then at the fitted
/// size, which works because monospace text scales linearly with the font size.
pub struct FitScreen {
    build: Box<dyn Fn(f32) -> Box<dyn Element>>,
    base_font_size: f32,
    min_font_size: f32,
    max_font_size: f32,
    child: Option<Box<dyn Element>>,
    offset_x: f32,
    size: Option<Vector2F>,
    origin: Option<Point>,
}

/// Bounds measurement layouts so unbounded constraints don't produce infinite sizes.
const MEASURE_LIMIT: f32 = 100_000.;

impl FitScreen {
    pub fn new(
        base_font_size: f32,
        min_font_size: f32,
        max_font_size: f32,
        build: impl Fn(f32) -> Box<dyn Element> + 'static,
    ) -> Self {
        Self {
            build: Box::new(build),
            base_font_size,
            min_font_size,
            max_font_size,
            child: None,
            offset_x: 0.,
            size: None,
            origin: None,
        }
    }
}

/// The font size that makes content measured as `measured` at `base` fit in `available`.
pub fn fitted_font_size(
    base: f32,
    measured: Vector2F,
    available: Vector2F,
    min_font_size: f32,
    max_font_size: f32,
) -> f32 {
    let scale = |available: f32, measured: f32| {
        if available.is_finite() && measured > 0. {
            available / measured
        } else {
            f32::INFINITY
        }
    };
    let factor = scale(available.x(), measured.x()).min(scale(available.y(), measured.y()));
    if factor.is_finite() {
        (base * factor).clamp(min_font_size, max_font_size)
    } else {
        base.clamp(min_font_size, max_font_size)
    }
}

impl Element for FitScreen {
    fn layout(
        &mut self,
        constraint: SizeConstraint,
        ctx: &mut LayoutContext,
        app: &AppContext,
    ) -> Vector2F {
        let unbounded = SizeConstraint::new(Vector2F::zero(), vec2f(MEASURE_LIMIT, MEASURE_LIMIT));
        let measured = (self.build)(self.base_font_size).layout(unbounded, ctx, app);
        let font_size = fitted_font_size(
            self.base_font_size,
            measured,
            constraint.max,
            self.min_font_size,
            self.max_font_size,
        );
        let mut child = (self.build)(font_size);
        let child_size = child.layout(unbounded, ctx, app);
        self.child = Some(child);
        let width = if constraint.max.x().is_finite() {
            constraint.max.x().max(child_size.x())
        } else {
            child_size.x()
        };
        self.offset_x = ((width - child_size.x()) / 2.).max(0.);
        let size = vec2f(width, child_size.y().max(constraint.min.y()));
        self.size = Some(size);
        size
    }

    fn after_layout(&mut self, ctx: &mut AfterLayoutContext, app: &AppContext) {
        if let Some(child) = self.child.as_mut() {
            child.after_layout(ctx, app);
        }
    }

    fn paint(&mut self, origin: Vector2F, ctx: &mut PaintContext, app: &AppContext) {
        self.origin = Some(Point::from_vec2f(origin, ctx.scene.z_index()));
        if let Some(child) = self.child.as_mut() {
            child.paint(origin + vec2f(self.offset_x, 0.), ctx, app);
        }
    }

    fn size(&self) -> Option<Vector2F> {
        self.size
    }

    fn origin(&self) -> Option<Point> {
        self.origin
    }

    fn dispatch_event(
        &mut self,
        event: &DispatchedEvent,
        ctx: &mut EventContext,
        app: &AppContext,
    ) -> bool {
        self.child
            .as_mut()
            .is_some_and(|child| child.dispatch_event(event, ctx, app))
    }
}

#[cfg(test)]
#[path = "player_elements_tests.rs"]
mod tests;
