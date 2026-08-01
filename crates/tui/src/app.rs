//! TUI application state.

use mvm_common::{ImageInfo, Sandbox};
use ratatui::widgets::TableState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Sandboxes,
    Images,
}

pub enum PollUpdate {
    Data {
        sandboxes: Vec<Sandbox>,
        images: Vec<ImageInfo>,
    },
    Logs(String),
    Error(String),
}

pub struct App {
    pub tab: Tab,
    pub sandboxes: Vec<Sandbox>,
    pub images: Vec<ImageInfo>,
    pub table_state: TableState,
    pub logs: String,
    pub status: String,
    pub daemon_ok: bool,
    pub should_quit: bool,
}

impl App {
    pub fn new() -> Self {
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        Self {
            tab: Tab::Sandboxes,
            sandboxes: vec![],
            images: vec![],
            table_state,
            logs: String::new(),
            status: "connecting…".into(),
            daemon_ok: false,
            should_quit: false,
        }
    }

    pub fn selected_index(&self) -> Option<usize> {
        self.table_state.selected()
    }

    pub fn selected_sandbox(&self) -> Option<&Sandbox> {
        if self.tab != Tab::Sandboxes {
            return None;
        }
        self.selected_index().and_then(|i| self.sandboxes.get(i))
    }

    pub fn next(&mut self) {
        let len = self.current_len();
        if len == 0 {
            return;
        }
        let i = self.selected_index().map(|i| (i + 1) % len).unwrap_or(0);
        self.table_state.select(Some(i));
    }

    pub fn previous(&mut self) {
        let len = self.current_len();
        if len == 0 {
            return;
        }
        let i = self
            .selected_index()
            .map(|i| if i == 0 { len - 1 } else { i - 1 })
            .unwrap_or(0);
        self.table_state.select(Some(i));
    }

    pub fn clamp_selection(&mut self) {
        let len = self.current_len();
        if len == 0 {
            self.table_state.select(None);
        } else if self.selected_index().map(|i| i >= len).unwrap_or(true) {
            self.table_state.select(Some(len - 1));
        }
    }

    fn current_len(&self) -> usize {
        match self.tab {
            Tab::Sandboxes => self.sandboxes.len(),
            Tab::Images => self.images.len(),
        }
    }
}
