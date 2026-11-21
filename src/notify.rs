//! Native macOS notifications via UNUserNotificationCenter.
//!
//! This only works when the running executable lives inside one of the
//! generated `.app` bundles: macOS attributes the notification, and its icon,
//! to that bundle.

use std::path::Path;
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_foundation::{ns_string, NSBundle, NSDictionary, NSError, NSObject, NSObjectProtocol, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationPresentationOptions,
    UNNotificationRequest, UNNotificationResponse, UNNotificationSound, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate, UNNotification,
};

use crate::event::{Agent, Event};
use crate::log;

const KEY_CWD: &str = "cwd";
const KEY_FOCUS: &str = "focus_bundle";
const DEFAULT_FOCUS_BUNDLE: &str = "com.microsoft.VSCode";

/// The agent whose bundle we're running inside, if any.
pub fn current_bundle_agent() -> Option<Agent> {
    let id = NSBundle::mainBundle().bundleIdentifier()?;
    Agent::from_bundle_id(&id.to_string())
}

fn error_string(err: *mut NSError) -> Option<String> {
    // SAFETY: the pointer comes straight from a completion handler; it's
    // either null or a valid NSError for the duration of the callback.
    unsafe { err.as_ref() }.map(|e| e.localizedDescription().to_string())
}

/// Ask for permission to post alerts. Shows the system prompt the first time;
/// afterwards it returns the stored decision immediately.
pub fn authorize(timeout: Duration) -> Result<bool, String> {
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(move |granted: Bool, err: *mut NSError| {
        let _ = tx.send((granted.as_bool(), error_string(err)));
    });
    center.requestAuthorizationWithOptions_completionHandler(
        UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
        &block,
    );
    match rx.recv_timeout(timeout) {
        Ok((_, Some(err))) => Err(err),
        Ok((granted, None)) => Ok(granted),
        Err(_) => Err("timed out waiting for notification authorization".into()),
    }
}

/// Post a notification for `ev` and wait until macOS accepted it.
pub fn post(ev: &Event, focus_bundle: Option<String>) -> Result<(), String> {
    if !authorize(Duration::from_secs(60))? {
        return Err("notifications are not allowed for this app (System Settings → Notifications)".into());
    }

    // Title is the git root's folder name; with no cwd it stays empty and
    // macOS shows the app name instead.
    let title = ev
        .cwd
        .as_deref()
        .map(project_root)
        .and_then(|root| Path::new(&root).file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default();

    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(&title));
    content.setBody(&NSString::from_str(&ev.body));
    content.setSound(Some(&UNNotificationSound::defaultSound()));
    if let Some(cwd) = &ev.cwd {
        // Group notifications per project in Notification Center.
        content.setThreadIdentifier(&NSString::from_str(cwd));
    }

    let cwd = NSString::from_str(ev.cwd.as_deref().unwrap_or(""));
    let focus = NSString::from_str(focus_bundle.as_deref().unwrap_or(""));
    let values: [&AnyObject; 2] = [&cwd, &focus];
    let info = NSDictionary::<NSString, AnyObject>::from_slices(
        &[ns_string!(KEY_CWD), ns_string!(KEY_FOCUS)],
        &values,
    );
    // SAFETY: userInfo must be a property-list dictionary; ours holds only
    // NSString keys and values. The cast only erases the key type.
    unsafe {
        let info: Retained<NSDictionary> = Retained::cast_unchecked(info);
        content.setUserInfo(&info);
    }

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let id = NSString::from_str(&format!("agent-pager-{nanos}"));
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&id, &content, None);

    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(move |err: *mut NSError| {
        let _ = tx.send(error_string(err));
    });
    UNUserNotificationCenter::currentNotificationCenter()
        .addNotificationRequest_withCompletionHandler(&request, Some(&block));
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(None) => Ok(()),
        Ok(Some(err)) => Err(err),
        Err(_) => Err("timed out posting notification".into()),
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "AgentPagerDelegate"]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            handler: &DynBlock<dyn Fn()>,
        ) {
            let info = response.notification().request().content().userInfo();
            let cwd = info_string(&info, ns_string!(KEY_CWD));
            let focus_bundle = info_string(&info, ns_string!(KEY_FOCUS));
            focus(cwd.as_deref(), focus_bundle.as_deref());
            handler.call(());
            // Give the system a moment to process the completion, then quit.
            std::thread::spawn(|| {
                std::thread::sleep(Duration::from_millis(300));
                std::process::exit(0);
            });
        }

        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            handler: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            handler.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound,));
        }
    }
);

impl Delegate {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(());
        // SAFETY: plain NSObject init.
        unsafe { msg_send![super(this), init] }
    }
}

fn info_string(info: &NSDictionary, key: &NSString) -> Option<String> {
    let obj = info.objectForKey(key)?;
    let s = obj.downcast_ref::<NSString>()?.to_string();
    (!s.is_empty()).then_some(s)
}

/// App mode: macOS launched the bundle because a notification was clicked.
/// Run an AppKit event loop just long enough to receive the response.
pub fn run_click_handler() -> ! {
    let mtm = MainThreadMarker::new().expect("must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    // The delegate must be set before the app finishes launching, or the
    // response that launched us is dropped.
    let delegate = Delegate::new();
    UNUserNotificationCenter::currentNotificationCenter()
        .setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

    // Opened without a click (e.g. double-clicked in Finder): don't linger.
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(10));
        std::process::exit(0);
    });

    app.run();
    std::process::exit(0);
}

/// Editors that can take a folder and focus the window that has it open.
const EDITOR_BUNDLES: &[&str] = &[
    "com.microsoft.VSCode",
    "com.microsoft.VSCodeInsiders",
    "com.vscodium",
    "com.todesktop.230313mzl4w4u92", // Cursor
    "com.exafunction.windsurf",
    "dev.zed.Zed",
];

/// Bring the app the agent ran in to the front. For editors, open the
/// project folder so the right window comes forward.
fn focus(cwd: Option<&str>, focus_bundle: Option<&str>) {
    let bundle = focus_bundle.unwrap_or(DEFAULT_FOCUS_BUNDLE);
    let mut cmd = Command::new("/usr/bin/open");
    cmd.arg("-b").arg(bundle);
    if EDITOR_BUNDLES.contains(&bundle) {
        if let Some(dir) = cwd.map(project_root) {
            cmd.arg(dir);
        }
    }
    if let Err(e) = cmd.status() {
        log::error(&format!("focus via open failed: {e}"));
    }
}

/// Prefer the enclosing git root, since that's usually the folder open in the
/// editor; opening a subfolder would spawn a new window instead.
fn project_root(cwd: &str) -> String {
    let start = Path::new(cwd);
    for dir in start.ancestors() {
        if dir.join(".git").exists() {
            return dir.to_string_lossy().into_owned();
        }
    }
    cwd.to_string()
}

/// Work out which app launched the agent, from the hook's environment.
pub fn detect_focus_bundle() -> Option<String> {
    if let Ok(id) = std::env::var("__CFBundleIdentifier") {
        if !id.is_empty() && !id.starts_with(crate::event::BUNDLE_ID_PREFIX) {
            return Some(id);
        }
    }
    let by_term = match std::env::var("TERM_PROGRAM").ok()?.as_str() {
        "vscode" => "com.microsoft.VSCode",
        "iTerm.app" => "com.googlecode.iterm2",
        "Apple_Terminal" => "com.apple.Terminal",
        "WarpTerminal" => "dev.warp.Warp-Stable",
        "ghostty" => "com.mitchellh.ghostty",
        _ => return None,
    };
    Some(by_term.to_string())
}
