//! Rendering for the mvm TUI.

use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Tabs, Wrap};
use ratatui::Frame;

use crate::app::{App, ResizeField, Tab};

pub fn draw(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Percentage(35),
            Constraint::Length(2),
        ])
        .split(f.area());

    // Header tabs.
    let titles = [" Sandboxes ", " Images "]
        .iter()
        .map(|t| Line::from(*t))
        .collect::<Vec<_>>();
    let selected = match app.tab {
        Tab::Sandboxes => 0,
        Tab::Images => 1,
    };
    let tabs = Tabs::new(titles)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" mvm — microVM sandboxes "),
        )
        .select(selected)
        .style(Style::default().fg(Color::Gray))
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, chunks[0]);

    // Main table.
    match app.tab {
        Tab::Sandboxes => draw_sandboxes(f, app, chunks[1]),
        Tab::Images => draw_images(f, app, chunks[1]),
    }

    // Logs pane.
    let log_lines: Vec<Line> = app
        .logs
        .lines()
        .rev()
        .take(chunks[2].height.saturating_sub(2) as usize)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(Line::from)
        .collect();
    let logs = Paragraph::new(log_lines)
        .block(Block::default().borders(Borders::ALL).title(" console "))
        .wrap(Wrap { trim: false });
    f.render_widget(logs, chunks[2]);

    // Footer.
    let (message, message_is_error) = app.footer_message();
    let footer = Paragraph::new(Line::from(vec![
        Span::styled(" q", Style::default().fg(Color::Yellow)),
        Span::raw(" quit  "),
        Span::styled("tab", Style::default().fg(Color::Yellow)),
        Span::raw(" switch  "),
        Span::styled("j/k", Style::default().fg(Color::Yellow)),
        Span::raw(" move  "),
        Span::styled("s", Style::default().fg(Color::Yellow)),
        Span::raw(" start  "),
        Span::styled("x", Style::default().fg(Color::Yellow)),
        Span::raw(" stop  "),
        Span::styled("r", Style::default().fg(Color::Yellow)),
        Span::raw(" resize  "),
        Span::styled("d", Style::default().fg(Color::Yellow)),
        Span::raw(" delete   "),
        Span::styled(
            message,
            Style::default().fg(if message_is_error {
                Color::Red
            } else {
                Color::Green
            }),
        ),
    ]));
    f.render_widget(footer, chunks[3]);

    // Modal last, so it sits on top of everything.
    if app.resize.is_some() {
        draw_resize(f, app);
    }
}

/// Modal vcpu/RAM editor for the selected sandbox.
fn draw_resize(f: &mut Frame, app: &App) {
    let Some(form) = app.resize.as_ref() else { return };
    let area = centered_rect(52, 11, f.area());
    f.render_widget(Clear, area);

    let field = |label: &str, value: &str, active: bool| {
        let value_style = if active {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        Line::from(vec![
            Span::raw(format!("  {label:8}")),
            Span::styled(format!(" {value:>7} "), value_style),
        ])
    };

    let mut lines = vec![
        Line::from(vec![Span::styled(
            format!("  {}", form.label),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        )]),
        Line::raw(""),
        field("vCPUs", &form.vcpus, form.field == ResizeField::Vcpus),
        field("MiB RAM", &form.ram_mib, form.field == ResizeField::Ram),
        Line::raw(""),
    ];
    if form.running {
        lines.push(Line::from(Span::styled(
            "  running — the VM keeps its size until reboot",
            Style::default().fg(Color::Yellow),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "  applies on next start",
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("tab", Style::default().fg(Color::Yellow)),
        Span::raw(" field  "),
        Span::styled("+/-", Style::default().fg(Color::Yellow)),
        Span::raw(" adjust  "),
        Span::styled("digits", Style::default().fg(Color::Yellow)),
        Span::raw(" type"),
    ]));
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("enter", Style::default().fg(Color::Yellow)),
        Span::raw(" apply  "),
        Span::styled("^r", Style::default().fg(Color::Yellow)),
        Span::raw(" apply+restart  "),
        Span::styled("esc", Style::default().fg(Color::Yellow)),
        Span::raw(" cancel"),
    ]));

    let popup = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" resize microVM "),
    );
    f.render_widget(popup, area);
}

/// A `width` x `height` rect centred in `area` (clamped to it).
fn centered_rect(width: u16, height: u16, area: ratatui::layout::Rect) -> ratatui::layout::Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    ratatui::layout::Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

fn state_color(state: mvm_common::SandboxState) -> Color {
    use mvm_common::SandboxState::*;
    match state {
        Running => Color::Green,
        Created => Color::Cyan,
        Exited => Color::Gray,
        Stopped => Color::Yellow,
        Failed => Color::Red,
    }
}

fn draw_sandboxes(f: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    let header = Row::new(["ID", "NAME", "IMAGE", "STATE", "EXIT", "VCPU/RAM", "COMMAND"])
        .style(Style::default().add_modifier(Modifier::BOLD))
        .bottom_margin(1);
    let rows: Vec<Row> = app
        .sandboxes
        .iter()
        .map(|sb| {
            let cmd = sb.spec.command.join(" ");
            let cells = vec![
                Cell::from(sb.id.to_string()),
                Cell::from(sb.spec.name.clone().unwrap_or_else(|| "-".into())),
                Cell::from(sb.spec.image.clone()),
                Cell::from(sb.state.to_string()).style(Style::default().fg(state_color(sb.state))),
                Cell::from(sb.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "-".into())),
                Cell::from(format!("{}/{}MiB", sb.spec.vcpus, sb.spec.ram_mib)),
                Cell::from(if cmd.is_empty() { "(image default)".into() } else { cmd }),
            ];
            Row::new(cells)
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(14),
            Constraint::Length(16),
            Constraint::Length(24),
            Constraint::Length(10),
            Constraint::Length(6),
            Constraint::Length(11),
            Constraint::Min(10),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL))
    .row_highlight_style(Style::default().bg(Color::DarkGray));
    f.render_stateful_widget(table, area, &mut app.table_state);
}

fn draw_images(f: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    let header = Row::new(["IMAGE", "DIGEST", "SIZE", "PULLED"])
        .style(Style::default().add_modifier(Modifier::BOLD))
        .bottom_margin(1);
    let rows: Vec<Row> = app
        .images
        .iter()
        .map(|img| {
            Row::new(vec![
                Cell::from(img.reference.clone()),
                Cell::from(img.digest.chars().take(19).collect::<String>()),
                Cell::from(human_size(img.size)),
                Cell::from(img.created_at.format("%Y-%m-%d %H:%M").to_string()),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Min(20),
            Constraint::Length(22),
            Constraint::Length(10),
            Constraint::Length(18),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL))
    .row_highlight_style(Style::default().bg(Color::DarkGray));
    f.render_stateful_widget(table, area, &mut app.table_state);
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1}{}", UNITS[unit])
}
