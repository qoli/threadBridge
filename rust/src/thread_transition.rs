use anyhow::{Context, Result};

use crate::execution_mode::SessionExecutionSnapshot;
use crate::repository::{RecentCodexSessionEntry, SessionBinding, ThreadMetadata, ThreadStatus};

pub enum ThreadTransition {
    BindWorkspace {
        workspace_cwd: String,
        codex_thread_id: String,
        execution: SessionExecutionSnapshot,
    },
    VerifySession,
    SelectSession {
        session_id: String,
    },
    MarkBroken {
        reason: String,
    },
    Archive,
    Restore {
        message_thread_id: i32,
        title: String,
    },
}

#[derive(Debug)]
pub enum BindingMutation {
    Unchanged,
    Write(SessionBinding),
}

#[derive(Debug)]
pub struct TransitionResult {
    pub metadata: ThreadMetadata,
    pub binding: BindingMutation,
    pub recent_session: Option<RecentCodexSessionEntry>,
}

pub fn apply_transition(
    metadata: &ThreadMetadata,
    binding: Option<SessionBinding>,
    transition: ThreadTransition,
    now: &str,
) -> Result<TransitionResult> {
    match transition {
        ThreadTransition::BindWorkspace {
            workspace_cwd,
            codex_thread_id,
            execution,
        } => Ok(bind_workspace(
            metadata,
            workspace_cwd,
            codex_thread_id,
            execution,
            now,
        )),
        ThreadTransition::VerifySession => verify_session(metadata, binding, now),
        ThreadTransition::SelectSession { session_id } => {
            select_session(metadata, binding, session_id, now)
        }
        ThreadTransition::MarkBroken { reason } => mark_broken(metadata, binding, reason, now),
        ThreadTransition::Archive => Ok(archive(metadata, binding, now)),
        ThreadTransition::Restore {
            message_thread_id,
            title,
        } => Ok(restore(metadata, binding, message_thread_id, title)),
    }
}

fn bind_workspace(
    metadata: &ThreadMetadata,
    workspace_cwd: String,
    codex_thread_id: String,
    execution: SessionExecutionSnapshot,
    now: &str,
) -> TransitionResult {
    let binding = SessionBinding::fresh(
        Some(workspace_cwd.clone()),
        Some(codex_thread_id.clone()),
        execution,
    );
    let execution_mode = binding.current_execution_mode;
    TransitionResult {
        metadata: ThreadMetadata {
            last_codex_turn_at: Some(now.to_owned()),
            session_broken: false,
            session_broken_at: None,
            session_broken_reason: None,
            ..metadata.clone()
        },
        binding: BindingMutation::Write(binding.touched(now)),
        recent_session: Some(RecentCodexSessionEntry {
            session_id: codex_thread_id,
            updated_at: now.to_owned(),
            execution_mode,
        }),
    }
}

fn verify_session(
    metadata: &ThreadMetadata,
    binding: Option<SessionBinding>,
    now: &str,
) -> Result<TransitionResult> {
    let binding = binding.context("session binding is missing")?;
    Ok(TransitionResult {
        metadata: ThreadMetadata {
            last_codex_turn_at: Some(now.to_owned()),
            session_broken: false,
            session_broken_at: None,
            session_broken_reason: None,
            ..metadata.clone()
        },
        binding: BindingMutation::Write(binding.verified(now)),
        recent_session: None,
    })
}

fn select_session(
    metadata: &ThreadMetadata,
    binding: Option<SessionBinding>,
    session_id: String,
    now: &str,
) -> Result<TransitionResult> {
    let binding = binding.context("session binding is missing")?;
    let workspace_cwd = binding.workspace_cwd.clone();
    let execution_mode = binding.current_execution_mode;
    Ok(TransitionResult {
        metadata: ThreadMetadata {
            last_codex_turn_at: Some(now.to_owned()),
            session_broken: false,
            session_broken_at: None,
            session_broken_reason: None,
            ..metadata.clone()
        },
        binding: BindingMutation::Write(binding.selected_session(session_id.clone(), now)),
        recent_session: workspace_cwd.map(|_| RecentCodexSessionEntry {
            session_id,
            updated_at: now.to_owned(),
            execution_mode,
        }),
    })
}

fn mark_broken(
    metadata: &ThreadMetadata,
    binding: Option<SessionBinding>,
    reason: String,
    now: &str,
) -> Result<TransitionResult> {
    let binding = binding.context("session binding is missing")?;
    if binding
        .workspace_cwd
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
    {
        anyhow::bail!("session binding workspace is missing");
    }
    Ok(TransitionResult {
        metadata: ThreadMetadata {
            session_broken: true,
            session_broken_at: Some(now.to_owned()),
            session_broken_reason: Some(reason.clone()),
            ..metadata.clone()
        },
        binding: BindingMutation::Write(binding.broken(reason, now)),
        recent_session: None,
    })
}

fn archive(
    metadata: &ThreadMetadata,
    _binding: Option<SessionBinding>,
    now: &str,
) -> TransitionResult {
    TransitionResult {
        metadata: ThreadMetadata {
            archived_at: Some(now.to_owned()),
            status: ThreadStatus::Archived,
            ..metadata.clone()
        },
        binding: BindingMutation::Unchanged,
        recent_session: None,
    }
}

fn restore(
    metadata: &ThreadMetadata,
    _binding: Option<SessionBinding>,
    message_thread_id: i32,
    title: String,
) -> TransitionResult {
    let mut previous = metadata.previous_message_thread_ids.clone();
    if let Some(current) = metadata.message_thread_id {
        if current != message_thread_id && !previous.contains(&current) {
            previous.push(current);
        }
    }
    TransitionResult {
        metadata: ThreadMetadata {
            archived_at: None,
            message_thread_id: Some(message_thread_id),
            previous_message_thread_ids: previous,
            status: ThreadStatus::Active,
            title: Some(title),
            ..metadata.clone()
        },
        binding: BindingMutation::Unchanged,
        recent_session: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{BindingMutation, ThreadTransition, apply_transition};
    use crate::execution_mode::{ExecutionMode, SessionExecutionSnapshot};
    use crate::repository::{
        RunningInputPolicy, SessionBinding, ThreadMetadata, ThreadScope, ThreadStatus,
    };

    fn metadata() -> ThreadMetadata {
        ThreadMetadata {
            archived_at: None,
            chat_id: 1,
            created_at: "2026-03-24T00:00:00.000Z".to_owned(),
            last_codex_turn_at: None,
            message_thread_id: Some(7),
            previous_message_thread_ids: Vec::new(),
            running_input_policy: RunningInputPolicy::default(),
            scope: ThreadScope::Thread,
            session_broken: false,
            session_broken_at: None,
            session_broken_reason: None,
            status: ThreadStatus::Active,
            title: Some("Title".to_owned()),
            updated_at: "2026-03-24T00:00:00.000Z".to_owned(),
            thread_key: "thread-1".to_owned(),
        }
    }

    fn binding() -> SessionBinding {
        SessionBinding::fresh(
            Some("/tmp/workspace".to_owned()),
            Some("thr_bot".to_owned()),
            SessionExecutionSnapshot::from_mode(ExecutionMode::FullAuto),
        )
    }

    #[test]
    fn bind_workspace_clears_broken_and_records_recent_session() {
        let result = apply_transition(
            &ThreadMetadata {
                session_broken: true,
                session_broken_reason: Some("old".to_owned()),
                ..metadata()
            },
            None,
            ThreadTransition::BindWorkspace {
                workspace_cwd: "/tmp/workspace".to_owned(),
                codex_thread_id: "thr_new".to_owned(),
                execution: SessionExecutionSnapshot::from_mode(ExecutionMode::FullAuto),
            },
            "2026-03-24T01:00:00.000Z",
        )
        .unwrap();

        assert!(!result.metadata.session_broken);
        match result.binding {
            BindingMutation::Write(binding) => {
                assert_eq!(binding.current_codex_thread_id.as_deref(), Some("thr_new"));
            }
            BindingMutation::Unchanged => panic!("expected binding write"),
        }
        assert_eq!(
            result
                .recent_session
                .as_ref()
                .map(|entry| entry.session_id.as_str()),
            Some("thr_new")
        );
    }

    #[test]
    fn select_session_clears_adoption_state() {
        let mut existing = binding();
        existing.tui_active_codex_thread_id = Some("thr_tui".to_owned());
        existing.tui_session_adoption_pending = true;
        existing.tui_session_adoption_prompt_message_id = Some(42);

        let result = apply_transition(
            &metadata(),
            Some(existing),
            ThreadTransition::SelectSession {
                session_id: "thr_cli".to_owned(),
            },
            "2026-03-24T01:00:00.000Z",
        )
        .unwrap();

        match result.binding {
            BindingMutation::Write(binding) => {
                assert_eq!(binding.current_codex_thread_id.as_deref(), Some("thr_cli"));
                assert_eq!(binding.tui_active_codex_thread_id, None);
                assert!(!binding.tui_session_adoption_pending);
                assert_eq!(binding.tui_session_adoption_prompt_message_id, None);
            }
            BindingMutation::Unchanged => panic!("expected binding write"),
        }
    }

    #[test]
    fn mark_broken_requires_workspace_binding() {
        let error = apply_transition(
            &metadata(),
            Some(SessionBinding::fresh(
                None,
                Some("thr_bot".to_owned()),
                SessionExecutionSnapshot::from_mode(ExecutionMode::FullAuto),
            )),
            ThreadTransition::MarkBroken {
                reason: "lost".to_owned(),
            },
            "2026-03-24T01:00:00.000Z",
        )
        .unwrap_err();

        assert!(error.to_string().contains("workspace is missing"));
    }

    #[test]
    fn restore_keeps_broken_state_and_updates_topic() {
        let result = apply_transition(
            &ThreadMetadata {
                archived_at: Some("2026-03-24T00:30:00.000Z".to_owned()),
                session_broken: true,
                session_broken_reason: Some("lost".to_owned()),
                status: ThreadStatus::Archived,
                ..metadata()
            },
            Some(binding()),
            ThreadTransition::Restore {
                message_thread_id: 9,
                title: "Restored".to_owned(),
            },
            "2026-03-24T01:00:00.000Z",
        )
        .unwrap();

        assert!(matches!(result.metadata.status, ThreadStatus::Active));
        assert!(result.metadata.session_broken);
        assert_eq!(result.metadata.message_thread_id, Some(9));
        assert!(matches!(result.binding, BindingMutation::Unchanged));
    }
}
