//! The HUD row's read-outs: model choices from omp's picker, the turn meter (working time and tokens per
//! second), the working clock entity, and the labels for context, cost and todo progress.

use std::time::{Duration, Instant};

use gpui::{Context, Task, Window, div, prelude::*};
use serde_json::Value;
use xctx_omp::SessionModel;
use xctx_ui::Theme;

/// A model the user can switch to, from a `get_model_picker` response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChoice {
    /// The provider.
    pub provider: String,
    /// The model id.
    pub id: String,
    /// The display name, when omp gives one.
    pub name: Option<String>,
    /// Whether the session runs it now.
    pub current: bool,
}

impl ModelChoice {
    /// The rows of a `get_model_picker` response's `data` (`{rows: [{provider, id, name, current}]}`); rows
    /// without a provider or id are skipped.
    pub fn from_picker(data: &Value) -> Vec<ModelChoice> {
        data.get("rows")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| {
                        Some(ModelChoice {
                            provider: row.get("provider")?.as_str()?.to_owned(),
                            id: row.get("id")?.as_str()?.to_owned(),
                            name: row.get("name").and_then(Value::as_str).map(str::to_owned),
                            current: row.get("current").and_then(Value::as_bool) == Some(true),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// What to show: the name, else the id.
    pub fn label(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }
}

/// Times turns from the snapshots it is shown: when the current one started and, once a turn ended and omp's
/// session stats moved, the tokens per second it used. omp reports only total tokens (input and output), so the
/// rate is total tokens over the turn's wall time.
#[derive(Debug, Clone, Default)]
pub struct TurnMeter {
    started: Option<Instant>,
    start_tokens: u64,
    pending: Option<(u64, Duration)>,
    rate: Option<f64>,
}

impl TurnMeter {
    /// Note `model` as of `now`.
    pub fn observe(&mut self, model: &SessionModel, now: Instant) {
        match (self.started, model.is_running()) {
            (None, true) => {
                self.started = Some(now);
                self.start_tokens = model.stats.tokens;
                self.pending = None;
            }
            (Some(start), false) => {
                self.started = None;
                self.pending = Some((self.start_tokens, now.saturating_duration_since(start)));
            }
            _ => {}
        }
        if let Some((before, took)) = self.pending
            && model.stats.tokens > before
            && !took.is_zero()
        {
            self.rate = Some((model.stats.tokens - before) as f64 / took.as_secs_f64());
            self.pending = None;
        }
    }

    /// When the running turn started, `None` while idle.
    pub fn started(&self) -> Option<Instant> {
        self.started
    }

    /// Tokens per second of the last finished turn, once known.
    pub fn rate(&self) -> Option<f64> {
        self.rate
    }
}

/// `12s`, `3m 05s`, `1h 02m`.
pub fn format_elapsed(elapsed: Duration) -> String {
    let s = elapsed.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3_600 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3_600, (s % 3_600) / 60),
    }
}

/// `ctx 34%`, once omp reported context use.
pub fn context_label(model: &SessionModel) -> Option<String> {
    model
        .context
        .map(|c| format!("ctx {:.0}%", c.percent.clamp(0.0, 100.0)))
}

/// Whether context use is high enough to warn about (80% or more).
pub fn context_is_high(model: &SessionModel) -> bool {
    model.context.is_some_and(|c| c.percent >= 80.0)
}

/// `$0.12`.
pub fn cost_label(model: &SessionModel) -> String {
    format!("${:.2}", model.stats.cost)
}

/// `42 tok/s`.
pub fn rate_label(rate: f64) -> String {
    format!("{rate:.0} tok/s")
}

/// Finished (completed or abandoned) and total todo items, when there are any.
pub fn todo_progress(model: &SessionModel) -> Option<(usize, usize)> {
    let tasks = model.todos.iter().flat_map(|p| &p.tasks);
    let total = tasks.clone().count();
    let done = tasks
        .filter(|t| matches!(t.status.as_str(), "completed" | "abandoned"))
        .count();
    (total > 0).then_some((done, total))
}

/// The working time of the running turn, re-drawn once a second. Its own entity so the tick re-renders only this
/// label, never the pane.
pub struct WorkingClock {
    started: Option<Instant>,
    _tick: Option<Task<()>>,
}

impl WorkingClock {
    /// A stopped clock.
    pub fn new() -> Self {
        WorkingClock {
            started: None,
            _tick: None,
        }
    }

    /// Count from `started`, or stop with `None`.
    pub fn set_started(&mut self, started: Option<Instant>, cx: &mut Context<Self>) {
        if self.started == started {
            return;
        }
        self.started = started;
        self._tick = started.map(|_| {
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            })
        });
        cx.notify();
    }
}

impl Default for WorkingClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Render for WorkingClock {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        let label = self
            .started
            .map(|s| format_elapsed(s.elapsed()))
            .unwrap_or_default();
        div()
            .debug_selector(|| "omp-working-elapsed".to_owned())
            .text_size(Theme::TEXT_SM)
            .text_color(theme.meta)
            .child(label)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use xctx_omp::model::{Todo, TodoPhase};

    use super::*;

    #[test]
    fn picker_rows_become_choices() {
        let data = json!({"rows": [
            {"provider": "cursor", "id": "auto-smart", "name": "Auto", "current": true},
            {"provider": "anthropic", "id": "claude"},
            {"id": "no-provider"}
        ]});
        let choices = ModelChoice::from_picker(&data);
        assert_eq!(choices.len(), 2);
        assert_eq!((choices[0].label(), choices[0].current), ("Auto", true));
        assert_eq!((choices[1].label(), choices[1].current), ("claude", false));
        assert!(ModelChoice::from_picker(&Value::Null).is_empty());
    }

    #[test]
    fn the_meter_times_a_turn_and_rates_it_once_stats_move() {
        let mut model = SessionModel::new();
        model.stats.tokens = 100;
        let mut meter = TurnMeter::default();
        let t0 = Instant::now();
        meter.observe(&model, t0);
        assert_eq!(meter.started(), None);

        model.apply(xctx_omp::Frame::parse(json!({"type": "agent_start"})));
        meter.observe(&model, t0);
        assert_eq!(meter.started(), Some(t0));

        model.apply(xctx_omp::Frame::parse(json!({"type": "agent_end"})));
        meter.observe(&model, t0 + Duration::from_secs(4));
        assert_eq!(meter.started(), None);
        assert_eq!(meter.rate(), None, "stats have not moved yet");

        model.stats.tokens = 500;
        meter.observe(&model, t0 + Duration::from_secs(5));
        assert_eq!(meter.rate(), Some(100.0));
        assert_eq!(rate_label(100.0), "100 tok/s");
    }

    #[test]
    fn labels() {
        assert_eq!(format_elapsed(Duration::from_secs(12)), "12s");
        assert_eq!(format_elapsed(Duration::from_secs(185)), "3m 05s");
        assert_eq!(format_elapsed(Duration::from_secs(3_720)), "1h 02m");

        let mut model = SessionModel::new();
        assert_eq!(context_label(&model), None);
        model.context = Some(xctx_omp::model::ContextUsage {
            tokens: 10,
            window: 100,
            percent: 83.4,
        });
        assert_eq!(context_label(&model).as_deref(), Some("ctx 83%"));
        assert!(context_is_high(&model));
        model.stats.cost = 0.126;
        assert_eq!(cost_label(&model), "$0.13");

        assert_eq!(todo_progress(&model), None);
        let todo = |status: &str| Todo {
            content: "x".into(),
            status: status.into(),
        };
        model.todos = vec![TodoPhase {
            name: "p".into(),
            tasks: vec![todo("completed"), todo("in_progress"), todo("abandoned")],
        }];
        assert_eq!(todo_progress(&model), Some((2, 3)));
    }
}
