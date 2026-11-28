//! `setup` / `teardown`: install the pager apps and wire every detected
//! agent to them. Re-running is safe: existing agent-pager entries are
//! detected and left alone. Every file is backed up before it's changed.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};

use crate::event::Agent;
use crate::install::{self, exe_path};

const CODEX_BEGIN: &str = "# >>> agent-pager >>>";
const CODEX_END: &str = "# <<< agent-pager <<<";
const OPENCODE_HEADER: &str = "// agent-pager: forwards OpenCode events";
const ANTIGRAVITY_HOOK_NAME: &str = "agent-pager";

pub fn setup(assets: Option<&Path>, dest: &Path, extra_claude_dirs: &[PathBuf]) -> Result<(), String> {
    install::install(assets, dest)?;
    println!();

    let mut configured = 0;
    for dir in claude_dirs(extra_claude_dirs) {
        report(&format!("Claude Code ({})", tilde(&dir)), setup_claude(&dir, dest));
        configured += 1;
    }
    if let Some(dir) = codex_dir() {
        report("Codex", setup_codex(&dir, dest));
        configured += 1;
    }
    if let Some(dir) = opencode_dir() {
        report("OpenCode", setup_opencode(&dir, dest));
        configured += 1;
    }
    if let Some(dir) = antigravity_dir() {
        report("Antigravity", setup_antigravity(&dir, dest));
        configured += 1;
    }
    if configured == 0 {
        println!("No agent configs found (~/.claude*, ~/.codex, ~/.config/opencode, ~/.gemini/config).");
    }

    println!("\nDone. Restart running agent sessions to pick up the hooks.");
    if codex_dir().is_some() {
        println!("Codex: run /hooks inside codex once and trust the agent-pager hooks.");
    }
    println!("Try it: agent-pager test claude approval");
    Ok(())
}

pub fn teardown(dest: &Path, extra_claude_dirs: &[PathBuf]) -> Result<(), String> {
    for dir in claude_dirs(extra_claude_dirs) {
        report(&format!("Claude Code ({})", tilde(&dir)), teardown_claude(&dir));
    }
    if let Some(dir) = codex_dir() {
        report("Codex", teardown_codex(&dir));
    }
    if let Some(dir) = opencode_dir() {
        report("OpenCode", teardown_opencode(&dir));
    }
    if let Some(dir) = antigravity_dir() {
        report("Antigravity", teardown_antigravity(&dir));
    }
    install::uninstall(dest)
}

fn report(name: &str, result: Result<String, String>) {
    match result {
        Ok(msg) => println!("• {name}: {msg}"),
        Err(e) => println!("• {name}: ! {e}"),
    }
}

// ---------- discovery ----------

/// `~/.claude` plus sibling configs like `~/.claudette`, `$CLAUDE_CONFIG_DIR`,
/// and any `--claude-dir` given explicitly.
fn claude_dirs(extra: &[PathBuf]) -> Vec<PathBuf> {
    let home = crate::home();
    let mut dirs: Vec<PathBuf> = fs::read_dir(&home)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(".claude"))
        .map(|e| e.path())
        .filter(|p| p.is_dir() && (p.join("settings.json").exists() || p.join("projects").is_dir()))
        .collect();
    if let Some(d) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        dirs.push(PathBuf::from(d));
    }
    dirs.extend(extra.iter().cloned());
    dirs.sort();
    dirs.dedup();
    dirs
}

fn codex_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::home().join(".codex"));
    dir.is_dir().then_some(dir)
}

fn opencode_dir() -> Option<PathBuf> {
    let dir = crate::home().join(".config/opencode");
    dir.is_dir().then_some(dir)
}

fn antigravity_dir() -> Option<PathBuf> {
    let gemini = crate::home().join(".gemini");
    let config = gemini.join("config");
    let detected = [
        config.clone(),
        gemini.join("antigravity"),
        gemini.join("antigravity-cli"),
        gemini.join("antigravity-ide"),
    ]
    .iter()
    .any(|d| d.is_dir());
    detected.then_some(config)
}

// ---------- Claude Code ----------

fn claude_groups(exe: &str) -> Vec<(&'static str, Value)> {
    let handler = json!([{ "type": "command", "command": format!("{exe} hook claude"), "async": true }]);
    vec![
        (
            "Notification",
            json!({
                "matcher": "permission_prompt|agent_needs_input|elicitation_dialog|elicitation_url_dialog",
                "hooks": handler,
            }),
        ),
        ("Stop", json!({ "hooks": handler })),
        ("StopFailure", json!({ "hooks": handler })),
    ]
}

fn is_ours(hook: &Value, agent: Agent) -> bool {
    hook.get("command")
        .and_then(Value::as_str)
        .is_some_and(|c| c.contains(&format!("agent-pager hook {}", agent.key())))
}

fn group_has_ours(group: &Value) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hs| hs.iter().any(|h| is_ours(h, Agent::Claude)))
}

fn setup_claude(dir: &Path, dest: &Path) -> Result<String, String> {
    let path = dir.join("settings.json");
    let mut root = read_json(&path)?;
    let exe = exe_path(dest, Agent::Claude).to_string_lossy().into_owned();

    let obj = root.as_object_mut().ok_or("settings.json is not a JSON object")?;
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or("\"hooks\" is not an object")?;

    let mut added = Vec::new();
    for (event, group) in claude_groups(&exe) {
        let list = hooks
            .entry(event)
            .or_insert_with(|| Value::Array(vec![]))
            .as_array_mut()
            .ok_or(format!("hooks.{event} is not an array"))?;
        if !list.iter().any(group_has_ours) {
            list.push(group);
            added.push(event);
        }
    }

    if added.is_empty() {
        return Ok("already configured".into());
    }
    write_with_backup(&path, &to_pretty(&root))?;
    Ok(format!("added {} hooks", added.join(", ")))
}

fn teardown_claude(dir: &Path) -> Result<String, String> {
    let path = dir.join("settings.json");
    if !path.exists() {
        return Ok("nothing to remove".into());
    }
    let mut root = read_json(&path)?;
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return Ok("nothing to remove".into());
    };

    let mut removed = 0;
    for list in hooks.values_mut().filter_map(Value::as_array_mut) {
        for group in list.iter_mut() {
            if let Some(hs) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                let before = hs.len();
                hs.retain(|h| !is_ours(h, Agent::Claude));
                removed += before - hs.len();
            }
        }
        list.retain(|g| g.get("hooks").and_then(Value::as_array).is_none_or(|hs| !hs.is_empty()));
    }
    hooks.retain(|_, v| v.as_array().is_none_or(|a| !a.is_empty()));
    if hooks.is_empty() {
        root.as_object_mut().map(|o| o.remove("hooks"));
    }

    if removed == 0 {
        return Ok("nothing to remove".into());
    }
    write_with_backup(&path, &to_pretty(&root))?;
    Ok(format!("removed {removed} hooks"))
}

// ---------- Codex ----------

pub fn codex_block(dest: &Path) -> String {
    let exe = exe_path(dest, Agent::Codex);
    let exe = exe.to_string_lossy();
    format!(
        r#"{CODEX_BEGIN}
# Managed by `agent-pager setup`; remove with `agent-pager teardown`.
[[hooks.Stop]]
[[hooks.Stop.hooks]]
type = "command"
command = "{exe} hook codex"
timeout = 10

[[hooks.PermissionRequest]]
[[hooks.PermissionRequest.hooks]]
type = "command"
command = "{exe} hook codex"
timeout = 10
{CODEX_END}
"#
    )
}

fn setup_codex(dir: &Path, dest: &Path) -> Result<String, String> {
    let path = dir.join("config.toml");
    let current = fs::read_to_string(&path).unwrap_or_default();
    let block = codex_block(dest);

    let updated = if let Some((start, end)) = codex_block_range(&current) {
        if current[start..end] == block {
            return Ok("already configured".into());
        }
        format!("{}{}{}", &current[..start], block, &current[end..])
    } else if current.contains("agent-pager hook codex") {
        return Ok("hooks already present (added by hand); leaving them alone".into());
    } else {
        // Appended at the end so these array-of-tables entries can't be
        // swallowed by a preceding [table] header.
        let sep = if current.is_empty() || current.ends_with("\n\n") {
            ""
        } else if current.ends_with('\n') {
            "\n"
        } else {
            "\n\n"
        };
        format!("{current}{sep}{block}")
    };
    write_with_backup(&path, &updated)?;
    Ok("added Stop, PermissionRequest hooks".into())
}

fn teardown_codex(dir: &Path) -> Result<String, String> {
    let path = dir.join("config.toml");
    let current = fs::read_to_string(&path).unwrap_or_default();
    let Some((start, end)) = codex_block_range(&current) else {
        return Ok("nothing to remove".into());
    };
    let updated = format!("{}{}", current[..start].trim_end_matches('\n'), &current[end..]);
    let updated = if updated.is_empty() { updated } else { format!("{}\n", updated.trim_end_matches('\n')) };
    write_with_backup(&path, &updated)?;
    Ok("removed hooks".into())
}

/// Byte range of the managed block, including its trailing newline.
fn codex_block_range(s: &str) -> Option<(usize, usize)> {
    let start = s.find(CODEX_BEGIN)?;
    let end_marker = start + s[start..].find(CODEX_END)?;
    let mut end = end_marker + CODEX_END.len();
    if s[end..].starts_with('\n') {
        end += 1;
    }
    Some((start, end))
}

// ---------- OpenCode ----------

fn setup_opencode(dir: &Path, dest: &Path) -> Result<String, String> {
    let path = dir.join("plugins/agent-pager.ts");
    let content = crate::config::opencode_plugin(dest);
    if let Ok(existing) = fs::read_to_string(&path) {
        if existing == content {
            return Ok("already configured".into());
        }
        if !existing.starts_with(OPENCODE_HEADER) {
            return Err(format!("{} exists and isn't ours; leaving it alone", tilde(&path)));
        }
    }
    fs::create_dir_all(path.parent().expect("has parent")).map_err(|e| e.to_string())?;
    write_atomic(&path, &content)?;
    Ok(format!("wrote {}", tilde(&path)))
}

fn teardown_opencode(dir: &Path) -> Result<String, String> {
    let path = dir.join("plugins/agent-pager.ts");
    match fs::read_to_string(&path) {
        Ok(s) if s.starts_with(OPENCODE_HEADER) => {
            fs::remove_file(&path).map_err(|e| e.to_string())?;
            Ok(format!("removed {}", tilde(&path)))
        }
        Ok(_) => Err(format!("{} isn't ours; leaving it alone", tilde(&path))),
        Err(_) => Ok("nothing to remove".into()),
    }
}

// ---------- Antigravity ----------

pub fn antigravity_hook_entry(dest: &Path) -> Value {
    let exe = exe_path(dest, Agent::Antigravity).to_string_lossy().into_owned();
    let cmd = format!("{exe} hook antigravity");
    json!({
        "PreToolUse": [
            {
                "matcher": "ask_question|ask_permission",
                "hooks": [
                    { "type": "command", "command": cmd, "timeout": 10 }
                ]
            }
        ],
        "Stop": [
            { "type": "command", "command": cmd, "timeout": 10 }
        ]
    })
}

fn setup_antigravity(dir: &Path, dest: &Path) -> Result<String, String> {
    let path = dir.join("hooks.json");
    let mut root = read_json(&path)?;
    let obj = root.as_object_mut().ok_or("hooks.json is not a JSON object")?;
    let entry = antigravity_hook_entry(dest);
    if obj.get(ANTIGRAVITY_HOOK_NAME) == Some(&entry) {
        return Ok("already configured".into());
    }
    let existed = obj.contains_key(ANTIGRAVITY_HOOK_NAME);
    obj.insert(ANTIGRAVITY_HOOK_NAME.to_string(), entry);
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    write_with_backup(&path, &to_pretty(&root))?;
    if existed {
        Ok("updated PreToolUse, Stop hooks".into())
    } else {
        Ok("added PreToolUse, Stop hooks".into())
    }
}

fn teardown_antigravity(dir: &Path) -> Result<String, String> {
    let path = dir.join("hooks.json");
    if !path.exists() {
        return Ok("nothing to remove".into());
    }
    let mut root = read_json(&path)?;
    let Some(obj) = root.as_object_mut() else {
        return Ok("nothing to remove".into());
    };
    if obj.remove(ANTIGRAVITY_HOOK_NAME).is_none() {
        return Ok("nothing to remove".into());
    }
    write_with_backup(&path, &to_pretty(&root))?;
    Ok("removed hooks".into())
}

// ---------- helpers ----------

fn read_json(path: &Path) -> Result<Value, String> {
    match fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => Ok(json!({})),
        Ok(s) => serde_json::from_str(&s).map_err(|e| format!("{}: invalid JSON ({e})", tilde(path))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(format!("read {}: {e}", tilde(path))),
    }
}

fn to_pretty(v: &Value) -> String {
    let mut s = serde_json::to_string_pretty(v).expect("serializable");
    s.push('\n');
    s
}

fn write_with_backup(path: &Path, content: &str) -> Result<(), String> {
    if path.exists() {
        let ts = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default();
        let name = format!("{}.bak-agent-pager-{ts}", path.file_name().unwrap_or_default().to_string_lossy());
        fs::copy(path, path.with_file_name(name)).map_err(|e| format!("backup {}: {e}", tilde(path)))?;
    }
    write_atomic(path, content)
}

/// Write via a temp file + rename so file watchers (OpenCode reloads its
/// plugins on change) never see a half-written file.
fn write_atomic(path: &Path, content: &str) -> Result<(), String> {
    let tmp = path.with_file_name(format!(
        ".{}.agent-pager-tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    fs::write(&tmp, content).map_err(|e| format!("write {}: {e}", tilde(&tmp)))?;
    // Keep the original's permissions (e.g. 0600 on Codex's config.toml).
    if let Ok(meta) = fs::metadata(path) {
        let _ = fs::set_permissions(&tmp, meta.permissions());
    }
    fs::rename(&tmp, path).map_err(|e| format!("write {}: {e}", tilde(path)))
}

fn tilde(p: &Path) -> String {
    let home = crate::home();
    match p.strip_prefix(&home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("agent-pager-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn claude_setup_is_idempotent_and_reversible() {
        let dir = tmpdir("claude");
        let dest = PathBuf::from("/Apps");
        let original = json!({
            "theme": "dark",
            "hooks": { "Stop": [{ "hooks": [{ "type": "command", "command": "say done" }] }] }
        });
        fs::write(dir.join("settings.json"), to_pretty(&original)).unwrap();

        assert!(setup_claude(&dir, &dest).unwrap().starts_with("added"));
        assert_eq!(setup_claude(&dir, &dest).unwrap(), "already configured");
        let v = read_json(&dir.join("settings.json")).unwrap();
        assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 2);
        assert_eq!(v["theme"], "dark");

        teardown_claude(&dir).unwrap();
        assert_eq!(read_json(&dir.join("settings.json")).unwrap(), original);
    }

    #[test]
    fn codex_setup_is_idempotent_and_reversible() {
        let dir = tmpdir("codex");
        let dest = PathBuf::from("/Apps");
        let original = "model = \"x\"\n\n[projects.\"/a\"]\ntrust_level = \"trusted\"\n";
        fs::write(dir.join("config.toml"), original).unwrap();

        setup_codex(&dir, &dest).unwrap();
        assert_eq!(setup_codex(&dir, &dest).unwrap(), "already configured");
        let s = fs::read_to_string(dir.join("config.toml")).unwrap();
        assert!(s.contains("hook codex") && s.ends_with(&format!("{CODEX_END}\n")));

        teardown_codex(&dir).unwrap();
        assert_eq!(fs::read_to_string(dir.join("config.toml")).unwrap(), original);
    }

    #[test]
    fn antigravity_setup_is_idempotent_and_reversible() {
        let dir = tmpdir("antigravity");
        let dest = PathBuf::from("/Apps");
        let original = json!({
            "my-linter": {
                "PostToolUse": [{ "matcher": "run_command", "hooks": [{ "type": "command", "command": "./lint.sh" }] }]
            }
        });
        fs::write(dir.join("hooks.json"), to_pretty(&original)).unwrap();

        assert_eq!(setup_antigravity(&dir, &dest).unwrap(), "added PreToolUse, Stop hooks");
        assert_eq!(setup_antigravity(&dir, &dest).unwrap(), "already configured");
        let v = read_json(&dir.join("hooks.json")).unwrap();
        assert!(v["agent-pager"]["Stop"].is_array());
        assert!(v["agent-pager"]["PreToolUse"].is_array());
        assert_eq!(v["my-linter"], original["my-linter"]);

        assert_eq!(teardown_antigravity(&dir).unwrap(), "removed hooks");
        assert_eq!(read_json(&dir.join("hooks.json")).unwrap(), original);
        assert_eq!(teardown_antigravity(&dir).unwrap(), "nothing to remove");
    }
}

#[cfg(test)]
mod perm_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn atomic_write_preserves_permissions() {
        let p = std::env::temp_dir().join(format!("agent-pager-perm-{}", std::process::id()));
        fs::write(&p, "a").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
        write_atomic(&p, "b").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "b");
        assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        fs::remove_file(p).unwrap();
    }
}
