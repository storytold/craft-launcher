//! How far a mouse wheel notch scrolls (#386).
//!
//! egui scrolls `line_scroll_speed` points per wheel line: 40 on the desktop, 8 on the web. On
//! macOS the system already accelerates line deltas, and in a browser a line is scaled for the
//! web. But on Windows and Linux winit reports one line per notch, where browsers and most
//! other programs move about 100 pixels, so 40 felt slow. Wheel input reported in points (most
//! trackpads) keeps its own speed, and ⌘/Ctrl-wheel zoom keeps egui's rate.

use egui::{Event, MouseWheelUnit, RawInput};
use serde::{Deserialize, Serialize};

/// The user's choice, relative to the platform's normal speed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScrollSpeed {
    Slow,
    #[default]
    Normal,
    Fast,
}

/// egui's own `line_scroll_speed`, which its wheel zoom rate is tuned against.
const EGUI_LINE_POINTS: f32 = if cfg!(target_arch = "wasm32") { 8.0 } else { 40.0 };

/// Points one wheel line scrolls at Normal speed.
const NORMAL_LINE_POINTS: f32 = if cfg!(target_arch = "wasm32") {
    // The browser's line deltas, which egui already scales for the web.
    8.0
} else if cfg!(target_os = "macos") {
    40.0
} else {
    100.0
};

/// The catalog context of the speed names: they qualify "speed".
const CONTEXT: &str = "scroll speed";

impl ScrollSpeed {
    pub const ALL: [Self; 3] = [Self::Slow, Self::Normal, Self::Fast];

    /// The English name, the catalog key.
    pub fn key(self) -> &'static str {
        match self {
            Self::Slow => "Slow",
            Self::Normal => "Normal",
            Self::Fast => "Fast",
        }
    }

    /// The name in the language the UI is drawn in.
    pub fn label(self) -> &'static str {
        crate::i18n::tr_ctx(crate::i18n::current(), CONTEXT, self.key())
    }

    /// Points one wheel line scrolls.
    pub fn line_points(self) -> f32 {
        NORMAL_LINE_POINTS
            * match self {
                Self::Slow => 0.5,
                Self::Normal => 1.0,
                Self::Fast => 2.0,
            }
    }

    /// Makes egui scroll this far per wheel line, from this frame's input on (call it before egui
    /// reads `raw`). egui scales line deltas before turning ⌘/Ctrl-wheel motion into zoom, so
    /// those events are scaled back to keep zooming at egui's own rate.
    pub(crate) fn apply(self, ctx: &egui::Context, raw: &mut RawInput) {
        self.set_line_speed(ctx);
        let zoom_modifier = ctx.options(|o| o.input_options.zoom_modifier);
        let zoom_scale = EGUI_LINE_POINTS / self.line_points();
        for event in &mut raw.events {
            if let Event::MouseWheel { unit: MouseWheelUnit::Line, delta, modifiers, .. } = event
                && modifiers.matches_any(zoom_modifier)
            {
                *delta *= zoom_scale;
            }
        }
    }

    /// Makes egui scroll this far per wheel line (from the next frame when called during one).
    pub(crate) fn set_line_speed(self, ctx: &egui::Context) {
        let points = self.line_points();
        if ctx.options(|o| o.input_options.line_scroll_speed) != points {
            ctx.options_mut(|o| o.input_options.line_scroll_speed = points);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wheel(unit: MouseWheelUnit, dy: f32, command: bool) -> Event {
        let modifiers = egui::Modifiers { command, ..egui::Modifiers::NONE };
        Event::MouseWheel { unit, delta: egui::vec2(0.0, dy), phase: egui::TouchPhase::Move, modifiers }
    }

    #[test]
    fn scrolling_follows_the_choice_and_zoom_keeps_egui_rate() {
        let ctx = egui::Context::default();
        for speed in ScrollSpeed::ALL {
            let mut raw = RawInput {
                events: vec![
                    wheel(MouseWheelUnit::Line, 1.0, true),
                    wheel(MouseWheelUnit::Line, 1.0, false),
                    wheel(MouseWheelUnit::Point, 30.0, true),
                ],
                ..Default::default()
            };
            speed.apply(&ctx, &mut raw);
            assert_eq!(ctx.options(|o| o.input_options.line_scroll_speed), speed.line_points());
            let dy: Vec<f32> = raw.events.iter().filter_map(|e| if let Event::MouseWheel { delta, .. } = e { Some(delta.y) } else { None }).collect();
            // A ⌘/Ctrl line is scaled back so egui's zoom sees its own points per line; a plain
            // line and a point delta are untouched.
            assert!((dy[0] * speed.line_points() - EGUI_LINE_POINTS).abs() < 1e-3, "{speed:?}: {dy:?}");
            assert_eq!((dy[1], dy[2]), (1.0, 30.0));
        }
        assert_eq!(ScrollSpeed::Slow.line_points() * 4.0, ScrollSpeed::Fast.line_points());
    }

    #[test]
    fn normal_is_faster_than_egui_only_where_a_notch_is_one_line() {
        let normal = ScrollSpeed::Normal.line_points();
        if cfg!(any(target_os = "windows", target_os = "linux")) {
            assert!(normal > EGUI_LINE_POINTS, "{normal}");
        } else {
            assert_eq!(normal, EGUI_LINE_POINTS);
        }
    }
}
