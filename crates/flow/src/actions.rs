//! Lifecycle actions the context menu can trigger on a node (and the three
//! children placeholders, not yet wired).

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
            _ => None,
        }
    }

    /// Start anything not running, stop only a running VM, delete always;
    /// children actions are placeholders and never clickable.
    pub fn enabled(self, state: SandboxState) -> bool {
        match self {
            Action::Start => !matches!(state, SandboxState::Running),
            Action::Stop => matches!(state, SandboxState::Running),
            Action::Delete => true,
            _ => false,
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
    fn children_actions_are_never_enabled() {
        for action in Action::ALL {
            if matches!(
                action,
                Action::StartChildren | Action::StopChildren | Action::DeleteChildren
            ) {
                for state in [Created, Running, Stopped, Exited, Failed] {
                    assert!(!action.enabled(state), "{action:?} on {state}");
                }
            }
        }
    }
}
