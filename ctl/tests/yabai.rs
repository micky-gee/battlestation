//! The yabai backend end to end: the real binary under `BSCTL_WM=yabai`,
//! against a fake `yabai` CLI on PATH that serves canned `-m query` JSON and
//! logs every other invocation. Keystrokes are disabled
//! (`BSCTL_NO_KEYSTROKES`), so desktop switches take the window-focus
//! fallback — deterministic, and the developer's desktops stay put.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_bsctl");

struct TestEnv {
    root: PathBuf,
    run: PathBuf,
    fix: PathBuf,
    path: String,
}

impl TestEnv {
    /// `windows` may reference `{PID}` — this test process's pid, an
    /// ancestor of every hook the test spawns.
    fn new(name: &str, windows: Value) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let root = PathBuf::from("/tmp").join(format!(
            "bsctl-yabai-{}-{}-{}",
            std::process::id(),
            name,
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        let (run, fix, fakebin) = (root.join("run"), root.join("fix"), root.join("fakebin"));
        for d in [
            &run,
            &fix,
            &fakebin,
            &root.join("home"),
            &root.join("state"),
        ] {
            fs::create_dir_all(d).unwrap();
        }
        let windows = windows
            .to_string()
            .replace("\"{PID}\"", &std::process::id().to_string());
        // Display ids no real Mac uses, so CoreGraphics calls both external.
        // The second display sits left of the first: shared numbering puts
        // it first. Space 3 is a native fullscreen app: no desktop number.
        fs::write(
            fix.join("displays"),
            json!([
                {"id": 90001, "index": 1, "has-focus": true, "frame": {"x": 0.0, "y": 0.0, "w": 1512.0, "h": 982.0}},
                {"id": 90002, "index": 2, "has-focus": false, "frame": {"x": -1920.0, "y": 0.0, "w": 1920.0, "h": 1080.0}}
            ])
            .to_string(),
        )
        .unwrap();
        fs::write(
            fix.join("spaces"),
            json!([
                {"index": 1, "display": 1, "is-visible": true, "has-focus": true, "is-native-fullscreen": false},
                {"index": 2, "display": 1, "is-visible": false, "has-focus": false, "is-native-fullscreen": false},
                {"index": 3, "display": 1, "is-visible": false, "has-focus": false, "is-native-fullscreen": true},
                {"index": 4, "display": 2, "is-visible": true, "has-focus": false, "is-native-fullscreen": false}
            ])
            .to_string(),
        )
        .unwrap();
        fs::write(fix.join("windows"), windows).unwrap();
        fs::write(fix.join("focused"), json!({"id": 4242}).to_string()).unwrap();
        let stub = fakebin.join("yabai");
        fs::write(
            &stub,
            format!(
                r#"#!/bin/sh
f='{fix}'
case "$*" in
  "-m query --displays") cat "$f/displays" ;;
  "-m query --spaces") cat "$f/spaces" ;;
  "-m query --windows") cat "$f/windows" ;;
  "-m query --windows --window") cat "$f/focused" ;;
  *) printf '%s\n' "$*" >> "$f/actions.log" ;;
esac
"#,
                fix = fix.display()
            ),
        )
        .unwrap();
        let sec = fakebin.join("security");
        fs::write(&sec, "#!/bin/sh\nexit 44\n").unwrap();
        for p in [&stub, &sec] {
            let mut perm = fs::metadata(p).unwrap().permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
            fs::set_permissions(p, perm).unwrap();
        }
        let path = format!(
            "{}:{}",
            fakebin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        TestEnv {
            root,
            run,
            fix,
            path,
        }
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new(BIN);
        c.env("BSCTL_WM", "yabai")
            .env("BSCTL_NO_KEYSTROKES", "1")
            .env("XDG_RUNTIME_DIR", &self.run)
            .env("HOME", self.root.join("home"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("PATH", &self.path)
            .env_remove("KITTY_LISTEN_ON")
            .env_remove("KITTY_WINDOW_ID");
        c
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        self.cmd().args(args).output().unwrap()
    }

    fn status(&self) -> Value {
        let out = self.run(&["status", "--format", "json"]);
        assert_eq!(out.status.code(), Some(0));
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn actions(&self) -> Vec<String> {
        fs::read_to_string(self.fix.join("actions.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn windows() -> Value {
    json!([
        {"id": 4242, "pid": "{PID}", "app": "kitty", "space": 2},
        {"id": 77, "pid": 1, "app": "Safari", "space": 1},
        {"id": 88, "pid": 1, "app": "Keynote", "space": 3},
        {"id": 99, "pid": 1, "app": "Notes", "space": 4}
    ])
}

#[test]
fn status_world_is_built_from_desktops() {
    let env = TestEnv::new("world", windows());
    let w = env.status();
    let ids: Vec<i64> = w["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["ws"].as_i64().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec![1, 2, 3],
        "desktops 1-3; the fullscreen space has no number"
    );
    assert_eq!(w["workspaces"][1]["windows"], 1);
    assert_eq!(w["workspaces"][2]["display"], "External Display 1");
    assert_eq!(w["workspaces"][0]["active"], true);
    // leftmost first: the display at x=-1920 is display 1
    assert_eq!(w["displays"][0]["name"], "External Display 1");
    assert_eq!(w["displays"][0]["activeWs"], 3);
    assert_eq!(w["displays"][1]["focused"], true);
}

#[test]
fn ws_verbs_translate_to_yabai_commands() {
    let env = TestEnv::new("verbs", windows());
    // keystrokes disabled: switching focuses a window on that desktop
    assert_eq!(
        env.run(&["ws", "focus", "--bs-id", "3"]).status.code(),
        Some(0)
    );
    assert_eq!(
        env.run(&["ws", "send", "window", "--bs-id", "2", "--focus"])
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        env.run(&["ws", "focus", "--display-id", "2"]).status.code(),
        Some(0)
    );
    assert_eq!(
        env.actions(),
        vec![
            "-m window --focus 99",
            "-m window 4242 --space 2",
            "-m window --focus 4242",
            "-m display --focus 1",
        ]
    );
}

#[test]
fn switching_to_an_empty_desktop_without_keystrokes_explains_itself() {
    let env = TestEnv::new("empty", json!([]));
    let out = env.run(&["ws", "focus", "--bs-id", "2"]);
    assert_ne!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stderr).contains("Accessibility"));
    assert!(env.actions().is_empty());
}

#[test]
fn moving_a_desktop_between_displays_refuses_with_directions() {
    let env = TestEnv::new("movews", windows());
    let out = env.run(&["ws", "send", "workspace", "--display-id", "1"]);
    assert_ne!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stderr).contains("Mission Control"));
    assert!(env.actions().is_empty());
}

#[test]
fn hook_places_the_session_on_its_windows_desktop() {
    let env = TestEnv::new("hook", windows());
    let mut child = env
        .cmd()
        .args(["agents", "set", "--kind", "claude", "thinking"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json!({"session_id": "sess-y"}).to_string().as_bytes())
        .unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(0));
    let rec: Value =
        serde_json::from_slice(&fs::read(env.run.join("battlestation-ws/sess-y")).unwrap())
            .unwrap();
    assert_eq!(rec["ws"], 2);
    assert_eq!(rec["win"], "4242");
    assert_eq!(
        env.run(&["ws", "focus", "--session", "sess-y"])
            .status
            .code(),
        Some(0)
    );
    assert_eq!(env.actions(), vec!["-m window --focus 4242"]);
}

#[test]
fn ws_changed_rewrites_the_wake_file() {
    let env = TestEnv::new("changed", windows());
    assert_eq!(env.run(&["ws", "changed"]).status.code(), Some(0));
    let f = env.root.join("state/battlestation-workspaces/wm-changed");
    assert!(f.is_file(), "wake file at {}", f.display());
}
