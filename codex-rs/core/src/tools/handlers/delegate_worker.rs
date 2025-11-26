use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use async_trait::async_trait;
use codex_protocol::parse_command::ParsedCommand;
use codex_protocol::protocol::DelegateAgentKind;
use codex_protocol::protocol::DelegateWorkerStatusEvent;
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
use tokio::sync::Mutex;
use tokio::sync::oneshot;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::AuthManager;
use crate::codex::Codex;
use crate::codex::Session;
use crate::codex::TurnContext;
use crate::codex_delegate::run_codex_conversation_interactive;
use crate::codex_delegate::run_codex_conversation_one_shot;
use crate::features::Feature;
use crate::function_tool::FunctionCallError;
use crate::manager_workers::ManagedWorker;
use crate::manager_workers::WorkerAction;
use crate::manager_workers::WorkerRunHandle;
use crate::manager_workers::WorkerRunSummary;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

use crate::error::CodexErr;

#[derive(Clone, Copy, Debug)]
pub struct DelegateAgentHandler {
    kind: DelegateAgentKind,
}

impl DelegateAgentHandler {
    pub const fn worker() -> Self {
        Self {
            kind: DelegateAgentKind::Worker,
        }
    }

    pub const fn manager() -> Self {
        Self {
            kind: DelegateAgentKind::Manager,
        }
    }
}

#[derive(Debug, Deserialize)]
struct DelegateAgentArgs {
    #[serde(default)]
    objective: Option<String>,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    persona: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    web_search: Option<bool>,
    #[serde(default, alias = "manager_model")]
    model: Option<String>,
    #[serde(default, alias = "manager_id")]
    worker_id: Option<String>,
    #[serde(default)]
    action: WorkerAction,
    #[serde(default)]
    blocking: Option<bool>,
}

const WORKER_STATUS_MIN_INTERVAL: Duration = Duration::from_millis(750);

const AUTO_DISPLAY_NAME_SYSTEM_PROMPT: &str = r#"You label Codex manager/worker delegations. Start with the agent's role (e.g., "Frontend Dev", "Backend Dev", "ML Eng") and add up to two more words that capture what success looks like. Keep it to 3 words total, under 48 characters, and avoid IDs, emojis, or trailing punctuation. Highlight how this task differs from sibling work rather than restating the full objective. Return only the label—no commentary."#;
const AUTO_DISPLAY_NAME_PROMPT_SECTION_LIMIT: usize = 480;
const AUTO_DISPLAY_NAME_EXISTING_LIMIT: usize = 6;
const AUTO_DISPLAY_NAME_MAX_CHARS: usize = 48;
const AUTO_DISPLAY_NAME_TIMEOUT: Duration = Duration::from_secs(6);

#[derive(Deserialize)]
struct GeneratedDisplayNamePayload {
    display_name: String,
}

#[allow(clippy::too_many_arguments)]
async fn generate_delegate_display_name(
    session: Arc<Session>,
    turn: Arc<TurnContext>,
    auth_manager: Arc<AuthManager>,
    agent_model: &str,
    kind: DelegateAgentKind,
    objective: &str,
    context: Option<&str>,
    persona: Option<&str>,
) -> Option<String> {
    let trimmed_objective = objective.trim();
    if trimmed_objective.is_empty() {
        return None;
    }
    let active_names = session.active_worker_display_names().await;
    let prompt =
        build_display_name_prompt(kind, trimmed_objective, context, persona, &active_names)?;

    let mut config = (*turn.client.config()).clone();
    config.model = agent_model.to_string();
    config.ceo.enabled = false;
    config.manager.enabled = false;
    config.base_instructions = Some(AUTO_DISPLAY_NAME_SYSTEM_PROMPT.to_string());
    config.developer_instructions = None;
    config.user_instructions = None;
    config.compact_prompt = None;
    config.project_doc_max_bytes = 0;
    config.project_doc_fallback_filenames.clear();

    let cancel = CancellationToken::new();
    let codex = match run_codex_conversation_one_shot(
        config,
        auth_manager,
        vec![UserInput::Text { text: prompt }],
        Arc::clone(&session),
        Arc::clone(&turn),
        cancel,
        None,
        SubAgentSource::Other("delegate_display_name".to_string()),
    )
    .await
    {
        Ok(codex) => codex,
        Err(err) => {
            warn!("failed to start delegate display name generator: {err}");
            return None;
        }
    };

    collect_generated_display_name(codex).await
}

async fn collect_generated_display_name(codex: Codex) -> Option<String> {
    loop {
        match timeout(AUTO_DISPLAY_NAME_TIMEOUT, codex.next_event()).await {
            Ok(Ok(event)) => match event.msg {
                EventMsg::TaskComplete(task) => {
                    if let Some(message) = task.last_agent_message {
                        if let Some(name) = sanitize_generated_display_name(&message) {
                            return Some(name);
                        }
                        warn!("display name generator returned an empty response");
                        return None;
                    }
                    warn!("display name generator completed without a response");
                    return None;
                }
                EventMsg::TurnAborted(aborted) => {
                    warn!(
                        reason = ?aborted.reason,
                        "display name generator aborted unexpectedly"
                    );
                    return None;
                }
                _ => continue,
            },
            Ok(Err(err)) => {
                warn!("display name generator event stream failed: {err}");
                return None;
            }
            Err(_) => {
                warn!("display name generator timed out waiting for completion");
                return None;
            }
        }
    }
}

fn build_display_name_prompt(
    kind: DelegateAgentKind,
    objective: &str,
    context: Option<&str>,
    persona: Option<&str>,
    existing_names: &[String],
) -> Option<String> {
    if objective.trim().is_empty() {
        return None;
    }
    let mut sections = Vec::new();
    let role = match kind {
        DelegateAgentKind::Worker => "worker",
        DelegateAgentKind::Manager => "manager",
    };
    sections.push(format!(
        "Name a {role} task so the UI hierarchy stays scannable. The label must explain what success looks like in a handful of words."
    ));

    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for name in existing_names {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_lowercase();
        if seen.insert(lower) {
            deduped.push(trimmed.to_string());
        }
        if deduped.len() == AUTO_DISPLAY_NAME_EXISTING_LIMIT {
            break;
        }
    }
    if !deduped.is_empty() {
        sections.push(format!(
            "Avoid collisions with existing labels: {}",
            deduped.join(", ")
        ));
    }

    sections.push(format!(
        "Objective summary:\n{}",
        clip_prompt_section(objective)
    ));

    if let Some(ctx) = context.and_then(|ctx| {
        let trimmed = ctx.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    }) {
        sections.push(format!("Relevant context:\n{}", clip_prompt_section(ctx)));
    }

    if let Some(persona) = persona.and_then(|persona| {
        let trimmed = persona.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    }) {
        sections.push(format!(
            "Persona/voice constraints:\n{}",
            clip_prompt_section(persona)
        ));
    }

    sections.push("Respond with a single line containing only the label.".to_string());
    Some(sections.join("\n\n"))
}

fn clip_prompt_section(input: &str) -> String {
    let trimmed = input.trim();
    truncate_preview(trimmed, AUTO_DISPLAY_NAME_PROMPT_SECTION_LIMIT)
}

fn sanitize_generated_display_name(raw: &str) -> Option<String> {
    if raw.trim().is_empty() {
        return None;
    }
    if let Some(from_json) = parse_json_display_name(raw) {
        return cleanse_display_name(&from_json);
    }
    cleanse_display_name(raw)
}

fn parse_json_display_name(raw: &str) -> Option<String> {
    if let Ok(payload) = serde_json::from_str::<GeneratedDisplayNamePayload>(raw) {
        return Some(payload.display_name);
    }
    if let (Some(start), Some(end)) = (raw.find('{'), raw.rfind('}'))
        && start < end
        && let Some(slice) = raw.get(start..=end)
        && let Ok(payload) = serde_json::from_str::<GeneratedDisplayNamePayload>(slice)
    {
        return Some(payload.display_name);
    }
    None
}

fn cleanse_display_name(text: &str) -> Option<String> {
    let mut line = text.lines().next()?.trim().to_string();
    if line.is_empty() {
        return None;
    }
    line = line
        .trim_start_matches(['-', '*', '"', '`', '•', '#'])
        .trim()
        .to_string();
    if line.is_empty() {
        return None;
    }
    if let Some((prefix, rest)) = line.split_once(':')
        && prefix.split_whitespace().count() <= 3
    {
        line = rest.trim().to_string();
    }
    line = line
        .trim_matches(|ch: char| matches!(ch, '"' | '\'' | '`'))
        .trim_matches(|ch: char| matches!(ch, '.' | ',' | ';' | ':' | '-'))
        .to_string();
    let condensed = line.split_whitespace().collect::<Vec<_>>().join(" ");
    let condensed = condensed
        .trim_matches(|ch: char| matches!(ch, '.' | ',' | ';' | ':'))
        .trim()
        .to_string();
    if condensed.is_empty() {
        return None;
    }
    Some(clamp_display_name(&condensed))
}

fn clamp_display_name(value: &str) -> String {
    value
        .chars()
        .enumerate()
        .take_while(|(idx, _)| *idx < AUTO_DISPLAY_NAME_MAX_CHARS)
        .map(|(_, ch)| ch)
        .collect()
}

fn append_persona_instructions(target: &mut Option<String>, persona: Option<&str>) {
    let Some(persona) = persona.map(str::trim).filter(|p| !p.is_empty()) else {
        return;
    };
    let persona_block = format!("Persona instructions:\n{persona}");
    let updated = match target.take() {
        Some(existing) if !existing.trim().is_empty() => {
            format!("{existing}\n\n{persona_block}")
        }
        _ => persona_block,
    };
    *target = Some(updated);
}

fn normalize_display_name(input: Option<String>) -> Option<String> {
    input
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

struct WorkerStatusEmitter {
    session: Arc<Session>,
    turn: Arc<TurnContext>,
    worker_id: String,
    worker_model: String,
    agent_kind: DelegateAgentKind,
    display_name: Option<String>,
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
        agent_kind: DelegateAgentKind,
        display_name: Option<String>,
    ) -> Self {
        Self {
            session,
            turn,
            worker_id,
            worker_model,
            agent_kind,
            display_name,
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
            let event = DelegateWorkerStatusEvent {
                worker_id: self.worker_id.clone(),
                worker_model: self.worker_model.clone(),
                agent_kind: self.agent_kind,
                parent_worker_id: None,
                status,
                message: trimmed.clone(),
                display_name: self.display_name.clone(),
            };
            self.session
                .notify_worker_status(self.turn.as_ref(), event)
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

enum WorkerCompletionHook {
    Start {
        session: Arc<Session>,
        worker: Arc<ManagedWorker>,
    },
    Resume {
        session: Arc<Session>,
        worker: Arc<ManagedWorker>,
        worker_id: String,
    },
}

impl WorkerCompletionHook {
    async fn on_completion(&self, worker_active: bool) {
        match self {
            WorkerCompletionHook::Start { session, worker } => {
                if worker_active {
                    session.insert_worker(Arc::clone(worker)).await;
                } else {
                    worker.shutdown().await;
                }
            }
            WorkerCompletionHook::Resume {
                session,
                worker,
                worker_id,
            } => {
                if !worker_active {
                    session.remove_worker(worker_id).await;
                    worker.shutdown().await;
                }
            }
        }
    }
}

fn parse_args(
    payload: &ToolPayload,
    kind: DelegateAgentKind,
) -> Result<DelegateAgentArgs, FunctionCallError> {
    if let ToolPayload::Function { arguments } = payload {
        let mut parsed: DelegateAgentArgs = serde_json::from_str(arguments).map_err(|err| {
            FunctionCallError::RespondToModel(
                format!(
                    "{} arguments must be valid JSON: {err}",
                    tool_name_for_kind(kind)
                )
                .into(),
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
                        format!(
                            "{} requires a non-empty objective when starting or messaging a {}",
                            tool_name_for_kind(kind),
                            noun_for_kind(kind)
                        )
                        .into(),
                    ));
                }
            }
            WorkerAction::Close | WorkerAction::Await | WorkerAction::Status => {}
        }

        if matches!(
            parsed.action,
            WorkerAction::Message
                | WorkerAction::Close
                | WorkerAction::Await
                | WorkerAction::Status
        ) && parsed.worker_id.is_none()
        {
            return Err(FunctionCallError::RespondToModel(
                format!(
                    "{} requires {} when messaging or closing a {}",
                    tool_name_for_kind(kind),
                    id_key_for_kind(kind),
                    noun_for_kind(kind)
                )
                .into(),
            ));
        }

        if kind == DelegateAgentKind::Worker
            && !matches!(parsed.action, WorkerAction::Start)
            && parsed.web_search.is_some()
        {
            return Err(FunctionCallError::RespondToModel(
                format!(
                    "{} web_search override is only supported when starting a new {}",
                    tool_name_for_kind(kind),
                    noun_for_kind(kind)
                )
                .into(),
            ));
        }

        if parsed.action == WorkerAction::Start && parsed.worker_id.is_some() {
            return Err(FunctionCallError::RespondToModel(
                format!(
                    "{} cannot be supplied when starting a new {}",
                    id_key_for_kind(kind),
                    noun_for_kind(kind)
                )
                .into(),
            ));
        }

        Ok(parsed)
    } else {
        Err(FunctionCallError::Fatal(format!(
            "{} handler received unsupported payload",
            tool_name_for_kind(kind)
        )))
    }
}

fn tool_name_for_kind(kind: DelegateAgentKind) -> &'static str {
    match kind {
        DelegateAgentKind::Worker => "delegate_worker",
        DelegateAgentKind::Manager => "delegate_manager",
    }
}

fn noun_for_kind(kind: DelegateAgentKind) -> &'static str {
    match kind {
        DelegateAgentKind::Worker => "worker",
        DelegateAgentKind::Manager => "manager",
    }
}

fn id_key_for_kind(kind: DelegateAgentKind) -> &'static str {
    match kind {
        DelegateAgentKind::Worker => "worker_id",
        DelegateAgentKind::Manager => "manager_id",
    }
}

fn title_for_kind(kind: DelegateAgentKind) -> &'static str {
    match kind {
        DelegateAgentKind::Worker => "Worker",
        DelegateAgentKind::Manager => "Manager",
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
    let label = match summary.agent_kind {
        DelegateAgentKind::Worker => "Worker",
        DelegateAgentKind::Manager => "Manager",
    };
    lines.push(format!("{label} ID: {}", summary.worker_id));
    lines.push(format!("Action: {}", summary.action));
    lines.push(format!(
        "{label} state: {}",
        if summary.worker_active {
            "active"
        } else {
            "closed"
        }
    ));
    lines.push(format!("{label} model: {}", summary.worker_model));
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
        lines.push(format!("{label} response:"));
        lines.push(msg.to_string());
    } else {
        lines.push(format!("{label} did not produce a final response."));
    }
    if !summary.errors.is_empty() {
        lines.push(format!("{label} warnings:"));
        for err in &summary.errors {
            lines.push(format!("- {err}"));
        }
    }
    lines.join("\n")
}

fn summary_output(summary: WorkerRunSummary) -> ToolOutput {
    ToolOutput::Function {
        content: format_summary(&summary),
        content_items: None,
        success: Some(summary.success()),
        history_content: None,
    }
}

fn spawn_worker_run(
    worker: Arc<ManagedWorker>,
    session: Arc<Session>,
    turn: Arc<TurnContext>,
    input: Vec<UserInput>,
    summary: Arc<Mutex<WorkerRunSummary>>,
    status_emitter: WorkerStatusEmitter,
    completion_hook: WorkerCompletionHook,
) -> WorkerRunHandle {
    let (tx, rx) = oneshot::channel();
    let summary_for_task = Arc::clone(&summary);
    let summary_for_completion = Arc::clone(&summary);
    let summary_for_result = Arc::clone(&summary);
    let join_handle = tokio::spawn(async move {
        let result = run_worker_turn(
            Arc::clone(&worker),
            Arc::clone(&session),
            Arc::clone(&turn),
            input,
            summary_for_task,
            status_emitter,
        )
        .await;
        let final_result = match result {
            Ok(worker_active) => {
                {
                    let mut guard = summary_for_completion.lock().await;
                    guard.worker_active = worker_active;
                }
                completion_hook.on_completion(worker_active).await;
                let guard = summary_for_result.lock().await;
                Ok(guard.clone())
            }
            Err(err) => Err(err),
        };
        let _ = tx.send(final_result);
    });

    WorkerRunHandle::new(summary, rx, join_handle)
}

#[async_trait]
impl ToolHandler for DelegateAgentHandler {
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

        let args = parse_args(&payload, self.kind)?;

        let blocking = args.blocking.unwrap_or(true);

        let output = match args.action {
            WorkerAction::Start => {
                let auth_manager = turn.client.get_auth_manager().ok_or_else(|| {
                    FunctionCallError::Fatal("missing auth manager for worker".to_string())
                })?;
                start_agent(
                    self.kind,
                    Arc::clone(&session),
                    Arc::clone(&turn),
                    &args,
                    auth_manager,
                    blocking,
                )
                .await?
            }
            WorkerAction::Message => {
                resume_agent(
                    self.kind,
                    Arc::clone(&session),
                    Arc::clone(&turn),
                    &args,
                    blocking,
                )
                .await?
            }
            WorkerAction::Close => close_agent(self.kind, Arc::clone(&session), &args).await?,
            WorkerAction::Await => await_agent(self.kind, Arc::clone(&session), &args).await?,
            WorkerAction::Status => agent_status(self.kind, Arc::clone(&session), &args).await?,
        };

        Ok(output)
    }
}

async fn start_agent(
    kind: DelegateAgentKind,
    session: Arc<Session>,
    turn: Arc<TurnContext>,
    args: &DelegateAgentArgs,
    auth_manager: Arc<AuthManager>,
    blocking: bool,
) -> Result<ToolOutput, FunctionCallError> {
    let mut agent_config = (*turn.client.config()).clone();
    agent_config.ceo.enabled = false;
    let web_search_override = if kind == DelegateAgentKind::Worker {
        args.web_search
    } else {
        None
    };

    let agent_model = match kind {
        DelegateAgentKind::Worker => {
            agent_config.manager.enabled = false;
            let model = args
                .model
                .clone()
                .or_else(|| agent_config.manager.worker_model.clone())
                .unwrap_or_else(|| agent_config.model.clone());
            agent_config.model = model.clone();
            let effort = agent_config
                .manager
                .worker_reasoning_effort
                .or(agent_config.manager.manager_reasoning_effort)
                .or(agent_config.model_reasoning_effort);
            agent_config.model_reasoning_effort = effort;
            agent_config.manager.manager_reasoning_effort = None;
            agent_config.manager.worker_reasoning_effort = None;
            model
        }
        DelegateAgentKind::Manager => {
            agent_config.manager.enabled = true;
            let model = args
                .model
                .clone()
                .or_else(|| agent_config.manager.manager_model.clone())
                .unwrap_or_else(|| agent_config.model.clone());
            agent_config.model = model.clone();
            agent_config.manager.manager_model = Some(model.clone());
            let effort = agent_config
                .manager
                .manager_reasoning_effort
                .or(agent_config.model_reasoning_effort);
            agent_config.model_reasoning_effort = effort;
            model
        }
    };

    if let Some(web_search_enabled) = web_search_override {
        if web_search_enabled {
            agent_config.features.enable(Feature::WebSearchRequest);
        } else {
            agent_config.features.disable(Feature::WebSearchRequest);
        }
        agent_config.tools_web_search_request = web_search_enabled;
    }

    append_persona_instructions(
        &mut agent_config.developer_instructions,
        args.persona.as_deref(),
    );

    let cancel = CancellationToken::new();
    let worker_codex = run_codex_conversation_interactive(
        agent_config,
        Arc::clone(&auth_manager),
        Arc::clone(&session),
        Arc::clone(&turn),
        cancel.clone(),
        None,
        match kind {
            DelegateAgentKind::Worker => SubAgentSource::Other("manager_worker".to_string()),
            DelegateAgentKind::Manager => SubAgentSource::Other("ceo_manager".to_string()),
        },
    )
    .await
    .map_err(|err| {
        FunctionCallError::Fatal(format!("failed to start {}: {err}", noun_for_kind(kind)))
    })?;

    let worker_id = session.allocate_worker_id(kind).await;
    let mut initial_display_name = normalize_display_name(args.display_name.clone());
    if initial_display_name.is_none() {
        initial_display_name = generate_delegate_display_name(
            Arc::clone(&session),
            Arc::clone(&turn),
            auth_manager,
            agent_model.as_str(),
            kind,
            args.objective.as_deref().unwrap_or_default(),
            args.context.as_deref(),
            args.persona.as_deref(),
        )
        .await;
    }
    let worker = Arc::new(ManagedWorker::new(
        worker_id.clone(),
        agent_model.clone(),
        kind,
        worker_codex,
        cancel,
        initial_display_name.clone(),
    ));

    let raw_display_name = args.display_name.clone();
    let cleaned_provided = normalize_display_name(raw_display_name.clone());
    if raw_display_name.is_some() {
        worker.set_display_name(cleaned_provided.clone()).await;
    }
    let mut display_name = cleaned_provided.or(initial_display_name);
    if display_name.is_none() {
        display_name = worker.display_name().await;
    }

    let objective = args.objective.clone().unwrap_or_default();
    let objective_preview = truncate_preview(&objective, 80);
    let input = build_worker_input(&objective, args.context.as_deref());
    let summary = Arc::new(Mutex::new(WorkerRunSummary::new(
        worker_id.clone(),
        objective,
        agent_model,
        WorkerAction::Start,
        kind,
    )));
    let mut status_emitter = WorkerStatusEmitter::new(
        Arc::clone(&session),
        Arc::clone(&turn),
        worker_id.clone(),
        worker.model.clone(),
        kind,
        display_name,
    );
    status_emitter
        .emit(
            DelegateWorkerStatusKind::Starting,
            format!("Starting {} for {objective_preview}", noun_for_kind(kind)),
        )
        .await;

    if blocking {
        let worker_active = run_worker_turn(
            Arc::clone(&worker),
            Arc::clone(&session),
            Arc::clone(&turn),
            input,
            Arc::clone(&summary),
            status_emitter,
        )
        .await?;
        {
            let mut guard = summary.lock().await;
            guard.worker_active = worker_active;
        }
        if worker_active {
            session.insert_worker(Arc::clone(&worker)).await;
        } else {
            worker.shutdown().await;
        }
        let summary_value = summary.lock().await.clone();
        return Ok(summary_output(summary_value));
    }

    let handle = spawn_worker_run(
        Arc::clone(&worker),
        Arc::clone(&session),
        Arc::clone(&turn),
        input,
        Arc::clone(&summary),
        status_emitter,
        WorkerCompletionHook::Start {
            session: Arc::clone(&session),
            worker: Arc::clone(&worker),
        },
    );
    session.insert_worker_run(worker_id.clone(), handle).await;
    let summary_value = summary.lock().await.clone();
    Ok(summary_output(summary_value))
}

async fn resume_agent(
    expected_kind: DelegateAgentKind,
    session: Arc<Session>,
    turn: Arc<TurnContext>,
    args: &DelegateAgentArgs,
    blocking: bool,
) -> Result<ToolOutput, FunctionCallError> {
    let worker_id = agent_id_or_bug(args, expected_kind)?;
    if session.worker_has_pending_run(&worker_id).await {
        return Err(agent_has_pending_run(expected_kind, &worker_id));
    }
    let worker = session
        .get_worker(&worker_id)
        .await
        .ok_or_else(|| agent_not_found(expected_kind, &worker_id))?;

    if worker.agent_kind != expected_kind {
        return Err(wrong_kind_error(expected_kind, worker.agent_kind));
    }

    if worker.is_closed() {
        session.remove_worker(&worker_id).await;
        return Err(agent_not_found(expected_kind, &worker_id));
    }

    let objective = args.objective.clone().unwrap_or_default();
    let raw_display_name = args.display_name.clone();
    let cleaned_provided = normalize_display_name(raw_display_name.clone());
    if raw_display_name.is_some() {
        worker.set_display_name(cleaned_provided.clone()).await;
    }
    let mut display_name = cleaned_provided.or(worker.display_name().await);
    if display_name.is_none() {
        if let Some(auth_manager) = turn.client.get_auth_manager() {
            let generated = generate_delegate_display_name(
                Arc::clone(&session),
                Arc::clone(&turn),
                auth_manager,
                worker.model.as_str(),
                expected_kind,
                objective.as_str(),
                args.context.as_deref(),
                args.persona.as_deref(),
            )
            .await;
            if let Some(name) = &generated {
                worker.set_display_name(Some(name.clone())).await;
            }
            display_name = generated;
        } else {
            warn!(
                "missing auth manager when generating display name for {}",
                worker_id
            );
        }
    }
    let objective_preview = truncate_preview(&objective, 80);
    let input = build_worker_input(&objective, args.context.as_deref());
    let summary = Arc::new(Mutex::new(WorkerRunSummary::new(
        worker_id.clone(),
        objective,
        worker.model.clone(),
        WorkerAction::Message,
        expected_kind,
    )));
    let mut status_emitter = WorkerStatusEmitter::new(
        Arc::clone(&session),
        Arc::clone(&turn),
        worker_id.clone(),
        worker.model.clone(),
        expected_kind,
        display_name,
    );
    status_emitter
        .emit(
            DelegateWorkerStatusKind::Running,
            format!(
                "Resuming {} for {objective_preview}",
                noun_for_kind(expected_kind)
            ),
        )
        .await;

    if blocking {
        let worker_active = run_worker_turn(
            Arc::clone(&worker),
            Arc::clone(&session),
            Arc::clone(&turn),
            input,
            Arc::clone(&summary),
            status_emitter,
        )
        .await?;
        {
            let mut guard = summary.lock().await;
            guard.worker_active = worker_active;
        }
        if !worker_active {
            session.remove_worker(&worker_id).await;
            worker.shutdown().await;
        }
        let summary_value = summary.lock().await.clone();
        return Ok(summary_output(summary_value));
    }

    let handle = spawn_worker_run(
        Arc::clone(&worker),
        Arc::clone(&session),
        Arc::clone(&turn),
        input,
        Arc::clone(&summary),
        status_emitter,
        WorkerCompletionHook::Resume {
            session: Arc::clone(&session),
            worker: Arc::clone(&worker),
            worker_id: worker_id.clone(),
        },
    );
    session.insert_worker_run(worker_id.clone(), handle).await;
    let summary_value = summary.lock().await.clone();
    Ok(summary_output(summary_value))
}

async fn close_agent(
    expected_kind: DelegateAgentKind,
    session: Arc<Session>,
    args: &DelegateAgentArgs,
) -> Result<ToolOutput, FunctionCallError> {
    let worker_id = agent_id_or_bug(args, expected_kind)?;
    if let Some(summary) = session.worker_run_summary(&worker_id).await {
        let agent_kind = summary.lock().await.agent_kind;
        if agent_kind != expected_kind {
            return Err(wrong_kind_error(expected_kind, agent_kind));
        }
    }
    if let Some(run) = session.take_worker_run(&worker_id).await {
        let _ = run.wait().await;
    }
    let worker = session
        .remove_worker(&worker_id)
        .await
        .ok_or_else(|| agent_not_found(expected_kind, &worker_id))?;
    if worker.agent_kind != expected_kind {
        return Err(wrong_kind_error(expected_kind, worker.agent_kind));
    }
    worker.shutdown().await;

    let mut summary = WorkerRunSummary::new(
        worker_id,
        String::new(),
        worker.model.clone(),
        WorkerAction::Close,
        expected_kind,
    );
    summary.worker_active = false;
    summary.completed = true;

    Ok(summary_output(summary))
}

async fn await_agent(
    expected_kind: DelegateAgentKind,
    session: Arc<Session>,
    args: &DelegateAgentArgs,
) -> Result<ToolOutput, FunctionCallError> {
    let worker_id = agent_id_or_bug(args, expected_kind)?;
    if let Some(summary) = session.worker_run_summary(&worker_id).await {
        let agent_kind = summary.lock().await.agent_kind;
        if agent_kind != expected_kind {
            return Err(wrong_kind_error(expected_kind, agent_kind));
        }
    }
    let handle = session
        .take_worker_run(&worker_id)
        .await
        .ok_or_else(|| no_pending_run(expected_kind, &worker_id))?;
    let summary = handle.wait().await?;
    Ok(summary_output(summary))
}

async fn agent_status(
    expected_kind: DelegateAgentKind,
    session: Arc<Session>,
    args: &DelegateAgentArgs,
) -> Result<ToolOutput, FunctionCallError> {
    let worker_id = agent_id_or_bug(args, expected_kind)?;
    let summary = session
        .worker_run_summary(&worker_id)
        .await
        .ok_or_else(|| no_pending_run(expected_kind, &worker_id))?;
    let summary_value = {
        let guard = summary.lock().await;
        if guard.agent_kind != expected_kind {
            return Err(wrong_kind_error(expected_kind, guard.agent_kind));
        }
        guard.clone()
    };
    Ok(summary_output(summary_value))
}

async fn run_worker_turn(
    worker: Arc<ManagedWorker>,
    session: Arc<Session>,
    turn: Arc<TurnContext>,
    input: Vec<UserInput>,
    summary: Arc<Mutex<WorkerRunSummary>>,
    mut status_emitter: WorkerStatusEmitter,
) -> Result<bool, FunctionCallError> {
    let noun = noun_for_kind(worker.agent_kind);
    let title = title_for_kind(worker.agent_kind);
    let parent_worker_id = worker.id.clone();
    let _permit = worker
        .acquire()
        .await
        .map_err(|_| agent_not_found(worker.agent_kind, &worker.id))?;
    worker
        .codex
        .submit(Op::UserInput { items: input })
        .await
        .map_err(|err| FunctionCallError::Fatal(format!("failed to submit {noun} input: {err}")))?;
    status_emitter
        .emit(
            DelegateWorkerStatusKind::Running,
            format!("Sent objective to {noun}"),
        )
        .await;

    loop {
        match worker.codex.next_event().await {
            Ok(event) => match event.msg {
                EventMsg::TaskStarted(TaskStartedEvent { .. }) => {
                    status_emitter
                        .emit(
                            DelegateWorkerStatusKind::Running,
                            format!("{title} is thinking"),
                        )
                        .await;
                }
                EventMsg::TaskComplete(task) => {
                    {
                        let mut guard = summary.lock().await;
                        guard.last_message = task.last_agent_message;
                        guard.completed = true;
                    }
                    status_emitter
                        .emit(
                            DelegateWorkerStatusKind::Completed,
                            format!("{title} completed the objective"),
                        )
                        .await;
                    return Ok(true);
                }
                EventMsg::TurnAborted(aborted) => {
                    {
                        let mut guard = summary.lock().await;
                        guard.aborted_reason = Some(format!("{:?}", aborted.reason));
                    }
                    status_emitter
                        .emit(
                            DelegateWorkerStatusKind::Failed,
                            format!("{title} aborted: {:?}", aborted.reason),
                        )
                        .await;
                    return Ok(false);
                }
                EventMsg::Error(err) => {
                    {
                        let mut guard = summary.lock().await;
                        guard.errors.push(err.message.clone());
                    }
                    status_emitter
                        .emit(
                            DelegateWorkerStatusKind::Warning,
                            format!("{title} warning: {}", err.message),
                        )
                        .await;
                }
                EventMsg::TurnDiff(diff) => {
                    let diff_index = {
                        let mut guard = summary.lock().await;
                        guard.diff_count += 1;
                        guard.diff_count
                    };
                    session
                        .send_event(turn.as_ref(), EventMsg::TurnDiff(diff))
                        .await;
                    status_emitter
                        .emit(
                            DelegateWorkerStatusKind::DiffApplied,
                            format!("Applied diff #{diff_index:02}"),
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
                EventMsg::StreamError(StreamErrorEvent { message, .. }) => {
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
                EventMsg::DelegateWorkerStatus(mut nested) => {
                    nested.parent_worker_id = Some(parent_worker_id.clone());
                    session
                        .send_event(turn.as_ref(), EventMsg::DelegateWorkerStatus(nested))
                        .await;
                }
                _ => {}
            },
            Err(CodexErr::InternalAgentDied) => {
                {
                    let mut guard = summary.lock().await;
                    guard.aborted_reason = Some(format!("{title} exited unexpectedly"));
                }
                status_emitter
                    .emit(
                        DelegateWorkerStatusKind::Failed,
                        format!("{title} exited unexpectedly"),
                    )
                    .await;
                return Ok(false);
            }
            Err(err) => {
                status_emitter
                    .emit(
                        DelegateWorkerStatusKind::Failed,
                        format!("{title} failed: {err}"),
                    )
                    .await;
                return Err(FunctionCallError::Fatal(format!(
                    "{noun} failed before completion: {err}"
                )));
            }
        }
    }
}

fn agent_not_found(kind: DelegateAgentKind, worker_id: &str) -> FunctionCallError {
    FunctionCallError::RespondToModel(
        format!(
            "{} `{worker_id}` is not active. Start a new {} instead.",
            title_for_kind(kind),
            noun_for_kind(kind)
        )
        .into(),
    )
}

fn agent_has_pending_run(kind: DelegateAgentKind, worker_id: &str) -> FunctionCallError {
    FunctionCallError::RespondToModel(
        format!(
            "{} `{worker_id}` already has a request in flight. Call {} with action:\"await\" to collect the result before sending another objective.",
            title_for_kind(kind),
            tool_name_for_kind(kind)
        )
        .into(),
    )
}

fn no_pending_run(kind: DelegateAgentKind, worker_id: &str) -> FunctionCallError {
    FunctionCallError::RespondToModel(
        format!(
            "{} `{worker_id}` has no pending asynchronous turn. Start a new objective with blocking:false before awaiting.",
            title_for_kind(kind)
        )
        .into(),
    )
}

fn agent_id_or_bug(
    args: &DelegateAgentArgs,
    kind: DelegateAgentKind,
) -> Result<String, FunctionCallError> {
    args.worker_id.clone().ok_or_else(|| {
        FunctionCallError::Fatal(format!(
            "{} missing {} after validation",
            tool_name_for_kind(kind),
            id_key_for_kind(kind)
        ))
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

fn wrong_kind_error(expected: DelegateAgentKind, actual: DelegateAgentKind) -> FunctionCallError {
    FunctionCallError::RespondToModel(
        format!(
            "{} cannot target {} IDs. Use {} instead.",
            tool_name_for_kind(expected),
            noun_for_kind(actual),
            tool_name_for_kind(actual),
        )
        .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn parse(input: &str) -> Result<DelegateAgentArgs, FunctionCallError> {
        parse_args(
            &ToolPayload::Function {
                arguments: input.to_string(),
            },
            DelegateAgentKind::Worker,
        )
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
    fn await_requires_worker_id() {
        let err = parse(r#"{"action":"await"}"#).unwrap_err();
        match err {
            FunctionCallError::RespondToModel(msg) => {
                assert!(msg.content.contains("worker_id"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn status_requires_worker_id() {
        let err = parse(r#"{"action":"status"}"#).unwrap_err();
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

    #[test]
    fn web_search_override_only_allowed_on_start() {
        let err = parse(
            r#"{"action":"message","worker_id":"worker-7","objective":"hi","web_search":true}"#,
        )
        .unwrap_err();
        match err {
            FunctionCallError::RespondToModel(msg) => {
                assert!(msg.content.contains("web_search override"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn web_search_override_is_preserved_on_start() {
        let args = parse(r#"{"objective":"research","web_search":false}"#).unwrap();
        assert_eq!(args.web_search, Some(false));
    }

    #[test]
    fn persona_instructions_are_appended() {
        let mut instructions = Some("Follow AGENTS".to_string());
        append_persona_instructions(&mut instructions, Some("Act as a strict reviewer."));
        let expected = "Follow AGENTS\n\nPersona instructions:\nAct as a strict reviewer.";
        assert_eq!(instructions.as_deref(), Some(expected));
    }

    #[test]
    fn persona_instructions_created_when_empty() {
        let mut instructions = None;
        append_persona_instructions(&mut instructions, Some("Lead with architecture first."));
        let expected = "Persona instructions:\nLead with architecture first.";
        assert_eq!(instructions.as_deref(), Some(expected));
    }

    #[test]
    fn blank_persona_is_ignored() {
        let mut instructions = Some("Existing".to_string());
        append_persona_instructions(&mut instructions, Some("   \n"));
        assert_eq!(instructions.as_deref(), Some("Existing"));
    }

    #[test]
    fn sanitize_display_name_from_json() {
        let raw = r#"{"display_name":"Docs Polish"}"#;
        assert_eq!(
            sanitize_generated_display_name(raw).as_deref(),
            Some("Docs Polish")
        );
    }

    #[test]
    fn sanitize_display_name_from_plain_text() {
        let raw = "- Worker Label:  Fix parsing loop.\nAdditional commentary here.";
        assert_eq!(
            sanitize_generated_display_name(raw).as_deref(),
            Some("Fix parsing loop")
        );
    }
}
