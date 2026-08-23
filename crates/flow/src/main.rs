mod client;
mod graph;

use std::io::stdout;
use std::sync::mpsc;
use std::time::Duration;

use clap::Parser;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use mvm_common::agent_api::AgentView;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Paragraph;
use ratatui::Terminal;
use rataflow::EventResponse;

use crate::client::Client;
use crate::graph::GraphState;

#[derive(Parser)]
#[command(name = "mvm-flow", about = "Live agent lineage graph for a running mvm daemon")]
struct Args {
    /// Daemon address
    #[arg(long, env = "MVM_HOST", default_value = "http://127.0.0.1:24642")]
    host: String,

    /// Root sandbox (id or name); its whole descendant tree is shown
    root: String,
}

enum PollUpdate {
    Agents(Vec<AgentView>),
    Error(String),
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

    'app: loop {
        let _ = terminal.draw(|f| {
            draw(f, &mut graph, &snapshot, &root, root_gone, last_error.as_deref());
        });

        while let Ok(update) = rx.try_recv() {
            match update {
                PollUpdate::Agents(agents) => {
                    root_gone = !graph.reconcile(&agents, &root);
                    snapshot = agents;
                    last_error = None;
                }
                PollUpdate::Error(e) => last_error = Some(e),
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
                    if key.code == KeyCode::Char('q')
                        || (key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL))
                    {
                        break 'app;
                    }
                    if key.code == KeyCode::Char('f') {
                        graph.flow.request_fit_view();
                    } else if matches!(
                        graph.flow.handle_controls_key_event(key),
                        EventResponse::NotHandled
                    ) {
                        graph.flow.handle_key_event(key);
                    }
                }
                Ok(Event::Mouse(mouse)) => {
                    graph.flow.handle_mouse_event(mouse);
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

fn draw(
    f: &mut ratatui::Frame,
    graph: &mut GraphState,
    snapshot: &[AgentView],
    root: &str,
    root_gone: bool,
    last_error: Option<&str>,
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
        "q quit · f fit · arrows/hjkl pan · Tab select · +/- zoom · drag nodes to rearrange"
            .to_string()
    };
    f.render_widget(
        Paragraph::new(footer).style(Style::default().fg(Color::DarkGray)),
        chunks[2],
    );
}
