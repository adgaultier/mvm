mod actions;
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

use crate::actions::Action;
use crate::client::{descendants_of, Client};
use crate::detail::Detail;
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

/// The open modal, if any: the info panel (pure inspect view), the mailbox,
/// or the Actions panel. Never more than one at once; the context menu closes
/// when one opens.
enum Modal {
    Info(Box<Detail>),
    Mailbox(Mailbox),
    Actions(ActionMenu),
}

impl Modal {
    fn id(&self) -> &str {
        match self {
            Modal::Info(d) => &d.id,
            Modal::Mailbox(m) => &m.id,
            Modal::Actions(a) => &a.id,
        }
    }
}

/// What a context-menu entry does when activated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuChoice {
    Info,
    Mailbox,
    Actions,
}

impl MenuChoice {
    fn label(self) -> &'static str {
        match self {
            MenuChoice::Info => "info",
            MenuChoice::Mailbox => "mailbox",
            MenuChoice::Actions => "action",
        }
    }
}

/// One fixed top-level entry of the context menu.
struct MenuEntry {
    choice: MenuChoice,
}


/// Right-click context menu on a node (rataflow's `NodeContextMenu`), with
/// just `info` / `mailbox` / `action` rows. The third opens the
/// small action picker.
struct ContextMenu {
    node_id: String,
    label: String,
    entries: Vec<MenuEntry>,
    selected: usize,
    /// Terminal position the menu was opened at.
    pos: (u16, u16),
    /// Menu rect from the last render, for hit-testing.
    area: ratatui::layout::Rect,
}

impl ContextMenu {
    fn new(node_id: &str, label: String, pos: (u16, u16)) -> Self {
        let entries = vec![
            MenuEntry { choice: MenuChoice::Info },
            MenuEntry { choice: MenuChoice::Mailbox },
            MenuEntry { choice: MenuChoice::Actions },
        ];
        Self {
            node_id: node_id.to_string(),
            label,
            entries,
            selected: 0,
            pos,
            area: ratatui::layout::Rect::default(),
        }
    }

    fn cycle(&mut self, delta: i32) {
        let n = self.entries.len() as i32;
        self.selected = ((self.selected as i32 + delta).rem_euclid(n)) as usize;
    }

    fn choice(&self) -> MenuChoice {
        self.entries[self.selected].choice
    }

    /// Entry index at a terminal position, if the click hit one.
    fn entry_at(&self, column: u16, row: u16) -> Option<usize> {
        let inner_y = self.area.y + 1;
        if column <= self.area.x || column >= self.area.x + self.area.width - 1 {
            return None;
        }
        let idx = row.checked_sub(inner_y)? as usize;
        (idx < self.entries.len()).then_some(idx)
    }

    fn contains(&self, column: u16, row: u16) -> bool {
        self.area
            .contains(ratatui::layout::Position::new(column, row))
    }
}

/// The small action-picker modal: state-applicable actions (start/stop as
/// gated, delete always) plus the three children actions (propagate to the
/// node's descendants; only shown when the node has children). Rows the
/// state forbids are dropped entirely. Delete is two-step: the first
/// click/Enter arms `confirming_delete`, the second fires (stop-then-remove
/// via the daemon).
struct ActionMenu {
    id: String,
    label: String,
    /// Rows: applicable actions + placeholder (colour-less) rows.
    entries: Vec<Action>,
    selected: usize,
    confirming_delete: bool,
    /// Modal rect from the last render, for click-outside-to-close.
    area: ratatui::layout::Rect,
}

impl ActionMenu {
    fn new(id: &str, label: &str, state: SandboxState, has_children: bool) -> Self {
        // Child actions only appear when the node actually has children.
        let entries = Action::ALL
            .into_iter()
            .filter(|a| {
                let is_children = matches!(
                    a,
                    Action::StartChildren | Action::StopChildren | Action::DeleteChildren
                );
                (is_children && has_children) || (!is_children && a.enabled(state))
            })
            .collect();
        Self {
            id: id.to_string(),
            label: label.to_string(),
            entries,
            selected: 0,
            confirming_delete: false,
            area: ratatui::layout::Rect::default(),
        }
    }

    fn cycle(&mut self, delta: i32) {
        let n = self.entries.len() as i32;
        self.selected = ((self.selected as i32 + delta).rem_euclid(n)) as usize;
    }

    /// Highlighted action, when it may run (placeholder rows are colour-less).
    fn choice(&self) -> Option<Action> {
        self.entries.get(self.selected).and_then(|a| {
            (a.color().is_some()).then_some(*a)
        })
    }

    /// Action at a terminal position, if it may run.
    fn action_at(&self, column: u16, row: u16) -> Option<Action> {
        let inner_y = self.area.y + 1;
        if column <= self.area.x || column >= self.area.x + self.area.width - 1 {
            return None;
        }
        let idx = row.checked_sub(inner_y)? as usize;
        let a = self.entries[idx];
        (idx < self.entries.len() && a.color().is_some()).then_some(a)
    }

    fn contains(&self, column: u16, row: u16) -> bool {
        self.area
            .contains(ratatui::layout::Position::new(column, row))
    }
}

fn draw_menu(f: &mut ratatui::Frame, menu: &mut ContextMenu) {
    let width = 12u16;
    let height = menu.entries.len() as u16 + 2;
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
    for (i, entry) in menu.entries.iter().enumerate() {
        let row = ratatui::layout::Rect::new(inner.x, inner.y + i as u16, inner.width, 1);
        let style = if i == menu.selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let label = entry.choice.label();
        f.render_widget(Paragraph::new(format!(" {label}")).style(style), row);
    }
}

fn draw_action_menu(f: &mut ratatui::Frame, menu: &mut ActionMenu) {
    // "delete children" is 14; the confirm line is longer.
    let width = if menu.confirming_delete { 24 } else { 16 };
    let height = menu.entries.len() as u16 + 2;
    let screen = f.area();
    let area = detail::centered_rect(width.min(screen.width), height.min(screen.height), screen);
    menu.area = area;
    f.render_widget(ratatui::widgets::Clear, area);
    let block = ratatui::widgets::Block::bordered()
        .border_style(Style::default().fg(Color::Cyan))
        .title(format!(" {} ", menu.label));
    let inner = block.inner(area);
    f.render_widget(block, area);

    for (i, action) in menu.entries.iter().enumerate() {
        let row = ratatui::layout::Rect::new(inner.x, inner.y + i as u16, inner.width, 1);
        let is_delete = *action == Action::Delete;
        let (text, base) = if menu.confirming_delete && is_delete {
            (
                " really delete? [y]/[n]".to_string(),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )
        } else {
            let base = match action.color() {
                Some(c) => Style::default().fg(c),
                None => Style::default().fg(Color::DarkGray),
            };
            (format!(" {}", action.label()), base)
        };
        let style = if i == menu.selected && action.color().is_some() {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            base
        };
        f.render_widget(Paragraph::new(text).style(style), row);
    }
}

enum PollUpdate {
    Agents(Vec<AgentView>),
    Error(String),
    /// Detail modal record fetch (on open; the modal is inspect-only now).
    Inspect {
        id: String,
        result: Result<Box<Sandbox>, String>,
    },
    /// A start/stop/delete triggered from the context menu finished.
    MenuAction { result: Result<(), String> },
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
                            }
                            Err(e) => {
                                det.error = Some(e);
                            }
                        }
                    }
                }
                PollUpdate::MenuAction { result } => {
                    // Errors surface in the header; on success the next poll
                    // reconciles the node (drops it on delete, flips state on
                    // start/stop).
                    if let Err(e) = result {
                        last_error = Some(e);
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
                                activate(&mut modal, choice, &node_id, &client, &tx, &snapshot);
                            }
                            _ => {}
                        }
                        continue;
                    }
                    // Modals own the keyboard while they are open.
                    match modal.as_mut() {
                        Some(Modal::Info(det)) => {
                            if handle_detail_key(det, key) {
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
                        Some(Modal::Actions(am)) => {
                            if handle_actions_key(am, key, &snapshot, &client, &tx) {
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
                                menu = Some(ContextMenu::new(&id, node_label(&snapshot, &id), pos));
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
                                    let choice = m.entries[idx].choice;
                                    let node_id = m.node_id.clone();
                                    menu = None;
                                    activate(&mut modal, choice, &node_id, &client, &tx, &snapshot);
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
                                if !det.contains(mouse.column, mouse.row) {
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
                        Some(Modal::Actions(am)) => match mouse.kind {
                            MouseEventKind::Down(MouseButton::Left) => {
                                if am.confirming_delete {
                                    if am.action_at(mouse.column, mouse.row).is_some() {
                                        let id = am.id.clone();
                                        modal = None;
                                        trigger_action(&id, Action::Delete, &snapshot, &client, &tx);
                                    } else if am.contains(mouse.column, mouse.row) {
                                        am.confirming_delete = false;
                                    } else {
                                        close = true;
                                    }
                                } else if let Some(action) = am.action_at(mouse.column, mouse.row) {
                                    if action == Action::Delete {
                                        am.confirming_delete = true;
                                    } else {
                                        let id = am.id.clone();
                                        modal = None;
                                        trigger_action(&id, action, &snapshot, &client, &tx);
                                    }
                                } else if !am.contains(mouse.column, mouse.row) {
                                    close = true;
                                }
                            }
                            MouseEventKind::ScrollUp => am.cycle(-1),
                            MouseEventKind::ScrollDown => am.cycle(1),
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
                                menu = Some(ContextMenu::new(
                                    &node_id,
                                    node_label(&snapshot, &node_id),
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

/// Open the modal a context-menu entry points at.
fn activate(
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
        MenuChoice::Actions => {
            let view = snapshot.iter().find(|a| a.id.as_str() == id);
            let state = view.map(|a| a.state).unwrap_or(SandboxState::Created);
            let has_children = view.map(|a| !a.children.is_empty()).unwrap_or(false);
            *modal = Some(Modal::Actions(ActionMenu::new(id, &label, state, has_children)));
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

/// Run a lifecycle action from the actions modal on a background thread.
/// Delete on a running sandbox is stopped before removal (same order as the
/// TUI); completion arrives as `PollUpdate::MenuAction`.
fn trigger_action(
    id: &str,
    action: Action,
    snapshot: &[AgentView],
    client: &Client,
    tx: &mpsc::Sender<PollUpdate>,
) {
    let c = client.clone();
    let t = tx.clone();
    let id = id.to_string();
    let descendants: Vec<String> = descendants_of(snapshot, &id).into_iter().map(str::to_owned).collect();
    std::thread::spawn(move || {
        let result: Result<(), String> = propagate(&c, action, &id, &descendants);
        let _ = t.send(PollUpdate::MenuAction { result });
    });
}

/// Apply one action to a node and, for the children actions, to its whole
/// descendant lineage (deepest-first for delete, mirroring the single-node
/// stop-then-remove order). Reuses the per-sandbox lifecycle routes, so no
/// dedicated propagate endpoint is needed. Errors on a node don't abort the
/// rest; the first error wins.
fn propagate(
    client: &Client,
    action: Action,
    id: &str,
    descendants: &[String],
) -> Result<(), String> {
    match action {
        Action::Start => client.start_sandbox(id),
        Action::Stop => client.stop_sandbox(id),
        Action::Delete => {
            // The record may have transitioned since the menu opened;
            // stopping a stopped sandbox is a no-op daemon-side.
            let _ = client.stop_sandbox(id);
            client.remove_sandbox(id)
        }
        Action::StartChildren => apply_each(client, descendants, Client::start_sandbox),
        Action::StopChildren => apply_each(client, descendants, Client::stop_sandbox),
        Action::DeleteChildren => {
            // Deepest descendants go first, so a child is gone before its
            // parent is removed (mirrors the Agent API's reversed walk).
            let mut order = descendants.to_vec();
            order.reverse();
            for node in order {
                // Stop-then-remove per node, matching single-node Delete.
                let _ = client.stop_sandbox(&node);
                client.remove_sandbox(&node)?;
            }
            Ok(())
        }
    }
}

fn apply_each(
    client: &Client,
    ids: &[String],
    op: fn(&Client, &str) -> Result<(), String>,
) -> Result<(), String> {
    for node in ids {
        op(client, node)?;
    }
    Ok(())
}

/// Actions-modal keyboard handler; returns true when the modal should close.
fn handle_actions_key(
    am: &mut ActionMenu,
    key: event::KeyEvent,
    snapshot: &[AgentView],
    client: &Client,
    tx: &mpsc::Sender<PollUpdate>,
) -> bool {
    if am.confirming_delete {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                trigger_action(&am.id.clone(), Action::Delete, snapshot, client, tx);
                return true;
            }
            _ => am.confirming_delete = false,
        }
        return false;
    }
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => return true,
        KeyCode::Up => am.cycle(-1),
        KeyCode::Down => am.cycle(1),
        KeyCode::Enter => {
            if let Some(action) = am.choice() {
                if action == Action::Delete {
                    am.confirming_delete = true;
                } else {
                    trigger_action(&am.id.clone(), action, snapshot, client, tx);
                    return true;
                }
            }
        }
        _ => {}
    }
    false
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

/// Detail-modal keyboard handler (inspect-only); returns true when the modal
/// should close.
fn handle_detail_key(det: &mut Detail, key: event::KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => return true,
        KeyCode::Up => det.scroll = det.scroll.saturating_sub(1),
        KeyCode::Down => det.scroll = det.scroll.saturating_add(1),
        KeyCode::PageUp => det.scroll = det.scroll.saturating_sub(10),
        KeyCode::PageDown => det.scroll = det.scroll.saturating_add(10),
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
        Some(Modal::Actions(am)) => draw_action_menu(f, am),
        None => {}
    }
    if let Some(m) = menu.as_mut() {
        draw_menu(f, m);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mvm_common::SandboxState;

    fn no_children(state: SandboxState) -> ActionMenu {
        ActionMenu::new("id", "l", state, false)
    }

    fn with_children(state: SandboxState) -> ActionMenu {
        ActionMenu::new("id", "l", state, true)
    }

    #[test]
    fn child_actions_hidden_without_children() {
        for state in [
            SandboxState::Created,
            SandboxState::Running,
            SandboxState::Stopped,
            SandboxState::Exited,
            SandboxState::Failed,
        ] {
            let am = no_children(state);
            for a in [Action::StartChildren, Action::StopChildren, Action::DeleteChildren] {
                assert!(!am.entries.contains(&a), "{a:?} visible on {state} without children");
            }
        }
    }

    #[test]
    fn child_actions_shown_with_children() {
        let am = with_children(SandboxState::Running);
        for a in [Action::StartChildren, Action::StopChildren, Action::DeleteChildren] {
            assert!(am.entries.contains(&a));
        }
    }

    #[test]
    fn lifecycle_rows_follow_state() {
        let am = no_children(SandboxState::Running);
        assert!(!am.entries.contains(&Action::Start));
        assert!(am.entries.contains(&Action::Stop));

        let am = no_children(SandboxState::Created);
        assert!(am.entries.contains(&Action::Start));
        assert!(!am.entries.contains(&Action::Stop));
        assert!(am.entries.contains(&Action::Delete));
    }

    #[test]
    fn action_at_hits_live_rows() {
        let mut am = with_children(SandboxState::Created);
        am.area = ratatui::layout::Rect::new(0, 0, 24, am.entries.len() as u16 + 2);
        // Lifecycle rows are clickable…
        let idx = am.entries.iter().position(|a| *a == Action::Start).unwrap();
        assert_eq!(am.action_at(2, 1 + idx as u16), Some(Action::Start));
        // …and so are the children rows once the node has a lineage.
        let idx = am.entries.iter().position(|a| *a == Action::StartChildren).unwrap();
        assert_eq!(am.action_at(2, 1 + idx as u16), Some(Action::StartChildren));
    }
}
