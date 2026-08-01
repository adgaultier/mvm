//! mvm-tui: terminal UI for the mvm daemon.

mod app;
mod client;
mod ui;

use std::sync::mpsc;
use std::time::Duration;

use clap::Parser;
use crossterm::event::{self, Event, KeyCode};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use app::{App, PollUpdate, Tab};
use client::Client;

#[derive(Parser)]
#[command(name = "mvm-tui", about = "Terminal UI for mvm microVM sandboxes")]
struct Args {
    /// Daemon address.
    #[arg(long, env = "MVM_HOST", default_value = "http://127.0.0.1:24642")]
    host: String,
}

fn main() -> std::io::Result<()> {
    let args = Args::parse();
    let client = Client::new(&args.host);

    // Channel fed by the background poller thread.
    let (tx, rx) = mpsc::channel::<PollUpdate>();
    spawn_poller(client.clone(), tx.clone());

    // Terminal setup.
    enable_raw_mode()?;
    std::io::stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;

    let mut app = App::new();
    let result = run_loop(&mut terminal, &mut app, &rx, &client, &tx);

    disable_raw_mode()?;
    std::io::stdout().execute(LeaveAlternateScreen)?;
    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
    rx: &mpsc::Receiver<PollUpdate>,
    client: &Client,
    tx: &mpsc::Sender<PollUpdate>,
) -> std::io::Result<()> {
    loop {
        terminal.draw(|f| ui::draw(f, app))?;

        // Drain poller updates.
        while let Ok(update) = rx.try_recv() {
            match update {
                PollUpdate::Data { sandboxes, images } => {
                    app.daemon_ok = true;
                    app.status = format!("connected — {} sandboxes", sandboxes.len());
                    app.sandboxes = sandboxes;
                    app.images = images;
                    app.clamp_selection();
                }
                PollUpdate::Logs(logs) => app.logs = logs,
                PollUpdate::Error(e) => {
                    app.daemon_ok = false;
                    app.status = format!("daemon: {e}");
                }
            }
        }

        if app.should_quit {
            return Ok(());
        }

        if event::poll(Duration::from_millis(150))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != crossterm::event::KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') => app.should_quit = true,
                    KeyCode::Tab | KeyCode::Char('2') | KeyCode::Char('1') => {
                        app.tab = match (app.tab, key.code) {
                            (_, KeyCode::Char('1')) => Tab::Sandboxes,
                            (_, KeyCode::Char('2')) => Tab::Images,
                            (Tab::Sandboxes, _) => Tab::Images,
                            (Tab::Images, _) => Tab::Sandboxes,
                        };
                        app.table_state.select(Some(0));
                    }
                    KeyCode::Char('j') | KeyCode::Down => app.next(),
                    KeyCode::Char('k') | KeyCode::Up => app.previous(),
                    KeyCode::Char('g') => app.table_state.select(Some(0)),
                    KeyCode::Char('s') => {
                        if let Some(sb) = app.selected_sandbox() {
                            let id = sb.id.to_string();
                            let c = client.clone();
                            let t = tx.clone();
                            std::thread::spawn(move || {
                                if let Err(e) = c.start(&id) {
                                    let _ = t.send(PollUpdate::Error(e));
                                }
                            });
                        }
                    }
                    KeyCode::Char('x') => {
                        if let Some(sb) = app.selected_sandbox() {
                            let id = sb.id.to_string();
                            let c = client.clone();
                            let t = tx.clone();
                            std::thread::spawn(move || {
                                if let Err(e) = c.stop(&id) {
                                    let _ = t.send(PollUpdate::Error(e));
                                }
                            });
                        }
                    }
                    KeyCode::Char('d') => {
                        if let Some(sb) = app.selected_sandbox() {
                            let id = sb.id.to_string();
                            let c = client.clone();
                            let t = tx.clone();
                            std::thread::spawn(move || {
                                let _ = c.stop(&id);
                                if let Err(e) = c.remove(&id) {
                                    let _ = t.send(PollUpdate::Error(e));
                                }
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Periodically fetch sandboxes/images (+ logs of the selected sandbox is
/// fetched in a second lightweight loop via the same channel).
fn spawn_poller(client: Client, tx: mpsc::Sender<PollUpdate>) {
    std::thread::spawn(move || loop {
        match client.list_sandboxes() {
            Ok(sandboxes) => {
                let images = client.list_images().unwrap_or_default();
                // Fetch logs for the newest running sandbox, if any.
                if let Some(running) = sandboxes.iter().find(|s| s.state.is_alive()) {
                    if let Ok(logs) = client.logs(&running.id.to_string()) {
                        let _ = tx.send(PollUpdate::Logs(logs));
                    }
                }
                let _ = tx.send(PollUpdate::Data { sandboxes, images });
            }
            Err(e) => {
                let _ = tx.send(PollUpdate::Error(e));
            }
        }
        std::thread::sleep(Duration::from_millis(1500));
    });
}
