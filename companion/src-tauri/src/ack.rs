//! Generic one-shot ack registry.
//!
//! Hands out a unique id paired with a `oneshot::Receiver`; the responder
//! fulfils it via `ack(id)`. Used today for `ui:ping` round-trips from
//! `/health` to verify that the WebView event loop is still alive.
//!
//! Event-driven: nothing runs unless someone explicitly registers a request.

use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::oneshot;
use uuid::Uuid;

pub struct AckRegistry {
    pending: Mutex<HashMap<String, oneshot::Sender<()>>>,
}

impl AckRegistry {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Register a new ack slot. Returns the id that the caller emits to the
    /// frontend, plus the receiver to await with a timeout.
    pub fn register(&self) -> (String, oneshot::Receiver<()>) {
        let id = Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.clone(), tx);
        (id, rx)
    }

    /// Fulfil an outstanding ack. No-op if the id is unknown (e.g. timed
    /// out and forgotten already).
    pub fn ack(&self, id: &str) {
        if let Some(tx) = self.pending.lock().unwrap().remove(id) {
            let _ = tx.send(());
        }
    }

    /// Drop an outstanding entry without firing it. Use after a timeout so
    /// the map cannot grow without bound under repeated WebView failures.
    pub fn forget(&self, id: &str) {
        self.pending.lock().unwrap().remove(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ack_resolves_receiver() {
        let r = AckRegistry::new();
        let (id, rx) = r.register();
        r.ack(&id);
        rx.blocking_recv().expect("ack should arrive");
    }

    #[test]
    fn ack_unknown_id_is_noop() {
        let r = AckRegistry::new();
        r.ack("no-such-id"); // must not panic
    }

    #[test]
    fn forget_drops_entry() {
        let r = AckRegistry::new();
        let (id, rx) = r.register();
        r.forget(&id);
        // Sender was dropped without sending — recv() now sees `Err(RecvError)`.
        assert!(rx.blocking_recv().is_err());
    }
}
