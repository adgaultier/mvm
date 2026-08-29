//! Modal agent detail view: the full sandbox record (the same data as the
//! TUI's inspect modal). Read-only — lifecycle actions live in the context
//! menu's Actions section.

use mvm_common::agent_api::AgentStatus;
use mvm_common::{Sandbox, SandboxState};
use ratatui::layout::{Constraint, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState};
use ratatui::Frame;

/// State of the open detail modal. The record is fetched fresh on open so
/// state, PID and exit code match reality.
pub struct Detail {
    pub id: String,
    pub label: String,
    /// Fetched record; None while loading or after a fetch error.
    pub sandbox: Option<Sandbox>,
    /// Fetch error (shown instead of the table).
    pub error: Option<String>,
    /// Rows scrolled past in the table.
    pub scroll: u16,
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
            scroll: 0,
            area: Rect::default(),
        }
    }

    /// Whether a terminal position falls inside the modal (outside clicks
    /// close it).
    pub fn contains(&self, column: u16, row: u16) -> bool {
        self.area.contains(Position::new(column, row))
    }
}

pub fn draw_detail(f: &mut Frame, det: &mut Detail) {
    let width = f.area().width.saturating_sub(8).clamp(30, 88);
    let height = f.area().height.saturating_sub(4).clamp(12, 34);
    let area = centered_rect(width, height, f.area());
    det.area = area;
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(format!(" agent: {} ", det.label));

    match &det.sandbox {
        Some(sb) => {
            let inner = block.inner(area);
            f.render_widget(block, area);

            let chunks = ratatui::layout::Layout::default()
                .direction(ratatui::layout::Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(1)])
                .split(inner);
            let (table_area, hint_area) = (chunks[0], chunks[1]);

            // --- field table (same fields as the TUI's inspect) -----------
            let pairs = detail_rows(sb);
            // Header row + its bottom margin.
            let visible = table_area.height.saturating_sub(2) as usize;
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
                .header(header);
            f.render_stateful_widget(table, table_area, &mut state);

            // --- hint ------------------------------------------------------
            let hint = Line::from(vec![
                Span::styled("  ↑/↓", Style::default().fg(Color::Yellow)),
                Span::raw(" scroll  "),
                Span::styled("q", Style::default().fg(Color::Yellow)),
                Span::raw(" close"),
            ]);
            f.render_widget(
                Paragraph::new(hint).style(Style::default().fg(Color::DarkGray)),
                hint_area,
            );
        }
        None => {
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
        ("PARENT", sb.agent.parent.as_ref().map(|p| p.to_string()).unwrap_or_else(|| dash.clone())),
        ("TTL DEADLINE", ts(sb.agent.ttl_deadline)),
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
pub fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}
