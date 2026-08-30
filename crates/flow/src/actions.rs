//! Lifecycle actions the context menu can trigger on a node. The three
//! children actions propagate their action to the node's whole lineage of
//! descendants, computed client-side and applied via the existing per-sandbox
//! lifecycle routes.

use mvm_common::SandboxState;
use ratatui::style::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
    Delete,
    StartChildren,
    StopChildren,
    DeleteChildren,
}

impl Action {
    pub const ALL: [Action; 6] = [
        Action::Start,
        Action::Stop,
        Action::Delete,
        Action::StartChildren,
        Action::StopChildren,
        Action::DeleteChildren,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Action::Start => "start",
            Action::Stop => "stop",
            Action::Delete => "delete",
            Action::StartChildren => "start children",
            Action::StopChildren => "stop children",
            Action::DeleteChildren => "delete children",
        }
    }

    pub fn color(self) -> Option<Color> {
        match self {
            Action::Start => Some(Color::Green),
            Action::Stop => Some(Color::Yellow),
            Action::Delete => Some(Color::Red),
            // Children actions are a distinct "lineage" category: they act on
            // the node's whole descendant tree, not on the node's own VM, so
            // they get a neutral colour separate from the single-VM status
            // colours above.
            Action::StartChildren | Action::StopChildren | Action::DeleteChildren => {
                Some(Color::Blue)
            }
        }
    }

    /// Start anything not running, stop only a running VM, delete always.
    /// Children actions propagate to descendants and so apply regardless of
    /// the parent's own state — they only need the node to have children,
    /// which the menu enforces when it decides to show the row.
    pub fn enabled(self, state: SandboxState) -> bool {
        match self {
            Action::Start => !matches!(state, SandboxState::Running),
            Action::Stop => matches!(state, SandboxState::Running),
            Action::Delete => true,
            Action::StartChildren | Action::StopChildren | Action::DeleteChildren => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mvm_common::SandboxState::*;

    #[test]
    fn action_state_gating() {
        for state in [Created, Stopped, Exited, Failed] {
            assert!(Action::Start.enabled(state));
            assert!(!Action::Stop.enabled(state));
            assert!(Action::Delete.enabled(state));
        }
        assert!(!Action::Start.enabled(Running));
        assert!(Action::Stop.enabled(Running));
        assert!(Action::Delete.enabled(Running));
    }

    #[test]
    fn children_actions_apply_regardless_of_state() {
        for action in Action::ALL {
            if matches!(
                action,
                Action::StartChildren | Action::StopChildren | Action::DeleteChildren
            ) {
                // Propagate acts on descendants, so the parent's own state
                // never disables it.
                for state in [Created, Running, Stopped, Exited, Failed] {
                    assert!(action.enabled(state), "{action:?} on {state}");
                }
                // And they're triggerable: a colour means the menu's
                // selection/click paths will pick them up.
                assert!(action.color().is_some(), "{action:?} should be triggerable");
            }
        }
    }
}
