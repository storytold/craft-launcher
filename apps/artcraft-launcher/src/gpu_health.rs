//! The window's graphics device (#274). A driver reset (for example after the PC slept) loses the
//! wgpu device the window draws with, and egui-wgpu can't draw another frame on it: one with shapes
//! panics creating its staging buffers ("Failed to create staging buffer for index data"), and
//! wgpu's default handler panics on the errors that follow. [`WindowGpu`] records the loss
//! instead, the shell then draws nothing, closes the window and opens a new one with a fresh
//! device, at most [`ReopenBudget::MAX`] times in [`ReopenBudget::WINDOW`].

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use eframe::wgpu;

/// The window's device and whether wgpu reported it lost.
pub struct WindowGpu {
    device: wgpu::Device,
    lost: Arc<AtomicBool>,
}

impl WindowGpu {
    /// Watch `device`: its loss sets a flag rather than panicking, and an uncaptured error is
    /// logged rather than ending the launcher (wgpu's default handler panics).
    pub fn watch(device: &wgpu::Device) -> Self {
        let lost = Arc::new(AtomicBool::new(false));
        let flag = lost.clone();
        // Called by wgpu with its own locks held: only record, never call back into wgpu.
        device.set_device_lost_callback(move |reason, message| {
            log::error!("graphics device lost ({reason:?}): {message}");
            flag.store(true, Ordering::Release);
        });
        device.on_uncaptured_error(Arc::new(|e: wgpu::Error| log::error!("graphics error: {e}")));
        Self { device: device.clone(), lost }
    }

    /// Whether the device is lost. Polls it (non-blocking) so a loss while the window sat idle is
    /// noticed before the next frame is drawn with it; `Device::poll` panics on a lost device, so
    /// a panic counts as a loss too, logged as handled rather than as an internal error.
    pub fn lost(&self) -> bool {
        if self.lost.load(Ordering::Acquire) {
            return true;
        }
        if crate::logging::catch_expected(|| self.device.poll(wgpu::PollType::Poll)).is_err() {
            self.lost.store(true, Ordering::Release);
        }
        self.lost.load(Ordering::Acquire)
    }
}

/// How often a lost window may be replaced: a GPU that keeps failing must not open windows in a
/// loop. Past the budget the launcher stays in the tray (or quits without one), as when the
/// window is closed.
#[derive(Default)]
pub struct ReopenBudget {
    recent: Vec<Instant>,
}

impl ReopenBudget {
    pub const MAX: usize = 3;
    pub const WINDOW: Duration = Duration::from_secs(10 * 60);

    /// Whether a new window may open at `now`, counting it if so.
    pub fn allow(&mut self, now: Instant) -> bool {
        self.recent.retain(|t| now.saturating_duration_since(*t) < Self::WINDOW);
        if self.recent.len() >= Self::MAX {
            return false;
        }
        self.recent.push(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reopening_is_limited_per_ten_minutes() {
        let mut budget = ReopenBudget::default();
        let t = Instant::now();
        assert!((0..ReopenBudget::MAX).all(|i| budget.allow(t + Duration::from_secs(i as u64))));
        assert!(!budget.allow(t + Duration::from_secs(60)), "a GPU that keeps failing");
        assert!(budget.allow(t + ReopenBudget::WINDOW + Duration::from_secs(1)), "an old loss no longer counts");
    }

    /// A destroyed device reads as lost once wgpu notices (after its submitted work), and the
    /// errors that follow don't panic. Skips without an adapter.
    #[test]
    fn a_destroyed_device_reads_as_lost_without_panicking() {
        let instance = wgpu::Instance::default();
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) else {
            eprintln!("skipping: no graphics adapter");
            return;
        };
        let Ok((device, _queue)) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())) else { return };
        let gpu = WindowGpu::watch(&device);
        assert!(!gpu.lost());
        device.destroy();
        let _ = device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(Duration::from_secs(30)) });
        assert!(gpu.lost());
        // An error on the lost device is logged, not a panic from wgpu's default handler.
        let _ = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 4, usage: wgpu::BufferUsages::UNIFORM, mapped_at_creation: false });
        assert!(gpu.lost());
    }
}
