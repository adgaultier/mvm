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
    Error(String),
    /// Result of an action the user triggered; shown briefly in the footer so
    /// the next poll's "connected — N sandboxes" doesn't swallow it.
    Notice {
        text: String,
        error: bool,
    },
}

/// Pending "really delete this?" prompt. Removing a sandbox destroys its
/// filesystem and cannot be undone, so it does not happen on one keystroke —
/// a mistyped key (or, before the console pane was sanitized, a byte the guest
/// provoked) should not be able to take a VM with it.
pub struct DeleteConfirm {
    pub id: String,
    pub label: String,
    pub running: bool,
}

impl DeleteConfirm {
    pub fn new(sb: &Sandbox) -> Self {
        Self {
            id: sb.id.to_string(),
            label: sb.name().to_string(),
            running: sb.state.is_alive(),
        }
    }
}

/// Which field the resize form is editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeField {
    Vcpus,
    Ram,
}

/// Editable vcpu/RAM values for one sandbox. Kept as text so typing digits
/// behaves the way it looks; parsed when applied.
pub struct ResizeForm {
    pub id: String,
    pub label: String,
    pub vcpus: String,
    pub ram_mib: String,
    pub field: ResizeField,
    /// A running VM keeps its allocation until it reboots.
    pub running: bool,
}

impl ResizeForm {
    pub fn new(sb: &Sandbox) -> Self {
        Self {
            id: sb.id.to_string(),
            label: sb.name().to_string(),
            vcpus: sb.spec.vcpus.to_string(),
            ram_mib: sb.spec.ram_mib.to_string(),
            field: ResizeField::Vcpus,
            running: sb.state.is_alive(),
        }
    }

    pub fn buffer(&mut self) -> &mut String {
        match self.field {
            ResizeField::Vcpus => &mut self.vcpus,
            ResizeField::Ram => &mut self.ram_mib,
        }
    }

    pub fn toggle_field(&mut self) {
        self.field = match self.field {
            ResizeField::Vcpus => ResizeField::Ram,
            ResizeField::Ram => ResizeField::Vcpus,
        };
    }

    pub fn type_digit(&mut self, c: char) {
        let buf = self.buffer();
        if buf.len() < 7 {
            buf.push(c);
        }
    }

    pub fn backspace(&mut self) {
        self.buffer().pop();
    }

    /// Nudge the active field: vcpus by 1, memory by 256 MiB.
    pub fn step(&mut self, up: bool) {
        let (step, min) = match self.field {
            ResizeField::Vcpus => (1u32, 1u32),
            ResizeField::Ram => (256, 64),
        };
        let current: u32 = self.buffer().parse().unwrap_or(min);
        let next = if up {
            current.saturating_add(step)
        } else {
            current.saturating_sub(step).max(min)
        };
        *self.buffer() = next.to_string();
    }

    /// Parsed values, or a message naming what is wrong.
    pub fn values(&self) -> Result<(u8, u32), String> {
        let vcpus: u8 = self
            .vcpus
            .parse()
            .map_err(|_| format!("'{}' is not a vcpu count (1-255)", self.vcpus))?;
        let ram: u32 = self
            .ram_mib
            .parse()
            .map_err(|_| format!("'{}' is not a memory size in MiB", self.ram_mib))?;
        Ok((vcpus, ram))
    }
}

pub struct App {
    pub tab: Tab,
    pub sandboxes: Vec<Sandbox>,
    pub images: Vec<ImageInfo>,
    pub table_state: TableState,
    pub status: String,
    pub daemon_ok: bool,
    pub should_quit: bool,
    /// Modal resize form; while it is open it owns the keyboard.
    pub resize: Option<ResizeForm>,
    /// Modal delete confirmation; same rule.
    pub confirm_delete: Option<DeleteConfirm>,
    notice: Option<(String, bool, std::time::Instant)>,
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
            status: "connecting…".into(),
            daemon_ok: false,
            should_quit: false,
            resize: None,
            confirm_delete: None,
            notice: None,
        }
    }

    /// How long an action's result stays in the footer.
    const NOTICE_TTL: std::time::Duration = std::time::Duration::from_secs(5);

    pub fn set_notice(&mut self, text: String, error: bool) {
        self.notice = Some((text, error, std::time::Instant::now()));
    }

    /// The footer message: a recent action result if there is one, else the
    /// connection status. `true` = render it as an error.
    pub fn footer_message(&self) -> (&str, bool) {
        match &self.notice {
            Some((text, error, at)) if at.elapsed() < Self::NOTICE_TTL => (text, *error),
            _ => (self.status.as_str(), !self.daemon_ok),
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
            return;
        }
        match self.selected_index() {
            // The list shrank under the cursor: hold on to the last row.
            Some(i) if i >= len => self.table_state.select(Some(len - 1)),
            // Nothing was selected because the list was empty. Come back to
            // the top: `s`/`x`/`d`/`r` act on the selected row, and the
            // bottom one is not what someone who just opened the TUI (or
            // whose sandboxes just reappeared) is looking at.
            None => self.table_state.select(Some(0)),
            Some(_) => {}
        }
    }

    fn current_len(&self) -> usize {
        match self.tab {
            Tab::Sandboxes => self.sandboxes.len(),
            Tab::Images => self.images.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::App;
    use mvm_common::{Sandbox, SandboxSpec};

    fn sandbox(name: &str) -> Sandbox {
        Sandbox::new(SandboxSpec {
            name: Some(name.to_string()),
            image: "alpine".into(),
            ..Default::default()
        })
    }

    #[test]
    fn selection_starts_on_the_first_row() {
        let mut app = App::new();
        assert_eq!(app.selected_index(), Some(0));
        // A poll's worth of data arriving must not move it: whatever row the
        // user is looking at is the row s/x/d/r act on.
        app.sandboxes = vec![sandbox("newest"), sandbox("older")];
        app.clamp_selection();
        assert_eq!(app.selected_index(), Some(0));
        assert_eq!(
            app.selected_sandbox().map(|s| s.name().to_string()),
            Some("newest".to_string())
        );
    }

    #[test]
    fn selection_recovers_when_the_list_shrinks() {
        let mut app = App::new();
        app.sandboxes = vec![sandbox("a"), sandbox("b"), sandbox("c")];
        app.next();
        app.next();
        assert_eq!(app.selected_index(), Some(2));
        app.sandboxes.truncate(1);
        app.clamp_selection();
        assert_eq!(app.selected_index(), Some(0));
        // An empty list has nothing to act on.
        app.sandboxes.clear();
        app.clamp_selection();
        assert_eq!(app.selected_index(), None);
        // …and once sandboxes exist again, selection comes back to the top.
        app.sandboxes = vec![sandbox("a"), sandbox("b")];
        app.clamp_selection();
        assert_eq!(app.selected_index(), Some(0));
    }
}
