use std::sync::Arc;

use tokio::sync::watch;

/// Emergency kill switch for the trading engine.
///
/// Provides a mechanism to trigger an emergency shutdown sequence
/// that is distinct from normal graceful shutdown. When activated,
/// the engine halts the controller, shuts down strategies, cancels
/// all open orders, and closes all positions.
///
/// `KillSwitch` is cheaply clonable via internal `Arc`, allowing both
/// the engine and external code to share the same switch.
#[derive(Debug, Clone)]
pub struct KillSwitch {
    tx: Arc<watch::Sender<bool>>,
    rx: watch::Receiver<bool>,
}

impl Default for KillSwitch {
    fn default() -> Self {
        Self::new()
    }
}

impl KillSwitch {
    /// Create a new inactive kill switch.
    pub fn new() -> Self {
        let (tx, rx) = watch::channel(false);
        Self {
            tx: Arc::new(tx),
            rx,
        }
    }

    /// Activate the kill switch. All subscribers will be notified.
    pub fn activate(&self) {
        let _ = self.tx.send(true);
    }

    /// Check whether the kill switch has been activated.
    pub fn is_activated(&self) -> bool {
        *self.rx.borrow()
    }

    /// Get a receiver that will be notified when the kill switch is activated.
    pub fn subscribe(&self) -> watch::Receiver<bool> {
        self.rx.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kill_switch_initially_inactive() {
        let ks = KillSwitch::new();
        assert!(!ks.is_activated());
    }

    #[test]
    fn test_kill_switch_activate() {
        let ks = KillSwitch::new();
        ks.activate();
        assert!(ks.is_activated());
    }

    #[tokio::test]
    async fn test_kill_switch_subscribe_receives_signal() {
        let ks = KillSwitch::new();
        let mut rx = ks.subscribe();

        // Not yet activated
        assert!(!*rx.borrow());

        ks.activate();

        // Subscriber receives the change
        let changed = rx.changed().await;
        assert!(changed.is_ok());
        assert!(*rx.borrow());
    }
}
