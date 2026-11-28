//! agent-pager: native macOS notifications for coding agents
//! (Claude Code, Codex, OpenCode, Antigravity), with a separate icon per agent.

mod config;
mod event;
mod install;
mod log;
mod notify;
mod setup;

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::time::Duration;

use event::Agent;

const USAGE: &str = "\
agent-pager: native macOS notifications for coding agents

USAGE:
  agent-pager setup [--claude-dir DIR]... [--assets DIR] [--dest DIR]
        Build the pager apps and add hooks to every detected agent config
        (~/.claude*, ~/.codex, ~/.config/opencode, ~/.gemini/config).
        Safe to re-run; backs up files.
  agent-pager teardown [--claude-dir DIR]... [--dest DIR]
        Remove the hooks and the pager apps.
  agent-pager test <claude|codex|opencode|antigravity> [approval|input|finished|error]
        Send a sample notification through the installed app.
  agent-pager install [--assets DIR] [--dest DIR]
        Only (re)build the pager apps (default dest ~/Applications/AgentPager).
        --assets overrides the built-in icons with DIR/<agent>.png or .icns.
  agent-pager print-config <claude|codex|opencode|antigravity> [--dest DIR]
        Print the config snippet for manual setup.
  agent-pager hook <claude|codex|opencode|antigravity>
        Read an agent hook payload on stdin and notify (used by the agents).
  agent-pager uninstall [--dest DIR]
  agent-pager --version

Logs: ~/Library/Logs/agent-pager.log
Debug payload logging: set AGENT_PAGER_DEBUG=1 or create ~/.config/agent-pager/debug
";

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Launched by macOS after a notification click: no args (older systems
    // pass a -psn_ argument).
    if args.iter().all(|a| a.starts_with("-psn")) {
        if notify::current_bundle_agent().is_some() {
            notify::run_click_handler();
        }
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }

    let cmd = args[0].as_str();
    let rest = &args[1..];
    if cmd != "hook" && cmd != "post" && rest.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let result = match cmd {
        "hook" => {
            // Always exit 0 and stay silent: a failing notifier must never
            // disturb the agent.
            match rest.first().and_then(|a| Agent::parse(a)) {
                Some(agent) => hook(agent),
                None => log::error(&format!("hook: unknown agent {:?}", rest.first())),
            }
            return ExitCode::SUCCESS;
        }
        "install" => {
            let assets = flag(rest, "--assets").map(PathBuf::from);
            install::install(assets.as_deref(), &dest_flag(rest)).map(|_| {
                println!("\nInstalled. Run `agent-pager setup` to also configure your agents.");
            })
        }
        "setup" => {
            let assets = flag(rest, "--assets").map(PathBuf::from);
            setup::setup(assets.as_deref(), &dest_flag(rest), &flags(rest, "--claude-dir"))
        }
        "teardown" => setup::teardown(&dest_flag(rest), &flags(rest, "--claude-dir")),
        "-V" | "--version" | "version" => {
            println!("agent-pager {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "uninstall" => install::uninstall(&dest_flag(rest)),
        "print-config" => agent_arg(rest).map(|a| config::print(a, &dest_flag(rest))),
        "test" => agent_arg(rest).and_then(|a| test(a, rest.get(1).map(String::as_str))),
        "authorize" => authorize(),
        "post" => {
            post(rest.first());
            return ExitCode::SUCCESS;
        }
        "help" | "-h" | "--help" => {
            print!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown command {other:?}\n\n{USAGE}")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("agent-pager: {e}");
            ExitCode::FAILURE
        }
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn flags(args: &[String], name: &str) -> Vec<PathBuf> {
    args.windows(2).filter(|w| w[0] == name).map(|w| PathBuf::from(&w[1])).collect()
}

fn dest_flag(args: &[String]) -> PathBuf {
    flag(args, "--dest").map(PathBuf::from).unwrap_or_else(install::default_dest)
}

fn agent_arg(args: &[String]) -> Result<Agent, String> {
    args.first()
        .and_then(|a| Agent::parse(a))
        .ok_or_else(|| "expected an agent: claude, codex, opencode or antigravity".to_string())
}

/// Runs inside a pager app (launched by `install` via LaunchServices).
/// Output goes to the log because LaunchServices discards stdout.
fn authorize() -> Result<(), String> {
    let agent = notify::current_bundle_agent()
        .ok_or("authorize must run from inside a pager app; use `install`")?;
    let result = notify::authorize(Duration::from_secs(120));
    match &result {
        Ok(true) => log::info(&format!("notifications allowed for {}", agent.display_name())),
        Ok(false) => log::error(&format!("notifications denied for {}", agent.display_name())),
        Err(e) => log::error(&format!("authorize {}: {e}", agent.display_name())),
    }
    result.map(|_| ())
}

/// Runs inside a pager app: post the already-normalized event.
fn post(arg: Option<&String>) {
    let Some(agent) = notify::current_bundle_agent() else {
        log::error("post must run from inside a pager app");
        return;
    };
    let Some((ev, focus)) = arg.and_then(|a| event::Event::from_json(a)) else {
        log::error(&format!("post {}: bad event argument {arg:?}", agent.key()));
        return;
    };
    if let Err(e) = notify::post(&ev, focus) {
        log::error(&format!("post {}: {e}", agent.key()));
    }
}

fn hook(agent: Agent) {
    let mut raw = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut raw) {
        log::error(&format!("hook {}: reading stdin: {e}", agent.key()));
        if agent == Agent::Antigravity {
            println!(r#"{{"decision":"allow"}}"#);
        }
        return;
    }
    if let Err(e) = deliver(agent, &raw) {
        log::error(&format!("hook {}: {e}", agent.key()));
    }
    if agent == Agent::Antigravity {
        println!("{}", antigravity_hook_response(&raw));
    }
}

/// Antigravity requires hook commands to write a JSON decision object to stdout.
fn antigravity_hook_response(raw: &str) -> &'static str {
    let payload = serde_json::from_str::<serde_json::Value>(raw).ok();
    let event = payload
        .as_ref()
        .and_then(|v| v.get("hookEventName").and_then(|e| e.as_str()));
    match event {
        Some("PreToolUse") => r#"{"decision":"allow"}"#,
        Some("Stop") => r#"{"decision":"stop"}"#,
        Some("PostToolUse" | "SessionStart" | "PreInvocation" | "PostInvocation") => "{}",
        _ => {
            if payload.as_ref().is_some_and(|v| v.get("toolCall").is_some()) {
                r#"{"decision":"allow"}"#
            } else if payload
                .as_ref()
                .is_some_and(|v| v.get("terminationReason").is_some() || v.get("executionNum").is_some())
            {
                r#"{"decision":"stop"}"#
            } else {
                r#"{"decision":"allow"}"#
            }
        }
    }
}

/// Normalize a raw hook payload and hand it to the agent's pager app.
///
/// The app is started through LaunchServices (`open`) rather than run as our
/// child: a child process is attributed to the terminal that ran the agent,
/// and macOS refuses its notification requests.
fn deliver(agent: Agent, raw: &str) -> Result<(), String> {
    log::debug(&format!("hook {} payload: {raw}", agent.key()));
    let payload: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("invalid JSON: {e}"))?;
    let Some(ev) = event::from_payload(agent, &payload) else {
        log::debug(&format!("hook {}: event ignored", agent.key()));
        return Ok(());
    };
    let session = ["session_id", "session", "conversationId"]
        .iter()
        .find_map(|k| payload.get(*k).and_then(|v| v.as_str()))
        .unwrap_or_default();
    if is_duplicate(&format!("{}|{session}|{}|{}", agent.key(), ev.kind.key(), ev.body)) {
        log::debug(&format!("hook {}: duplicate suppressed", agent.key()));
        return Ok(());
    }
    // Detect here: the hook inherits the agent's environment, the app won't.
    let focus = notify::detect_focus_bundle();
    let app = install::app_path(&install::default_dest(), agent);
    launch_app(&app, &["post", &ev.to_json(focus.as_deref())], false)
}

/// True if an identical notification was sent in the last few seconds.
/// Uses an atomically-created marker file per event, so concurrent hook
/// processes racing on the same event let exactly one through.
fn is_duplicate(key: &str) -> bool {
    use std::hash::{Hash, Hasher};
    const WINDOW: Duration = Duration::from_secs(3);

    let mut h = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut h);
    let dir = std::env::temp_dir().join("agent-pager-dedupe");
    let _ = std::fs::create_dir_all(&dir);
    let marker = dir.join(format!("{:016x}", h.finish()));

    let fresh = |p: &std::path::Path| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age < WINDOW)
    };
    if std::fs::OpenOptions::new().write(true).create_new(true).open(&marker).is_ok() {
        return false;
    }
    if fresh(&marker) {
        return true;
    }
    // Stale marker from an earlier, identical event: claim it again.
    let _ = std::fs::remove_file(&marker);
    std::fs::OpenOptions::new().write(true).create_new(true).open(&marker).is_err()
}

/// `open -n -g <app> --args ...`: new instance, don't steal focus.
pub fn launch_app(app: &std::path::Path, args: &[&str], wait: bool) -> Result<(), String> {
    if !app.exists() {
        return Err(format!("{} not found; run `agent-pager install`", app.display()));
    }
    let mut cmd = Command::new("/usr/bin/open");
    cmd.args(["-n", "-g"]);
    if wait {
        cmd.arg("-W");
    }
    cmd.arg(app).arg("--args").args(args);
    let out = cmd.output().map_err(|e| format!("open: {e}"))?;
    if !out.status.success() {
        return Err(format!("open failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}

/// Build a sample payload in the agent's native hook format and send it
/// through the same path a real hook would take.
fn test(agent: Agent, kind: Option<&str>) -> Result<(), String> {
    use serde_json::json;
    let cwd = std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let kind = kind.unwrap_or("finished");
    let payload = match (agent, kind) {
        (Agent::Claude, "approval") => json!({"hook_event_name":"Notification","notification_type":"permission_prompt","message":"Claude needs your permission to use Bash","cwd":cwd}),
        (Agent::Claude, "input") => json!({"hook_event_name":"Notification","notification_type":"agent_needs_input","message":"Claude has a question for you","cwd":cwd}),
        (Agent::Claude, "error") => json!({"hook_event_name":"StopFailure","error":"API rate limit reached","cwd":cwd}),
        (Agent::Codex, "approval") => json!({"hook_event_name":"PermissionRequest","tool_name":"shell","tool_input":{"command":["git","push"]},"cwd":cwd}),
        (Agent::Codex, "input" | "error") => {
            return Err(format!("codex has no hook for `{kind}`; try approval or finished"))
        }
        (Agent::Claude | Agent::Codex, "finished") => json!({"hook_event_name":"Stop","last_assistant_message":"All tests pass. I refactored the parser and added three regression tests.","cwd":cwd}),
        (Agent::OpenCode, "approval") => json!({"hook_event_name":"permission","message":"bash: git push","cwd":cwd}),
        (Agent::OpenCode, "input") => json!({"hook_event_name":"input","message":"Which database should I use?","cwd":cwd}),
        (Agent::OpenCode, "error") => json!({"hook_event_name":"error","message":"Provider returned 529 overloaded","cwd":cwd}),
        (Agent::OpenCode, "finished") => json!({"hook_event_name":"finished","cwd":cwd}),
        (Agent::Antigravity, "approval") => json!({"hookEventName":"PreToolUse","toolCall":{"name":"ask_permission","args":{"Action":"command","Target":"git push","Reason":"Antigravity needs permission to run git push"}},"cwd":cwd,"workspacePaths":[cwd]}),
        (Agent::Antigravity, "input") => json!({"hookEventName":"PreToolUse","toolCall":{"name":"ask_question","args":{"questions":[{"question":"Which database should I use?","options":["Postgres","SQLite"]}]}},"cwd":cwd,"workspacePaths":[cwd]}),
        (Agent::Antigravity, "error") => json!({"hookEventName":"Stop","terminationReason":"ERROR","error":"Model request failed","fullyIdle":true,"cwd":cwd,"workspacePaths":[cwd]}),
        (Agent::Antigravity, "finished") => json!({"hookEventName":"Stop","terminationReason":"NO_TOOL_CALL","fullyIdle":true,"last_assistant_message":"All tests pass. I refactored the parser and added three regression tests.","cwd":cwd,"workspacePaths":[cwd]}),
        _ => return Err(format!("unknown kind {kind:?}; use approval, input, finished or error")),
    };
    deliver(agent, &payload.to_string())?;
    println!("sent {kind} notification for {} (check ~/Library/Logs/agent-pager.log if nothing appears)", agent.display_name());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{antigravity_hook_response, is_duplicate};

    #[test]
    fn duplicate_within_window_is_suppressed() {
        let key = format!("test|{}|{:?}", std::process::id(), std::time::SystemTime::now());
        assert!(!is_duplicate(&key));
        assert!(is_duplicate(&key));
        assert!(!is_duplicate(&format!("{key}-other")));
    }

    #[test]
    fn antigravity_responses_are_valid_passthrough_json() {
        assert_eq!(
            antigravity_hook_response(r#"{"hookEventName":"PreToolUse","toolCall":{"name":"ask_question"}}"#),
            r#"{"decision":"allow"}"#
        );
        assert_eq!(
            antigravity_hook_response(r#"{"hookEventName":"Stop","terminationReason":"NO_TOOL_CALL"}"#),
            r#"{"decision":"stop"}"#
        );
        assert_eq!(
            antigravity_hook_response(r#"{"hookEventName":"PostToolUse"}"#),
            "{}"
        );
    }
}
