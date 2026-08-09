//! Rendering for the mvm TUI.

use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Tabs, Wrap,
};
use ratatui::Frame;

use crate::app::{App, ResizeField, Tab};

pub fn draw(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
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
        Span::raw(" delete  "),
        Span::styled("i", Style::default().fg(Color::Yellow)),
        Span::raw(" inspect   "),
        Span::styled(
            message,
            Style::default().fg(if message_is_error {
                Color::Red
            } else {
                Color::Green
            }),
        ),
    ]))
    // The hints must not fall off a narrow terminal; the footer has 2 rows.
    .wrap(Wrap { trim: true });
    f.render_widget(footer, chunks[2]);

    // Modals last, so they sit on top of everything.
    if app.resize.is_some() {
        draw_resize(f, app);
    }
    if app.confirm_delete.is_some() {
        draw_confirm_delete(f, app);
    }
    if app.inspect.is_some() {
        draw_inspect(f, app);
    }
}

/// "Really delete this?" — removing a sandbox destroys its filesystem.
fn draw_confirm_delete(f: &mut Frame, app: &App) {
    let Some(confirm) = app.confirm_delete.as_ref() else { return };
    let area = centered_rect(54, 9, f.area());
    f.render_widget(Clear, area);

    let mut lines = vec![
        Line::from(vec![
            Span::raw("  delete sandbox "),
            Span::styled(
                &confirm.label,
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            ),
            Span::raw("?"),
        ]),
        Line::raw(""),
        Line::from(Span::styled(
            "  its filesystem goes with it — this cannot be undone",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    if confirm.running {
        lines.push(Line::from(Span::styled(
            "  it is running and will be stopped first",
            Style::default().fg(Color::Yellow),
        )));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("y", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
        Span::raw(" delete  "),
        Span::styled("n", Style::default().fg(Color::Yellow)),
        Span::raw(" / "),
        Span::styled("esc", Style::default().fg(Color::Yellow)),
        Span::raw(" cancel"),
    ]));

    let popup = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Red))
            .title(" confirm delete "),
    );
    f.render_widget(popup, area);
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

/// Modal `mvm inspect` viewer: the full sandbox record as a key/value table.
fn draw_inspect(f: &mut Frame, app: &mut App) {
    let Some(ins) = app.inspect.as_mut() else { return };
    let area = centered_rect(
        f.area().width.saturating_sub(8).max(20),
        f.area().height.saturating_sub(6).max(8),
        f.area(),
    );
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(format!(" inspect: {} ", ins.label));

    match &ins.sandbox {
        Some(sb) => {
            let pairs = inspect_rows(sb);
            // Header row + its bottom margin + the two borders.
            let visible = area.height.saturating_sub(4) as usize;
            ins.scroll = (ins.scroll as usize).min(pairs.len().saturating_sub(visible)) as u16;

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
            *state.offset_mut() = ins.scroll as usize;
            let table = Table::new(rows, [Constraint::Length(14), Constraint::Min(24)])
                .header(header)
                .block(block);
            f.render_stateful_widget(table, area, &mut state);
        }
        None => {
            let line = match &ins.error {
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

/// The inspect table body: every field `mvm inspect` reports, as label/value
/// pairs. Values are joined into single lines so the table rows stay flat.
fn inspect_rows(sb: &mvm_common::Sandbox) -> Vec<(&'static str, String)> {
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
    let labels = join(spec.labels.iter().map(|(k, v)| format!("{k}={v}")).collect());
    let command = if spec.command.is_empty() {
        "(image default)".to_string()
    } else {
        spec.command.join(" ")
    };

    vec![
        ("ID", sb.id.to_string()),
        ("NAME", spec.name.clone().unwrap_or(dash.clone())),
        ("STATE", sb.state.to_string()),
        (
            "EXIT CODE",
            sb.exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| dash.clone()),
        ),
        (
            "PID",
            sb.pid.map(|p| p.to_string()).unwrap_or_else(|| dash.clone()),
        ),
        (
            "GVPROXY PID",
            sb.gvproxy_pid
                .map(|p| p.to_string())
                .unwrap_or_else(|| dash.clone()),
        ),
        ("CREATED", ts(Some(sb.created_at))),
        ("STARTED", ts(sb.started_at)),
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
            spec.tty_size
                .map(|(c, r)| format!("{c}x{r}"))
                .unwrap_or_else(|| dash.clone()),
        ),
        ("ATTACH STDIN", spec.attach_stdin.to_string()),
        ("LABELS", labels),
    ]
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
