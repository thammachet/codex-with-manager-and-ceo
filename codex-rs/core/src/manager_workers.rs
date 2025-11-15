use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use tokio::sync::AcquireError;
use tokio::sync::OwnedSemaphorePermit;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::codex::Codex;
use codex_protocol::protocol::Op;

/// A worker managed by the planning manager layer.
pub(crate) struct ManagedWorker {
    pub(crate) id: String,
    pub(crate) model: String,
    pub(crate) codex: Codex,
    pub(crate) cancel_token: CancellationToken,
    semaphore: Arc<Semaphore>,
    closed: AtomicBool,
}

impl ManagedWorker {
    pub(crate) fn new(
        id: String,
        model: String,
        codex: Codex,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            id,
            model,
            codex,
            cancel_token,
            semaphore: Arc::new(Semaphore::new(1)),
            closed: AtomicBool::new(false),
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
}

#[derive(Default)]
pub(crate) struct ManagedWorkerRegistry {
    next_worker_id: u64,
    workers: HashMap<String, Arc<ManagedWorker>>,
}

impl ManagedWorkerRegistry {
    pub(crate) fn new() -> Self {
        Self {
            next_worker_id: 1,
            workers: HashMap::new(),
        }
    }

    pub(crate) fn allocate_id(&mut self) -> String {
        let id = format!("worker-{}", self.next_worker_id);
        self.next_worker_id += 1;
        id
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
}
