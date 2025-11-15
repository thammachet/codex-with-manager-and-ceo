use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use async_trait::async_trait;
use codex_protocol::parse_command::ParsedCommand;
use codex_protocol::protocol::DelegateWorkerStatusKind;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ExecCommandBeginEvent;
use codex_protocol::protocol::McpToolCallBeginEvent;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::PatchApplyBeginEvent;
use codex_protocol::protocol::StreamErrorEvent;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::protocol::TaskStartedEvent;
use codex_protocol::user_input::UserInput;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::AuthManager;
use crate::codex::Session;
use crate::codex::TurnContext;
use crate::codex_delegate::run_codex_conversation_interactive;
use crate::function_tool::FunctionCallError;
use crate::manager_workers::ManagedWorker;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

use crate::error::CodexErr;

pub struct DelegateWorkerHandler;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
enum WorkerAction {
    #[default]
    Start,
    Message,
    Close,
}

impl fmt::Display for WorkerAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            WorkerAction::Start => "start",
            WorkerAction::Message => "message",
            WorkerAction::Close => "close",
        };
        f.write_str(label)
    }
}

#[derive(Debug, Deserialize)]
struct DelegateWorkerArgs {
    #[serde(default)]
    objective: Option<String>,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    worker_id: Option<String>,
    #[serde(default)]
    action: WorkerAction,
}

struct WorkerRunSummary {
    worker_id: String,
    objective: String,
    worker_model: String,
    action: WorkerAction,
    last_message: Option<String>,
    aborted_reason: Option<String>,
    completed: bool,
    diff_count: usize,
    errors: Vec<String>,
    worker_active: bool,
}

impl WorkerRunSummary {
    fn new(
        worker_id: String,
        objective: String,
        worker_model: String,
        action: WorkerAction,
    ) -> Self {
        Self {
            worker_id,
            objective,
            worker_model,
            action,
            last_message: None,
            aborted_reason: None,
            completed: false,
            diff_count: 0,
            errors: Vec::new(),
            worker_active: true,
        }
    }

    fn status_line(&self) -> String {
        if let Some(reason) = &self.aborted_reason {
            format!("failed ({reason})")
        } else if self.completed {
            "completed".to_string()
        } else {
            "incomplete".to_string()
        }
    }

    fn success(&self) -> bool {
        self.aborted_reason.is_none() && self.completed
    }
}

const WORKER_STATUS_MIN_INTERVAL: Duration = Duration::from_millis(750);

struct WorkerStatusEmitter {
    session: Arc<Session>,
    turn: Arc<TurnContext>,
    worker_id: String,
    worker_model: String,
    last_status: Option<DelegateWorkerStatusKind>,
    last_message: Option<String>,
    last_emit_at: Option<Instant>,
}

impl WorkerStatusEmitter {
    fn new(
        session: Arc<Session>,
        turn: Arc<TurnContext>,
        worker_id: String,
        worker_model: String,
    ) -> Self {
        Self {
            session,
            turn,
            worker_id,
            worker_model,
            last_status: None,
            last_message: None,
            last_emit_at: None,
        }
    }

    async fn emit(&mut self, status: DelegateWorkerStatusKind, message: impl Into<String>) {
        let mut message = message.into();
        if message.trim().is_empty() {
            message = default_status_message(status).to_string();
        }

        let trimmed = message.trim().to_string();
        let should_emit = self.should_emit(&trimmed, status);
        if should_emit {
            self.session
                .notify_worker_status(
                    self.turn.as_ref(),
                    self.worker_id.clone(),
                    self.worker_model.clone(),
                    status,
                    trimmed.clone(),
                )
                .await;
            self.last_status = Some(status);
            self.last_message = Some(trimmed);
            self.last_emit_at = Some(Instant::now());
        }
    }

    fn should_emit(&self, message: &str, status: DelegateWorkerStatusKind) -> bool {
        if self.last_status.is_none_or(|prev| prev != status)
            || (self.last_message.as_deref() != Some(message))
        {
            return true;
        }

        match self.last_emit_at {
            None => true,
            Some(last) => last.elapsed() >= WORKER_STATUS_MIN_INTERVAL,
        }
    }
}

fn parse_args(payload: &ToolPayload) -> Result<DelegateWorkerArgs, FunctionCallError> {
    if let ToolPayload::Function { arguments } = payload {
        let mut parsed: DelegateWorkerArgs = serde_json::from_str(arguments).map_err(|err| {
            FunctionCallError::RespondToModel(
                format!("delegate_worker arguments must be valid JSON: {err}").into(),
            )
        })?;

        parsed.objective = parsed
            .objective
            .take()
            .map(|obj| obj.trim().to_string())
            .filter(|obj| !obj.is_empty());
        parsed.context = parsed
            .context
            .take()
            .map(|ctx| ctx.trim().to_string())
            .filter(|ctx| !ctx.is_empty());
        parsed.worker_id = parsed
            .worker_id
            .take()
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty());

        match parsed.action {
            WorkerAction::Start | WorkerAction::Message => {
                if parsed.objective.is_none() {
                    return Err(FunctionCallError::RespondToModel(
                        "delegate_worker requires a non-empty objective when starting or messaging a worker"
                            .into(),
                    ));
                }
            }
            WorkerAction::Close => {}
        }

        if parsed.action != WorkerAction::Start && parsed.worker_id.is_none() {
            return Err(FunctionCallError::RespondToModel(
                "delegate_worker requires worker_id when messaging or closing a worker".into(),
            ));
        }

        if parsed.action == WorkerAction::Start && parsed.worker_id.is_some() {
            return Err(FunctionCallError::RespondToModel(
                "worker_id cannot be supplied when starting a new worker".into(),
            ));
        }

        Ok(parsed)
    } else {
        Err(FunctionCallError::Fatal(
            "delegate_worker handler received unsupported payload".to_string(),
        ))
    }
}

fn build_worker_input(objective: &str, context: Option<&str>) -> Vec<UserInput> {
    let mut text = objective.trim().to_string();
    if let Some(ctx) = context.map(str::trim).filter(|ctx| !ctx.is_empty()) {
        text.push_str("\n\nContext:\n");
        text.push_str(ctx);
    }
    vec![UserInput::Text { text }]
}

fn format_summary(summary: &WorkerRunSummary) -> String {
    let mut lines = Vec::new();
    lines.push(format!("Worker ID: {}", summary.worker_id));
    lines.push(format!("Action: {}", summary.action));
    lines.push(format!(
        "Worker state: {}",
        if summary.worker_active {
            "active"
        } else {
            "closed"
        }
    ));
    lines.push(format!("Worker model: {}", summary.worker_model));
    if !summary.objective.trim().is_empty() {
        lines.push(format!("Objective: {}", summary.objective.trim()));
    }
    lines.push(format!("Status: {}", summary.status_line()));
    if summary.diff_count > 0 {
        lines.push(format!("Applied diffs: {}", summary.diff_count));
    }
    if let Some(msg) = summary
        .last_message
        .as_ref()
        .map(|m| m.trim())
        .filter(|m| !m.is_empty())
    {
        lines.push("Worker response:".to_string());
        lines.push(msg.to_string());
    } else {
        lines.push("Worker did not produce a final response.".to_string());
    }
    if !summary.errors.is_empty() {
        lines.push("Worker warnings:".to_string());
        for err in &summary.errors {
            lines.push(format!("- {err}"));
        }
    }
    lines.join("\n")
}

#[async_trait]
impl ToolHandler for DelegateWorkerHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            payload,
            ..
        } = invocation;

        let args = parse_args(&payload)?;

        let output = match args.action {
            WorkerAction::Start => {
                let auth_manager = turn.client.get_auth_manager().ok_or_else(|| {
                    FunctionCallError::Fatal("missing auth manager for worker".to_string())
                })?;
                start_worker(Arc::clone(&session), Arc::clone(&turn), &args, auth_manager).await?
            }
            WorkerAction::Message => {
                resume_worker(Arc::clone(&session), Arc::clone(&turn), &args).await?
            }
            WorkerAction::Close => close_worker(Arc::clone(&session), &args).await?,
        };

        Ok(output)
    }
}

async fn start_worker(
    session: Arc<Session>,
    turn: Arc<TurnContext>,
    args: &DelegateWorkerArgs,
    auth_manager: Arc<AuthManager>,
) -> Result<ToolOutput, FunctionCallError> {
    let mut worker_config = (*turn.client.config()).clone();
    worker_config.manager.enabled = false;

    let worker_model = args
        .model
        .clone()
        .or_else(|| worker_config.manager.worker_model.clone())
        .unwrap_or_else(|| worker_config.model.clone());
    worker_config.model = worker_model.clone();
    let worker_effort = worker_config
        .manager
        .worker_reasoning_effort
        .or(worker_config.manager.manager_reasoning_effort)
        .or(worker_config.model_reasoning_effort);
    worker_config.model_reasoning_effort = worker_effort;
    worker_config.manager.manager_reasoning_effort = None;
    worker_config.manager.worker_reasoning_effort = None;

    let cancel = CancellationToken::new();
    let worker_codex = run_codex_conversation_interactive(
        worker_config,
        auth_manager,
        Arc::clone(&session),
        Arc::clone(&turn),
        cancel.clone(),
        None,
        SubAgentSource::Other("manager_worker".to_string()),
    )
    .await
    .map_err(|err| FunctionCallError::Fatal(format!("failed to start worker: {err}")))?;

    let worker_id = session.allocate_worker_id().await;
    let worker = Arc::new(ManagedWorker::new(
        worker_id.clone(),
        worker_model.clone(),
        worker_codex,
        cancel,
    ));

    let objective = args.objective.clone().unwrap_or_default();
    let objective_preview = truncate_preview(&objective, 80);
    let input = build_worker_input(&objective, args.context.as_deref());
    let mut summary = WorkerRunSummary::new(
        worker_id.clone(),
        objective,
        worker_model,
        WorkerAction::Start,
    );
    let mut status_emitter = WorkerStatusEmitter::new(
        Arc::clone(&session),
        Arc::clone(&turn),
        worker_id.clone(),
        worker.model.clone(),
    );
    status_emitter
        .emit(
            DelegateWorkerStatusKind::Starting,
            format!("Starting worker for {objective_preview}"),
        )
        .await;
    let worker_active = run_worker_turn(
        Arc::clone(&worker),
        Arc::clone(&session),
        Arc::clone(&turn),
        input,
        &mut summary,
        &mut status_emitter,
    )
    .await?;
    summary.worker_active = worker_active;

    if worker_active {
        session.insert_worker(worker).await;
    } else {
        worker.shutdown().await;
    }

    Ok(ToolOutput::Function {
        content: format_summary(&summary),
        content_items: None,
        success: Some(summary.success()),
        history_content: None,
    })
}

async fn resume_worker(
    session: Arc<Session>,
    turn: Arc<TurnContext>,
    args: &DelegateWorkerArgs,
) -> Result<ToolOutput, FunctionCallError> {
    let worker_id = worker_id_or_bug(args)?;
    let worker = session
        .get_worker(&worker_id)
        .await
        .ok_or_else(|| worker_not_found(&worker_id))?;

    if worker.is_closed() {
        session.remove_worker(&worker_id).await;
        return Err(worker_not_found(&worker_id));
    }

    let objective = args.objective.clone().unwrap_or_default();
    let objective_preview = truncate_preview(&objective, 80);
    let input = build_worker_input(&objective, args.context.as_deref());
    let mut summary = WorkerRunSummary::new(
        worker_id.clone(),
        objective,
        worker.model.clone(),
        WorkerAction::Message,
    );
    let mut status_emitter = WorkerStatusEmitter::new(
        Arc::clone(&session),
        Arc::clone(&turn),
        worker_id.clone(),
        worker.model.clone(),
    );
    status_emitter
        .emit(
            DelegateWorkerStatusKind::Running,
            format!("Resuming worker for {objective_preview}"),
        )
        .await;
    let worker_active = run_worker_turn(
        Arc::clone(&worker),
        Arc::clone(&session),
        Arc::clone(&turn),
        input,
        &mut summary,
        &mut status_emitter,
    )
    .await?;
    summary.worker_active = worker_active;
    if !worker_active {
        session.remove_worker(&worker_id).await;
        worker.shutdown().await;
    }

    Ok(ToolOutput::Function {
        content: format_summary(&summary),
        content_items: None,
        success: Some(summary.success()),
        history_content: None,
    })
}

async fn close_worker(
    session: Arc<Session>,
    args: &DelegateWorkerArgs,
) -> Result<ToolOutput, FunctionCallError> {
    let worker_id = worker_id_or_bug(args)?;
    let worker = session
        .remove_worker(&worker_id)
        .await
        .ok_or_else(|| worker_not_found(&worker_id))?;
    worker.shutdown().await;

    let mut summary = WorkerRunSummary::new(
        worker_id,
        String::new(),
        worker.model.clone(),
        WorkerAction::Close,
    );
    summary.worker_active = false;
    summary.completed = true;

    Ok(ToolOutput::Function {
        content: format_summary(&summary),
        content_items: None,
        success: Some(true),
        history_content: None,
    })
}

async fn run_worker_turn(
    worker: Arc<ManagedWorker>,
    session: Arc<Session>,
    turn: Arc<TurnContext>,
    input: Vec<UserInput>,
    summary: &mut WorkerRunSummary,
    status_emitter: &mut WorkerStatusEmitter,
) -> Result<bool, FunctionCallError> {
    let _permit = worker
        .acquire()
        .await
        .map_err(|_| worker_not_found(&worker.id))?;
    worker
        .codex
        .submit(Op::UserInput { items: input })
        .await
        .map_err(|err| FunctionCallError::Fatal(format!("failed to submit worker input: {err}")))?;
    status_emitter
        .emit(
            DelegateWorkerStatusKind::Running,
            "Sent objective to worker",
        )
        .await;

    loop {
        match worker.codex.next_event().await {
            Ok(event) => match event.msg {
                EventMsg::TaskStarted(TaskStartedEvent { .. }) => {
                    status_emitter
                        .emit(DelegateWorkerStatusKind::Running, "Worker is thinking")
                        .await;
                }
                EventMsg::TaskComplete(task) => {
                    summary.last_message = task.last_agent_message;
                    summary.completed = true;
                    status_emitter
                        .emit(
                            DelegateWorkerStatusKind::Completed,
                            "Worker completed the objective",
                        )
                        .await;
                    return Ok(true);
                }
                EventMsg::TurnAborted(aborted) => {
                    summary.aborted_reason = Some(format!("{:?}", aborted.reason));
                    status_emitter
                        .emit(
                            DelegateWorkerStatusKind::Failed,
                            format!("Worker aborted: {:?}", aborted.reason),
                        )
                        .await;
                    return Ok(false);
                }
                EventMsg::Error(err) => {
                    summary.errors.push(err.message.clone());
                    status_emitter
                        .emit(
                            DelegateWorkerStatusKind::Warning,
                            format!("Worker warning: {}", err.message),
                        )
                        .await;
                }
                EventMsg::TurnDiff(diff) => {
                    summary.diff_count += 1;
                    session
                        .send_event(turn.as_ref(), EventMsg::TurnDiff(diff))
                        .await;
                    status_emitter
                        .emit(
                            DelegateWorkerStatusKind::DiffApplied,
                            format!("Applied diff #{:02}", summary.diff_count),
                        )
                        .await;
                }
                EventMsg::ExecCommandBegin(ev) => {
                    status_emitter
                        .emit(
                            DelegateWorkerStatusKind::RunningCommand,
                            format!("Running {}", describe_exec_command(&ev)),
                        )
                        .await;
                }
                EventMsg::StreamError(StreamErrorEvent { message }) => {
                    status_emitter
                        .emit(DelegateWorkerStatusKind::Warning, message)
                        .await;
                }
                EventMsg::McpToolCallBegin(ev) => {
                    status_emitter
                        .emit(
                            DelegateWorkerStatusKind::RunningTool,
                            describe_tool_invocation(&ev),
                        )
                        .await;
                }
                EventMsg::PatchApplyBegin(ev) => {
                    status_emitter
                        .emit(
                            DelegateWorkerStatusKind::ApplyingPatch,
                            describe_patch_targets(&ev),
                        )
                        .await;
                }
                _ => {}
            },
            Err(CodexErr::InternalAgentDied) => {
                summary.aborted_reason = Some("worker exited unexpectedly".to_string());
                status_emitter
                    .emit(
                        DelegateWorkerStatusKind::Failed,
                        "Worker exited unexpectedly",
                    )
                    .await;
                return Ok(false);
            }
            Err(err) => {
                status_emitter
                    .emit(
                        DelegateWorkerStatusKind::Failed,
                        format!("Worker failed: {err}"),
                    )
                    .await;
                return Err(FunctionCallError::Fatal(format!(
                    "worker failed before completion: {err}"
                )));
            }
        }
    }
}

fn worker_not_found(worker_id: &str) -> FunctionCallError {
    FunctionCallError::RespondToModel(
        format!("worker `{worker_id}` is not active. Start a new worker instead.").into(),
    )
}

fn worker_id_or_bug(args: &DelegateWorkerArgs) -> Result<String, FunctionCallError> {
    args.worker_id.clone().ok_or_else(|| {
        FunctionCallError::Fatal("delegate_worker missing worker_id after validation".to_string())
    })
}

fn default_status_message(status: DelegateWorkerStatusKind) -> &'static str {
    match status {
        DelegateWorkerStatusKind::Starting => "Starting worker",
        DelegateWorkerStatusKind::Running => "Worker is thinking",
        DelegateWorkerStatusKind::RunningCommand => "Running command",
        DelegateWorkerStatusKind::RunningTool => "Calling tool",
        DelegateWorkerStatusKind::ApplyingPatch => "Applying patch",
        DelegateWorkerStatusKind::DiffApplied => "Recording edits",
        DelegateWorkerStatusKind::Warning => "Worker reported a warning",
        DelegateWorkerStatusKind::Completed => "Worker completed",
        DelegateWorkerStatusKind::Failed => "Worker failed",
    }
}

fn describe_exec_command(event: &ExecCommandBeginEvent) -> String {
    if !event.parsed_cmd.is_empty() {
        let joined: Vec<String> = event
            .parsed_cmd
            .iter()
            .map(parsed_command_label)
            .map(ToString::to_string)
            .collect();
        return truncate_preview(&joined.join(" && "), 64);
    }
    truncate_preview(&event.command.join(" "), 64)
}

fn parsed_command_label(cmd: &ParsedCommand) -> &str {
    match cmd {
        ParsedCommand::Read { cmd, .. }
        | ParsedCommand::ListFiles { cmd, .. }
        | ParsedCommand::Search { cmd, .. }
        | ParsedCommand::Unknown { cmd } => cmd,
    }
}

fn describe_tool_invocation(event: &McpToolCallBeginEvent) -> String {
    let tool = event.invocation.tool.as_str();
    format!("Calling tool {tool}")
}

fn describe_patch_targets(event: &PatchApplyBeginEvent) -> String {
    if event.changes.is_empty() {
        return "Applying patch".to_string();
    }
    let mut paths: Vec<_> = event
        .changes
        .keys()
        .map(|p| p.display().to_string())
        .collect();
    paths.sort();
    let first = paths.first().cloned().unwrap_or_default();
    if paths.len() == 1 {
        truncate_preview(&format!("Applying patch to {first}"), 64)
    } else {
        truncate_preview(
            &format!("Applying patch to {first} (+{} more)", paths.len() - 1),
            64,
        )
    }
}

fn truncate_preview(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    if max_chars <= 3 {
        return input.chars().take(max_chars).collect();
    }
    let prefix: String = input.chars().take(max_chars - 3).collect();
    format!("{prefix}...")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn parse(input: &str) -> Result<DelegateWorkerArgs, FunctionCallError> {
        parse_args(&ToolPayload::Function {
            arguments: input.to_string(),
        })
    }

    #[test]
    fn start_requires_objective() {
        let err = parse("{}").unwrap_err();
        match err {
            FunctionCallError::RespondToModel(msg) => {
                assert!(msg.content.contains("objective"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn message_requires_worker_id() {
        let err = parse(r#"{"action":"message","objective":"hi"}"#).unwrap_err();
        match err {
            FunctionCallError::RespondToModel(msg) => {
                assert!(msg.content.contains("worker_id"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn close_without_objective_is_allowed() {
        let args = parse(r#"{"action":"close","worker_id":"worker-7"}"#).unwrap();
        assert_eq!(args.action, WorkerAction::Close);
        assert_eq!(args.worker_id.as_deref(), Some("worker-7"));
        assert!(args.objective.is_none());
    }
}
