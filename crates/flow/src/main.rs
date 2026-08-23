mod client;
mod detail;
mod graph;
mod mailbox;

use std::io::stdout;
use std::sync::mpsc;
use std::time::Duration;

use clap::Parser;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEventKind,
};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use mvm_common::agent_api::AgentView;
use mvm_common::{Sandbox, SandboxState};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Paragraph;
use ratatui::Terminal;
use rataflow::{EventResponse, FlowEvent};

use crate::client::Client;
use crate::detail::{Action, Detail};
use crate::graph::GraphState;
use crate::mailbox::Mailbox;

#[derive(Parser)]
#[command(name = "mvm-flow", about = "Live agent lineage graph for a running mvm daemon")]
struct Args {
    /// Daemon address
    #[arg(long, env = "MVM_HOST", default_value = "http://127.0.0.1:24642")]
    host: String,

    /// Root sandbox (id or name); its whole descendant tree is shown
    root: String,
}

/// The open modal, if any: the info panel (full record + actions) or the
/// mailbox. Never both at once.
enum Modal {
    Info(Box<Detail>),
    Mailbox(Mailbox),
}

impl Modal {
    fn id(&self) -> &str {
        match self {
            Modal::Info(d) => &d.id,
            Modal::Mailbox(m) => &m.id,
        }
    }
}

/// What a context-menu entry opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuChoice {
    Info,
    Mailbox,
}

const MENU_ENTRIES: [(&str, MenuChoice); 2] = [
    ("info", MenuChoice::Info),
    ("mailbox", MenuChoice::Mailbox),
];

/// Right-click context menu on a node (rataflow's `NodeContextMenu`).
struct ContextMenu {
    node_id: String,
    label: String,
    selected: usize,
    /// Terminal position the menu was opened at.
    pos: (u16, u16),
    /// Menu rect from the last render, for hit-testing.
    area: ratatui::layout::Rect,
}

impl ContextMenu {
    fn new(node_id: &str, label: String, pos: (u16, u16)) -> Self {
        Self {
            node_id: node_id.to_string(),
            label,
            selected: 0,
            pos,
            area: ratatui::layout::Rect::default(),
        }
    }

    fn cycle(&mut self, delta: i32) {
        let n = MENU_ENTRIES.len() as i32;
        self.selected = ((self.selected as i32 + delta).rem_euclid(n)) as usize;
    }

    fn choice(&self) -> MenuChoice {
        MENU_ENTRIES[self.selected].1
    }

    /// Entry index at a terminal position, if the click hit one.
    fn entry_at(&self, column: u16, row: u16) -> Option<usize> {
        let inner_y = self.area.y + 1;
        if column <= self.area.x || column >= self.area.x + self.area.width - 1 {
            return None;
        }
        let idx = row.checked_sub(inner_y)? as usize;
        (idx < MENU_ENTRIES.len()).then_some(idx)
    }

    fn contains(&self, column: u16, row: u16) -> bool {
        self.area
            .contains(ratatui::layout::Position::new(column, row))
    }
}

fn draw_menu(f: &mut ratatui::Frame, menu: &mut ContextMenu) {
    let width = 16u16;
    let height = MENU_ENTRIES.len() as u16 + 2;
    let screen = f.area();
    let x = menu.pos.0.min(screen.width.saturating_sub(width));
    let y = menu.pos.1.min(screen.height.saturating_sub(height));
    let area = ratatui::layout::Rect::new(x, y, width, height);
    menu.area = area;
    f.render_widget(ratatui::widgets::Clear, area);
    let block = ratatui::widgets::Block::bordered()
        .border_style(Style::default().fg(Color::Cyan))
        .title(format!(" {} ", menu.label));
    let inner = block.inner(area);
    f.render_widget(block, area);
    for (i, (label, _)) in MENU_ENTRIES.iter().enumerate() {
        let row = ratatui::layout::Rect::new(inner.x, inner.y + i as u16, inner.width, 1);
        let style = if i == menu.selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        f.render_widget(Paragraph::new(format!(" {label}")).style(style), row);
    }
}

enum PollUpdate {
    Agents(Vec<AgentView>),
    Error(String),
    /// Detail modal record fetch (on open and as a post-action refresh).
    Inspect {
        id: String,
        result: Result<Box<Sandbox>, String>,
    },
    /// A start/stop/delete triggered from the detail modal finished.
    Action {
        id: String,
        action: Action,
        result: Result<(), String>,
    },
}

fn main() {
    let args = Args::parse();
    let client = Client::new(&args.host);

    let agents = match client.list_agents() {
        Ok(agents) => agents,
        Err(e) => {
            eprintln!("error: cannot reach daemon at {}: {e}", args.host);
            std::process::exit(1);
        }
    };
    let root = match resolve_root(&agents, &args.root) {
        Some(id) => id,
        None => {
            eprintln!("error: no sandbox matching id or name {:?}", args.root);
            std::process::exit(1);
        }
    };

    let mut graph = GraphState::new();
    graph.reconcile(&agents, &root);

    if enable_raw_mode().is_err() {
        eprintln!("error: failed to enter raw mode");
        std::process::exit(1);
    }
    let mut out = stdout();
    if out
        .execute(EnterAlternateScreen)
        .and_then(|out| out.execute(EnableMouseCapture))
        .is_err()
    {
        let _ = disable_raw_mode();
        eprintln!("error: failed to set up terminal");
        std::process::exit(1);
    }
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend).expect("terminal backend");

    let (tx, rx) = mpsc::channel::<PollUpdate>();
    {
        let client = client.clone();
        let tx = tx.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(1000));
            let update = match client.list_agents() {
                Ok(agents) => PollUpdate::Agents(agents),
                Err(e) => PollUpdate::Error(e),
            };
            if tx.send(update).is_err() {
                break;
            }
        });
    }

    let mut snapshot = agents;
    let mut last_error: Option<String> = None;
    let mut root_gone = false;
    let mut modal: Option<Modal> = None;
    let mut menu: Option<ContextMenu> = None;

    'app: loop {
        let _ = terminal.draw(|f| {
            draw(
                f,
                &mut graph,
                &snapshot,
                &root,
                root_gone,
                last_error.as_deref(),
                &mut modal,
                &mut menu,
            );
        });

        while let Ok(update) = rx.try_recv() {
            match update {
                PollUpdate::Agents(agents) => {
                    root_gone = !graph.reconcile(&agents, &root);
                    // Keep the open modal in sync with the fresher poll view.
                    let view = modal
                        .as_ref()
                        .and_then(|m| agents.iter().find(|a| a.id.as_str() == m.id()));
                    match (modal.as_mut(), view) {
                        (Some(Modal::Info(det)), Some(view)) => {
                            if let Some(sb) = det.sandbox.as_mut() {
                                sb.state = view.state;
                                sb.booted_at = view.booted_at;
                                sb.ready_at = view.ready_at;
                            }
                        }
                        (Some(Modal::Mailbox(mb)), Some(view)) => {
                            mb.sync(&view.pending_notifications, &view.recent_notifications);
                        }
                        _ => {}
                    }
                    snapshot = agents;
                    last_error = None;
                }
                PollUpdate::Error(e) => last_error = Some(e),
                PollUpdate::Inspect { id, result } => {
                    if let Some(Modal::Info(det)) =
                        modal.as_mut().filter(|m| m.id() == id)
                    {
                        match result {
                            Ok(sb) => {
                                det.sandbox = Some(*sb);
                                det.error = None;
                                det.action_error = None;
                            }
                            Err(e) => {
                                // A refresh failure keeps the stale record.
                                if det.sandbox.is_some() {
                                    det.action_error = Some(e);
                                } else {
                                    det.error = Some(e);
                                }
                            }
                        }
                    }
                }
                PollUpdate::Action { id, action, result } => {
                    let mut close = false;
                    if let Some(Modal::Info(det)) =
                        modal.as_mut().filter(|m| m.id() == id)
                    {
                        det.busy = false;
                        match result {
                            Ok(()) => {
                                if action == Action::Delete {
                                    // The next poll drops the node.
                                    close = true;
                                } else {
                                    spawn_fetch(&client, &tx, &id);
                                }
                            }
                            Err(e) => det.action_error = Some(e),
                        }
                    }
                    if close {
                        modal = None;
                    }
                }
            }
        }

        // Drain every pending event before the next frame: mouse streams arrive
        // at terminal rate and rataflow needs all of them for smooth pan/drag.
        let mut waited = false;
        loop {
            let timeout = if waited {
                Duration::ZERO
            } else {
                Duration::from_millis(100)
            };
            let has_event = match event::poll(timeout) {
                Ok(v) => v,
                Err(_) => break 'app,
            };
            if !has_event {
                break;
            }
            waited = true;
            match event::read() {
                Ok(Event::Key(key)) => {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        break 'app;
                    }
                    // The context menu owns the keyboard while it is open.
                    if let Some(m) = menu.as_mut() {
                        match key.code {
                            KeyCode::Esc | KeyCode::Char('q') => menu = None,
                            KeyCode::Up => m.cycle(-1),
                            KeyCode::Down => m.cycle(1),
                            KeyCode::Enter => {
                                let choice = m.choice();
                                let node_id = m.node_id.clone();
                                menu = None;
                                open_choice(&mut modal, choice, &node_id, &client, &tx, &snapshot);
                            }
                            _ => {}
                        }
                        continue;
                    }
                    // Modals own the keyboard while they are open.
                    match modal.as_mut() {
                        Some(Modal::Info(det)) => {
                            if handle_detail_key(det, key, &client, &tx) {
                                modal = None;
                            }
                            continue;
                        }
                        Some(Modal::Mailbox(mb)) => {
                            if handle_mailbox_key(mb, key) {
                                modal = None;
                            }
                            continue;
                        }
                        None => {}
                    }
                    if key.code == KeyCode::Char('q') {
                        break 'app;
                    }
                    match key.code {
                        KeyCode::Char('f') => graph.flow.request_fit_view(),
                        KeyCode::Enter => {
                            if let Some(id) = graph.flow.first_selected_node_id() {
                                let pos = graph
                                    .flow
                                    .node_terminal_rect(&id)
                                    .map(|(l, t, _, _)| (l.max(0) as u16, t.max(0) as u16))
                                    .unwrap_or((2, 2));
                                menu = Some(menu_for(&id, &snapshot, pos));
                            }
                        }
                        _ => {
                            if matches!(
                                graph.flow.handle_controls_key_event(key),
                                EventResponse::NotHandled
                            ) {
                                graph.flow.handle_key_event(key);
                            }
                        }
                    }
                }
                Ok(Event::Mouse(mouse)) => {
                    // The context menu owns the mouse while it is open.
                    if let Some(m) = menu.as_mut() {
                        match mouse.kind {
                            MouseEventKind::Down(MouseButton::Left) => {
                                if let Some(idx) = m.entry_at(mouse.column, mouse.row) {
                                    let choice = MENU_ENTRIES[idx].1;
                                    let node_id = m.node_id.clone();
                                    menu = None;
                                    open_choice(
                                        &mut modal, choice, &node_id, &client, &tx, &snapshot,
                                    );
                                } else if !m.contains(mouse.column, mouse.row) {
                                    menu = None;
                                }
                            }
                            MouseEventKind::ScrollUp => m.cycle(-1),
                            MouseEventKind::ScrollDown => m.cycle(1),
                            _ => {}
                        }
                        continue;
                    }
                    // Modals own the mouse while they are open.
                    let mut close = false;
                    match modal.as_mut() {
                        Some(Modal::Info(det)) => match mouse.kind {
                            MouseEventKind::Down(MouseButton::Left) => {
                                if let Some(action) = det.action_at(mouse.column, mouse.row) {
                                    trigger_action(det, action, &client, &tx);
                                } else if !det.contains(mouse.column, mouse.row) {
                                    close = true;
                                }
                            }
                            MouseEventKind::ScrollUp => {
                                det.scroll = det.scroll.saturating_sub(1);
                            }
                            MouseEventKind::ScrollDown => {
                                det.scroll = det.scroll.saturating_add(1);
                            }
                            _ => {}
                        },
                        Some(Modal::Mailbox(mb)) => match mouse.kind {
                            MouseEventKind::Down(MouseButton::Left) => {
                                if !mb.click(mouse.column, mouse.row)
                                    && !mb.contains(mouse.column, mouse.row)
                                {
                                    close = true;
                                }
                            }
                            MouseEventKind::ScrollUp => mb.select_move(-1),
                            MouseEventKind::ScrollDown => mb.select_move(1),
                            _ => {}
                        },
                        None => {}
                    }
                    if close {
                        modal = None;
                    }
                    if menu.is_none() && modal.is_none() {
                        let resp = graph.flow.handle_mouse_event(mouse);
                        for ev in resp.into_events() {
                            if let FlowEvent::NodeContextMenu { node_id } = ev {
                                menu = Some(menu_for(
                                    &node_id,
                                    &snapshot,
                                    (mouse.column, mouse.row),
                                ));
                            }
                        }
                    }
                    continue;
                }
                Ok(Event::Resize(_, _)) => {
                    graph.flow.request_fit_view();
                }
                Err(_) => break 'app,
                Ok(_) => {}
            }
        }
    }

    let _ = stdout().execute(DisableMouseCapture);
    let _ = stdout().execute(LeaveAlternateScreen);
    let _ = disable_raw_mode();
}

fn resolve_root(agents: &[AgentView], root: &str) -> Option<String> {
    agents
        .iter()
        .find(|a| a.id.as_str() == root || a.name.as_deref() == Some(root))
        .map(|a| a.id.to_string())
}

fn node_label(snapshot: &[AgentView], id: &str) -> String {
    snapshot
        .iter()
        .find(|a| a.id.as_str() == id)
        .and_then(|a| a.name.clone())
        .unwrap_or_else(|| id.chars().take(8).collect())
}

/// Build the context menu for a node, opened at terminal position `pos`.
fn menu_for(id: &str, snapshot: &[AgentView], pos: (u16, u16)) -> ContextMenu {
    ContextMenu::new(id, node_label(snapshot, id), pos)
}

/// Open the modal a context-menu entry points at.
fn open_choice(
    modal: &mut Option<Modal>,
    choice: MenuChoice,
    id: &str,
    client: &Client,
    tx: &mpsc::Sender<PollUpdate>,
    snapshot: &[AgentView],
) {
    let label = node_label(snapshot, id);
    match choice {
        MenuChoice::Info => {
            *modal = Some(Modal::Info(Box::new(Detail::loading(id, &label))));
            spawn_fetch(client, tx, id);
        }
        MenuChoice::Mailbox => {
            let mut mb = Mailbox::new(id, &label);
            if let Some(view) = snapshot.iter().find(|a| a.id.as_str() == id) {
                mb.sync(&view.pending_notifications, &view.recent_notifications);
            }
            *modal = Some(Modal::Mailbox(mb));
        }
    }
}

/// Fetch the full sandbox record on a background thread (the daemon call is
/// blocking; the record arrives as `PollUpdate::Inspect`).
fn spawn_fetch(client: &Client, tx: &mpsc::Sender<PollUpdate>, id: &str) {
    let c = client.clone();
    let t = tx.clone();
    let id = id.to_string();
    std::thread::spawn(move || {
        let result = c.get_sandbox(&id).map(Box::new);
        let _ = t.send(PollUpdate::Inspect { id, result });
    });
}

/// Run a state-gated action on a background thread. `Delete` first arms the
/// in-modal confirmation; once confirmed, a running sandbox is stopped before
/// removal (same order as the TUI).
fn trigger_action(det: &mut Detail, action: Action, client: &Client, tx: &mpsc::Sender<PollUpdate>) {
    let Some(sb) = det.sandbox.as_ref() else {
        return;
    };
    if det.busy || !action.enabled(sb.state) {
        return;
    }
    if action == Action::Delete && !det.confirming_delete {
        det.confirming_delete = true;
        return;
    }
    det.busy = true;
    det.confirming_delete = false;
    det.action_error = None;
    let c = client.clone();
    let t = tx.clone();
    let id = det.id.clone();
    let running = sb.state == SandboxState::Running;
    std::thread::spawn(move || {
        let result: Result<(), String> = (|| {
            match action {
                Action::Start => c.start_sandbox(&id),
                Action::Stop => c.stop_sandbox(&id),
                Action::Delete => {
                    if running {
                        c.stop_sandbox(&id)?;
                    }
                    c.remove_sandbox(&id)
                }
            }
        })();
        let _ = t.send(PollUpdate::Action { id, action, result });
    });
}

/// Mailbox keyboard handler; returns true when the modal should close.
fn handle_mailbox_key(mb: &mut Mailbox, key: event::KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => return true,
        KeyCode::Up => mb.select_move(-1),
        KeyCode::Down => mb.select_move(1),
        KeyCode::PageUp => mb.body_scroll_by(-10),
        KeyCode::PageDown => mb.body_scroll_by(10),
        _ => {}
    }
    false
}

/// Detail-modal keyboard handler; returns true when the modal should close.
fn handle_detail_key(
    det: &mut Detail,
    key: event::KeyEvent,
    client: &Client,
    tx: &mpsc::Sender<PollUpdate>,
) -> bool {
    if det.confirming_delete {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                trigger_action(det, Action::Delete, client, tx);
            }
            _ => det.confirming_delete = false,
        }
        return false;
    }
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => return true,
        KeyCode::Up => det.scroll = det.scroll.saturating_sub(1),
        KeyCode::Down => det.scroll = det.scroll.saturating_add(1),
        KeyCode::PageUp => det.scroll = det.scroll.saturating_sub(10),
        KeyCode::PageDown => det.scroll = det.scroll.saturating_add(10),
        KeyCode::Tab | KeyCode::Right => det.cycle_button(true),
        KeyCode::BackTab | KeyCode::Left => det.cycle_button(false),
        KeyCode::Enter => {
            if let Some(action) = det.selected_action() {
                trigger_action(det, action, client, tx);
            }
        }
        KeyCode::Char(c) => {
            if let Some(action) = Action::from_key(c) {
                trigger_action(det, action, client, tx);
            }
        }
        _ => {}
    }
    false
}

#[allow(clippy::too_many_arguments)]
fn draw(
    f: &mut ratatui::Frame,
    graph: &mut GraphState,
    snapshot: &[AgentView],
    root: &str,
    root_gone: bool,
    last_error: Option<&str>,
    modal: &mut Option<Modal>,
    menu: &mut Option<ContextMenu>,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
        .split(f.area());

    let mut header = vec![
        ratatui::text::Span::styled(
            "mvm-flow",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        ratatui::text::Span::raw(format!("  root: {root}  agents: {}", snapshot.len())),
    ];
    if root_gone {
        header.push(ratatui::text::Span::styled(
            "  root sandbox is gone",
            Style::default().fg(Color::Red),
        ));
    }
    if let Some(err) = last_error {
        header.push(ratatui::text::Span::styled(
            format!("  {err}"),
            Style::default().fg(Color::Red),
        ));
    }
    f.render_widget(Paragraph::new(ratatui::text::Line::from(header)), chunks[0]);

    f.render_widget(&mut graph.flow, chunks[1]);

    let footer = if let Some(id) = graph.flow.first_selected_node_id() {
        match snapshot.iter().find(|a| a.id.as_str() == id) {
            Some(view) => {
                let mut parts = format!(
                    "{}  {}  {}cpu/{}MiB",
                    view.name.clone().unwrap_or_else(|| id.clone()),
                    view.state,
                    view.vcpus,
                    view.ram_mib
                );
                if let Some(ready) = view.ready_at {
                    parts.push_str(&format!("  ready {}", ready.format("%H:%M:%S")));
                }
                if let Some(deadline) = view.ttl_deadline {
                    parts.push_str(&format!("  ttl deadline {}", deadline.format("%H:%M:%S")));
                }
                parts
            }
            None => format!("{id} (not in last snapshot)"),
        }
    } else {
        "q quit · f fit · right-click/enter menu · Tab select · arrows/hjkl pan · +/- zoom · drag nodes to rearrange"
            .to_string()
    };
    f.render_widget(
        Paragraph::new(footer).style(Style::default().fg(Color::DarkGray)),
        chunks[2],
    );

    // Overlays render last = on top of the graph (menu on top of a modal).
    match modal.as_mut() {
        Some(Modal::Info(det)) => detail::draw_detail(f, det),
        Some(Modal::Mailbox(mb)) => mailbox::draw_mailbox(f, mb),
        None => {}
    }
    if let Some(m) = menu.as_mut() {
        draw_menu(f, m);
    }
}
