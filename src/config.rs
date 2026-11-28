//! Prints the settings each agent needs. Nothing here edits your config
//! files; copy the snippet in yourself.

use std::path::Path;

use crate::event::Agent;
use crate::install::exe_path;

const OPENCODE_PLUGIN: &str = include_str!("../integrations/opencode/agent-pager.ts");

pub fn print(agent: Agent, dest: &Path) {
    let exe = exe_path(dest, agent);
    let exe = exe.to_string_lossy();
    match agent {
        Agent::Claude => print!(
            r#"# Merge into ~/.claude/settings.json under "hooks":
{{
  "hooks": {{
    "Notification": [
      {{
        "matcher": "permission_prompt|agent_needs_input|elicitation_dialog|elicitation_url_dialog",
        "hooks": [{{ "type": "command", "command": "{exe} hook claude", "async": true }}]
      }}
    ],
    "Stop": [
      {{ "hooks": [{{ "type": "command", "command": "{exe} hook claude", "async": true }}] }}
    ],
    "StopFailure": [
      {{ "hooks": [{{ "type": "command", "command": "{exe} hook claude", "async": true }}] }}
    ]
  }}
}}
"#
        ),
        Agent::Codex => {
            println!("# Append to the end of ~/.codex/config.toml, then run /hooks inside codex to trust them.");
            println!("# The hook never prints anything, so it can't approve or deny a request.");
            print!("{}", crate::setup::codex_block(dest));
            println!("\n# Optional: turn off Codex's own terminal notifications to avoid duplicates:\n# [tui]\n# notifications = false");
        }
        Agent::OpenCode => {
            println!("// Save as ~/.config/opencode/plugins/agent-pager.ts");
            println!("// Optional: set \"attention.notifications\": false in ~/.config/opencode/cli.json to avoid duplicates.");
            print!("{}", opencode_plugin(dest));
        }
        Agent::Antigravity => {
            let snippet = serde_json::json!({
                "agent-pager": crate::setup::antigravity_hook_entry(dest),
            });
            println!("# Merge into ~/.gemini/config/hooks.json:");
            println!("# Optional: turn off Antigravity's built-in notifications to avoid duplicates");
            println!("# (\"notifications\": false in ~/.gemini/antigravity-cli/settings.json, or Settings -> Notifications in the app).");
            println!("{}", serde_json::to_string_pretty(&snippet).expect("serializable"));
        }
    }
}

/// The OpenCode plugin source with the notifier path filled in.
pub fn opencode_plugin(dest: &Path) -> String {
    let exe = exe_path(dest, Agent::OpenCode);
    OPENCODE_PLUGIN.replace("__AGENT_PAGER_BIN__", &exe.to_string_lossy())
}
