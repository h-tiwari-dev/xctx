//! Frame-time telemetry ([`FrameMetrics`]) and the debug [`FrameOverlay`] HUD (`⌘⇧.`).
//!
//! Per the Performance section in `docs/m6.md`:
//! - Budget is **8ms** per frame (120Hz).
//! - Displays last frame time, p99 over a 5-second sliding window, and count of frames > 8ms.
//! - Detects frames > 16ms and logs them with the view name.
//! - [`FrameOverlay`] is its own GPUI entity (Performance Rule 4) so updating metrics never
//!   invalidates the parent workspace.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use gpui::{Context, FontWeight, SharedString, Window, div, prelude::*, px};

use crate::theme::{Theme, Tone};

/// Target per-frame budget for 120Hz displays, in milliseconds.
pub const FRAME_BUDGET_MS: f64 = 8.0;

/// Threshold above which a slow frame is logged with its view name, in milliseconds.
pub const SLOW_FRAME_LOG_MS: f64 = 16.0;

/// Sliding time window used to compute p99 and over-budget frame counts.
pub const FRAME_WINDOW: Duration = Duration::from_secs(5);

/// Default ring-buffer capacity (comfortably holds 5 seconds at 120Hz+ with zero heap growth).
pub const DEFAULT_FRAME_CAPACITY: usize = 1024;

/// Details of a frame that exceeded [`SLOW_FRAME_LOG_MS`] (16ms).
#[derive(Debug, Clone, PartialEq)]
pub struct SlowFrameEvent {
    /// Measured frame time in milliseconds.
    pub duration_ms: f64,
    /// Name of the view associated with the slow frame.
    pub view: SharedString,
}

impl SlowFrameEvent {
    /// Format a single diagnostic log line for this slow frame.
    pub fn log_line(&self) -> String {
        format!(
            "xctx-perf: slow frame {:.2}ms (> {:.0}ms) in {}",
            self.duration_ms, SLOW_FRAME_LOG_MS, self.view
        )
    }
}

/// Summary statistics computed over the active sliding window of [`FrameMetrics`].
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FrameStats {
    /// Number of frame samples inside the sliding window.
    pub sample_count: usize,
    /// Most recent frame duration inside the window, in milliseconds.
    pub last_ms: Option<f64>,
    /// 99th-percentile frame duration over the window, in milliseconds.
    pub p99_ms: Option<f64>,
    /// Number of frames inside the window strictly slower than [`FRAME_BUDGET_MS`] (8ms).
    pub over_budget_count: usize,
    /// Number of frames inside the window strictly slower than [`SLOW_FRAME_LOG_MS`] (16ms).
    pub over_slow_count: usize,
}

impl FrameStats {
    /// Semantic tone reflecting whether p99 meets the 8ms budget.
    pub fn tone(&self) -> Tone {
        match self.p99_ms {
            None => Tone::Neutral,
            Some(p99) if p99 > SLOW_FRAME_LOG_MS => Tone::Danger,
            Some(p99) if p99 > FRAME_BUDGET_MS => Tone::Warning,
            Some(_) => Tone::Success,
        }
    }

    /// Compact single-line readout shown in [`FrameOverlay`].
    pub fn summary(&self) -> String {
        match (self.last_ms, self.p99_ms) {
            (Some(last), Some(p99)) => format!(
                "last {last:.1}ms · p99(5s) {p99:.1}ms · >8ms: {}",
                self.over_budget_count
            ),
            _ => "last — · p99(5s) — · >8ms: 0".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct FrameSample {
    recorded_at: Instant,
    duration_ms: f64,
}

/// UI-free bounded ring buffer tracking frame durations over a sliding time window.
pub struct FrameMetrics {
    window: Duration,
    capacity: usize,
    samples: VecDeque<FrameSample>,
}

impl Default for FrameMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameMetrics {
    /// Create a metrics collector with [`FRAME_WINDOW`] (5s) and [`DEFAULT_FRAME_CAPACITY`].
    pub fn new() -> Self {
        Self::with_window_and_capacity(FRAME_WINDOW, DEFAULT_FRAME_CAPACITY)
    }

    /// Create a metrics collector with a custom sliding window and maximum sample capacity.
    pub fn with_window_and_capacity(window: Duration, capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            window,
            capacity,
            samples: VecDeque::with_capacity(capacity),
        }
    }

    /// Record a frame duration at `now`, pruning expired samples.
    ///
    /// Returns `Some(SlowFrameEvent)` when `duration` strictly exceeds [`SLOW_FRAME_LOG_MS`]
    /// (16ms) so callers can log or assert on slow frames deterministically.
    pub fn record_at(
        &mut self,
        now: Instant,
        duration: Duration,
        view: Option<&str>,
    ) -> Option<SlowFrameEvent> {
        self.prune(now);

        if self.samples.len() >= self.capacity {
            self.samples.pop_front();
        }

        let duration_ms = duration.as_secs_f64() * 1_000.0;
        self.samples.push_back(FrameSample {
            recorded_at: now,
            duration_ms,
        });

        if duration_ms > SLOW_FRAME_LOG_MS {
            Some(SlowFrameEvent {
                duration_ms,
                view: view.unwrap_or("unknown").to_owned().into(),
            })
        } else {
            None
        }
    }

    /// Record a frame duration at [`Instant::now`], logging to stderr if it exceeds
    /// [`SLOW_FRAME_LOG_MS`] (16ms).
    pub fn record(&mut self, duration: Duration, view: Option<&str>) -> Option<SlowFrameEvent> {
        let slow = self.record_at(Instant::now(), duration, view);
        if let Some(event) = &slow {
            eprintln!("{}", event.log_line());
        }
        slow
    }

    /// Remove samples older than `self.window` relative to `now`.
    pub fn prune(&mut self, now: Instant) {
        while let Some(front) = self.samples.front() {
            if now.saturating_duration_since(front.recorded_at) > self.window {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }

    /// Compute [`FrameStats`] over all samples still within `self.window` of `now`.
    pub fn snapshot_at(&self, now: Instant) -> FrameStats {
        let mut active = Vec::with_capacity(self.samples.len());
        let mut last_ms = None;
        let mut over_budget_count = 0usize;
        let mut over_slow_count = 0usize;

        for sample in &self.samples {
            if now.saturating_duration_since(sample.recorded_at) <= self.window {
                active.push(sample.duration_ms);
                last_ms = Some(sample.duration_ms);
                if sample.duration_ms > FRAME_BUDGET_MS {
                    over_budget_count += 1;
                }
                if sample.duration_ms > SLOW_FRAME_LOG_MS {
                    over_slow_count += 1;
                }
            }
        }

        let sample_count = active.len();
        let p99_ms = if sample_count == 0 {
            None
        } else {
            active.sort_unstable_by(f64::total_cmp);
            // Nearest-rank 99th percentile: ceil(0.99 * n) - 1, clamped to valid bounds.
            let rank = ((sample_count as f64) * 0.99).ceil() as usize;
            let index = rank.saturating_sub(1).min(sample_count - 1);
            Some(active[index])
        };

        FrameStats {
            sample_count,
            last_ms,
            p99_ms,
            over_budget_count,
            over_slow_count,
        }
    }

    /// Compute [`FrameStats`] as of [`Instant::now`].
    pub fn snapshot(&self) -> FrameStats {
        self.snapshot_at(Instant::now())
    }

    /// Clear all recorded samples.
    pub fn clear(&mut self) {
        self.samples.clear();
    }

    /// Number of currently stored samples (prior to pruning).
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// Whether no samples are currently stored.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

/// Isolated GPUI entity rendering the frame-time debug HUD (`⌘⇧.`).
///
/// Hosts can push exact render durations via [`FrameOverlay::record_frame`], or enable
/// [`FrameOverlay::auto_sample`] to measure inter-frame wall time automatically while visible.
pub struct FrameOverlay {
    metrics: FrameMetrics,
    visible: bool,
    view_name: SharedString,
    auto_sample: bool,
    last_frame_at: Option<Instant>,
}

impl FrameOverlay {
    /// Create a hidden [`FrameOverlay`] labelled with `view_name`.
    pub fn new(view_name: impl Into<SharedString>) -> Self {
        Self {
            metrics: FrameMetrics::new(),
            visible: false,
            view_name: view_name.into(),
            auto_sample: false,
            last_frame_at: None,
        }
    }

    /// Start the overlay visible or hidden.
    pub fn visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    /// Enable automatic inter-frame sampling via `window.on_next_frame` while visible.
    pub fn auto_sample(mut self, enabled: bool) -> Self {
        self.auto_sample = enabled;
        self
    }

    /// Whether the current binary was compiled with debug assertions (where `⌘⇧.` is active).
    pub const fn is_debug_build() -> bool {
        cfg!(debug_assertions)
    }

    /// Whether the HUD is currently visible.
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Show or hide the HUD, resetting auto-sample timestamps when toggled.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible != visible {
            self.visible = visible;
            self.last_frame_at = None;
            cx.notify();
        }
    }

    /// Toggle HUD visibility.
    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        self.set_visible(!self.visible, cx);
    }

    /// Record an externally measured frame duration (e.g. a view's render time).
    ///
    /// Slow frames (> 16ms) are logged even while the HUD is hidden. When visible, notifies only
    /// this overlay entity.
    pub fn record_frame(
        &mut self,
        duration: Duration,
        view: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Option<SlowFrameEvent> {
        let view_label = view.unwrap_or(self.view_name.as_ref());
        let slow = self.metrics.record(duration, Some(view_label));
        if self.visible {
            cx.notify();
        }
        slow
    }

    /// Current 5-second window statistics.
    pub fn stats(&self) -> FrameStats {
        self.metrics.snapshot()
    }

    /// Borrow the underlying [`FrameMetrics`].
    pub fn metrics(&self) -> &FrameMetrics {
        &self.metrics
    }

    /// Clear recorded samples.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.metrics.clear();
        if self.visible {
            cx.notify();
        }
    }
}

impl Render for FrameOverlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.visible {
            return div().into_any_element();
        }

        if self.auto_sample {
            let now = Instant::now();
            if let Some(prev) = self.last_frame_at {
                let dt = now.saturating_duration_since(prev);
                let view = self.view_name.clone();
                let _ = self.metrics.record(dt, Some(view.as_ref()));
            }
            self.last_frame_at = Some(now);

            let entity = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let _ = entity.update(cx, |this, cx| {
                    if this.visible && this.auto_sample {
                        cx.notify();
                    }
                });
            });
        }

        let theme = Theme::of(cx);
        let stats = self.metrics.snapshot();
        let tone = stats.tone();
        let tone_colors = theme.tone(tone);

        div()
            .id("frame-overlay-hud")
            .debug_selector(|| "frame-overlay-hud".to_owned())
            .flex()
            .items_center()
            .gap(Theme::SPACE_2)
            .px(Theme::SPACE_2)
            .py(Theme::SPACE_1)
            .rounded(Theme::RADIUS_SM)
            .border_1()
            .border_color(tone_colors.border)
            .bg(theme.overlay)
            .text_size(Theme::TEXT_XS)
            .font_family("Menlo")
            .child(
                div()
                    .size(px(6.0))
                    .rounded(Theme::RADIUS_FULL)
                    .bg(tone_colors.fg),
            )
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(tone_colors.fg)
                    .child(stats.summary()),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(v: f64) -> Duration {
        Duration::from_secs_f64(v / 1_000.0)
    }

    #[test]
    fn empty_metrics_produces_empty_stats() {
        let metrics = FrameMetrics::new();
        let stats = metrics.snapshot();
        assert_eq!(stats.sample_count, 0);
        assert_eq!(stats.last_ms, None);
        assert_eq!(stats.p99_ms, None);
        assert_eq!(stats.over_budget_count, 0);
        assert_eq!(stats.over_slow_count, 0);
        assert_eq!(stats.tone(), Tone::Neutral);
    }

    #[test]
    fn computes_last_p99_and_over_8ms_budget_count() {
        let mut metrics = FrameMetrics::new();
        let t0 = Instant::now();

        // 98 frames at 4.0ms, 1 frame at 8.0ms (within budget), 1 frame at 12.0ms (over budget).
        for i in 0..98 {
            assert!(
                metrics
                    .record_at(
                        t0 + Duration::from_millis(i * 10),
                        ms(4.0),
                        Some("Workspace")
                    )
                    .is_none()
            );
        }
        assert!(
            metrics
                .record_at(t0 + Duration::from_millis(980), ms(8.0), Some("Workspace"))
                .is_none()
        );
        assert!(
            metrics
                .record_at(t0 + Duration::from_millis(990), ms(12.0), Some("Workspace"))
                .is_none()
        );

        let stats = metrics.snapshot_at(t0 + Duration::from_millis(995));
        assert_eq!(stats.sample_count, 100);
        assert!((stats.last_ms.unwrap() - 12.0).abs() < 1e-3);
        // Nearest-rank p99 of 100 samples is the 99th sorted value = 8.0ms.
        assert!((stats.p99_ms.unwrap() - 8.0).abs() < 1e-3);
        assert_eq!(stats.over_budget_count, 1);
        assert_eq!(stats.over_slow_count, 0);
        assert_eq!(stats.tone(), Tone::Success);

        // Add another 12.0ms frame -> 2 outliers out of 101 -> p99 jumps to 12.0ms.
        metrics.record_at(
            t0 + Duration::from_millis(1000),
            ms(12.0),
            Some("Workspace"),
        );
        let stats = metrics.snapshot_at(t0 + Duration::from_millis(1005));
        assert_eq!(stats.sample_count, 101);
        assert!((stats.p99_ms.unwrap() - 12.0).abs() < 1e-3);
        assert_eq!(stats.over_budget_count, 2);
        assert_eq!(stats.tone(), Tone::Warning);
    }

    #[test]
    fn sliding_5s_window_expires_old_samples() {
        let mut metrics = FrameMetrics::new();
        let t0 = Instant::now();

        // Slow spike at t = 0s.
        let slow = metrics.record_at(t0, ms(25.0), Some("ReviewSheet"));
        assert!(slow.is_some());

        // Fast frames at t = 3s and t = 6s.
        metrics.record_at(t0 + Duration::from_secs(3), ms(3.0), Some("Workspace"));
        let stats_at_3s = metrics.snapshot_at(t0 + Duration::from_secs(3));
        assert_eq!(stats_at_3s.sample_count, 2);
        assert_eq!(stats_at_3s.over_budget_count, 1);
        assert_eq!(stats_at_3s.over_slow_count, 1);
        assert!((stats_at_3s.p99_ms.unwrap() - 25.0).abs() < 1e-3);

        // At t = 5.001s the spike at t = 0s is outside the 5s window.
        let stats_after_expiry =
            metrics.snapshot_at(t0 + Duration::from_secs(5) + Duration::from_millis(1));
        assert_eq!(stats_after_expiry.sample_count, 1);
        assert_eq!(stats_after_expiry.over_budget_count, 0);
        assert_eq!(stats_after_expiry.over_slow_count, 0);
        assert!((stats_after_expiry.p99_ms.unwrap() - 3.0).abs() < 1e-3);
        assert_eq!(stats_after_expiry.tone(), Tone::Success);

        // Recording a new frame at t = 6s physically prunes expired samples.
        metrics.record_at(t0 + Duration::from_secs(6), ms(4.0), Some("Workspace"));
        assert_eq!(metrics.len(), 2);
    }

    #[test]
    fn detects_frames_over_16ms_with_view_name() {
        let mut metrics = FrameMetrics::new();
        let t0 = Instant::now();

        // Exactly 16.0ms does not trigger the >16ms slow-frame event.
        assert!(metrics.record_at(t0, ms(16.0), Some("SplitView")).is_none());

        // Strictly above 16ms triggers with the supplied view name.
        let event = metrics
            .record_at(t0 + Duration::from_millis(20), ms(16.5), Some("SplitView"))
            .expect("frame > 16ms must report a SlowFrameEvent");
        assert!((event.duration_ms - 16.5).abs() < 1e-3);
        assert_eq!(event.view.as_ref(), "SplitView");
        assert!(event.log_line().contains("SplitView"));
        assert!(event.log_line().contains("16.50ms"));

        // Missing view name defaults to "unknown".
        let unknown = metrics
            .record_at(t0 + Duration::from_millis(40), ms(22.0), None)
            .expect("frame > 16ms must report a SlowFrameEvent");
        assert_eq!(unknown.view.as_ref(), "unknown");

        let stats = metrics.snapshot_at(t0 + Duration::from_millis(50));
        assert_eq!(stats.over_budget_count, 3);
        assert_eq!(stats.over_slow_count, 2);
        assert_eq!(stats.tone(), Tone::Danger);
    }

    #[test]
    fn ring_buffer_respects_capacity_bound() {
        let mut metrics = FrameMetrics::with_window_and_capacity(FRAME_WINDOW, 4);
        let t0 = Instant::now();
        for i in 0..10 {
            metrics.record_at(
                t0 + Duration::from_millis(i),
                ms((i + 1) as f64),
                Some("Test"),
            );
        }
        assert_eq!(metrics.len(), 4);
        let stats = metrics.snapshot_at(t0 + Duration::from_millis(10));
        assert_eq!(stats.sample_count, 4);
        assert!((stats.last_ms.unwrap() - 10.0).abs() < 1e-3);
    }
}
