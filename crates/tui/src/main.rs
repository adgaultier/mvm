//! mvm-tui: terminal UI for the mvm daemon.

mod app;
mod client;
mod ui;

use std::sync::mpsc;
use std::time::Duration;

use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use app::{App, DeleteConfirm, PollUpdate, ResizeForm, Tab};
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
                PollUpdate::Error(e) => {
                    app.daemon_ok = false;
                    app.status = format!("daemon: {e}");
                }
                PollUpdate::Notice { text, error } => app.set_notice(text, error),
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
                // Modals own the keyboard while they are open.
                if app.resize.is_some() {
                    handle_resize_key(app, key, client, tx);
                    continue;
                }
                if app.confirm_delete.is_some() {
                    handle_confirm_key(app, key, client, tx);
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
                            app.confirm_delete = Some(DeleteConfirm::new(sb));
                        }
                    }
                    KeyCode::Char('r') => {
                        if let Some(sb) = app.selected_sandbox() {
                            app.resize = Some(ResizeForm::new(sb));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Console history the poller keeps around — more than the pane can show, so
/// scrolling room is there without hauling the whole log every 1.5s.
const CONSOLE_TAIL_LINES: usize = 200;

/// Keys for the delete confirmation. Only `y` goes through; anything that is
/// not an explicit yes or no leaves the prompt up rather than guessing.
fn handle_confirm_key(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    client: &Client,
    tx: &mpsc::Sender<PollUpdate>,
) {
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            let Some(confirm) = app.confirm_delete.take() else { return };
            let c = client.clone();
            let t = tx.clone();
            std::thread::spawn(move || {
                let _ = c.stop(&confirm.id);
                let notice = match c.remove(&confirm.id) {
                    Ok(()) => PollUpdate::Notice {
                        text: format!("{} deleted", confirm.label),
                        error: false,
                    },
                    Err(e) => PollUpdate::Notice {
                        text: format!("delete {}: {e}", confirm.label),
                        error: true,
                    },
                };
                let _ = t.send(notice);
            });
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q') => {
            app.confirm_delete = None;
        }
        _ => {}
    }
}

/// Keys for the modal resize form. Enter applies; ^R also reboots the VM,
/// which is the only way a running one picks up the new size.
fn handle_resize_key(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    client: &Client,
    tx: &mpsc::Sender<PollUpdate>,
) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let Some(form) = app.resize.as_mut() else { return };

    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => app.resize = None,
        KeyCode::Tab | KeyCode::Up | KeyCode::Down | KeyCode::BackTab => form.toggle_field(),
        KeyCode::Char(c @ '0'..='9') => form.type_digit(c),
        KeyCode::Backspace => form.backspace(),
        KeyCode::Char('+') | KeyCode::Char('=') | KeyCode::Right => form.step(true),
        KeyCode::Char('-') | KeyCode::Char('_') | KeyCode::Left => form.step(false),
        KeyCode::Char('r') if ctrl => apply_resize(app, client, tx, true),
        KeyCode::Enter => apply_resize(app, client, tx, false),
        _ => {}
    }
}

fn apply_resize(
    app: &mut App,
    client: &Client,
    tx: &mpsc::Sender<PollUpdate>,
    restart: bool,
) {
    let Some(form) = app.resize.as_ref() else { return };
    let (vcpus, ram_mib) = match form.values() {
        Ok(values) => values,
        Err(message) => {
            app.set_notice(message, true);
            return;
        }
    };
    let id = form.id.clone();
    let label = form.label.clone();
    let running = form.running;
    app.resize = None;
    if running && restart {
        // The VM is only down for ~100ms, far less than the 1.5s poll, so the
        // table never shows "stopped" — without this the reboot is invisible
        // and looks like nothing happened.
        app.set_notice(format!("restarting {label}…"), false);
    }

    let c = client.clone();
    let t = tx.clone();
    std::thread::spawn(move || {
        let notice = match c.resize(&id, vcpus, ram_mib) {
            Err(e) => PollUpdate::Notice { text: format!("resize {label}: {e}"), error: true },
            Ok(sb) => {
                let size = format!("{} vcpu / {} MiB", sb.spec.vcpus, sb.spec.ram_mib);
                if !running {
                    PollUpdate::Notice { text: format!("{label} resized to {size}"), error: false }
                } else if !restart {
                    PollUpdate::Notice {
                        text: format!("{label} resized to {size} — restart to apply (^r does both)"),
                        error: false,
                    }
                } else {
                    match c.stop(&id).and_then(|_| c.start(&id)) {
                        Ok(()) => PollUpdate::Notice {
                            text: format!("{label} restarted with {size}"),
                            error: false,
                        },
                        Err(e) => PollUpdate::Notice {
                            text: format!("{label} resized to {size} but restart failed: {e}"),
                            error: true,
                        },
                    }
                }
            }
        };
        let _ = t.send(notice);
    });
}

/// Periodically fetch sandboxes/images.
fn spawn_poller(client: Client, tx: mpsc::Sender<PollUpdate>) {
    std::thread::spawn(move || loop {
        match client.list_sandboxes() {
            Ok(sandboxes) => {
                let images = client.list_images().unwrap_or_default();
                let _ = tx.send(PollUpdate::Data { sandboxes, images });
            }
            Err(e) => {
                let _ = tx.send(PollUpdate::Error(e));
            }
        }
        std::thread::sleep(Duration::from_millis(1500));
    });
}
