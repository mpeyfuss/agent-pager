//! Builds one tiny `.app` bundle per agent. Each bundle has its own bundle ID
//! and icon but runs a copy of this same binary, so macOS shows each agent
//! with its own icon and its own row in System Settings → Notifications.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::event::{Agent, ALL_AGENTS, BUNDLE_ID_PREFIX};

pub const EXE_NAME: &str = "agent-pager";
const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";

pub fn default_dest() -> PathBuf {
    crate::home().join("Applications/AgentPager")
}

/// Icons compiled into the binary, so `install` works without the repo.
fn embedded_icon(agent: Agent) -> &'static [u8] {
    match agent {
        Agent::Claude => include_bytes!("../assets/claude.png"),
        Agent::Codex => include_bytes!("../assets/codex.png"),
        Agent::OpenCode => include_bytes!("../assets/opencode.png"),
        Agent::Antigravity => include_bytes!("../assets/antigravity.png"),
    }
}

pub fn app_path(dest: &Path, agent: Agent) -> PathBuf {
    dest.join(agent.app_dir_name())
}

pub fn exe_path(dest: &Path, agent: Agent) -> PathBuf {
    app_path(dest, agent).join("Contents/MacOS").join(EXE_NAME)
}

pub fn install(assets: Option<&Path>, dest: &Path) -> Result<(), String> {
    let self_exe = std::env::current_exe().map_err(|e| format!("can't locate own binary: {e}"))?;
    fs::create_dir_all(dest).map_err(|e| format!("create {}: {e}", dest.display()))?;

    for agent in ALL_AGENTS {
        let app = app_path(dest, agent);
        println!("• {} → {}", agent.display_name(), app.display());
        remove_own_bundle(&app)?;

        let macos = app.join("Contents/MacOS");
        let resources = app.join("Contents/Resources");
        fs::create_dir_all(&macos).map_err(|e| e.to_string())?;
        fs::create_dir_all(&resources).map_err(|e| e.to_string())?;

        fs::copy(&self_exe, macos.join(EXE_NAME)).map_err(|e| format!("copy binary: {e}"))?;

        build_icon(assets, agent, &resources.join("AppIcon.icns"))?;
        fs::write(app.join("Contents/Info.plist"), info_plist(agent))
            .map_err(|e| format!("write Info.plist: {e}"))?;

        run(Command::new("/usr/bin/codesign").args(["--force", "--sign", "-"]).arg(&app))?;
        run(Command::new(LSREGISTER).arg("-f").arg(&app))?;

        // Trigger the one-time "allow notifications" prompt now rather than
        // in the middle of an agent session.
        // It has to be launched via LaunchServices; see main::deliver.
        println!("  requesting notification permission (click Allow)…");
        if let Err(e) = crate::launch_app(&app, &["authorize"], true) {
            println!("  ! {e}");
        }
    }
    Ok(())
}

pub fn uninstall(dest: &Path) -> Result<(), String> {
    for agent in ALL_AGENTS {
        let app = app_path(dest, agent);
        if app.exists() {
            let _ = Command::new(LSREGISTER).arg("-u").arg(&app).status();
            remove_own_bundle(&app)?;
            println!("removed {}", app.display());
        }
    }
    // Only removes the directory if it's now empty.
    let _ = fs::remove_dir(dest);
    Ok(())
}

/// Delete `app` only if it's a bundle we generated.
fn remove_own_bundle(app: &Path) -> Result<(), String> {
    if !app.exists() {
        return Ok(());
    }
    let plist = fs::read_to_string(app.join("Contents/Info.plist")).unwrap_or_default();
    if !plist.contains(BUNDLE_ID_PREFIX) {
        return Err(format!(
            "{} exists but wasn't created by agent-pager; refusing to overwrite it",
            app.display()
        ));
    }
    fs::remove_dir_all(app).map_err(|e| format!("remove {}: {e}", app.display()))
}

/// Produce AppIcon.icns using the built-in `sips` and `iconutil`. An override
/// directory may supply `<agent>.icns` or `<agent>.png`; otherwise the
/// embedded PNG is used.
fn build_icon(assets: Option<&Path>, agent: Agent, out: &Path) -> Result<(), String> {
    let work = std::env::temp_dir().join(format!("agent-pager-{}-{}", agent.key(), std::process::id()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let result = build_icon_in(assets, agent, out, &work);
    let _ = fs::remove_dir_all(&work);
    result
}

fn build_icon_in(assets: Option<&Path>, agent: Agent, out: &Path, work: &Path) -> Result<(), String> {
    let mut png = None;
    if let Some(dir) = assets {
        let icns = dir.join(format!("{}.icns", agent.key()));
        if icns.is_file() {
            return fs::copy(&icns, out).map(|_| ()).map_err(|e| format!("copy icon: {e}"));
        }
        let candidate = dir.join(format!("{}.png", agent.key()));
        if candidate.is_file() {
            png = Some(candidate);
        } else {
            println!("  ! no {}.png/.icns in {}; using built-in icon", agent.key(), dir.display());
        }
    }
    let png = match png {
        Some(p) => p,
        None => {
            let p = work.join("source.png");
            fs::write(&p, embedded_icon(agent)).map_err(|e| e.to_string())?;
            p
        }
    };

    let iconset = work.join("AppIcon.iconset");
    fs::create_dir_all(&iconset).map_err(|e| e.to_string())?;
    for base in [16u32, 32, 128, 256, 512] {
        for scale in [1u32, 2] {
            let px = (base * scale).to_string();
            let name = if scale == 1 {
                format!("icon_{base}x{base}.png")
            } else {
                format!("icon_{base}x{base}@2x.png")
            };
            run(Command::new("/usr/bin/sips")
                .args(["-s", "format", "png", "-z", &px, &px])
                .arg(&png)
                .arg("--out")
                .arg(iconset.join(name)))?;
        }
    }
    run(Command::new("/usr/bin/iconutil").args(["-c", "icns"]).arg(&iconset).arg("-o").arg(out))
}

fn info_plist(agent: Agent) -> String {
    let version = env!("CARGO_PKG_VERSION");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>{id}</string>
	<key>CFBundleName</key>
	<string>{name}</string>
	<key>CFBundleDisplayName</key>
	<string>{name}</string>
	<key>CFBundleExecutable</key>
	<string>{EXE_NAME}</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>{version}</string>
	<key>CFBundleVersion</key>
	<string>{version}</string>
	<key>CFBundleIconFile</key>
	<string>AppIcon</string>
	<key>LSUIElement</key>
	<true/>
	<key>LSMinimumSystemVersion</key>
	<string>11.0</string>
	<key>NSHighResolutionCapable</key>
	<true/>
</dict>
</plist>
"#,
        id = agent.bundle_id(),
        name = agent.display_name(),
    )
}

fn run(cmd: &mut Command) -> Result<(), String> {
    let out = cmd.output().map_err(|e| format!("{:?}: {e}", cmd.get_program()))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{:?} failed: {}",
            cmd.get_program(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}
