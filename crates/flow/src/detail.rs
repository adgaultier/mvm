//! Modal agent detail view: the full sandbox record (the same data as the
//! TUI's inspect modal) plus state-gated Start/Stop/Delete buttons that work
//! with both mouse clicks and the keyboard.

use mvm_common::agent_api::AgentStatus;
use mvm_common::{Sandbox, SandboxState};
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState};
use ratatui::Frame;

/// One lifecycle action the modal can trigger on its sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
    Delete,
}

impl Action {
    pub const ALL: [Action; 3] = [Action::Start, Action::Stop, Action::Delete];

    pub fn label(self) -> &'static str {
        match self {
            Action::Start => "Start",
            Action::Stop => "Stop",
            Action::Delete => "Delete",
        }
    }

    /// Keyboard shortcut (docker-style, same letters as the TUI).
    pub fn key(self) -> char {
        match self {
            Action::Start => 's',
            Action::Stop => 'x',
            Action::Delete => 'd',
        }
    }

    pub fn from_key(c: char) -> Option<Action> {
        Action::ALL.into_iter().find(|a| a.key() == c)
    }

    fn color(self) -> Color {
        match self {
            Action::Start => Color::Green,
            Action::Stop => Color::Yellow,
            Action::Delete => Color::Red,
        }
    }

    /// State gating: start anything that is not running, stop only a running
    /// VM, delete always (a running sandbox is stopped first by the caller).
    pub fn enabled(self, state: SandboxState) -> bool {
        match self {
            Action::Start => !matches!(state, SandboxState::Running),
            Action::Stop => matches!(state, SandboxState::Running),
            Action::Delete => true,
        }
    }

    fn next(self) -> Action {
        match self {
            Action::Start => Action::Stop,
            Action::Stop => Action::Delete,
            Action::Delete => Action::Start,
        }
    }

    fn prev(self) -> Action {
        match self {
            Action::Start => Action::Delete,
            Action::Stop => Action::Start,
            Action::Delete => Action::Stop,
        }
    }
}

/// State of the open detail modal. The record is fetched fresh on open (and
/// after each action) so state, PID and exit code match reality.
pub struct Detail {
    pub id: String,
    pub label: String,
    /// Fetched record; None while loading or after a fetch error.
    pub sandbox: Option<Sandbox>,
    /// Fetch error (shown instead of the table).
    pub error: Option<String>,
    /// Error from the last action attempt (shown in the hint line).
    pub action_error: Option<String>,
    /// Rows scrolled past in the table.
    pub scroll: u16,
    /// Keyboard-highlighted button.
    pub selected: Action,
    /// True while a start/stop/delete request is in flight.
    pub busy: bool,
    /// The delete confirmation step is showing.
    pub confirming_delete: bool,
    /// Button rects from the last render, for mouse hit-testing.
    button_rects: Vec<(Action, Rect)>,
    /// Modal area from the last render, for click-outside-to-close.
    area: Rect,
}

impl Detail {
    pub fn loading(id: &str, label: &str) -> Self {
        Self {
            id: id.to_string(),
            label: label.to_string(),
            sandbox: None,
            error: None,
            action_error: None,
            scroll: 0,
            selected: Action::Start,
            busy: false,
            confirming_delete: false,
            button_rects: Vec::new(),
            area: Rect::default(),
        }
    }

    /// Button at a terminal position, if an enabled one was rendered there.
    pub fn action_at(&self, column: u16, row: u16) -> Option<Action> {
        if self.busy || self.confirming_delete {
            return None;
        }
        let state = self.sandbox.as_ref()?.state;
        self.button_rects
            .iter()
            .find(|(a, r)| a.enabled(state) && r.contains(Position::new(column, row)))
            .map(|(a, _)| *a)
    }

    /// Whether a terminal position falls inside the modal (outside clicks
    /// close it).
    pub fn contains(&self, column: u16, row: u16) -> bool {
        self.area.contains(Position::new(column, row))
    }

    pub fn cycle_button(&mut self, forward: bool) {
        self.selected = if forward {
            self.selected.next()
        } else {
            self.selected.prev()
        };
    }

    /// The currently highlighted action, if it may be triggered right now.
    pub fn selected_action(&self) -> Option<Action> {
        if self.busy || self.confirming_delete {
            return None;
        }
        let state = self.sandbox.as_ref()?.state;
        self.selected.enabled(state).then_some(self.selected)
    }
}

pub fn draw_detail(f: &mut Frame, det: &mut Detail) {
    let width = f.area().width.saturating_sub(8).clamp(30, 88);
    let height = f.area().height.saturating_sub(4).clamp(12, 34);
    let area = centered_rect(width, height, f.area());
    det.area = area;
    det.button_rects.clear();
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(format!(" agent: {} ", det.label));

    match &det.sandbox {
        Some(sb) => {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(0),
                    Constraint::Length(1),
                    Constraint::Length(1),
                ])
                .split(area);
            let (table_area, button_area, hint_area) = (chunks[0], chunks[1], chunks[2]);

            // --- field table (same fields as the TUI's inspect) -----------
            let pairs = detail_rows(sb);
            // Header row + its bottom margin + the two borders.
            let visible = table_area.height.saturating_sub(4) as usize;
            det.scroll = (det.scroll as usize).min(pairs.len().saturating_sub(visible)) as u16;

            let header = Row::new(["FIELD", "VALUE"])
                .style(
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                )
                .bottom_margin(1);
            let rows: Vec<Row> = pairs
                .iter()
                .map(|(k, v)| {
                    let value_style = if *k == "STATE" {
                        Style::default().fg(state_color(sb.state))
                    } else {
                        Style::default()
                    };
                    Row::new(vec![
                        Cell::from(*k).style(Style::default().fg(Color::Cyan)),
                        Cell::from(v.as_str()).style(value_style),
                    ])
                })
                .collect();
            let mut state = TableState::default();
            *state.offset_mut() = det.scroll as usize;
            let table = Table::new(rows, [Constraint::Length(14), Constraint::Min(24)])
                .header(header)
                .block(block);
            f.render_stateful_widget(table, table_area, &mut state);

            // --- buttons (or the delete confirmation) ----------------------
            if det.confirming_delete {
                let confirm = Line::from(vec![
                    Span::styled(
                        format!("  really delete {}? ", det.label),
                        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled("[y]", Style::default().fg(Color::Red)),
                    Span::raw(" confirm  "),
                    Span::styled("[n]", Style::default().fg(Color::Yellow)),
                    Span::raw(" cancel"),
                ]);
                f.render_widget(Paragraph::new(confirm), button_area);
            } else {
                draw_buttons(f, det, sb.state, button_area);
            }

            // --- hint / action error ---------------------------------------
            let hint = match &det.action_error {
                Some(e) => Line::from(Span::styled(
                    format!("  {e}"),
                    Style::default().fg(Color::Red),
                )),
                None => Line::from(vec![
                    Span::styled("  ↑/↓", Style::default().fg(Color::Yellow)),
                    Span::raw(" scroll  "),
                    Span::styled("tab", Style::default().fg(Color::Yellow)),
                    Span::raw(" button  "),
                    Span::styled("enter/click", Style::default().fg(Color::Yellow)),
                    Span::raw(" run  "),
                    Span::styled("s/x/d", Style::default().fg(Color::Yellow)),
                    Span::raw(" action  "),
                    Span::styled("q", Style::default().fg(Color::Yellow)),
                    Span::raw(" close"),
                ]),
            };
            f.render_widget(
                Paragraph::new(hint).style(Style::default().fg(Color::DarkGray)),
                hint_area,
            );
        }
        None => {
            det.button_rects.clear();
            let line = match &det.error {
                Some(e) => Line::from(Span::styled(
                    format!("  inspect failed: {e}"),
                    Style::default().fg(Color::Red),
                )),
                None => Line::from(Span::styled(
                    "  fetching…",
                    Style::default().fg(Color::DarkGray),
                )),
            };
            let para = Paragraph::new(vec![Line::raw(""), line]).block(block);
            f.render_widget(para, area);
        }
    }
}

fn draw_buttons(f: &mut Frame, det: &mut Detail, state: SandboxState, area: Rect) {
    let texts: Vec<String> = Action::ALL
        .iter()
        .map(|a| format!(" [{} {}] ", a.key(), a.label()))
        .collect();
    let mut constraints = vec![Constraint::Length(2)];
    for t in &texts {
        constraints.push(Constraint::Length(t.len() as u16));
        constraints.push(Constraint::Length(2));
    }
    constraints.push(Constraint::Min(0));
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(area);

    for (i, action) in Action::ALL.iter().enumerate() {
        let rect = chunks[1 + i * 2];
        let enabled = action.enabled(state) && !det.busy;
        let style = if !enabled {
            Style::default().fg(Color::DarkGray)
        } else if *action == det.selected {
            Style::default()
                .fg(Color::Black)
                .bg(action.color())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(action.color())
        };
        if enabled {
            det.button_rects.push((*action, rect));
        }
        f.render_widget(Paragraph::new(texts[i].as_str()).style(style), rect);
    }
}

fn state_color(state: SandboxState) -> Color {
    use SandboxState::*;
    match state {
        Running => Color::Green,
        Created => Color::Cyan,
        Exited => Color::Gray,
        Stopped => Color::Yellow,
        Failed => Color::Red,
    }
}

/// The table body: every field `mvm inspect` reports, plus the agent-specific
/// status/lineage/TTL, as label/value pairs joined into single lines.
fn detail_rows(sb: &Sandbox) -> Vec<(&'static str, String)> {
    let spec = &sb.spec;
    let dash = "-".to_string();
    let ts = |t: Option<chrono::DateTime<chrono::Utc>>| -> String {
        t.map(|t| t.format("%Y-%m-%d %H:%M:%S UTC").to_string())
            .unwrap_or_else(|| dash.clone())
    };
    let join = |items: Vec<String>| -> String {
        if items.is_empty() {
            dash.clone()
        } else {
            items.join(", ")
        }
    };
    let mounts = join(
        spec.mounts
            .iter()
            .map(|m| {
                let mut s = format!("{}:{}", m.host.display(), m.guest.display());
                if m.read_only {
                    s.push_str(":ro");
                }
                s
            })
            .collect(),
    );
    let labels = join(
        spec.labels
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect(),
    );
    let command = if spec.command.is_empty() {
        "(image default)".to_string()
    } else {
        spec.command.join(" ")
    };

    vec![
        ("ID", sb.id.to_string()),
        ("NAME", spec.name.clone().unwrap_or(dash.clone())),
        ("STATE", sb.state.to_string()),
        ("AGENT STATUS", AgentStatus::derive(sb).to_string()),
        ("PARENT", sb.parent.as_ref().map(|p| p.to_string()).unwrap_or_else(|| dash.clone())),
        ("TTL DEADLINE", ts(sb.ttl_deadline)),
        (
            "EXIT CODE",
            sb.exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| dash.clone()),
        ),
        (
            "PID",
            sb.pid
                .map(|p| p.to_string())
                .unwrap_or_else(|| dash.clone()),
        ),
        (
            "GVPROXY PID",
            sb.gvproxy_pid
                .map(|p| p.to_string())
                .unwrap_or_else(|| dash.clone()),
        ),
        ("CREATED", ts(Some(sb.created_at))),
        ("STARTED", ts(sb.started_at)),
        ("BOOTED", ts(sb.booted_at)),
        ("AGENT_READY", ts(sb.ready_at)),
        ("FINISHED", ts(sb.finished_at)),
        ("IMAGE", spec.image.clone()),
        ("COMMAND", command),
        ("VCPUS", spec.vcpus.to_string()),
        ("RAM (MiB)", spec.ram_mib.to_string()),
        ("NETWORK", spec.network.to_string()),
        ("PORTS", join(spec.ports.clone())),
        ("MOUNTS", mounts),
        ("ENV", join(spec.env.clone())),
        ("WORKDIR", spec.workdir.clone().unwrap_or(dash.clone())),
        ("USER", spec.user.clone().unwrap_or(dash.clone())),
        ("TTY", spec.tty.to_string()),
        (
            "TTY SIZE",
            sb.console_size
                .or(spec.tty_size)
                .map(|(c, r)| format!("{c}x{r}"))
                .unwrap_or_else(|| dash.clone()),
        ),
        ("ATTACH STDIN", spec.attach_stdin.to_string()),
        ("LABELS", labels),
    ]
}

/// A `width` x `height` rect centred in `area` (clamped to it).
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox(state: SandboxState) -> Sandbox {
        let mut sb = Sandbox::new(mvm_common::SandboxSpec {
            image: "alpine:latest".into(),
            ..Default::default()
        });
        sb.state = state;
        sb
    }

    #[test]
    fn action_state_gating() {
        for state in [
            SandboxState::Created,
            SandboxState::Stopped,
            SandboxState::Exited,
            SandboxState::Failed,
        ] {
            assert!(Action::Start.enabled(state), "start on {state}");
            assert!(!Action::Stop.enabled(state), "stop on {state}");
            assert!(Action::Delete.enabled(state), "delete on {state}");
        }
        assert!(!Action::Start.enabled(SandboxState::Running));
        assert!(Action::Stop.enabled(SandboxState::Running));
        assert!(Action::Delete.enabled(SandboxState::Running));
    }

    #[test]
    fn action_keys() {
        assert_eq!(Action::from_key('s'), Some(Action::Start));
        assert_eq!(Action::from_key('x'), Some(Action::Stop));
        assert_eq!(Action::from_key('d'), Some(Action::Delete));
        assert_eq!(Action::from_key('q'), None);
    }

    #[test]
    fn button_cycle_wraps() {
        let mut det = Detail::loading("id", "label");
        assert_eq!(det.selected, Action::Start);
        det.cycle_button(true);
        assert_eq!(det.selected, Action::Stop);
        det.cycle_button(true);
        assert_eq!(det.selected, Action::Delete);
        det.cycle_button(true);
        assert_eq!(det.selected, Action::Start);
        det.cycle_button(false);
        assert_eq!(det.selected, Action::Delete);
    }

    #[test]
    fn hit_test_respects_gating() {
        let mut det = Detail::loading("id", "label");
        det.sandbox = Some(sandbox(SandboxState::Running));
        det.button_rects = vec![
            (Action::Start, Rect::new(0, 0, 10, 1)),
            (Action::Stop, Rect::new(12, 0, 10, 1)),
            (Action::Delete, Rect::new(24, 0, 10, 1)),
        ];
        // Running: start is disabled, so its rect is not clickable.
        assert_eq!(det.action_at(5, 0), None);
        assert_eq!(det.action_at(15, 0), Some(Action::Stop));
        assert_eq!(det.action_at(27, 0), Some(Action::Delete));
        assert_eq!(det.action_at(50, 5), None);

        // While busy or confirming, nothing is clickable.
        det.busy = true;
        assert_eq!(det.action_at(15, 0), None);
        det.busy = false;
        det.confirming_delete = true;
        assert_eq!(det.action_at(15, 0), None);
    }

    #[test]
    fn selected_action_respects_gating() {
        let mut det = Detail::loading("id", "label");
        // No record yet: nothing can run.
        assert_eq!(det.selected_action(), None);
        det.sandbox = Some(sandbox(SandboxState::Created));
        assert_eq!(det.selected_action(), Some(Action::Start));
        det.cycle_button(true); // Stop, disabled on Created
        assert_eq!(det.selected_action(), None);
    }
}
