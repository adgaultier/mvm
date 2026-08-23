use std::collections::{HashMap, HashSet};

use mvm_common::agent_api::{AgentStatus, AgentView, NotificationKind, TerminationReason};
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use rataflow::{
    Edge, Flow, HandlePosition, Node, NodeContent, NodeRenderContext, StepEdge, Sugiyama,
};

pub const NODE_WIDTH: f64 = 30.0;
pub const NODE_HEIGHT: f64 = 6.0;

#[derive(Debug)]
pub struct AgentNode {
    pub name: String,
    pub short_id: String,
    pub status: AgentStatus,
    pub vcpus: u8,
    pub ram_mib: u32,
    pub ttl_secs: Option<i64>,
    /// Queued-but-undelivered notifications (the mailbox badge).
    pub pending: usize,
}

fn ttl_remaining(deadline: Option<chrono::DateTime<chrono::Utc>>) -> Option<i64> {
    deadline.map(|d| (d - chrono::Utc::now()).num_seconds())
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

impl AgentNode {
    pub fn from_view(view: &AgentView) -> Self {
        let label = view
            .name
            .clone()
            .unwrap_or_else(|| short_id(view.id.as_str()));
        Self {
            name: label,
            short_id: short_id(view.id.as_str()),
            status: view.status,
            vcpus: view.vcpus,
            ram_mib: view.ram_mib,
            ttl_secs: ttl_remaining(view.ttl_deadline),
            pending: view.pending_notifications.len(),
        }
    }

    pub fn update(&mut self, view: &AgentView) {
        self.status = view.status;
        self.vcpus = view.vcpus;
        self.ram_mib = view.ram_mib;
        self.ttl_secs = ttl_remaining(view.ttl_deadline);
        self.pending = view.pending_notifications.len();
    }
}

impl NodeContent for AgentNode {
    fn render(&self, ctx: &NodeRenderContext, buf: &mut Buffer) {
        let (color, label) = match self.status {
            AgentStatus::Ready => (Color::Green, "READY  "),
            AgentStatus::Running => (Color::Yellow, "RUNNING"),
            AgentStatus::Booting => (Color::Cyan, "BOOTING"),
            AgentStatus::Stopped => (Color::DarkGray, "STOPPED"),
            AgentStatus::Failed => (Color::Red, "FAILED "),
            AgentStatus::Idle => (Color::Magenta, "IDLE   "),
        };
        let mut status_style = Style::default().fg(color);
        let mut border_style = Style::default().fg(Color::DarkGray);
        if ctx.selected {
            status_style = status_style.add_modifier(Modifier::BOLD);
            border_style = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
        }
        let ttl_span = match self.ttl_secs {
            None => Span::styled("no ttl", Style::default().fg(Color::DarkGray)),
            Some(s) if s <= 0 => Span::styled("ttl expired", Style::default().fg(Color::Red)),
            Some(s) => Span::styled(
                format!("ttl {}s", format_secs(s)),
                Style::default().fg(Color::LightMagenta),
            ),
        };
        let mut lines = vec![
            Line::from(vec![
                Span::styled(label, status_style),
                Span::styled(
                    format!(" {}", self.short_id),
                    Style::default().fg(Color::DarkGray),
                ),
            ]),
            Line::from(format!("{} cpu / {} MiB", self.vcpus, self.ram_mib)),
            Line::from(ttl_span),
        ];
        // Mailbox badge: undelivered notifications waiting on this agent.
        if self.pending > 0 {
            lines.push(Line::from(Span::styled(
                format!("✉ {} pending", self.pending),
                Style::default()
                    .fg(Color::LightYellow)
                    .add_modifier(Modifier::BOLD),
            )));
        }
        let block = ratatui::widgets::Block::bordered()
            .title(self.name.clone())
            .border_style(border_style);
        Paragraph::new(lines).block(block).render(ctx.area, buf);
    }
}

fn format_secs(s: i64) -> String {
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        s.to_string()
    }
}

/// Short edge label for the last notification the child received.
pub fn edge_label(view: &AgentView) -> Option<String> {
    let n = view.last_notification.as_ref()?;
    Some(match &n.kind {
        NotificationKind::Finished { exit_code, .. } => match exit_code {
            Some(c) => format!("finished:{c}"),
            None => "finished".to_string(),
        },
        NotificationKind::NeedInput { .. } => "need input".to_string(),
        NotificationKind::Input { .. } => "input".to_string(),
        NotificationKind::Terminated { reason } => match reason {
            TerminationReason::Faulted => "faulted".to_string(),
            TerminationReason::TtlExpired => "ttl expired".to_string(),
        },
        NotificationKind::RestartedAfterIdle => "restarted".to_string(),
        NotificationKind::ChildTtlAboutToExpire { remaining_secs, .. } => {
            format!("ttl soon {remaining_secs}s")
        }
    })
}

fn edge_id(parent: &str, child: &str) -> String {
    format!("{parent}->{child}")
}

pub struct GraphState {
    pub flow: Flow<AgentNode, StepEdge>,
    laid_out: bool,
}

impl GraphState {
    pub fn new() -> Self {
        Self {
            flow: Flow::with_graph(Vec::new(), Vec::new()).expect("empty graph is valid"),
            laid_out: false,
        }
    }

    /// Diff the flow against the fresh daemon snapshot: root + descendants
    /// only. Returns true when the root sandbox no longer exists.
    pub fn reconcile(&mut self, agents: &[AgentView], root: &str) -> bool {
        let by_id: HashMap<&str, &AgentView> =
            agents.iter().map(|a| (a.id.as_str(), a)).collect();

        let mut visible: HashSet<String> = HashSet::new();
        if let Some(root_view) = by_id.get(root) {
            visible.insert(root.to_string());
            let mut stack = vec![*root_view];
            while let Some(v) = stack.pop() {
                for child in &v.children {
                    if visible.insert(child.to_string()) {
                        if let Some(cv) = by_id.get(child.as_str()) {
                            stack.push(cv);
                        }
                    }
                }
            }
        }

        let gone: Vec<String> = self
            .flow
            .nodes()
            .map(|n| n.id.clone())
            .filter(|id| !visible.contains(id))
            .collect();
        for id in &gone {
            self.flow.remove_node(id);
        }
        let mut structural = !gone.is_empty();

        for id in &visible {
            let view = by_id[id.as_str()];
            if self.flow.node(id).is_none() {
                // Vertical graph: edges leave the parent's bottom and enter
                // the child's top. These must be set at creation: the
                // `set_handles_hidden` below materializes them as the node's
                // (hidden) handles, and `apply_layout` cannot change them
                // afterwards.
                let node = Node::new(
                    id.clone(),
                    (0.0, 0.0),
                    (NODE_WIDTH, NODE_HEIGHT),
                    AgentNode::from_view(view),
                )
                .with_source_position(HandlePosition::Bottom)
                .with_target_position(HandlePosition::Top)
                .with_connectable(false)
                .with_deletable(false);
                let _ = self.flow.add_node(node);
                self.flow.set_handles_hidden(id, true);
                structural = true;
            }
            if let Some(content) = self.flow.node_content_mut(id) {
                content.update(view);
            }
        }

        let mut want_edges: HashSet<String> = HashSet::new();
        for id in &visible {
            let view = by_id[id.as_str()];
            if let Some(parent) = &view.parent {
                if visible.contains(parent.as_str()) {
                    want_edges.insert(edge_id(parent.as_str(), id));
                }
            }
        }
        let stale: Vec<String> = self
            .flow
            .edges()
            .iter()
            .map(|e| e.id.clone())
            .filter(|id| !want_edges.contains(id))
            .collect();
        for id in &stale {
            self.flow.remove_edge(id);
        }
        if !stale.is_empty() {
            structural = true;
        }
        for eid in &want_edges {
            if self.flow.edge(eid).is_none() {
                if let Some((p, c)) = eid.split_once("->") {
                    let edge =
                        Edge::new(eid.clone(), p.to_string(), c.to_string()).with_deletable(false);
                    let _ = self.flow.add_edge(edge);
                    structural = true;
                }
            }
        }

        for id in &visible {
            let view = by_id[id.as_str()];
            if let Some(parent) = &view.parent {
                if visible.contains(parent.as_str()) {
                    let eid = edge_id(parent.as_str(), id);
                    self.flow.set_edge_label(&eid, edge_label(view));
                }
            }
        }

        if structural {
            self.flow.apply_layout(
                Sugiyama::vertical()
                    .with_node_spacing(6.0)
                    .with_rank_spacing(3.0),
            );
            if !self.laid_out {
                self.flow.request_fit_view();
                self.laid_out = true;
            }
        }

        visible.contains(root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mvm_common::agent_api::AgentView;
    use mvm_common::{SandboxId, SandboxSpec, SandboxState};

    fn view(id: &str, parent: Option<&str>) -> AgentView {
        view_state(id, parent, SandboxState::Running)
    }

    fn view_state(id: &str, parent: Option<&str>, state: SandboxState) -> AgentView {
        let spec = SandboxSpec {
            image: "alpine".into(),
            ..Default::default()
        };
        let mut sb = mvm_common::Sandbox::new(spec);
        sb.id = SandboxId::from(id);
        sb.state = state;
        sb.booted_at = Some(chrono::Utc::now());
        sb.ready_at = Some(chrono::Utc::now());
        sb.parent = parent.map(SandboxId::from);
        AgentView::new(&sb, Vec::new())
    }

    fn with_children(mut v: AgentView, children: &[&str]) -> AgentView {
        v.children = children.iter().map(|c| SandboxId::from(*c)).collect();
        v
    }

    #[test]
    fn reconcile_builds_root_tree_and_updates_content() {
        let root = with_children(view("rootaaaa", None), &["childbbbb"]);
        let child = view("childbbbb", Some("rootaaaa"));
        let orphan = view("orphanc1", None);

        let mut g = GraphState::new();
        let alive = g.reconcile(&[root.clone(), child.clone(), orphan.clone()], "rootaaaa");
        assert!(alive);
        assert!(g.flow.node("rootaaaa").is_some());
        assert!(g.flow.node("childbbbb").is_some());
        assert!(g.flow.node("orphanc1").is_none());
        assert_eq!(g.flow.edges().len(), 1);
        assert_eq!(g.flow.edges()[0].source, "rootaaaa");
        assert_eq!(g.flow.edges()[0].target, "childbbbb");

        // vertical tree: edges leave the parent's bottom, enter the child's
        // top (must hold even though handles are hidden — they are
        // materialized from these positions at node creation)
        for id in ["rootaaaa", "childbbbb"] {
            let node = g.flow.node(id).unwrap();
            assert_eq!(node.source_position, HandlePosition::Bottom);
            assert_eq!(node.target_position, HandlePosition::Top);
        }

        // status change flows into the node content without rebuilding
        let child2 = view_state("childbbbb", Some("rootaaaa"), SandboxState::Stopped);
        let root2 = with_children(view("rootaaaa", None), &["childbbbb"]);
        g.reconcile(&[root2, child2], "rootaaaa");
        let content = g.flow.node_content_mut("childbbbb").unwrap();
        assert_eq!(content.status, AgentStatus::Stopped);
        assert_eq!(g.flow.edges().len(), 1);
    }

    #[test]
    fn reconcile_removes_departed_subtree() {
        let root = with_children(view("rootaaaa", None), &["childbbbb"]);
        let child = view("childbbbb", Some("rootaaaa"));
        let mut g = GraphState::new();
        g.reconcile(&[root.clone(), child], "rootaaaa");
        assert_eq!(g.flow.nodes().count(), 2);

        let root_alone = view("rootaaaa", None);
        g.reconcile(&[root_alone], "rootaaaa");
        assert_eq!(g.flow.nodes().count(), 1);
        assert!(g.flow.edges().is_empty());

        // root gone: everything is cleared and reconcile reports it
        assert!(!g.reconcile(&[], "rootaaaa"));
        assert_eq!(g.flow.nodes().count(), 0);
    }

    #[test]
    fn edge_label_formats_notification_kinds() {
        let mut v = view("childbbbb", Some("rootaaaa"));
        assert_eq!(edge_label(&v), None);
        v.last_notification = Some(mvm_common::agent_api::Notification::finished(
            SandboxId::from("x"),
            Some(0),
            serde_json::Value::Null,
        ));
        assert_eq!(edge_label(&v).unwrap(), "finished:0");
    }
}
