//! Fewer animations when the OS asks for that: Windows' "Animation effects" switched off, macOS'
//! "Reduce motion", a Linux desktop's animations setting, a browser's `prefers-reduced-motion`.
//! The shell says what the OS wants ([`crate::Host::reduce_motion`]); this makes egui follow it.
//! Progress bars and spinners keep moving: they say that work is going on.

use egui::{Event, MouseWheelUnit, RawInput, TouchPhase};

/// Seconds between asking the shell again: the setting can change while the launcher runs.
pub(crate) const RECHECK_EVERY: f64 = 1.0;

/// With `reduce`, popups, tooltips and dialogs appear at once instead of fading in, and scrolling
/// to a place jumps there. Without it, egui's own timings.
pub(crate) fn apply(ctx: &egui::Context, reduce: bool) {
    let stock = egui::Style::default();
    ctx.all_styles_mut(|style| {
        style.animation_time = if reduce { 0.0 } else { stock.animation_time };
        style.scroll_animation = if reduce { egui::style::ScrollAnimation::none() } else { stock.scroll_animation };
    });
}

/// The most one step of a rewritten wheel event moves, in points: under what egui takes for a
/// trackpad's step (8), also once Shift has folded both axes into one.
const WHEEL_STEP: f32 = 4.0;
/// A wheel event that would take more steps than this is left as it came.
const WHEEL_MAX_STEPS: f32 = 4096.0;

/// Makes a mouse wheel scroll at once. egui spreads each notch of a wheel over several frames and
/// has no switch for that, but it applies a trackpad's small steps as they come: so every wheel
/// event is rewritten as a run of such steps, adding up to the same distance.
pub(crate) fn unsmooth_wheel(ctx: &egui::Context, raw: &mut RawInput) {
    let wheel = |event: &Event| matches!(event, Event::MouseWheel { phase: TouchPhase::Move, .. });
    if !raw.events.iter().any(wheel) {
        return;
    }
    let line = ctx.options(|o| o.input_options.line_scroll_speed);
    let page = raw.screen_rect.unwrap_or_else(|| ctx.viewport_rect()).height();
    let mut events = Vec::with_capacity(raw.events.len());
    for event in std::mem::take(&mut raw.events) {
        let Event::MouseWheel { unit, delta, phase: TouchPhase::Move, modifiers, source } = &event else {
            events.push(event);
            continue;
        };
        let points = match unit {
            MouseWheelUnit::Point => *delta,
            MouseWheelUnit::Line => line * *delta,
            MouseWheelUnit::Page => page * *delta,
        };
        let steps = ((points.x.abs() + points.y.abs()) / WHEEL_STEP).ceil();
        // Nothing to move, or nonsense (NaN included): egui's to deal with.
        if !(1.0..=WHEEL_MAX_STEPS).contains(&steps) {
            events.push(event);
            continue;
        }
        let step =
            Event::MouseWheel { unit: MouseWheelUnit::Point, delta: points / steps, phase: TouchPhase::Move, modifiers: *modifiers, source: *source };
        events.extend(std::iter::repeat_n(step, steps as usize));
    }
    raw.events = events;
}

#[cfg(test)]
mod tests {
    use egui::MouseWheelSource;
    use egui::{Modifiers, Vec2, vec2};

    use super::*;

    const FRAME: f64 = 1.0 / 60.0;

    fn wheel(unit: MouseWheelUnit, delta: Vec2, modifiers: Modifiers) -> Event {
        Event::MouseWheel { unit, delta, phase: TouchPhase::Move, modifiers, source: MouseWheelSource::Wheel }
    }

    /// One frame: how far egui scrolls in it.
    fn scrolled(ctx: &egui::Context, frame: u32, events: Vec<Event>, reduce: bool) -> Vec2 {
        let mut raw = RawInput { time: Some(f64::from(frame) * FRAME), events, ..Default::default() };
        if reduce {
            unsmooth_wheel(ctx, &mut raw);
        }
        let mut seen = Vec2::ZERO;
        ctx.run_ui(raw, |ui| seen = ui.input(|i| i.smooth_scroll_delta)).drop_without_applying_deltas();
        seen
    }

    fn close(a: Vec2, b: Vec2) -> bool {
        (a - b).length() < 0.01
    }

    #[test]
    fn egui_spreads_a_wheel_notch_over_frames() {
        let ctx = egui::Context::default();
        let line = ctx.options(|o| o.input_options.line_scroll_speed);
        let first = scrolled(&ctx, 0, vec![wheel(MouseWheelUnit::Line, vec2(0.0, -3.0), Modifiers::NONE)], false);
        assert!(first.y < 0.0 && first.y > -1.5 * line, "{first:?}");
        assert!(scrolled(&ctx, 1, Vec::new(), false).y < 0.0);
    }

    #[test]
    fn a_wheel_notch_scrolls_at_once() {
        let ctx = egui::Context::default();
        let line = ctx.options(|o| o.input_options.line_scroll_speed);
        let first = scrolled(&ctx, 0, vec![wheel(MouseWheelUnit::Line, vec2(0.0, -3.0), Modifiers::NONE)], true);
        assert!(close(first, vec2(0.0, -3.0 * line)), "{first:?}");
        assert_eq!(scrolled(&ctx, 1, Vec::new(), true), Vec2::ZERO);
    }

    #[test]
    fn a_big_step_in_points_scrolls_at_once_too() {
        let ctx = egui::Context::default();
        let first = scrolled(&ctx, 0, vec![wheel(MouseWheelUnit::Point, vec2(30.0, -90.0), Modifiers::NONE)], true);
        assert!(close(first, vec2(30.0, -90.0)), "{first:?}");
        assert_eq!(scrolled(&ctx, 1, Vec::new(), true), Vec2::ZERO);
    }

    #[test]
    fn shift_scrolls_sideways_at_once() {
        let ctx = egui::Context::default();
        let line = ctx.options(|o| o.input_options.line_scroll_speed);
        // Both axes at once: Shift adds them up, which must still pass for a trackpad's step.
        let first = scrolled(&ctx, 0, vec![wheel(MouseWheelUnit::Line, vec2(1.0, 2.0), Modifiers::SHIFT)], true);
        assert!(close(first, vec2(3.0 * line, 0.0)), "{first:?}");
        assert_eq!(scrolled(&ctx, 1, Vec::new(), true), Vec2::ZERO);
    }

    #[test]
    fn other_input_is_left_alone() {
        let ctx = egui::Context::default();
        let events = vec![
            Event::PointerMoved(egui::pos2(10.0, 10.0)),
            wheel(MouseWheelUnit::Line, Vec2::ZERO, Modifiers::NONE),
            wheel(MouseWheelUnit::Line, vec2(0.0, f32::NAN), Modifiers::NONE),
            wheel(MouseWheelUnit::Page, vec2(f32::INFINITY, 0.0), Modifiers::NONE),
            wheel(MouseWheelUnit::Point, vec2(0.0, 1.0e9), Modifiers::NONE),
            Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta: vec2(0.0, 40.0),
                phase: TouchPhase::Start,
                modifiers: Modifiers::NONE,
                source: MouseWheelSource::Unknown,
            },
        ];
        let mut raw = RawInput { events: events.clone(), ..Default::default() };
        unsmooth_wheel(&ctx, &mut raw);
        // NaN is not equal to itself: compare what the events look like.
        assert_eq!(format!("{:?}", raw.events), format!("{events:?}"));
    }

    #[test]
    fn every_step_keeps_the_wheel_source() {
        let ctx = egui::Context::default();
        for source in [MouseWheelSource::Wheel, MouseWheelSource::Trackpad, MouseWheelSource::Momentum, MouseWheelSource::Unknown] {
            let event =
                Event::MouseWheel { unit: MouseWheelUnit::Line, delta: vec2(0.0, -3.0), phase: TouchPhase::Move, modifiers: Modifiers::NONE, source };
            let mut raw = RawInput { events: vec![event], ..Default::default() };
            unsmooth_wheel(&ctx, &mut raw);
            assert!(raw.events.len() > 1, "{source:?} was not split");
            assert!(
                raw.events.iter().all(|event| matches!(event, Event::MouseWheel { source: kept, .. } if *kept == source)),
                "{source:?} was not preserved: {:?}",
                raw.events
            );
        }
    }

    #[test]
    fn the_styles_follow_the_setting() {
        let ctx = egui::Context::default();
        let stock = egui::Style::default();
        apply(&ctx, true);
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            assert_eq!(ctx.style_of(theme).animation_time, 0.0);
            assert_eq!(ctx.style_of(theme).scroll_animation, egui::style::ScrollAnimation::none());
        }
        apply(&ctx, false);
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            assert_eq!(ctx.style_of(theme).animation_time, stock.animation_time);
            assert_eq!(ctx.style_of(theme).scroll_animation, stock.scroll_animation);
        }
    }
}
