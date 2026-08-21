//! Rendering for the mvm TUI.

use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Tabs, Wrap,
};
use ratatui::Frame;

use crate::app::{App, InspectPane, ResizeField, Tab};

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
    let Some(confirm) = app.confirm_delete.as_ref() else {
        return;
    };
    let area = centered_rect(54, 9, f.area());
    f.render_widget(Clear, area);

    let mut lines = vec![
        Line::from(vec![
            Span::raw("  delete sandbox "),
            Span::styled(
                &confirm.label,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
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
        Span::styled(
            "y",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" / "),
        Span::styled(
            "enter",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
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
    let Some(form) = app.resize.as_ref() else {
        return;
    };
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
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
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

/// Modal `mvm inspect` viewer: the full sandbox record as a key/value table,
/// with the lifecycle latency flamegraph below it.
fn draw_inspect(f: &mut Frame, app: &mut App) {
    let Some(ins) = app.inspect.as_mut() else {
        return;
    };
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
            // Two equal panes: the field table and the lifecycle flamegraph.
            // `tab` focuses one; ↑/↓ scroll it.
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Percentage(50),
                    Constraint::Percentage(50),
                    Constraint::Length(1),
                ])
                .split(area);
            let (info_area, flame_area, hint_area) = (chunks[0], chunks[1], chunks[2]);

            // --- info pane -------------------------------------------------
            let info_block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(if ins.pane == InspectPane::Info {
                    Color::Cyan
                } else {
                    Color::DarkGray
                }))
                .title(format!(" inspect: {} ", ins.label));
            let pairs = inspect_rows(sb);
            // Header row + its bottom margin + the two borders.
            let visible = info_area.height.saturating_sub(4) as usize;
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
                .block(info_block);
            f.render_stateful_widget(table, info_area, &mut state);

            // --- flamegraph pane -------------------------------------------
            let flame_block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(if ins.pane == InspectPane::Flame {
                    Color::Cyan
                } else {
                    Color::DarkGray
                }))
                .title(" lifecycle timings ");
            let lines = flame_lines(
                &sb.timeline,
                flame_area.width.saturating_sub(2) as usize,
            );
            let viewport = flame_area.height.saturating_sub(2) as usize;
            ins.flame_scroll =
                (ins.flame_scroll as usize).min(lines.len().saturating_sub(viewport)) as u16;
            let para = Paragraph::new(lines)
                .scroll((ins.flame_scroll, 0))
                .block(flame_block);
            f.render_widget(para, flame_area);

            // --- hint ------------------------------------------------------
            let hint = Line::from(vec![
                Span::styled("tab", Style::default().fg(Color::Yellow)),
                Span::raw(" pane  "),
                Span::styled("↑/↓", Style::default().fg(Color::Yellow)),
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

/// One `Line` per flamegraph row. Each lifecycle is a `start` boot (the only
/// thing worth tracing): a bar spanning `start_start` → `start_stop` with its
/// \>1ms phases laid out sequentially, a legend of those phase timings, and the
/// point events as timestamped lines below the bar (`agent_ready`, `stop`).
fn flame_lines(timeline: &[Vec<mvm_common::TimelineEvent>], bar_w: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    if timeline.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no lifecycle timing recorded yet",
            Style::default().fg(Color::DarkGray),
        )));
        return lines;
    }

    for lifecycle in timeline {
        let is_start = lifecycle
            .first()
            .map(|e| e.event == "start_start")
            .unwrap_or(false);
        if is_start {
            lines.push(flame_bar_line(lifecycle, bar_w));
            lines.push(flame_legend(lifecycle));
            // Point events (agent_ready, stop) sit after the boot bar, each on
            // its own timestamped line, in chronological order.
            let boot_end = lifecycle
                .iter()
                .find(|e| e.event == "start_stop")
                .map(|e| e.at);
            for e in lifecycle {
                if is_point_event(&e.event) {
                    lines.push(point_event_line(e, boot_end));
                }
            }
        } else {
            for e in lifecycle {
                lines.push(point_event_line(e, None));
            }
        }
    }

    lines
}

/// `HH:MM:SS  start  ████████  1234ms` — the boot bar. It spans
/// `start_start` → `start_stop`; phases that lasted > 1ms are laid out
/// sequentially (each ≥ 1 column so narrow phases stay visible).
fn flame_bar_line(events: &[mvm_common::TimelineEvent], width: usize) -> Line<'static> {
    let start = events.first().expect("lifecycle has events");
    let ts = start.at.format("%H:%M:%S").to_string();
    let label = format!("{:<6}", "start");

    let boot_end = events
        .iter()
        .find(|e| e.event == "start_stop")
        .map(|e| e.at)
        .unwrap_or(start.at);
    let span_ms = (boot_end.timestamp_millis() - start.at.timestamp_millis()).max(1) as f64;
    let total_ms = span_ms as u64;
    let total = format!("{:>6}ms", total_ms);

    let bar_w = width.saturating_sub(ts.len() + label.len() + total.len() + 3);

    let mut spans = vec![
        Span::styled(ts, Style::default().fg(Color::DarkGray)),
        Span::raw(" "),
        Span::styled(label, Style::default().fg(Color::Gray)),
    ];
    spans.extend(bar_segments(events, bar_w));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(total, Style::default().fg(Color::Gray)));
    Line::from(spans)
}

/// The >1ms phases of one boot bar, laid out sequentially: each phase gets a
/// segment proportional to its duration relative to the drawn phases (each at
/// least one column so narrow phases stay visible), and the last phase absorbs
/// the rounding remainder so the bar always fills exactly `cols`. Phases ≤ 1ms
/// (absent from the legend) are skipped.
fn bar_segments(events: &[mvm_common::TimelineEvent], cols: usize) -> Vec<Span<'static>> {
    if cols == 0 {
        return Vec::new();
    }

    // Collect (phase, duration) for every phase that lasted > 1ms, in
    // chronological order.
    let mut phase_starts: std::collections::HashMap<&str, chrono::DateTime<chrono::Utc>> =
        std::collections::HashMap::new();
    let mut phases: Vec<(&str, u64)> = Vec::new();
    for e in events {
        let Some(phase) = phase_of(&e.event) else {
            continue;
        };
        if e.event.ends_with("_start") {
            phase_starts.insert(phase, e.at);
        } else if let Some(start) = phase_starts.remove(phase) {
            let us = (e.at - start).num_microseconds().unwrap_or(0).max(0);
            let ms = (us as f64 / 1000.0).round() as u64;
            if ms > 1 {
                phases.push((phase, ms));
            }
        }
    }

    let total_ms: u64 = phases.iter().map(|(_, ms)| ms).sum();
    let mut cells: Vec<Option<Color>> = vec![None; cols];
    let mut col = 0usize;
    for (i, (phase, ms)) in phases.iter().enumerate() {
        if col >= cols {
            break;
        }
        let last = i + 1 == phases.len();
        let width = if last {
            cols - col
        } else {
            let want = ((*ms as f64 / total_ms as f64) * cols as f64).round() as usize;
            want.max(1).min(cols - col)
        };
        for cell in cells.iter_mut().skip(col).take(width) {
            *cell = Some(phase_color(phase));
        }
        col += width;
    }

    // Group consecutive blanks and colored runs into spans.
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut i = 0;
    while i < cols {
        match cells[i] {
            None => {
                let mut j = i;
                while j < cols && cells[j].is_none() {
                    j += 1;
                }
                spans.push(Span::raw(" ".repeat(j - i)));
                i = j;
            }
            Some(color) => {
                let mut j = i;
                while j < cols && cells[j] == Some(color) {
                    j += 1;
                }
                spans.push(Span::styled("█".repeat(j - i), Style::default().fg(color)));
                i = j;
            }
        }
    }
    spans
}

/// One legend line per boot bar: the ms timing of every phase that lasted
/// > 1ms. Phases that didn't reach 1ms are not worth reporting.
fn flame_legend(events: &[mvm_common::TimelineEvent]) -> Line<'static> {
    let mut spans = vec![Span::raw("   ")];
    let mut phase_starts: std::collections::HashMap<&str, chrono::DateTime<chrono::Utc>> =
        std::collections::HashMap::new();
    let mut phase_durs: Vec<(&str, u64)> = Vec::new();
    for e in events {
        let Some(phase) = phase_of(&e.event) else {
            continue;
        };
        if e.event.ends_with("_start") {
            phase_starts.insert(phase, e.at);
        } else if let Some(start) = phase_starts.remove(phase) {
            let us = (e.at - start).num_microseconds().unwrap_or(0).max(0);
            let ms = (us as f64 / 1000.0).round() as u64;
            phase_durs.push((phase, ms));
        }
    }
    let shown: Vec<_> = phase_durs.into_iter().filter(|(_, ms)| *ms > 1).collect();
    if shown.is_empty() {
        spans.push(Span::styled("no phases", Style::default().fg(Color::DarkGray)));
    } else {
        for (name, ms) in shown {
            spans.push(Span::styled("█", Style::default().fg(phase_color(name))));
            spans.push(Span::raw(format!(" {name}={ms}ms  ")));
        }
    }
    Line::from(spans)
}

/// A timestamped line for a point event (`agent_ready`, `stop`) below the
/// boot bar. For `agent_ready`, the time elapsed since `start_stop`
/// (`boot_end`) is shown as `+Nms`.
fn point_event_line(
    e: &mvm_common::TimelineEvent,
    boot_end: Option<chrono::DateTime<chrono::Utc>>,
) -> Line<'static> {
    let ts = e.at.format("%H:%M:%S").to_string();
    let label = match e.event.as_str() {
        "agent_ready" => "✓ agent_ready".to_string(),
        other => other.to_string(),
    };
    let mut spans = vec![
        Span::styled(ts, Style::default().fg(Color::DarkGray)),
        Span::raw("  "),
        Span::styled(label, Style::default().fg(event_color(&e.event))),
    ];
    if e.event == "agent_ready" {
        if let Some(boot_end) = boot_end {
            let after_ms = e.at.signed_duration_since(boot_end).num_milliseconds().max(0) as u64;
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                format!("+{after_ms}ms"),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }
    Line::from(spans)
}

/// Whether an event is a point-in-time signal (`agent_ready`, `stop`) rather
/// than a phase boundary.
fn is_point_event(name: &str) -> bool {
    !name.ends_with("_start") && !name.ends_with("_stop")
}

/// The phase name of a boundary event (`rootfs_start` → "rootfs"), or `None`
/// for point-in-time events. The root span (`start`) is excluded: it defines
/// the bar itself (`start_start` → `start_stop`) and is never drawn as a
/// segment nor listed in the legend.
fn phase_of(name: &str) -> Option<&str> {
    let phase = name
        .strip_suffix("_start")
        .or_else(|| name.strip_suffix("_stop"))?;
    if phase == "start" {
        return None;
    }
    Some(phase)
}

/// Color for a point event's label.
fn event_color(name: &str) -> Color {
    match name {
        "agent_ready" => Color::Cyan,
        _ => Color::Gray,
    }
}

/// A fixed color per phase *name*, so `rootfs` is always the same color in
/// every op and every lifecycle record — never dependent on a phase's rank.
/// Known phases get hand-picked (visually distinct) colors; anything else is
/// hashed deterministically into the same palette.
fn phase_color(name: &str) -> Color {
    const PALETTE: [Color; 8] = [
        Color::Cyan,
        Color::Magenta,
        Color::Yellow,
        Color::Green,
        Color::Blue,
        Color::Red,
        Color::LightBlue,
        Color::LightMagenta,
    ];
    match name {
        "validate" => Color::Cyan,
        "register" => Color::LightCyan,
        "disk" => Color::Blue,
        "rootfs" => Color::Yellow,
        "guestd" => Color::Magenta,
        "gvproxy" => Color::LightMagenta,
        "ports" => Color::LightBlue,
        "shim" => Color::Green,
        "boot" => Color::Red,
        "persist" => Color::DarkGray,
        "terminate" => Color::LightRed,
        other => PALETTE[fnv32(other) % PALETTE.len()],
    }
}

/// FNV-1a 32-bit — deterministic across runs and processes, so unknown phase
/// names always resolve to the same color.
fn fnv32(s: &str) -> usize {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.bytes() {
        h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
    }
    h as usize
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
    let header = Row::new([
        "ID", "NAME", "IMAGE", "STATE", "EXIT", "VCPU/RAM", "COMMAND",
    ])
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
                Cell::from(
                    sb.exit_code
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "-".into()),
                ),
                Cell::from(format!("{}/{}MiB", sb.spec.vcpus, sb.spec.ram_mib)),
                Cell::from(if cmd.is_empty() {
                    "(image default)".into()
                } else {
                    cmd
                }),
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

#[cfg(test)]
mod tests {
    use super::*;
    use mvm_common::TimelineEvent;

    fn ts(seconds: i64) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp(seconds, 0).unwrap()
    }

    fn ev(event: &str, at_seconds: i64) -> TimelineEvent {
        TimelineEvent {
            event: event.to_string(),
            at: ts(at_seconds),
        }
    }

    fn ev_ms(event: &str, at_seconds: i64, millis: i64) -> TimelineEvent {
        TimelineEvent {
            event: event.to_string(),
            at: ts(at_seconds) + chrono::Duration::milliseconds(millis),
        }
    }

    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn bar_colors(line: &Line) -> Vec<Color> {
        line.spans.iter().filter_map(|s| s.style.fg).collect()
    }

    /// A realistic start lifecycle: start_start, rootfs (1s) and boot (2s)
    /// phases, start_stop, then the point events (agent_ready, stop) appended
    /// by the manager.
    fn start_lifecycle(start_t: i64) -> Vec<TimelineEvent> {
        vec![
            ev("start_start", start_t),
            ev("rootfs_start", start_t),
            ev("rootfs_stop", start_t + 1), // +1s
            ev("boot_start", start_t + 1),
            ev("boot_stop", start_t + 3), // +3s
            ev("start_stop", start_t + 3),
        ]
    }

    #[test]
    fn empty_timeline_renders_nothing() {
        let lines = flame_lines(&[], 80);
        assert_eq!(lines.len(), 1);
        assert!(line_text(&lines[0]).contains("no lifecycle"));
    }

    #[test]
    fn bar_spans_start_to_start_stop_ignoring_point_events() {
        // agent_ready and stop sit after start_stop: they must not move the
        // bar's total (regression: the total used to equal the agent_ready
        // offset).
        let mut lifecycle = start_lifecycle(100);
        lifecycle.push(ev("agent_ready", 110)); // +10s after boot start
        lifecycle.push(ev("stop", 120));
        let bar = line_text(&flame_bar_line(&lifecycle, 80));
        assert!(bar.contains("3000ms"), "bar: {bar}");
        assert!(!bar.contains("10000ms"), "total leaked agent_ready: {bar}");
    }

    #[test]
    fn legend_lists_only_phases_over_1ms() {
        // rootfs: 1s, guestd: 0ms (below the floor), boot: 2s.
        let lifecycle = vec![
            ev("start_start", 200),
            ev("rootfs_start", 200),
            ev("rootfs_stop", 201), // +1s
            ev("guestd_start", 201),
            ev("guestd_stop", 201), // +0ms
            ev("boot_start", 201),
            ev("boot_stop", 203), // +2s
            ev("start_stop", 203),
        ];
        let legend = line_text(&flame_legend(&lifecycle));
        assert!(legend.contains("rootfs=1000ms"), "legend: {legend}");
        assert!(legend.contains("boot=2000ms"), "legend: {legend}");
        assert!(!legend.contains("guestd"), "sub-1ms phase leaked: {legend}");
    }

    #[test]
    fn point_events_render_below_the_bar() {
        // agent_ready sits 3s after start_stop; stop is a plain timestamp.
        let mut lifecycle = start_lifecycle(300);
        lifecycle.push(ev("agent_ready", 306));
        lifecycle.push(ev("stop", 400));
        let lines = flame_lines(&[lifecycle], 80);
        // bar + legend + agent_ready + stop = 4 lines.
        assert_eq!(lines.len(), 4);
        let ready = line_text(&lines[2]);
        assert!(ready.contains("✓ agent_ready"), "ready: {ready}");
        assert!(ready.contains("+3000ms"), "ready: {ready}");
        let stop = line_text(&lines[3]);
        assert!(stop.contains("stop"), "stop: {stop}");
        assert!(!stop.contains("+"), "stop has no duration: {stop}");
    }

    #[test]
    fn root_span_start_is_never_a_segment_or_legend_entry() {
        // start_start/start_stop define the bar; "start" must not be drawn as
        // a segment (it would overwrite every other phase) nor listed in the
        // legend as start=... (it is the total, which is already on the bar).
        let lifecycle = vec![
            ev("start_start", 400),
            ev("rootfs_start", 400),
            ev("rootfs_stop", 401), // +1s
            ev("boot_start", 401),
            ev("boot_stop", 403), // +2s
            ev("start_stop", 403),
        ];
        let bar = line_text(&flame_bar_line(&lifecycle, 40));
        assert!(bar.contains("3000ms"), "bar: {bar}");
        // Both sub-phases survive: the root span must not have painted over them.
        let colors = bar_colors(&flame_bar_line(&lifecycle, 40));
        assert!(colors.contains(&Color::Yellow), "rootfs painted over: {colors:?}");
        assert!(colors.contains(&Color::Red), "boot painted over: {colors:?}");
        let legend = line_text(&flame_legend(&lifecycle));
        assert!(!legend.contains("start="), "root span in legend: {legend}");
        assert!(legend.contains("rootfs=1000ms"), "legend: {legend}");
        assert!(legend.contains("boot=2000ms"), "legend: {legend}");
    }

    #[test]
    fn narrow_phase_still_gets_a_sliver() {
        // ports lasts 2ms inside a 3s boot: far too narrow for a proportional
        // column, but it's in the legend (>1ms) so it still gets a segment —
        // laid out sequentially it's at least one column wide, right after
        // rootfs, so it stays visible.
        let lifecycle = vec![
            ev("start_start", 500),
            ev("rootfs_start", 500),
            ev("rootfs_stop", 501), // +1s
            ev("ports_start", 501),
            ev_ms("ports_stop", 501, 2), // +2ms
            ev_ms("boot_start", 501, 2),
            ev("boot_stop", 503), // +2s
            ev("start_stop", 503),
        ];
        let colors = bar_colors(&flame_bar_line(&lifecycle, 40));
        assert!(colors.contains(&Color::LightBlue), "ports sliver missing: {colors:?}");
    }

    #[test]
    fn bar_always_fills_the_full_width() {
        // Two boot bars with very different totals (3s vs 30s) must render the
        // bar at the same fixed width — no trailing gap, no right-alignment.
        let short = start_lifecycle(100);
        let mut long = start_lifecycle(200);
        long.push(ev("agent_ready", 230));
        long.push(ev("stop", 300));
        let a = line_text(&flame_bar_line(&short, 80));
        let b = line_text(&flame_bar_line(&long, 80));
        // Everything before the first "ms" (the total) is the fixed-width bar.
        let bar_a = a.split("ms").next().unwrap().to_string();
        let bar_b = b.split("ms").next().unwrap().to_string();
        assert_eq!(bar_a.len(), bar_b.len(), "bars differ in length\nA: {a}\nB: {b}");
    }

    #[test]
    fn phase_color_is_fixed_per_name_not_rank() {
        assert_eq!(phase_color("rootfs"), Color::Yellow);
        assert_eq!(phase_color("guestd"), Color::Magenta);
        assert_eq!(phase_color("boot"), Color::Red);
        assert_eq!(phase_color("future-phase"), phase_color("future-phase"));
    }
}
