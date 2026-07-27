/// UI state for the Approvals view — browse and resolve pending approval requests.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum ApprovalsMode {
    #[default]
    List,
    /// Show the 3-option confirm widget for the selected item.
    Confirm,
    /// Typing a reject reason (optional free-text).
    RejectReason,
}

/// UI state for the Approvals view.
#[derive(Debug, Default)]
pub struct ApprovalsViewState {
    /// Index of the currently highlighted approval in the list.
    pub selected_idx:   usize,
    /// Current interaction mode.
    pub mode:           ApprovalsMode,
    /// Reject reason typed by the operator (empty = no reason). ux.10: `tui_input`
    /// backed for cursor movement / word-edit / paste.
    pub reject_reason:  tui_input::Input,
    /// Feedback shown after a write (approve / reject result or error).
    pub result_msg:     Option<String>,
    /// ID of the approval item that entered Confirm/RejectReason mode.
    /// Pinned at List→Confirm time so a list refresh cannot swap the target.
    pub confirmed_id:   Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approvals_mode_default_is_list() {
        assert_eq!(ApprovalsMode::default(), ApprovalsMode::List);
    }

    #[test]
    fn approvals_view_state_default_is_zero_idx() {
        let s = ApprovalsViewState::default();
        assert_eq!(s.selected_idx, 0);
        assert_eq!(s.mode, ApprovalsMode::List);
        assert!(s.reject_reason.value().is_empty());
        assert!(s.result_msg.is_none());
    }

    #[test]
    fn approvals_view_state_can_accumulate_reason() {
        let s = ApprovalsViewState {
            reject_reason: tui_input::Input::new("too risky".to_string()),
            ..Default::default()
        };
        assert_eq!(s.reject_reason.value(), "too risky");
    }

    #[test]
    fn approvals_mode_transitions_are_distinct() {
        assert_ne!(ApprovalsMode::List, ApprovalsMode::Confirm);
        assert_ne!(ApprovalsMode::Confirm, ApprovalsMode::RejectReason);
        assert_ne!(ApprovalsMode::List, ApprovalsMode::RejectReason);
    }
}
