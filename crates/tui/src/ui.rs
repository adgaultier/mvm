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
            let running = sb.state == mvm_common::SandboxState::Running;
            let lines = flame_lines(
                &sb.timeline,
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

/// One `Line` per flamegraph row. Each lifecycle is a `start` (boot timings
/// are the only thing worth tracing). The last one is live if the sandbox is
/// still running. Point events that aren't part of the boot bar (`stop`) are
/// rendered as simple timestamped lines below the bar+legend.
fn flame_lines(
    timeline: &[Vec<mvm_common::TimelineEvent>],
    running: bool,
    bar_w: usize,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    if timeline.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no lifecycle timing recorded yet",
            Style::default().fg(Color::DarkGray),
        )));
        return lines;
    }

    let last = timeline.len().saturating_sub(1);
    for (i, lifecycle) in timeline.iter().enumerate() {
        let is_start = lifecycle
            .first()
            .map(|e| e.event == "start_start")
            .unwrap_or(false);
        if is_start {
            let live = running && i == last;
            // The stop event lives in the same lifecycle array but is not
            // part of the boot bar — split it out so the bar spans only the
            // boot phases, and render stop as a simple line below.
            let bar_events: Vec<&mvm_common::TimelineEvent> = lifecycle
                .iter()
                .filter(|e| e.event != "stop")
                .collect();
            lines.push(flame_bar_line(&bar_events, live, bar_w));
            lines.push(flame_legend(&bar_events));
            for e in lifecycle {
                if e.event == "stop" {
                    lines.push(event_line(e));
                }
            }
        } else {
            // Non-start lifecycles (if any ever appear): one simple line each.
            for e in lifecycle {
                lines.push(event_line(e));
            }
        }
    }

    lines
}

/// A simple timestamped event line (for orphan events like create/stop that
/// don't belong to a boot cycle bar).
fn event_line(e: &mvm_common::TimelineEvent) -> Line<'static> {
    let ts = e.at.format("%H:%M:%S").to_string();
    let label = event_label(&e.event);
    let color = event_color(&e.event);
    Line::from(vec![
        Span::styled(ts, Style::default().fg(Color::DarkGray)),
        Span::raw("  "),
        Span::styled(label, Style::default().fg(color)),
    ])
}

/// Human-readable label for an event name. Phase boundaries become the phase
/// name; op boundaries become the op name; point events keep their name.
fn event_label(name: &str) -> String {
    if let Some(phase) = name.strip_suffix("_start") {
        return format!("▶ {phase}");
    }
    if let Some(phase) = name.strip_suffix("_stop") {
        return format!("◀ {phase}");
    }
    name.to_string()
}

/// Marker glyph for a point-in-time event, overlaid on the bar at its position.
fn event_marker(name: &str) -> char {
    match name {
        "agent_ready" => '✓',
        _ => '·',
    }
}

/// Whether an event is a point-in-time marker (not a phase boundary).
fn is_point_event(name: &str) -> bool {
    !name.ends_with("_start") && !name.ends_with("_stop")
}

/// Fixed color per event name. Phase boundaries use `phase_color` on the
/// stripped phase name; point events get their own colors.
fn event_color(name: &str) -> Color {
    if let Some(phase) = name.strip_suffix("_start") {
        return phase_color(phase);
    }
    if let Some(phase) = name.strip_suffix("_stop") {
        return phase_color(phase);
    }
    match name {
        "agent_ready" => Color::Cyan,
        _ => Color::Gray,
    }
}

/// `HH:MM:SS  start  ████…████  1234ms` — timestamp left, total right. The
/// bar is built from the lifecycle's sorted timestamps: phase segments are
/// drawn between `<phase>_start` and `<phase>_stop` pairs, point events
/// (agent_ready) are overlaid as markers.
fn flame_bar_line(
    events: &[&mvm_common::TimelineEvent],
    live: bool,
    width: usize,
) -> Line<'static> {
    let start = events.first().expect("lifecycle has events");
    let ts = start.at.format("%H:%M:%S").to_string();
    let label = format!("{:<6}", "start");

    // Compute total span: from start to the last boot event in the lifecycle.
    let last_at = events.last().map(|e| e.at).unwrap_or(start.at);
    let span_ms = (last_at.timestamp_millis() - start.at.timestamp_millis()).max(1) as f64;
    let total_ms = span_ms as u64;
    let total = format!("{:>6}ms", total_ms);

    let mut suffix = String::new();
    if live {
        suffix.push_str(" ▶ running");
    }
    let bar_w = width
        .saturating_sub(ts.len() + label.len() + total.len() + 3 + suffix.chars().count());

    let mut spans = vec![
        Span::styled(ts, Style::default().fg(Color::DarkGray)),
        Span::raw(" "),
        Span::styled(label, Style::default().fg(Color::Gray)),
    ];
    spans.extend(bar_spans(events, bar_w, span_ms));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(total, Style::default().fg(Color::Gray)));
    if live {
        spans.push(Span::styled(" ▶ running", Style::default().fg(Color::Green)));
    }
    Line::from(spans)
}

/// The colored bar for one lifecycle, scaled to `cols`. Each column is
/// assigned to a phase (or a faint tail for gaps), then point-event markers
/// are overlaid at their relative position.
fn bar_spans(
    events: &[&mvm_common::TimelineEvent],
    cols: usize,
    span_ms: f64,
) -> Vec<Span<'static>> {
    if cols == 0 {
        return Vec::new();
    }
    let start = events.first().expect("lifecycle has events");
    let start_ms = start.at.timestamp_millis() as f64;

    // Build per-column (char, color) pairs. Default to faint tail.
    let mut cells: Vec<(char, Color)> = vec![('░', Color::DarkGray); cols];

    // Collect phase segments: (phase_name, start_ms, end_ms).
    let mut phase_starts: std::collections::HashMap<&str, f64> = std::collections::HashMap::new();
    let mut segments: Vec<(&str, f64, f64)> = Vec::new();
    for e in events {
        if let Some(phase) = e.event.strip_suffix("_start") {
            let offset = (e.at.timestamp_millis() as f64 - start_ms).max(0.0);
            phase_starts.insert(phase, offset);
        } else if let Some(phase) = e.event.strip_suffix("_stop") {
            if let Some(phase_start) = phase_starts.remove(phase) {
                let phase_end = (e.at.timestamp_millis() as f64 - start_ms).max(0.0);
                segments.push((phase, phase_start, phase_end));
            }
        }
    }

    // Paint phase segments onto the bar.
    for (phase, seg_start, seg_end) in &segments {
        let seg_dur = (seg_end - seg_start).max(0.0);
        if seg_dur == 0.0 {
            continue;
        }
        let start_col = ((seg_start / span_ms) * cols as f64).round() as usize;
        let end_col = (((seg_end) / span_ms) * cols as f64).round() as usize;
        let start_col = start_col.min(cols - 1);
        let end_col = end_col.min(cols);
        for col in start_col..end_col {
            cells[col] = ('█', phase_color(phase));
        }
    }

    // Overlay point-event markers at their position.
    for e in events {
        if !is_point_event(&e.event) {
            continue;
        }
        let offset = (e.at.timestamp_millis() as f64 - start_ms).max(0.0);
        let col = if offset >= span_ms {
            cols - 1
        } else {
            (offset / span_ms * cols as f64).round() as usize
        };
        let col = col.min(cols - 1);
        let marker = event_marker(&e.event);
        let color = event_color(&e.event);
        cells[col] = (marker, color);
    }

    // Group consecutive identical (char, color) cells into spans.
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut cur: Option<(char, Color)> = None;
    for (ch, color) in cells {
        if cur.map_or(false, |(c, col)| c == ch && col == color) {
            buf.push(ch);
        } else {
            if !buf.is_empty() {
                if let Some((_, color)) = cur {
                    spans.push(Span::styled(std::mem::take(&mut buf), Style::default().fg(color)));
                }
            }
            buf.push(ch);
            cur = Some((ch, color));
        }
    }
    if !buf.is_empty() {
        if let Some((_, color)) = cur {
            spans.push(Span::styled(buf, Style::default().fg(color)));
        }
    }
    spans
}

fn flame_legend(events: &[&mvm_common::TimelineEvent]) -> Line<'static> {
    let mut spans = vec![Span::raw("   ")];

    // Collect phase durations from the lifecycle's events.
    let mut phase_starts: std::collections::HashMap<&str, i64> = std::collections::HashMap::new();
    let mut phase_durs: Vec<(&str, u64)> = Vec::new();
    for e in events {
        if let Some(phase) = e.event.strip_suffix("_start") {
            phase_starts.insert(phase, e.at.timestamp_millis());
        } else if let Some(phase) = e.event.strip_suffix("_stop") {
            if let Some(start) = phase_starts.remove(phase) {
                let dur = (e.at.timestamp_millis() - start).max(0) as u64;
                phase_durs.push((phase, dur));
            }
        }
    }

    let start_at = events.first().map(|e| e.at.timestamp_millis()).unwrap_or(0);
    if phase_durs.is_empty() && !events.iter().any(|e| is_point_event(&e.event)) {
        spans.push(Span::styled("no phases", Style::default().fg(Color::DarkGray)));
    } else {
        for (name, ms) in &phase_durs {
            spans.push(Span::styled("█", Style::default().fg(phase_color(name))));
            spans.push(Span::raw(format!(" {name}={ms}ms  ")));
        }
        // Point events with their offset from boot start.
        for e in events {
            if !is_point_event(&e.event) {
                continue;
            }
            let offset_ms = (e.at.timestamp_millis() - start_at).max(0) as u64;
            let color = event_color(&e.event);
            spans.push(Span::styled(
                event_marker(&e.event).to_string(),
                Style::default().fg(color),
            ));
            spans.push(Span::raw(format!(" {} +{}ms  ", e.event, offset_ms)));
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
        ("BOOTED", ts(sb.booted_at)),
        ("READY", ts(sb.ready_at)),
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

    /// A realistic start lifecycle: start at t, rootfs/guestd/boot phases,
    /// then agent_ready appended to the same lifecycle.
    fn start_lifecycle(start_t: i64) -> Vec<TimelineEvent> {
        vec![
            ev("start_start", start_t),
            ev("rootfs_start", start_t),
            ev("rootfs_stop", start_t),
            ev("guestd_start", start_t),
            ev("guestd_stop", start_t),
            ev("boot_start", start_t),
            ev("boot_stop", start_t),
            ev("start_stop", start_t),
        ]
    }

    #[test]
    fn phase_color_is_fixed_per_name_not_rank() {
        assert_eq!(phase_color("boot"), phase_color("boot"));
        assert_eq!(phase_color("persist"), phase_color("persist"));
        assert_eq!(phase_color("rootfs"), Color::Yellow);
        assert_eq!(phase_color("guestd"), Color::Magenta);
        assert_eq!(phase_color("boot"), Color::Red);
        assert_eq!(phase_color("terminate"), Color::LightRed);
        assert_eq!(phase_color("future-phase"), phase_color("future-phase"));
    }

    #[test]
    fn empty_timeline_renders_nothing() {
        let lines = flame_lines(&[], false, 80);
        assert_eq!(lines.len(), 1);
        assert!(line_text(&lines[0]).contains("no lifecycle"));
    }

    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn boot_cycle_bar_with_overlaid_events() {
        // Two start lifecycles. The second has agent_ready.
        let mut cycle2 = start_lifecycle(400);
        cycle2.push(ev("agent_ready", 405)); // +5s into boot

        let timeline: Vec<Vec<TimelineEvent>> = vec![start_lifecycle(200), cycle2];
        let lines = flame_lines(&timeline, true, 120);
        // 2 start lifecycles × (bar + legend) = 4 lines.
        assert_eq!(lines.len(), 4);

        // The first bar has no point-event markers.
        let bar1 = line_text(&lines[0]);
        assert!(bar1.contains("start"), "bar 0: {bar1}");
        assert!(!bar1.contains("✓"), "no ready marker on bar 0: {bar1}");

        // The second bar has the ready marker and the running marker.
        let bar2 = line_text(&lines[2]);
        assert!(bar2.contains("start"), "bar 1: {bar2}");
        assert!(bar2.contains("✓"), "ready marker on bar: {bar2}");
        assert!(bar2.contains("▶"), "live marker: {bar2}");

        // The legend lists the point event.
        let legend2 = line_text(&lines[3]);
        assert!(legend2.contains("agent_ready"), "legend 1: {legend2}");
    }

    #[test]
    fn stop_is_a_simple_line_after_the_bar() {
        // stop lives in the same lifecycle array as the start events.
        let mut lifecycle = start_lifecycle(200);
        lifecycle.push(ev("stop", 300));
        let timeline: Vec<Vec<TimelineEvent>> = vec![lifecycle];
        let lines = flame_lines(&timeline, false, 80);
        // start (bar + legend) + stop (1 line) = 3 lines.
        assert_eq!(lines.len(), 3);
        assert!(line_text(&lines[0]).contains("start"));
        assert!(line_text(&lines[2]).contains("stop"));
    }

    #[test]
    fn legend_shows_phase_durations() {
        // rootfs: 0ms, guestd: 0ms, boot: 1000ms.
        let lifecycle = vec![
            ev("start_start", 200),
            ev("rootfs_start", 200),
            ev("rootfs_stop", 200), // +0ms
            ev("guestd_start", 200),
            ev("guestd_stop", 200), // +0ms
            ev("boot_start", 200),
            ev("boot_stop", 201), // +1000ms
            ev("start_stop", 201),
        ];
        let lines = flame_lines(&[lifecycle], false, 120);
        let legend = line_text(&lines[1]);
        assert!(legend.contains("rootfs"), "legend: {legend}");
        assert!(legend.contains("boot"), "legend: {legend}");
    }
}
