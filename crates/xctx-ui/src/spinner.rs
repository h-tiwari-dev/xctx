//! Self-contained [`Spinner`] GPUI entity for running activity indicators.
//!
//! Per Performance Rule 4 in `docs/m6.md`, `Spinner` is its own GPUI entity so each animation
//! frame notifies only the spinner view and never re-renders the parent workspace or pane tree.
//! When reduced motion is enabled via [`MotionSettings`] or [`gpui::App::reduce_motion`], the
//! spinner renders statically and schedules zero animation frames.

use std::f32::consts::TAU;
use std::time::{Duration, Instant};

use gpui::{Context, Hsla, Radians, Window, prelude::*, radians};

use crate::icon::{Icon, IconName, IconSize};
use crate::motion::{MotionSettings, SPINNER_PERIOD};
use crate::theme::Tone;

/// Compute the rotation angle of a spinner at `elapsed` within `period`.
///
/// Returns `0` radians when `reduced_motion` is `true` or `period` is zero.
pub fn spinner_angle(elapsed: Duration, period: Duration, reduced_motion: bool) -> Radians {
    if reduced_motion || period.is_zero() {
        return radians(0.0);
    }
    let period_nanos = period.as_nanos();
    let phase = (elapsed.as_nanos() % period_nanos) as f64 / period_nanos as f64;
    radians((phase as f32) * TAU)
}

/// An isolated GPUI entity displaying a rotating activity indicator.
///
/// Construct with `cx.new(|_| Spinner::new())` and mount the `Entity<Spinner>` directly inside a
/// parent view. Ticking calls `window.request_animation_frame()`, which only invalidates this
/// entity.
pub struct Spinner {
    size: IconSize,
    color: Option<Hsla>,
    tone: Tone,
    active: bool,
    period: Duration,
    started_at: Instant,
}

impl Default for Spinner {
    fn default() -> Self {
        Self::new()
    }
}

impl Spinner {
    /// Create an active [`IconSize::Sm`] spinner tinted with [`Tone::Accent`].
    pub fn new() -> Self {
        Self {
            size: IconSize::Sm,
            color: None,
            tone: Tone::Accent,
            active: true,
            period: SPINNER_PERIOD,
            started_at: Instant::now(),
        }
    }

    /// Set the icon size.
    pub fn size(mut self, size: IconSize) -> Self {
        self.size = size;
        self
    }

    /// Override the colour explicitly.
    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }

    /// Set the semantic tone (used when no explicit [`Spinner::color`] is set).
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }

    /// Set whether the spinner starts active (spinning) or idle (static).
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    /// Set a custom revolution period.
    pub fn period(mut self, period: Duration) -> Self {
        self.period = period;
        self
    }

    /// Whether the spinner is currently marked active.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Toggle whether the spinner is active, notifying only this entity.
    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.active != active {
            self.active = active;
            if active {
                self.started_at = Instant::now();
            }
            cx.notify();
        }
    }

    /// Update the spinner's semantic tone and repaint this entity.
    pub fn set_tone(&mut self, tone: Tone, cx: &mut Context<Self>) {
        if self.tone != tone {
            self.tone = tone;
            cx.notify();
        }
    }

    /// Update the spinner's explicit colour override and repaint this entity.
    pub fn set_color(&mut self, color: Option<Hsla>, cx: &mut Context<Self>) {
        if self.color != color {
            self.color = color;
            cx.notify();
        }
    }

    /// Update the spinner's size and repaint this entity.
    pub fn set_size(&mut self, size: IconSize, cx: &mut Context<Self>) {
        if self.size != size {
            self.size = size;
            cx.notify();
        }
    }
}

impl Render for Spinner {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let reduced = MotionSettings::is_reduced(cx);
        let (icon_name, angle) = if !self.active {
            (IconName::Idle, None)
        } else if reduced {
            // Static running glyph when reduced motion is enabled; schedule no frames.
            (IconName::Running, None)
        } else {
            let angle = spinner_angle(self.started_at.elapsed(), self.period, false);
            window.request_animation_frame();
            (IconName::Running, Some(angle))
        };

        let mut icon = Icon::new(icon_name).size(self.size).tone(self.tone);
        if let Some(color) = self.color {
            icon = icon.color(color);
        }
        if let Some(angle) = angle {
            icon = icon.rotation(angle);
        }
        icon
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spinner_angle_wraps_and_stops_for_reduced_motion() {
        let period = Duration::from_millis(1000);
        assert_eq!(
            spinner_angle(Duration::from_millis(250), period, true),
            radians(0.0)
        );
        assert_eq!(
            spinner_angle(Duration::from_millis(250), Duration::ZERO, false),
            radians(0.0)
        );

        let quarter: f32 = spinner_angle(Duration::from_millis(250), period, false).0;
        assert!((quarter - TAU * 0.25).abs() < 1e-5);

        let wrapped: f32 = spinner_angle(Duration::from_millis(1250), period, false).0;
        assert!((wrapped - TAU * 0.25).abs() < 1e-5);
    }
}
