//! The frontmost app and activation (`NSWorkspace` / `NSRunningApplication`).

use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication, NSWorkspace};
use retone_core::input::ports::{RunningApp, Workspace};

fn describe(app: &NSRunningApplication) -> RunningApp {
    RunningApp {
        pid: app.processIdentifier(),
        name: app.localizedName().map(|name| name.to_string()),
        executable_path: app
            .executableURL()
            .and_then(|url| url.path())
            .map(|path| path.to_string()),
    }
}

/// The frontmost app, as `NSWorkspace` reports it.
pub fn frontmost_app() -> Option<RunningApp> {
    NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .map(|app| describe(&app))
}

pub fn app_for_pid(pid: i32) -> Option<RunningApp> {
    NSRunningApplication::runningApplicationWithProcessIdentifier(pid).map(|app| describe(&app))
}

/// Requests activation of `pid` with all its windows, so one becomes key.
pub fn activate(pid: i32) -> bool {
    let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) else {
        return false;
    };
    // IgnoringOtherApps is deprecated (and ignored) on macOS 14+, where
    // activation is cooperative; kept for older systems.
    #[allow(deprecated)]
    let options = NSApplicationActivationOptions::ActivateAllWindows
        | NSApplicationActivationOptions::ActivateIgnoringOtherApps;
    app.activateWithOptions(options)
}

/// [`Workspace`] over AppKit.
#[derive(Debug, Default, Clone, Copy)]
pub struct MacWorkspace;

impl Workspace for MacWorkspace {
    fn frontmost_app(&self) -> Option<RunningApp> {
        frontmost_app()
    }

    fn app_for_pid(&self, pid: i32) -> Option<RunningApp> {
        app_for_pid(pid)
    }

    fn activate(&self, pid: i32) -> bool {
        activate(pid)
    }
}
