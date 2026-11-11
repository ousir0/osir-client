//! Keep the entire tray handle lifetime on the event-loop thread.
//!
//! Tauri marshals tray setters, but `tray_by_id` also clones tray-icon's
//! `Rc<RefCell<_>>`. Looking it up or dropping that clone on a worker can race
//! the main thread and destroy NSStatusItem on a Tokio worker.

use tauri::{tray::TrayIcon, AppHandle};

pub(crate) fn with_tray(
    app: &AppHandle,
    action: impl FnOnce(Option<&TrayIcon>) + Send + 'static,
) -> tauri::Result<()> {
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let tray = handle.tray_by_id(crate::tray::TRAY_ID);
        action(tray.as_ref());
        // Both the lookup clone and setter clones are dropped on this thread.
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn tray_handles_cannot_be_acquired_outside_main_thread_dispatch() {
        // Guard the native lifetime boundary, including async IPC and shutdown.
        // A setter's own dispatch does not protect lookup/clone/drop at its caller.
        for source in [include_str!("lib.rs"), include_str!("tray.rs")] {
            assert!(!source.contains(".tray_by_id("));
            assert!(!source.contains(".remove_tray_by_id("));
        }
    }
}
