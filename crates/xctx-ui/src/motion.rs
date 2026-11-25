//! Motion constants, easing functions, opacity/offset interpolation helpers, and the
//! [`MotionSettings`] GPUI global.
//!
//! All transitions follow Performance Rule 5 from `docs/m6.md`:
//! - Opacity and offset only.
//! - At most [`MAX_DURATION_MS`] (160ms); overlays use [`OVERLAY_DURATION_MS`] (140ms).
//! - Never delays input: interactive handlers are active from frame 0.
//! - Disabled immediately when reduced motion is enabled.

use std::time::Duration;

use gpui::{App, Global, Pixels, px};

/// Default fade + slide duration for overlays and toasts, in milliseconds.
pub const OVERLAY_DURATION_MS: u64 = 140;

/// Maximum allowed duration for any UI transition, in milliseconds (Performance Rule 5).
pub const MAX_DURATION_MS: u64 = 160;

/// Default overlay transition duration ([`OVERLAY_DURATION_MS`]).
pub const OVERLAY_DURATION: Duration = Duration::from_millis(OVERLAY_DURATION_MS);

/// Maximum transition duration ([`MAX_DURATION_MS`]).
pub const MAX_DURATION: Duration = Duration::from_millis(MAX_DURATION_MS);

/// Full revolution period for the running [`crate::Spinner`], in milliseconds.
pub const SPINNER_PERIOD_MS: u64 = 900;

/// Full revolution period for [`crate::Spinner`].
pub const SPINNER_PERIOD: Duration = Duration::from_millis(SPINNER_PERIOD_MS);

/// Default slide offset (in pixels) when an overlay or toast enters.
pub const OVERLAY_OFFSET_PX: f32 = 8.0;

/// Global motion preferences for `xctx-ui` components.
///
/// Mirrors [`App::reduce_motion`] and can be set via [`MotionSettings::set_reduced`] so both
/// custom helpers and GPUI's built-in `AnimationExt` stay in sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MotionSettings {
    /// When `true`, all transitions snap immediately to their final state and continuous
    /// indicators (such as [`crate::Spinner`]) render statically with zero scheduled frames.
    pub reduced_motion: bool,
}

impl Global for MotionSettings {}

impl MotionSettings {
    /// Read the active [`MotionSettings`] from `cx`, combining the global setting with
    /// [`App::reduce_motion`].
    pub fn of(cx: &App) -> Self {
        let global = cx.try_global::<Self>().copied().unwrap_or_default();
        Self {
            reduced_motion: global.reduced_motion || cx.reduce_motion(),
        }
    }

    /// Whether reduced motion is active in either [`MotionSettings`] or [`App::reduce_motion`].
    pub fn is_reduced(cx: &App) -> bool {
        Self::of(cx).reduced_motion
    }

    /// Update both the [`MotionSettings`] global and GPUI's [`App::set_reduce_motion`].
    pub fn set_reduced(cx: &mut App, reduced: bool) {
        cx.set_global(Self {
            reduced_motion: reduced,
        });
        cx.set_reduce_motion(reduced);
    }
}

/// Clamp a transition duration to at most [`MAX_DURATION`] (160ms).
pub fn clamp_duration(duration: Duration) -> Duration {
    duration.min(MAX_DURATION)
}

/// The effective transition duration: zero when reduced motion is enabled, otherwise clamped to
/// [`MAX_DURATION`].
pub fn effective_duration(duration: Duration, reduced_motion: bool) -> Duration {
    if reduced_motion {
        Duration::ZERO
    } else {
        clamp_duration(duration)
    }
}

/// Cubic ease-out curve: fast start, smooth settle. Input is clamped to `0.0..=1.0`.
pub fn ease_out_cubic(t: f32) -> f32 {
    if !t.is_finite() {
        return 1.0;
    }
    let clamped = t.clamp(0.0, 1.0);
    let inv = 1.0 - clamped;
    1.0 - inv * inv * inv
}

/// Linear interpolation between `from` and `to` for `t` clamped to `0.0..=1.0`.
pub fn lerp(from: f32, to: f32, t: f32) -> f32 {
    let clamped = if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        1.0
    };
    from + (to - from) * clamped
}

/// Compute eased transition progress in `0.0..=1.0` after `elapsed` of `duration`.
///
/// Returns `1.0` immediately when `reduced_motion` is true or `duration` is zero.
pub fn progress(elapsed: Duration, duration: Duration, reduced_motion: bool) -> f32 {
    let duration = effective_duration(duration, reduced_motion);
    if duration.is_zero() {
        return 1.0;
    }
    let raw = (elapsed.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0);
    ease_out_cubic(raw)
}

/// Opacity in `0.0..=1.0` for an eased `progress` value.
pub fn opacity_for(progress: f32) -> f32 {
    lerp(0.0, 1.0, progress)
}

/// Offset in pixels sliding from `from_px` toward `0.0` as `progress` moves from `0.0` to `1.0`.
pub fn offset_px(from_px: f32, progress: f32) -> Pixels {
    px(lerp(from_px, 0.0, progress))
}

/// A single evaluated frame of an opacity + offset transition.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionFrame {
    /// Current opacity (`0.0..=1.0`).
    pub opacity: f32,
    /// Current pixel offset (approaches `0px`).
    pub offset: Pixels,
    /// `true` once the transition has reached its settled end state.
    pub done: bool,
}

/// Evaluate an overlay entrance transition at `elapsed` using [`OVERLAY_DURATION`].
pub fn overlay_motion(elapsed: Duration, from_offset_px: f32, reduced_motion: bool) -> MotionFrame {
    let duration = effective_duration(OVERLAY_DURATION, reduced_motion);
    if duration.is_zero() || elapsed >= duration {
        return MotionFrame {
            opacity: 1.0,
            offset: px(0.0),
            done: true,
        };
    }
    let p = progress(elapsed, duration, false);
    MotionFrame {
        opacity: opacity_for(p),
        offset: offset_px(from_offset_px, p),
        done: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_respect_160ms_budget() {
        const { assert!(OVERLAY_DURATION_MS == 140) };
        const { assert!(MAX_DURATION_MS == 160) };
        const { assert!(OVERLAY_DURATION_MS <= MAX_DURATION_MS) };
        assert_eq!(clamp_duration(Duration::from_millis(500)), MAX_DURATION);
        assert_eq!(
            clamp_duration(Duration::from_millis(120)),
            Duration::from_millis(120)
        );
        assert_eq!(effective_duration(OVERLAY_DURATION, true), Duration::ZERO);
    }

    #[test]
    fn ease_out_cubic_bounds_and_monotonicity() {
        assert_eq!(ease_out_cubic(-0.5), 0.0);
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        assert_eq!(ease_out_cubic(1.5), 1.0);
        assert_eq!(ease_out_cubic(f32::NAN), 1.0);

        let mid = ease_out_cubic(0.5);
        assert!((mid - 0.875).abs() < 1e-6);

        let mut prev = 0.0;
        for step in 1..=20 {
            let v = ease_out_cubic(step as f32 / 20.0);
            assert!(v >= prev);
            prev = v;
        }
    }

    #[test]
    fn overlay_motion_interpolates_and_respects_reduced_motion() {
        let reduced = overlay_motion(Duration::ZERO, OVERLAY_OFFSET_PX, true);
        assert!(reduced.done);
        assert_eq!(reduced.opacity, 1.0);
        assert_eq!(reduced.offset, px(0.0));

        let start = overlay_motion(Duration::ZERO, 8.0, false);
        assert!(!start.done);
        assert_eq!(start.opacity, 0.0);
        assert_eq!(start.offset, px(8.0));

        let mid = overlay_motion(Duration::from_millis(70), 8.0, false);
        assert!(!mid.done);
        assert!((mid.opacity - 0.875).abs() < 1e-5);
        assert!((f32::from(mid.offset) - 1.0).abs() < 1e-4);

        let end = overlay_motion(Duration::from_millis(140), 8.0, false);
        assert!(end.done);
        assert_eq!(end.opacity, 1.0);
        assert_eq!(end.offset, px(0.0));
    }
}
