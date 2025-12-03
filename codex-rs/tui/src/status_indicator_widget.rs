//! A live status indicator that shows the *latest* log line emitted by the
//! application while the agent is processing a long‑running task.

use std::time::Duration;
use std::time::Instant;

use codex_core::protocol::DelegateWorkerStatusKind;
use codex_core::protocol::Op;
use codex_core::protocol_config_types::ReasoningEffort;
use crossterm::event::KeyCode;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::WidgetRef;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::exec_cell::spinner;
use crate::key_hint;
use crate::render::renderable::Renderable;
use crate::shimmer::shimmer_spans;
use crate::tui::FrameRequester;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentRole {
    Ceo,
    Manager,
    Worker,
}

impl AgentRole {
    fn label(self) -> &'static str {
        match self {
            AgentRole::Ceo => "CEO",
            AgentRole::Manager => "Manager",
            AgentRole::Worker => "Worker",
        }
    }

    fn short_label(self) -> &'static str {
        match self {
            AgentRole::Ceo => "CEO",
            AgentRole::Manager => "Mgr",
            AgentRole::Worker => "Wkr",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentStatusEntry {
    pub(crate) role: AgentRole,
    pub(crate) worker_id: Option<String>,
    pub(crate) worker_model: Option<String>,
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
    pub(crate) display_name: Option<String>,
    pub(crate) message: String,
    pub(crate) depth: u16,
    pub(crate) status: DelegateWorkerStatusKind,
    pub(crate) updated_at: Instant,
}

impl AgentStatusEntry {
    fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            DelegateWorkerStatusKind::Completed | DelegateWorkerStatusKind::Failed
        )
    }
}

pub(crate) struct StatusIndicatorWidget {
    /// Animated header text (defaults to "Working").
    header: String,
    show_interrupt_hint: bool,
    top_role: AgentRole,
    agent_rows: Vec<AgentStatusEntry>,
    header_updated_at: Instant,

    elapsed_running: Duration,
    last_resume_at: Instant,
    is_paused: bool,
    app_event_tx: AppEventSender,
    frame_requester: FrameRequester,
    animations_enabled: bool,
}

// Format elapsed seconds into a compact human-friendly form used by the status line.
// Examples: 0s, 59s, 1m 00s, 59m 59s, 1h 00m 00s, 2h 03m 09s
pub fn fmt_elapsed_compact(elapsed_secs: u64) -> String {
    if elapsed_secs < 60 {
        return format!("{elapsed_secs}s");
    }
    if elapsed_secs < 3600 {
        let minutes = elapsed_secs / 60;
        let seconds = elapsed_secs % 60;
        return format!("{minutes}m {seconds:02}s");
    }
    let hours = elapsed_secs / 3600;
    let minutes = (elapsed_secs % 3600) / 60;
    let seconds = elapsed_secs % 60;
    format!("{hours}h {minutes:02}m {seconds:02}s")
}

fn short_worker_id(worker_id: &str) -> String {
    if let Some(rest) = worker_id.strip_prefix("worker-") {
        return format!("wkr{rest}");
    }
    if let Some(rest) = worker_id.strip_prefix("manager-") {
        return format!("mng{rest}");
    }
    worker_id.to_string()
}

impl StatusIndicatorWidget {
    pub(crate) fn new(
        app_event_tx: AppEventSender,
        frame_requester: FrameRequester,
        animations_enabled: bool,
    ) -> Self {
        Self {
            header: String::from("Working"),
            show_interrupt_hint: true,
            top_role: AgentRole::Worker,
            agent_rows: Vec::new(),
            header_updated_at: Instant::now(),
            elapsed_running: Duration::ZERO,
            last_resume_at: Instant::now(),
            is_paused: false,

            app_event_tx,
            frame_requester,
            animations_enabled,
        }
    }

    pub(crate) fn interrupt(&self) {
        self.app_event_tx.send(AppEvent::CodexOp(Op::Interrupt));
    }

    /// Update the animated header label (left of the brackets).
    pub(crate) fn update_header(&mut self, header: String) {
        self.header = header;
        self.header_updated_at = Instant::now();
    }

    #[cfg(test)]
    pub(crate) fn header(&self) -> &str {
        &self.header
    }

    pub(crate) fn set_interrupt_hint_visible(&mut self, visible: bool) {
        self.show_interrupt_hint = visible;
    }

    #[cfg(test)]
    pub(crate) fn interrupt_hint_visible(&self) -> bool {
        self.show_interrupt_hint
    }

    pub(crate) fn update_agent_hierarchy(
        &mut self,
        top_role: AgentRole,
        agent_rows: Vec<AgentStatusEntry>,
    ) {
        self.top_role = top_role;
        self.agent_rows = agent_rows;
    }

    #[cfg(test)]
    pub(crate) fn agent_rows(&self) -> &[AgentStatusEntry] {
        &self.agent_rows
    }

    pub(crate) fn pause_timer(&mut self) {
        self.pause_timer_at(Instant::now());
    }

    pub(crate) fn resume_timer(&mut self) {
        self.resume_timer_at(Instant::now());
    }

    pub(crate) fn pause_timer_at(&mut self, now: Instant) {
        if self.is_paused {
            return;
        }
        self.elapsed_running += now.saturating_duration_since(self.last_resume_at);
        self.is_paused = true;
    }

    pub(crate) fn resume_timer_at(&mut self, now: Instant) {
        if !self.is_paused {
            return;
        }
        self.last_resume_at = now;
        self.is_paused = false;
        self.frame_requester.schedule_frame();
    }

    fn elapsed_duration_at(&self, now: Instant) -> Duration {
        let mut elapsed = self.elapsed_running;
        if !self.is_paused {
            elapsed += now.saturating_duration_since(self.last_resume_at);
        }
        elapsed
    }

    fn elapsed_seconds_at(&self, now: Instant) -> u64 {
        self.elapsed_duration_at(now).as_secs()
    }

    pub fn elapsed_seconds(&self) -> u64 {
        self.elapsed_seconds_at(Instant::now())
    }
}

impl Renderable for StatusIndicatorWidget {
    fn desired_height(&self, _width: u16) -> u16 {
        1u16.saturating_add(self.agent_rows.len().try_into().unwrap_or(u16::MAX))
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        // Schedule next animation frame.
        self.frame_requester
            .schedule_frame_in(Duration::from_millis(32));
        let now = Instant::now();
        let elapsed_duration = self.elapsed_duration_at(now);
        let pretty_elapsed = fmt_elapsed_compact(elapsed_duration.as_secs());

        let mut lines: Vec<Line> = Vec::with_capacity(1 + self.agent_rows.len());
        let mut top_spans = Vec::with_capacity(7);
        top_spans.push(spinner(Some(self.last_resume_at), self.animations_enabled));
        top_spans.push(" ".into());
        top_spans.push(self.top_role.label().bold());
        top_spans.push(" ".dim());
        top_spans.push("·".dim());
        top_spans.push(" ".dim());
        if self.animations_enabled {
            top_spans.extend(shimmer_spans(&self.header));
        } else if !self.header.is_empty() {
            top_spans.push(self.header.clone().into());
        }
        top_spans.push(" ".into());
        let since_update = now.saturating_duration_since(self.header_updated_at);
        top_spans.push(
            format!(
                "updated {} ago",
                fmt_elapsed_compact(since_update.as_secs())
            )
            .dim(),
        );
        top_spans.push(" ".into());
        if self.show_interrupt_hint {
            top_spans.extend(vec![
                format!("({pretty_elapsed} • ").dim(),
                key_hint::plain(KeyCode::Esc).into(),
                " to interrupt)".dim(),
            ]);
        } else {
            top_spans.push(format!("({pretty_elapsed})").dim());
        }
        lines.push(Line::from(top_spans));

        for row in &self.agent_rows {
            let mut spans = Vec::new();
            let depth = usize::from(row.depth);
            if depth > 0 {
                spans.push("  ".repeat(depth).into());
            }
            let status_span = match row.status {
                DelegateWorkerStatusKind::Completed => "+".green(),
                DelegateWorkerStatusKind::Failed => "x".red(),
                DelegateWorkerStatusKind::Warning => "!".magenta(),
                DelegateWorkerStatusKind::RunningCommand
                | DelegateWorkerStatusKind::RunningTool
                | DelegateWorkerStatusKind::ApplyingPatch
                | DelegateWorkerStatusKind::DiffApplied
                | DelegateWorkerStatusKind::Starting
                | DelegateWorkerStatusKind::Running => "-".dim(),
            };
            spans.push(status_span);
            spans.push(" ".into());
            spans.push(row.role.short_label().bold());
            if let Some(display_name) = &row.display_name {
                spans.push(" ".into());
                spans.push(display_name.as_str().into());
            }
            if let Some(worker_id) = &row.worker_id {
                spans.push(" ".into());
                let display_id = short_worker_id(worker_id);
                if row.display_name.is_some() {
                    spans.push(format!("({display_id})").dim());
                } else {
                    spans.push(display_id.dim());
                }
            }
            if let Some(worker_model) = &row.worker_model {
                let reasoning_label = row
                    .reasoning_effort
                    .map(|effort| effort.to_string())
                    .unwrap_or_else(|| "auto".to_string());
                let detail = format!("({worker_model} · {reasoning_label})");
                spans.push(" ".into());
                spans.push(detail.dim());
            } else if let Some(reasoning) = row.reasoning_effort {
                spans.push(" ".into());
                spans.push(format!("({reasoning})").dim());
            }
            spans.push(" ".into());
            spans.push("·".dim());
            spans.push(" ".dim());
            let message_span = match row.status {
                DelegateWorkerStatusKind::Failed => row.message.clone().red(),
                DelegateWorkerStatusKind::Warning => row.message.clone().magenta(),
                _ if row.is_terminal() => row.message.clone().dim(),
                _ => row.message.clone().into(),
            };
            spans.push(message_span);
            let age = now.saturating_duration_since(row.updated_at);
            spans.push(" ".into());
            spans.push(format!("({} ago)", fmt_elapsed_compact(age.as_secs())).dim());
            lines.push(Line::from(spans));
        }

        for (idx, line) in lines.into_iter().enumerate() {
            if idx as u16 >= area.height {
                break;
            }
            let row_area = Rect {
                x: area.x,
                y: area.y + idx as u16,
                width: area.width,
                height: 1,
            };
            line.render_ref(row_area, buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_event::AppEvent;
    use crate::app_event_sender::AppEventSender;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::time::Duration;
    use std::time::Instant;
    use tokio::sync::mpsc::unbounded_channel;

    use pretty_assertions::assert_eq;

    #[test]
    fn fmt_elapsed_compact_formats_seconds_minutes_hours() {
        assert_eq!(fmt_elapsed_compact(0), "0s");
        assert_eq!(fmt_elapsed_compact(1), "1s");
        assert_eq!(fmt_elapsed_compact(59), "59s");
        assert_eq!(fmt_elapsed_compact(60), "1m 00s");
        assert_eq!(fmt_elapsed_compact(61), "1m 01s");
        assert_eq!(fmt_elapsed_compact(3 * 60 + 5), "3m 05s");
        assert_eq!(fmt_elapsed_compact(59 * 60 + 59), "59m 59s");
        assert_eq!(fmt_elapsed_compact(3600), "1h 00m 00s");
        assert_eq!(fmt_elapsed_compact(3600 + 60 + 1), "1h 01m 01s");
        assert_eq!(fmt_elapsed_compact(25 * 3600 + 2 * 60 + 3), "25h 02m 03s");
    }

    #[test]
    fn short_worker_id_compacts_known_prefixes() {
        assert_eq!(short_worker_id("worker-1"), "wkr1");
        assert_eq!(short_worker_id("manager-2"), "mng2");
        assert_eq!(short_worker_id("other"), "other");
    }

    #[test]
    fn renders_with_working_header() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        let w = StatusIndicatorWidget::new(tx, crate::tui::FrameRequester::test_dummy(), true);

        // Render into a fixed-size test terminal and snapshot the backend.
        let mut terminal = Terminal::new(TestBackend::new(80, 2)).expect("terminal");
        terminal
            .draw(|f| w.render(f.area(), f.buffer_mut()))
            .expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn renders_truncated() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        let w = StatusIndicatorWidget::new(tx, crate::tui::FrameRequester::test_dummy(), true);

        // Render into a fixed-size test terminal and snapshot the backend.
        let mut terminal = Terminal::new(TestBackend::new(20, 2)).expect("terminal");
        terminal
            .draw(|f| w.render(f.area(), f.buffer_mut()))
            .expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn timer_pauses_when_requested() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        let mut widget =
            StatusIndicatorWidget::new(tx, crate::tui::FrameRequester::test_dummy(), true);

        let baseline = Instant::now();
        widget.last_resume_at = baseline;

        let before_pause = widget.elapsed_seconds_at(baseline + Duration::from_secs(5));
        assert_eq!(before_pause, 5);

        widget.pause_timer_at(baseline + Duration::from_secs(5));
        let paused_elapsed = widget.elapsed_seconds_at(baseline + Duration::from_secs(10));
        assert_eq!(paused_elapsed, before_pause);

        widget.resume_timer_at(baseline + Duration::from_secs(10));
        let after_resume = widget.elapsed_seconds_at(baseline + Duration::from_secs(13));
        assert_eq!(after_resume, before_pause + 3);
    }
}
