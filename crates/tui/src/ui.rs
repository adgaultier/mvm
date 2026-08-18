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
            // `tab` focuses one; j/k scroll it.
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
            let ops = visible_lifecycle(sb);
            let running = sb.state == mvm_common::SandboxState::Running;
            let lines = flame_lines(
                &ops,
                running,
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
                Span::styled("j/k", Style::default().fg(Color::Yellow)),
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

/// Which lifecycle ops to render: the full recorded history, oldest first.
/// Nothing is hidden — a sandbox that has been started and stopped a few times
/// shows every op, and the pane scrolls when they don't fit.
fn visible_lifecycle(sb: &mvm_common::Sandbox) -> Vec<&mvm_common::LifecycleOp> {
    sb.lifecycle.iter().collect()
}

/// One `Line` per flamegraph row: a `start` immediately followed by a `stop`
/// is collapsed onto one line (boot -> teardown side by side); everything
/// else — `create`, or a `start` with no following `stop` (a cycle that ended
/// by exit, or the still-running boot) — gets its own line. Each op/cycle
/// keeps a legend line beneath it.
fn flame_lines(
    ops: &[&mvm_common::LifecycleOp],
    running: bool,
    bar_w: usize,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    if ops.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no lifecycle timing recorded yet",
            Style::default().fg(Color::DarkGray),
        )));
        return lines;
    }
    let mut i = 0;
    while i < ops.len() {
        let op = ops[i];
        if op.op == "start" && ops.get(i + 1).map(|n| n.op.as_str()) == Some("stop") {
            let stop = ops[i + 1];
            lines.push(flame_pair_line(op, stop, bar_w));
            lines.push(flame_pair_legend(op, stop));
            i += 2;
        } else {
            let live = running && i + 1 == ops.len() && op.op == "start";
            lines.push(flame_bar_line(op, bar_w, live));
            lines.push(flame_legend(op));
            i += 1;
        }
    }
    lines
}

/// `HH:MM:SS  start  ████…████  1234ms` — timestamp left, total right; `live`
/// appends a running marker for the current boot that has no `stop` yet.
fn flame_bar_line(op: &mvm_common::LifecycleOp, width: usize, live: bool) -> Line<'static> {
    let ts = op.at.format("%H:%M:%S").to_string();
    let label = format!("{:<6}", op.op);
    let total = format!("{:>6}ms", op.total_ms);
    let bar_w = width.saturating_sub(ts.len() + label.len() + total.len() + 3);

    let mut spans = vec![
        Span::styled(ts, Style::default().fg(Color::DarkGray)),
        Span::raw(" "),
        Span::styled(label, Style::default().fg(Color::Gray)),
    ];
    spans.extend(bar_spans(op, bar_w));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(total, Style::default().fg(Color::Gray)));
    if live {
        spans.push(Span::styled(" ▶ running", Style::default().fg(Color::Green)));
    }
    Line::from(spans)
}

/// `HH:MM:SS  start ████…█ 1234ms   stop ████ 300ms` — the boot and teardown
/// of one lifecycle cycle side by side, each bar scaled to its own total.
fn flame_pair_line(
    start: &mvm_common::LifecycleOp,
    stop: &mvm_common::LifecycleOp,
    width: usize,
) -> Line<'static> {
    let ts = start.at.format("%H:%M:%S").to_string();
    let start_label = format!("{:<6}", "start");
    let stop_label = format!("{:<6}", "stop");
    let start_total = format!("{:>6}ms", start.total_ms);
    let stop_total = format!("{:>6}ms", stop.total_ms);
    let fixed = ts.len()
        + 1
        + start_label.len()
        + 1
        + start_total.len()
        + 3
        + stop_label.len()
        + 1
        + stop_total.len();
    let rest = width.saturating_sub(fixed);
    let start_cols = rest / 2;
    let stop_cols = rest - start_cols;

    let mut spans = vec![
        Span::styled(ts, Style::default().fg(Color::DarkGray)),
        Span::raw(" "),
        Span::styled(start_label, Style::default().fg(Color::Gray)),
    ];
    spans.extend(bar_spans(start, start_cols));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(start_total, Style::default().fg(Color::Gray)));
    spans.push(Span::raw("   "));
    spans.push(Span::styled(stop_label, Style::default().fg(Color::Gray)));
    spans.extend(bar_spans(stop, stop_cols));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(stop_total, Style::default().fg(Color::Gray)));
    Line::from(spans)
}

/// The colored phase segments of one bar (scaled to `cols`), plus a faint
/// `░` tail for whatever the phases don't account for.
fn bar_spans(op: &mvm_common::LifecycleOp, cols: usize) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    if cols == 0 {
        return spans;
    }
    let total_ms = op.total_ms.max(1);
    let mut remaining = cols;
    if op.phases.is_empty() {
        spans.push(Span::styled(
            "█".repeat(cols),
            Style::default().fg(Color::DarkGray),
        ));
        remaining = 0;
    } else {
        for (i, (name, ms)) in op.phases.iter().enumerate() {
            // The last phase absorbs whatever the rounded earlier ones left,
            // so the bar always sums to the full width.
            let c = if i + 1 == op.phases.len() {
                remaining
            } else {
                ((*ms as f64 / total_ms as f64) * cols as f64).round() as usize
            };
            let c = c.min(remaining);
            if c > 0 {
                spans.push(Span::styled(
                    "█".repeat(c),
                    Style::default().fg(phase_color(name)),
                ));
                remaining -= c;
            }
        }
    }
    if remaining > 0 {
        spans.push(Span::styled(
            "░".repeat(remaining),
            Style::default().fg(Color::DarkGray),
        ));
    }
    spans
}

fn flame_pair_legend(
    start: &mvm_common::LifecycleOp,
    stop: &mvm_common::LifecycleOp,
) -> Line<'static> {
    let mut spans = vec![Span::raw("   ")];
    let start_shown: Vec<_> = start.phases.iter().filter(|(_, ms)| *ms > 0).collect();
    let stop_shown: Vec<_> = stop.phases.iter().filter(|(_, ms)| *ms > 0).collect();
    if start_shown.is_empty() && stop_shown.is_empty() {
        spans.push(Span::styled("no phases", Style::default().fg(Color::DarkGray)));
        return Line::from(spans);
    }
    for (name, ms) in start_shown.iter() {
        spans.push(Span::styled("█", Style::default().fg(phase_color(name))));
        spans.push(Span::raw(format!(" {name}={ms}ms  ")));
    }
    if !start_shown.is_empty() && !stop_shown.is_empty() {
        spans.push(Span::raw("│  "));
    }
    for (name, ms) in stop_shown.iter() {
        spans.push(Span::styled("█", Style::default().fg(phase_color(name))));
        spans.push(Span::raw(format!(" {name}={ms}ms  ")));
    }
    Line::from(spans)
}

fn flame_legend(op: &mvm_common::LifecycleOp) -> Line<'static> {
    let mut spans = vec![Span::raw("   ")];
    let shown: Vec<_> = op.phases.iter().filter(|(_, ms)| *ms > 0).collect();
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
    use mvm_common::{LifecycleOp, Sandbox, SandboxSpec, SandboxState};

    fn op(name: &str, at_seconds: i64) -> LifecycleOp {
        LifecycleOp {
            op: name.to_string(),
            at: chrono::DateTime::from_timestamp(at_seconds, 0).unwrap(),
            total_ms: 1000,
            phases: vec![("a".to_string(), 400), ("b".to_string(), 600)],
        }
    }

    fn sandbox() -> Sandbox {
        Sandbox::new(SandboxSpec {
            name: Some("web".into()),
            image: "alpine".into(),
            ..Default::default()
        })
    }

    #[test]
    fn shows_all_recorded_ops_in_order_even_while_running() {
        let mut sb = sandbox();
        sb.lifecycle = vec![
            op("create", 100),
            op("start", 200),
            op("stop", 300),
            op("start", 400),
        ];
        sb.state = SandboxState::Running;
        let seen = visible_lifecycle(&sb);
        let ops: Vec<&str> = seen.iter().map(|o| o.op.as_str()).collect();
        // Nothing is hidden between start/stop cycles: the whole history stays.
        assert_eq!(ops, vec!["create", "start", "stop", "start"]);
    }

    #[test]
    fn phase_color_is_fixed_per_name_not_rank() {
        // The same name gets the same color no matter its position.
        assert_eq!(phase_color("boot"), phase_color("boot"));
        assert_eq!(phase_color("persist"), phase_color("persist"));
        // Different names differ (hand-picked palette), and a name's color
        // never depends on how many phases precede it in a given op.
        assert_eq!(phase_color("rootfs"), Color::Yellow);
        assert_eq!(phase_color("guestd"), Color::Magenta);
        assert_eq!(phase_color("boot"), Color::Red);
        assert_eq!(phase_color("terminate"), Color::LightRed);
        // Unknown names hash to a stable palette color.
        assert_eq!(phase_color("future-phase"), phase_color("future-phase"));
    }

    #[test]
    fn empty_history_renders_nothing() {
        assert!(visible_lifecycle(&sandbox()).is_empty());
    }

    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn start_stop_cycle_pairs_on_one_line() {
        let mut sb = sandbox();
        sb.lifecycle = vec![
            op("create", 100),
            op("start", 200),
            op("stop", 300),
            op("start", 400),
        ];
        let ops = visible_lifecycle(&sb);
        let lines = flame_lines(&ops, true, 60);
        // create (bar+legend), start+stop pair (bar+legend), live start (bar+legend)
        assert_eq!(lines.len(), 6);

        // The paired row carries both labels and both totals.
        let pair = line_text(&lines[2]);
        assert!(pair.contains("start"), "pair row: {pair}");
        assert!(pair.contains("stop"), "pair row: {pair}");
        assert!(pair.contains("1000ms"), "pair row: {pair}");

        // The live (still running) start gets the running marker.
        let live = line_text(&lines[4]);
        assert!(live.contains("▶"), "live row: {live}");
    }

    #[test]
    fn legend_skips_zero_ms_phases() {
        let op = LifecycleOp {
            op: "start".into(),
            at: chrono::DateTime::from_timestamp(100, 0).unwrap(),
            total_ms: 1000,
            phases: vec![
                ("rootfs".to_string(), 800),
                ("gvproxy".to_string(), 0), // not on gvproxy net: 0ms, hide it
                ("boot".to_string(), 200),
            ],
        };
        let legend = line_text(&flame_legend(&op));
        assert!(legend.contains("rootfs=800ms"), "legend: {legend}");
        assert!(legend.contains("boot=200ms"), "legend: {legend}");
        assert!(!legend.contains("gvproxy"), "0ms phase leaked: {legend}");

        let stop = LifecycleOp {
            op: "stop".into(),
            at: chrono::DateTime::from_timestamp(200, 0).unwrap(),
            total_ms: 1,
            phases: vec![("terminate".to_string(), 1), ("persist".to_string(), 0)],
        };
        let pair = line_text(&flame_pair_legend(&op, &stop));
        assert!(pair.contains("terminate=1ms"), "pair: {pair}");
        assert!(!pair.contains("persist"), "0ms phase leaked in pair: {pair}");
    }
}
