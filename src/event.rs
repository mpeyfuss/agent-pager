//! Agent identities and normalization of each agent's hook payload into a
//! common `Event`.

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
    OpenCode,
}

pub const ALL_AGENTS: [Agent; 3] = [Agent::Claude, Agent::Codex, Agent::OpenCode];

impl Agent {
    pub fn parse(s: &str) -> Option<Agent> {
        match s.to_ascii_lowercase().as_str() {
            "claude" | "claude-code" => Some(Agent::Claude),
            "codex" => Some(Agent::Codex),
            "opencode" => Some(Agent::OpenCode),
            _ => None,
        }
    }

    /// Short key used in CLI args, bundle IDs and asset file names.
    pub fn key(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::OpenCode => "opencode",
        }
    }

    /// Name macOS shows in the notification header and System Settings.
    pub fn display_name(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Codex => "Codex",
            Agent::OpenCode => "OpenCode",
        }
    }

    /// Bundle directory name (no spaces, so hook commands need no quoting).
    pub fn app_dir_name(self) -> &'static str {
        match self {
            Agent::Claude => "ClaudePager.app",
            Agent::Codex => "CodexPager.app",
            Agent::OpenCode => "OpenCodePager.app",
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
    let name = str_at(v, "hook_event_name").unwrap_or_default();
    let cwd = str_at(v, "cwd");

    let (kind, body) = match (agent, name.as_str()) {
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
}
