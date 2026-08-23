//! Modal mailbox view: the agent's notifications as an inbox — a scrollable
//! list (timestamp, id, kind; `●` pending / `○` delivered) on the left, the
//! selected notification rendered like an email (From/Kind/Id/Date/Status
//! headers + human-readable body) on the right. Data syncs from the 1s
//! `/api/v1/agents` poll.

use mvm_common::agent_api::{Notification, NotificationFrom, NotificationKind, TerminationReason};
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::detail::centered_rect;

/// One row in the mailbox: a notification plus whether it was delivered.
#[derive(Debug, Clone)]
pub struct MailEntry {
    pub notification: Notification,
    /// false = still queued on the control plane (pending).
    pub delivered: bool,
}

/// State of the open mailbox modal.
pub struct Mailbox {
    pub id: String,
    pub label: String,
    /// Merged mailbox (pending + delivered), newest first.
    entries: Vec<MailEntry>,
    /// Highlighted row.
    selected: usize,
    /// Rows skipped at the top of the list.
    list_scroll: usize,
    /// Rows scrolled past in the reading pane.
    body_scroll: u16,
    /// Id of the selected notification, to keep the selection across polls.
    selected_id: Option<String>,
    /// List inner rect from the last render (row hit-testing).
    list_area: Rect,
    /// Modal area from the last render, for click-outside-to-close.
    area: Rect,
}

impl Mailbox {
    pub fn new(id: &str, label: &str) -> Self {
        Self {
            id: id.to_string(),
            label: label.to_string(),
            entries: Vec::new(),
            selected: 0,
            list_scroll: 0,
            body_scroll: 0,
            selected_id: None,
            list_area: Rect::default(),
            area: Rect::default(),
        }
    }

    /// Rebuild from a fresh poll: pending (undelivered) plus the delivered
    /// history, merged newest-first. The selection follows the notification
    /// id so a refresh does not move the reader.
    pub fn sync(&mut self, pending: &[Notification], recent: &[Notification]) {
        let mut entries: Vec<MailEntry> = pending
            .iter()
            .map(|n| MailEntry {
                notification: n.clone(),
                delivered: false,
            })
            .chain(recent.iter().map(|n| MailEntry {
                notification: n.clone(),
                delivered: true,
            }))
            .collect();
        entries.sort_by_key(|e| std::cmp::Reverse(e.notification.created_at));
        self.entries = entries;
        self.selected = match &self.selected_id {
            Some(id) => self
                .entries
                .iter()
                .position(|e| &e.notification.id == id)
                .unwrap_or(0),
            None => 0,
        };
        if self.entries.is_empty() {
            self.selected = 0;
            self.selected_id = None;
        } else {
            self.selected = self.selected.min(self.entries.len() - 1);
            self.selected_id = Some(self.entries[self.selected].notification.id.clone());
        }
    }

    /// Move the selection by `delta` rows (clamped); resets the body scroll.
    pub fn select_move(&mut self, delta: i32) {
        if self.entries.is_empty() {
            return;
        }
        let max = self.entries.len() as i32 - 1;
        let next = (self.selected as i32 + delta).clamp(0, max);
        if next as usize != self.selected {
            self.selected = next as usize;
            self.selected_id = Some(self.entries[self.selected].notification.id.clone());
            self.body_scroll = 0;
        }
    }

    /// Scroll the reading pane body.
    pub fn body_scroll_by(&mut self, delta: i32) {
        self.body_scroll = (self.body_scroll as i32 + delta).max(0) as u16;
    }

    /// The highlighted entry, if any.
    pub fn selected_mail(&self) -> Option<&MailEntry> {
        self.entries.get(self.selected)
    }

    /// Select the row at a terminal position; true when the click landed in
    /// the list.
    pub fn click(&mut self, column: u16, row: u16) -> bool {
        if !self.list_area.contains(Position::new(column, row)) {
            return false;
        }
        let idx = self.list_scroll + (row - self.list_area.y) as usize;
        if idx < self.entries.len() {
            self.selected = idx;
            self.selected_id = Some(self.entries[idx].notification.id.clone());
            self.body_scroll = 0;
        }
        true
    }

    /// Whether a terminal position falls inside the modal (outside clicks
    /// close it).
    pub fn contains(&self, column: u16, row: u16) -> bool {
        self.area.contains(Position::new(column, row))
    }
}

pub fn draw_mailbox(f: &mut Frame, mb: &mut Mailbox) {
    let width = f.area().width.saturating_sub(6).clamp(50, 110);
    let height = f.area().height.saturating_sub(4).clamp(12, 34);
    let area = centered_rect(width, height, f.area());
    mb.area = area;
    f.render_widget(Clear, area);

    let pending = mb.entries.iter().filter(|e| !e.delivered).count();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(format!(
            " mailbox: {} ({}, {} pending) ",
            mb.label,
            mb.entries.len(),
            pending
        ));

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);
    let (main_area, hint_area) = (chunks[0], chunks[1]);

    let inner = block.inner(main_area);
    f.render_widget(block, main_area);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Min(30)])
        .split(inner);

    // --- inbox list ---------------------------------------------------------
    let list_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(" inbox ");
    let list_inner = list_block.inner(cols[0]);
    mb.list_area = list_inner;
    f.render_widget(list_block, cols[0]);

    if mb.entries.is_empty() {
        f.render_widget(
            Paragraph::new("  (no notifications)").style(Style::default().fg(Color::DarkGray)),
            list_inner,
        );
    } else {
        // Keep the selection visible, then render the window around it.
        let height = list_inner.height as usize;
        if mb.selected < mb.list_scroll {
            mb.list_scroll = mb.selected;
        } else if mb.selected >= mb.list_scroll + height {
            mb.list_scroll = mb.selected + 1 - height;
        }
        let rows: Vec<Line> = mb
            .entries
            .iter()
            .enumerate()
            .skip(mb.list_scroll)
            .take(height)
            .map(|(i, e)| {
                let n = &e.notification;
                let (dot, dot_style) = if e.delivered {
                    ("○", Style::default().fg(Color::DarkGray))
                } else {
                    ("●", Style::default().fg(Color::LightYellow))
                };
                let line = Line::from(vec![
                    Span::styled(dot, dot_style),
                    Span::raw(format!(
                        " {} {} {}",
                        n.created_at.format("%H:%M:%S"),
                        short_id(&n.id),
                        kind_label(&n.kind),
                    )),
                ]);
                if i == mb.selected {
                    line.style(
                        Style::default()
                            .bg(Color::DarkGray)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    line
                }
            })
            .collect();
        f.render_widget(Paragraph::new(rows), list_inner);
    }

    // --- reading pane --------------------------------------------------------
    let read_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(" message ");
    let read_inner = read_block.inner(cols[1]);
    f.render_widget(read_block, cols[1]);

    if let Some(lines) = mb.selected_mail().map(mail_lines) {
        let visible = read_inner.height as usize;
        mb.body_scroll = (mb.body_scroll as usize).min(lines.len().saturating_sub(visible)) as u16;
        let body = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((mb.body_scroll, 0));
        f.render_widget(body, read_inner);
    }

    // --- hint ----------------------------------------------------------------
    let hint = Line::from(vec![
        Span::styled("  ↑/↓", Style::default().fg(Color::Yellow)),
        Span::raw(" select  "),
        Span::styled("pgup/pgdn", Style::default().fg(Color::Yellow)),
        Span::raw(" scroll body  "),
        Span::styled("q", Style::default().fg(Color::Yellow)),
        Span::raw(" close"),
    ]);
    f.render_widget(
        Paragraph::new(hint).style(Style::default().fg(Color::DarkGray)),
        hint_area,
    );
}

/// The email-style reading pane: headers, then the human-readable body.
fn mail_lines(entry: &MailEntry) -> Vec<Line<'static>> {
    let n = &entry.notification;
    let mut lines = vec![
        Line::from(vec![
            Span::styled("From:   ", Style::default().fg(Color::Cyan)),
            Span::raw(from_label(&n.from)),
        ]),
        Line::from(vec![
            Span::styled("Kind:   ", Style::default().fg(Color::Cyan)),
            Span::raw(kind_label(&n.kind)),
        ]),
        Line::from(vec![
            Span::styled("Id:     ", Style::default().fg(Color::Cyan)),
            Span::raw(n.id.clone()),
        ]),
        Line::from(vec![
            Span::styled("Date:   ", Style::default().fg(Color::Cyan)),
            Span::raw(n.created_at.format("%Y-%m-%d %H:%M:%S UTC").to_string()),
        ]),
        Line::from(vec![
            Span::styled("Status: ", Style::default().fg(Color::Cyan)),
            if entry.delivered {
                Span::raw("delivered")
            } else {
                Span::styled("pending delivery", Style::default().fg(Color::LightYellow))
            },
        ]),
        Line::raw(""),
    ];
    for l in n.to_text().lines() {
        lines.push(Line::raw(l.to_string()));
    }
    lines
}

/// Short id form for list columns (notification ids are uuids; 8 is plenty).
fn short_id(id: &str) -> &str {
    &id[..id.len().min(8)]
}

/// The sender as an email-style `From:` value.
fn from_label(from: &NotificationFrom) -> String {
    match from {
        NotificationFrom::Daddy => "daddy (parent agent)".to_string(),
        NotificationFrom::LifecycleAlert => "control plane".to_string(),
        NotificationFrom::Child { id } => format!("child {id}"),
    }
}

/// The kind as an email-style subject label (kebab-case, the wire name).
fn kind_label(kind: &NotificationKind) -> String {
    match kind {
        NotificationKind::ChildTtlAboutToExpire { .. } => "child-ttl-about-to-expire",
        NotificationKind::RestartedAfterIdle => "restarted-after-idle",
        NotificationKind::NeedInput { .. } => "need-input",
        NotificationKind::Finished { .. } => "finished",
        NotificationKind::Terminated { reason } => match reason {
            TerminationReason::Faulted => "terminated (faulted)",
            TerminationReason::TtlExpired => "terminated (ttl-expired)",
        },
        NotificationKind::Input { .. } => "input",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notif_at(secs: i64) -> Notification {
        let mut n = Notification::input(serde_json::json!("task"));
        n.created_at = chrono::DateTime::from_timestamp(secs, 0).unwrap();
        n.id = format!("id-{secs}");
        n
    }

    #[test]
    fn sync_merges_newest_first_and_marks_pending() {
        let mut mb = Mailbox::new("id", "label");
        let pending = vec![notif_at(10), notif_at(40)];
        let recent = vec![notif_at(20), notif_at(30)];
        mb.sync(&pending, &recent);

        let ids: Vec<&str> = mb
            .entries
            .iter()
            .map(|e| e.notification.id.as_str())
            .collect();
        assert_eq!(ids, ["id-40", "id-30", "id-20", "id-10"]);
        assert!(!mb.entries[0].delivered); // id-40 came from pending
        assert!(mb.entries[1].delivered); // id-30 came from recent
        assert_eq!(mb.selected_mail().unwrap().notification.id, "id-40");
    }

    #[test]
    fn selection_follows_the_notification_id_across_polls() {
        let mut mb = Mailbox::new("id", "label");
        mb.sync(&[notif_at(10)], &[notif_at(20)]);
        mb.select_move(1); // select id-10
        assert_eq!(mb.selected_mail().unwrap().notification.id, "id-10");

        // Next poll: id-10 was delivered (moved to recent), a newer one queued.
        mb.sync(&[notif_at(30)], &[notif_at(10), notif_at(20)]);
        assert_eq!(mb.selected_mail().unwrap().notification.id, "id-10");
        assert!(mb.selected_mail().unwrap().delivered);

        // Selected id gone entirely: fall back to the top.
        mb.sync(&[], &[notif_at(30)]);
        assert_eq!(mb.selected_mail().unwrap().notification.id, "id-30");
    }

    #[test]
    fn select_move_clamps_at_the_edges() {
        let mut mb = Mailbox::new("id", "label");
        mb.sync(&[notif_at(1)], &[notif_at(2), notif_at(3)]);
        mb.select_move(-1);
        assert_eq!(mb.selected, 0);
        mb.select_move(10);
        assert_eq!(mb.selected, 2);
        mb.select_move(10);
        assert_eq!(mb.selected, 2);

        mb.sync(&[], &[]);
        mb.select_move(1); // empty: no panic, no-op
        assert_eq!(mb.selected, 0);
        assert!(mb.selected_mail().is_none());
    }

    #[test]
    fn mail_lines_have_email_headers_and_text_body() {
        let mut n = Notification::need_input("childdeadbeef".into(), serde_json::json!("which file?"));
        n.id = "n-1".to_string();
        let entry = MailEntry {
            notification: n,
            delivered: false,
        };
        let lines = mail_lines(&entry);
        let text: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        assert!(text[0].starts_with("From:   child childdeadbeef"));
        assert!(text[1].starts_with("Kind:   need-input"));
        assert!(text[2].starts_with("Id:     n-1"));
        assert!(text[4].contains("pending delivery"));
        assert_eq!(text.last().unwrap(), "Child childdeadbeef is requesting input: which file?");
    }
}
