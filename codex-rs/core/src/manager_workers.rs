use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use serde::Deserialize;
use tokio::sync::AcquireError;
use tokio::sync::Mutex;
use tokio::sync::OwnedSemaphorePermit;
use tokio::sync::Semaphore;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::codex::Codex;
use crate::function_tool::FunctionCallError;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::DelegateAgentKind;
use codex_protocol::protocol::Op;

/// A worker managed by the planning manager layer.
pub(crate) struct ManagedWorker {
    pub(crate) id: String,
    pub(crate) model: String,
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
    pub(crate) agent_kind: DelegateAgentKind,
    pub(crate) codex: Codex,
    pub(crate) cancel_token: CancellationToken,
    semaphore: Arc<Semaphore>,
    closed: AtomicBool,
    display_name: Mutex<Option<String>>,
}

impl ManagedWorker {
    pub(crate) fn new(
        id: String,
        model: String,
        reasoning_effort: Option<ReasoningEffort>,
        agent_kind: DelegateAgentKind,
        codex: Codex,
        cancel_token: CancellationToken,
        display_name: Option<String>,
    ) -> Self {
        Self {
            id,
            model,
            reasoning_effort,
            agent_kind,
            codex,
            cancel_token,
            semaphore: Arc::new(Semaphore::new(1)),
            closed: AtomicBool::new(false),
            display_name: Mutex::new(display_name),
        }
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub(crate) fn mark_closed(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.semaphore.close();
    }

    pub(crate) async fn acquire(&self) -> Result<OwnedSemaphorePermit, AcquireError> {
        Arc::clone(&self.semaphore).acquire_owned().await
    }

    pub(crate) async fn shutdown(&self) {
        self.mark_closed();
        let _ = self.codex.submit(Op::Shutdown {}).await;
        self.cancel_token.cancel();
    }

    pub(crate) async fn display_name(&self) -> Option<String> {
        self.display_name.lock().await.clone()
    }

    pub(crate) async fn set_display_name(&self, value: Option<String>) {
        *self.display_name.lock().await = value;
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub(crate) enum WorkerAction {
    #[default]
    Start,
    Message,
    Close,
    Await,
    Status,
}

impl fmt::Display for WorkerAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            WorkerAction::Start => "start",
            WorkerAction::Message => "message",
            WorkerAction::Close => "close",
            WorkerAction::Await => "await",
            WorkerAction::Status => "status",
        };
        f.write_str(label)
    }
}

#[derive(Clone)]
pub(crate) struct WorkerRunSummary {
    pub(crate) worker_id: String,
    pub(crate) objective: String,
    pub(crate) worker_model: String,
    pub(crate) action: WorkerAction,
    pub(crate) agent_kind: DelegateAgentKind,
    pub(crate) last_message: Option<String>,
    pub(crate) aborted_reason: Option<String>,
    pub(crate) completed: bool,
    pub(crate) diff_count: usize,
    pub(crate) errors: Vec<String>,
    pub(crate) worker_active: bool,
}

impl WorkerRunSummary {
    pub(crate) fn new(
        worker_id: String,
        objective: String,
        worker_model: String,
        action: WorkerAction,
        agent_kind: DelegateAgentKind,
    ) -> Self {
        Self {
            worker_id,
            objective,
            worker_model,
            action,
            agent_kind,
            last_message: None,
            aborted_reason: None,
            completed: false,
            diff_count: 0,
            errors: Vec::new(),
            worker_active: true,
        }
    }

    pub(crate) fn status_line(&self) -> String {
        if let Some(reason) = &self.aborted_reason {
            return format!("failed ({reason})");
        }
        if self.completed {
            return "completed".to_string();
        }
        "incomplete".to_string()
    }

    pub(crate) fn success(&self) -> bool {
        self.aborted_reason.is_none() && self.completed
    }
}

pub(crate) struct WorkerRunHandle {
    summary: Arc<Mutex<WorkerRunSummary>>,
    completion_rx: Mutex<Option<oneshot::Receiver<Result<WorkerRunSummary, FunctionCallError>>>>,
    join_handle: JoinHandle<()>,
}

impl WorkerRunHandle {
    pub(crate) fn new(
        summary: Arc<Mutex<WorkerRunSummary>>,
        completion_rx: oneshot::Receiver<Result<WorkerRunSummary, FunctionCallError>>,
        join_handle: JoinHandle<()>,
    ) -> Self {
        Self {
            summary,
            completion_rx: Mutex::new(Some(completion_rx)),
            join_handle,
        }
    }

    pub(crate) fn summary(&self) -> Arc<Mutex<WorkerRunSummary>> {
        Arc::clone(&self.summary)
    }

    pub(crate) async fn wait(self) -> Result<WorkerRunSummary, FunctionCallError> {
        let rx = {
            let mut guard = self.completion_rx.lock().await;
            guard
                .take()
                .ok_or_else(|| FunctionCallError::Fatal("worker run already awaited".to_string()))?
        };
        let result = rx.await.map_err(|_| {
            FunctionCallError::Fatal("worker run channel dropped unexpectedly".to_string())
        })?;
        let _ = self.join_handle.await;
        result
    }
}

pub(crate) struct ManagedWorkerRegistry {
    next_worker_id: u64,
    next_manager_id: u64,
    workers: HashMap<String, Arc<ManagedWorker>>,
}

impl ManagedWorkerRegistry {
    pub(crate) fn new() -> Self {
        Self {
            next_worker_id: 1,
            next_manager_id: 1,
            workers: HashMap::new(),
        }
    }

    pub(crate) fn allocate_id(&mut self, kind: DelegateAgentKind) -> String {
        match kind {
            DelegateAgentKind::Worker => {
                let id = format!("worker-{}", self.next_worker_id);
                self.next_worker_id += 1;
                id
            }
            DelegateAgentKind::Manager => {
                let id = format!("manager-{}", self.next_manager_id);
                self.next_manager_id += 1;
                id
            }
        }
    }

    pub(crate) fn insert(&mut self, worker: Arc<ManagedWorker>) {
        self.workers.insert(worker.id.clone(), worker);
    }

    pub(crate) fn get(&self, worker_id: &str) -> Option<Arc<ManagedWorker>> {
        self.workers.get(worker_id).cloned()
    }

    pub(crate) fn remove(&mut self, worker_id: &str) -> Option<Arc<ManagedWorker>> {
        self.workers.remove(worker_id)
    }

    pub(crate) fn take_all(&mut self) -> Vec<Arc<ManagedWorker>> {
        self.workers.drain().map(|(_, worker)| worker).collect()
    }

    pub(crate) fn all(&self) -> Vec<Arc<ManagedWorker>> {
        self.workers.values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocates_ids_per_agent_kind() {
        let mut registry = ManagedWorkerRegistry::new();
        assert_eq!(
            registry.allocate_id(DelegateAgentKind::Manager),
            "manager-1"
        );
        assert_eq!(registry.allocate_id(DelegateAgentKind::Worker), "worker-1");
        assert_eq!(
            registry.allocate_id(DelegateAgentKind::Manager),
            "manager-2"
        );
        assert_eq!(registry.allocate_id(DelegateAgentKind::Worker), "worker-2");
    }
}
