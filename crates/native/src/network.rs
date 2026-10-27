//! macOS Low Data Mode, through safe Network.framework bindings.

use cidre::{arc, dispatch, nw};

pub struct Monitor(arc::R<nw::PathMonitor>);

impl Monitor {
    pub fn new(mut changed: impl FnMut(bool) + Sync + 'static) -> Self {
        let queue = dispatch::Queue::new();
        let mut monitor = nw::PathMonitor::new();
        monitor.set_queue(&queue);
        monitor.set_update_handler(move |path| {
            changed(path.status() == nw::PathStatus::Satisfied && !path.is_constrained());
        });
        monitor.start();
        Self(monitor)
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn macos_reports_an_initial_path_without_a_network_request() {
        let (sent, received) = std::sync::mpsc::channel();
        let _monitor = super::Monitor::new(move |allowed| {
            let _ = sent.send(allowed);
        });
        assert!(received.recv_timeout(std::time::Duration::from_secs(5)).is_ok(), "Network.framework should report the current path");
    }
}
