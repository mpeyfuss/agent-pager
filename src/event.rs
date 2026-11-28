//! Agent identities and normalization of each agent's hook payload into a
//! common `Event`.

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
    OpenCode,
    Antigravity,
}

pub const ALL_AGENTS: [Agent; 4] = [Agent::Claude, Agent::Codex, Agent::OpenCode, Agent::Antigravity];

impl Agent {
    pub fn parse(s: &str) -> Option<Agent> {
        match s.to_ascii_lowercase().as_str() {
            "claude" | "claude-code" => Some(Agent::Claude),
            "codex" => Some(Agent::Codex),
            "opencode" => Some(Agent::OpenCode),
            "antigravity" | "agy" => Some(Agent::Antigravity),
            _ => None,
        }
    }

    /// Short key used in CLI args, bundle IDs and asset file names.
    pub fn key(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::OpenCode => "opencode",
            Agent::Antigravity => "antigravity",
        }
    }

    /// Name macOS shows in the notification header and System Settings.
    pub fn display_name(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Codex => "Codex",
            Agent::OpenCode => "OpenCode",
            Agent::Antigravity => "Antigravity",
        }
    }

    /// Bundle directory name (no spaces, so hook commands need no quoting).
    pub fn app_dir_name(self) -> &'static str {
        match self {
            Agent::Claude => "ClaudePager.app",
            Agent::Codex => "CodexPager.app",
            Agent::OpenCode => "OpenCodePager.app",
            Agent::Antigravity => "AntigravityPager.app",
        }
    }

    pub fn bundle_id(self) -> String {
        format!("{}{}", BUNDLE_ID_PREFIX, self.key())
    }

    pub fn from_bundle_id(id: &str) -> Option<Agent> {
        id.strip_prefix(BUNDLE_ID_PREFIX).and_then(Agent::parse)
    }
}

pub const BUNDLE_ID_PREFIX: &str = "local.agent-pager.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Approval,
    Input,
    Finished,
    Error,
}

impl Kind {
    pub fn key(self) -> &'static str {
        match self {
            Kind::Approval => "approval",
            Kind::Input => "input",
            Kind::Finished => "finished",
            Kind::Error => "error",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        [Kind::Approval, Kind::Input, Kind::Finished, Kind::Error]
            .into_iter()
            .find(|k| k.key() == s)
    }
}

#[derive(Debug)]
pub struct Event {
    pub kind: Kind,
    pub body: String,
    pub cwd: Option<String>,
}

impl Event {
    /// Wire format for handing a normalized event to the pager app.
    pub fn to_json(&self, focus_bundle: Option<&str>) -> String {
        serde_json::json!({
            "kind": self.kind.key(),
            "body": self.body,
            "cwd": self.cwd,
            "focus_bundle": focus_bundle,
        })
        .to_string()
    }

    /// Inverse of `to_json`; returns the event and the focus bundle.
    pub fn from_json(s: &str) -> Option<(Event, Option<String>)> {
        let v: Value = serde_json::from_str(s).ok()?;
        let ev = Event {
            kind: Kind::parse(v.get("kind")?.as_str()?)?,
            body: str_at(&v, "body").unwrap_or_default(),
            cwd: str_at(&v, "cwd"),
        };
        Some((ev, str_at(&v, "focus_bundle")))
    }
}

const MAX_BODY_CHARS: usize = 90;

/// Map a raw hook payload to an `Event`. Returns `None` for events we
/// deliberately ignore (e.g. Claude's idle reminder, which `Stop` covers).
pub fn from_payload(agent: Agent, v: &Value) -> Option<Event> {
    let raw_name = str_at(v, "hook_event_name")
        .or_else(|| str_at(v, "hookEventName"))
        .unwrap_or_default();
    let name = if !raw_name.is_empty() {
        raw_name.as_str()
    } else if agent == Agent::Antigravity && v.get("toolCall").is_some() {
        "PreToolUse"
    } else if agent == Agent::Antigravity
        && (v.get("terminationReason").is_some() || v.get("executionNum").is_some())
    {
        "Stop"
    } else {
        ""
    };
    let cwd = str_at(v, "cwd")
        .filter(|s| !s.is_empty())
        .or_else(|| first_workspace_path(v));

    let (kind, body) = match (agent, name) {
        (Agent::Claude, "Notification") => {
            let kind = match str_at(v, "notification_type").as_deref() {
                Some("permission_prompt") => Kind::Approval,
                Some("agent_needs_input" | "elicitation_dialog" | "elicitation_url_dialog") => {
                    Kind::Input
                }
                _ => return None,
            };
            (kind, str_at(v, "message"))
        }
        (Agent::Claude | Agent::Codex, "Stop") => {
            (Kind::Finished, str_at(v, "last_assistant_message"))
        }
        (Agent::Claude, "StopFailure") => (Kind::Error, error_text(v)),
        (Agent::Codex, "PermissionRequest") => (Kind::Approval, codex_permission_text(v)),

        // The OpenCode plugin (opencode/agent-pager.ts) pre-normalizes
        // its events into this small shape.
        (Agent::OpenCode, "finished") => (Kind::Finished, str_at(v, "message")),
        (Agent::OpenCode, "error") => (Kind::Error, str_at(v, "message")),
        (Agent::OpenCode, "permission") => (Kind::Approval, str_at(v, "message")),
        (Agent::OpenCode, "input") => (Kind::Input, str_at(v, "message")),

        (Agent::Antigravity, "Stop") => {
            let reason = str_at(v, "terminationReason").unwrap_or_default();
            let reason_upper = reason.to_ascii_uppercase();
            if reason_upper == "USER_CANCELED" {
                return None;
            }
            let err = str_at(v, "error").filter(|s| !s.trim().is_empty());
            if err.is_some() || reason_upper == "ERROR" {
                (Kind::Error, err)
            } else {
                // Background commands or subagents are still running; wait for the final Stop.
                if v.get("fullyIdle").and_then(Value::as_bool) == Some(false) {
                    return None;
                }
                let msg = str_at(v, "last_assistant_message").or_else(|| {
                    str_at(v, "transcriptPath").and_then(|p| last_transcript_message(&p))
                });
                (Kind::Finished, msg)
            }
        }
        (Agent::Antigravity, "PreToolUse") => {
            let tool_call = v.get("toolCall")?;
            match str_at(tool_call, "name").as_deref() {
                Some("ask_question") => (Kind::Input, agy_question_text(tool_call)),
                Some("ask_permission") => (Kind::Approval, agy_permission_text(tool_call)),
                _ => return None,
            }
        }
        _ => return None,
    };

    let body = body
        .map(|b| clean(&b))
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| default_body(kind).to_string());

    Some(Event { kind, body, cwd })
}

fn default_body(kind: Kind) -> &'static str {
    match kind {
        Kind::Approval => "Waiting for your approval",
        Kind::Input => "Waiting for your answer",
        Kind::Finished => "Finished responding",
        Kind::Error => "The turn ended with an error",
    }
}

fn str_at(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// Claude's StopFailure payload shape isn't fully documented; accept the
/// likely variants.
fn error_text(v: &Value) -> Option<String> {
    for key in ["error", "error_message", "message", "reason"] {
        match v.get(key) {
            Some(Value::String(s)) => return Some(s.clone()),
            Some(obj @ Value::Object(_)) => {
                if let Some(s) = str_at(obj, "message") {
                    return Some(s);
                }
            }
            _ => {}
        }
    }
    None
}

fn codex_permission_text(v: &Value) -> Option<String> {
    let tool = str_at(v, "tool_name");
    let input = v.get("tool_input");
    let detail = input.and_then(|i| {
        str_at(i, "description").or_else(|| match i.get("command") {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Array(parts)) => Some(
                parts
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            _ => None,
        })
    });
    match (tool, detail) {
        (Some(t), Some(d)) => Some(format!("{t}: {d}")),
        (None, Some(d)) => Some(d),
        (Some(t), None) => Some(format!("Wants to run {t}")),
        (None, None) => None,
    }
}

fn first_workspace_path(v: &Value) -> Option<String> {
    v.get("workspacePaths")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .find(|s| !s.is_empty())
        .map(str::to_owned)
}

fn agy_question_text(tool_call: &Value) -> Option<String> {
    tool_call
        .get("args")?
        .get("questions")?
        .as_array()?
        .first()?
        .get("question")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn agy_permission_text(tool_call: &Value) -> Option<String> {
    let args = tool_call.get("args")?;
    if let Some(reason) = str_at(args, "Reason")
        .or_else(|| str_at(args, "reason"))
        .filter(|s| !s.trim().is_empty())
    {
        return Some(reason);
    }
    let action = str_at(args, "Action")
        .or_else(|| str_at(args, "action"))
        .filter(|s| !s.trim().is_empty());
    let target = str_at(args, "Target")
        .or_else(|| str_at(args, "target"))
        .filter(|s| !s.trim().is_empty());
    match (action, target) {
        (Some(a), Some(t)) => Some(format!("{a}: {t}")),
        (None, Some(t)) => Some(t),
        (Some(a), None) => Some(format!("Wants permission for {a}")),
        (None, None) => None,
    }
}

/// Read the last assistant reply from an Antigravity `transcript.jsonl` file.
fn last_transcript_message(path: &str) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};

    let expanded = if let Some(rest) = path.strip_prefix("~/") {
        crate::home().join(rest)
    } else {
        std::path::PathBuf::from(path)
    };
    let mut f = std::fs::File::open(expanded).ok()?;
    const TAIL_BYTES: u64 = 128 * 1024;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    if start > 0 {
        f.seek(SeekFrom::Start(start)).ok()?;
    }
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.lines();
    if start > 0 {
        // The tail slice may start mid-line.
        lines.next();
    }
    for line in lines.rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if str_at(&entry, "type").as_deref() == Some("PLANNER_RESPONSE") {
            if let Some(content) = str_at(&entry, "content").filter(|s| !s.trim().is_empty()) {
                return Some(content);
            }
        }
    }
    None
}

/// Replace markdown links `[label](target)` with `label`.
fn strip_markdown_links(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open_b) = rest.find('[') {
        out.push_str(&rest[..open_b]);
        let after_open = &rest[open_b + 1..];
        if let Some(close_b) = after_open.find("](") {
            let label = &after_open[..close_b];
            let after_paren = &after_open[close_b + 2..];
            if !label.contains('\n') {
                if let Some(close_p) = after_paren.find(')') {
                    out.push_str(label);
                    rest = &after_paren[close_p + 1..];
                    continue;
                }
            }
        }
        out.push('[');
        rest = after_open;
    }
    out.push_str(rest);
    out
}

/// Reduce a message to a one-liner: first non-empty line, markdown markers
/// stripped, first sentence only, truncated.
fn clean(s: &str) -> String {
    let lines: Vec<&str> = s.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    // Skip markdown headings like "## Summary" unless that's all there is.
    let line = lines
        .iter()
        .find(|l| !l.starts_with('#'))
        .or(lines.first())
        .map(|l| l.trim_start_matches(['#', '-', '*', '>', ' ']))
        .unwrap_or_default();
    let line = strip_markdown_links(line);
    let line: String = line.chars().filter(|c| !matches!(c, '`' | '*')).collect();
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    let line = match line.find(". ") {
        Some(i) => &line[..=i],
        None => &line,
    };
    if line.chars().count() <= MAX_BODY_CHARS {
        return line.to_string();
    }
    let mut out: String = line.chars().take(MAX_BODY_CHARS - 1).collect();
    out = out.trim_end().to_string();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claude_permission_prompt() {
        let ev = from_payload(
            Agent::Claude,
            &json!({"hook_event_name":"Notification","notification_type":"permission_prompt",
                    "message":"Claude needs your permission to use Bash","cwd":"/tmp/x"}),
        )
        .unwrap();
        assert_eq!(ev.kind, Kind::Approval);
        assert_eq!(ev.cwd.as_deref(), Some("/tmp/x"));
    }

    #[test]
    fn claude_idle_prompt_is_ignored() {
        assert!(from_payload(
            Agent::Claude,
            &json!({"hook_event_name":"Notification","notification_type":"idle_prompt"})
        )
        .is_none());
    }

    #[test]
    fn stop_truncates_long_message() {
        let long = "word ".repeat(100);
        let ev = from_payload(
            Agent::Codex,
            &json!({"hook_event_name":"Stop","last_assistant_message": long}),
        )
        .unwrap();
        assert_eq!(ev.kind, Kind::Finished);
        assert!(ev.body.chars().count() <= MAX_BODY_CHARS);
        assert!(ev.body.ends_with('…'));
    }

    #[test]
    fn one_liner_takes_first_sentence_without_markdown() {
        assert_eq!(
            clean("\n## Summary\nI **refactored** the `parser`. Then I added tests.\n- more"),
            "I refactored the parser."
        );
        assert_eq!(
            clean("I **refactored** the `parser`. Then I added tests."),
            "I refactored the parser."
        );
    }

    #[test]
    fn codex_permission_uses_command_array() {
        let ev = from_payload(
            Agent::Codex,
            &json!({"hook_event_name":"PermissionRequest","tool_name":"shell",
                    "tool_input":{"command":["git","push"]}}),
        )
        .unwrap();
        assert_eq!(ev.body, "shell: git push");
    }

    #[test]
    fn claude_stop_failure_object_error() {
        let ev = from_payload(
            Agent::Claude,
            &json!({"hook_event_name":"StopFailure","error":{"message":"rate limited"}}),
        )
        .unwrap();
        assert_eq!(ev.kind, Kind::Error);
        assert_eq!(ev.body, "rate limited");
    }

    #[test]
    fn wire_roundtrip() {
        let ev = Event { kind: Kind::Input, body: "hi \"there\"".into(), cwd: Some("/a b".into()) };
        let (back, focus) = Event::from_json(&ev.to_json(Some("com.microsoft.VSCode"))).unwrap();
        assert_eq!(back.kind, Kind::Input);
        assert_eq!(back.body, ev.body);
        assert_eq!(back.cwd, ev.cwd);
        assert_eq!(focus.as_deref(), Some("com.microsoft.VSCode"));
    }

    #[test]
    fn empty_body_gets_default() {
        let ev = from_payload(Agent::OpenCode, &json!({"hook_event_name":"finished"})).unwrap();
        assert_eq!(ev.body, "Finished responding");
    }

    #[test]
    fn one_liner_strips_markdown_links() {
        assert_eq!(
            clean("Updated [src/main.rs](file:///tmp/src/main.rs#L10-L20) and tests."),
            "Updated src/main.rs and tests."
        );
    }

    #[test]
    fn antigravity_stop_reads_transcript_and_workspace_path() {
        let p = std::env::temp_dir().join(format!("agent-pager-agy-transcript-{}.jsonl", std::process::id()));
        let transcript = concat!(
            "{\"type\":\"USER_INPUT\",\"content\":\"Fix it\"}\n",
            "{\"type\":\"PLANNER_RESPONSE\",\"content\":\"All **tests** pass in [main.rs](file:///a/main.rs). More details.\"}\n",
            "{\"type\":\"GENERIC\",\"content\":\"ignored\"}\n",
        );
        std::fs::write(&p, transcript).unwrap();

        let ev = from_payload(
            Agent::Antigravity,
            &json!({
                "hookEventName": "Stop",
                "terminationReason": "NO_TOOL_CALL",
                "fullyIdle": true,
                "workspacePaths": ["/repo/project"],
                "transcriptPath": p.to_string_lossy(),
            }),
        )
        .unwrap();
        std::fs::remove_file(&p).unwrap();

        assert_eq!(ev.kind, Kind::Finished);
        assert_eq!(ev.body, "All tests pass in main.rs.");
        assert_eq!(ev.cwd.as_deref(), Some("/repo/project"));
    }

    #[test]
    fn antigravity_stop_error_and_ignores() {
        let err = from_payload(
            Agent::Antigravity,
            &json!({
                "hookEventName": "Stop",
                "terminationReason": "ERROR",
                "error": "Quota exceeded",
                "cwd": "/tmp/x",
            }),
        )
        .unwrap();
        assert_eq!(err.kind, Kind::Error);
        assert_eq!(err.body, "Quota exceeded");

        assert!(from_payload(
            Agent::Antigravity,
            &json!({"hookEventName": "Stop", "terminationReason": "USER_CANCELED", "fullyIdle": true})
        )
        .is_none());

        assert!(from_payload(
            Agent::Antigravity,
            &json!({"hookEventName": "Stop", "terminationReason": "NO_TOOL_CALL", "fullyIdle": false})
        )
        .is_none());
    }

    #[test]
    fn antigravity_pre_tool_use_question_and_permission() {
        let q = from_payload(
            Agent::Antigravity,
            &json!({
                "hookEventName": "PreToolUse",
                "toolCall": {
                    "name": "ask_question",
                    "args": { "questions": [{ "question": "Which database should I use?" }] }
                },
                "cwd": "/tmp/x"
            }),
        )
        .unwrap();
        assert_eq!(q.kind, Kind::Input);
        assert_eq!(q.body, "Which database should I use?");

        let perm = from_payload(
            Agent::Antigravity,
            &json!({
                "hookEventName": "PreToolUse",
                "toolCall": {
                    "name": "ask_permission",
                    "args": { "Action": "command", "Target": "git push" }
                },
                "cwd": "/tmp/x"
            }),
        )
        .unwrap();
        assert_eq!(perm.kind, Kind::Approval);
        assert_eq!(perm.body, "command: git push");

        // Regular tools should not notify.
        assert!(from_payload(
            Agent::Antigravity,
            &json!({
                "hookEventName": "PreToolUse",
                "toolCall": { "name": "run_command", "args": { "CommandLine": "ls" } }
            })
        )
        .is_none());
    }
}
