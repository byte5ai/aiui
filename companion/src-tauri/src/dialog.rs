use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::oneshot;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DialogRequest {
    pub id: String,
    pub spec: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DialogResult {
    pub id: String,
    pub cancelled: bool,
    pub result: serde_json::Value,
}

pub struct DialogState {
    pending: Mutex<HashMap<String, oneshot::Sender<DialogResult>>>,
}

impl DialogState {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Registers a new dialog and returns (id, future). The caller is
    /// responsible for surfacing the window + emitting the dialog-show event.
    pub fn register(&self) -> (String, oneshot::Receiver<DialogResult>) {
        let id = Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.clone(), tx);
        (id, rx)
    }

    pub fn complete(&self, id: &str, result: serde_json::Value) {
        let sender = self.pending.lock().unwrap().remove(id);
        if let Some(sender) = sender {
            let _ = sender.send(DialogResult {
                id: id.to_string(),
                cancelled: false,
                result,
            });
        }
    }

    pub fn cancel(&self, id: &str) {
        let sender = self.pending.lock().unwrap().remove(id);
        if let Some(sender) = sender {
            let _ = sender.send(DialogResult {
                id: id.to_string(),
                cancelled: true,
                result: serde_json::Value::Null,
            });
        }
    }
}
