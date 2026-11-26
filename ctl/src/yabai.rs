//! yabai backend (macOS, native Spaces) — the window-manager boundary when
//! bsctl's workspaces are real Mission Control desktops. [`crate::ipc`]
//! routes here; like [`crate::aerospace`] this answers the Hyprland queries
//! in Hyprland's JSON shape and translates [`Action`]s.
//!
//! Why native Spaces (contract in lib.rs, "Window managers"): AeroSpace
//! emulates workspaces by parking windows off-screen, which with "Displays
//! have separate Spaces" on loses track of windows that change display. Here
//! every workspace is a real desktop, so macOS owns placement and other apps
//! behave as they always do. yabai only tiles what its rules manage
//! (kitty — stow/home-darwin/yabai), queries, and moves windows.
//!
//! Model mapping:
//! - ws-id = the desktop's Mission Control number: user desktops (native
//!   fullscreen spaces excluded) in yabai's global space order, 1-based —
//!   exactly what macOS's "Switch to Desktop N" shortcut counts, so
//!   `ws focus 3` and Ctrl+3 land on the same desktop. It shifts when
//!   desktops are added, removed or reordered in Mission Control, and the
//!   bsctl-owned names ([`ws::names_file`]) follow the number, not the
//!   desktop.
//! - Displays are named from CoreGraphics (built-in vs external; yabai
//!   reports no names), with real frame geometry, so the shared leftmost-
//!   first numbering is by position.
//! - Clients: `address` is the window id (the CGWindowID — the same number
//!   kitty reports as `platform_window_id`, so the kitty disambiguation is
//!   shared with the AeroSpace backend).
//!
//! What native Spaces cannot do with SIP enabled (verified on macOS 27.2,
//! yabai HEAD 477e859, 2026-11-26):
//! - Switching desktop through yabai: its SIP-free path synthesizes a dock
//!   swipe, which macOS 27 ignores. [`Action::Focus`] instead posts the
//!   system's own Ctrl+<N> "Switch to Desktop N" shortcut (it must be
//!   enabled; setup/deps-00-yabai.md), which needs the calling app to hold
//!   Accessibility permission; without it, a desktop holding a window is
//!   reached by focusing that window.
//! - Moving a desktop to another display (yabai needs its scripting
//!   addition, i.e. SIP disabled): [`Action::MoveWorkspace`] refuses and
//!   points at Mission Control, where dragging a desktop between displays
//!   is native.
//!
//! Change events: yabai has no event stream to subscribe to. Its signals
//! (yabairc) run `bsctl ws changed`, which touches a file the `--stream`
//! engine already watches; every [`act`] does the same itself.
//!
//! The CLI is reached through [`crate::sys::tool`] (`yabai`), so tests
//! substitute a fake on PATH. One query round is memoized for 250ms.

use std::collections::BTreeMap;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::ws::{self, Action, WsToken};

const MEMO: Duration = Duration::from_millis(250);

/// One raw query round, as the CLI returned it, plus each display's
/// "is built-in" fact (CoreGraphics; tests inject it).
#[derive(Clone)]
pub struct Snap {
    pub spaces: Value,
    pub windows: Value,
    pub displays: Value,
    pub builtin: BTreeMap<i64, bool>,
}

static CACHE: Mutex<Option<(Instant, Snap)>> = Mutex::new(None);

fn cli_json(args: &[&str]) -> Option<Value> {
    let out = Command::new(crate::sys::tool("yabai"))
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// Run a yabai command for its effect; stderr passes through (yabai's own
/// messages are the useful diagnostics), 127 when missing.
fn cli_run(args: &[&str]) -> i32 {
    match Command::new(crate::sys::tool("yabai"))
        .args(args)
        .stdout(Stdio::null())
        .status()
    {
        Ok(s) => s.code().unwrap_or(1),
        Err(_) => {
            eprintln!("bsctl: yabai not found");
            127
        }
    }
}

/// The memoized query round; None when yabai isn't reachable.
pub fn snap() -> Option<Snap> {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, s)) = cache.as_ref()
        && at.elapsed() < MEMO
    {
        return Some(s.clone());
    }
    let displays = cli_json(&["-m", "query", "--displays"])?;
    let builtin = displays
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| {
            let id = int(d, "id")?;
            Some((id, display_is_builtin(id)))
        })
        .collect();
    let s = Snap {
        spaces: cli_json(&["-m", "query", "--spaces"])?,
        windows: cli_json(&["-m", "query", "--windows"])?,
        displays,
        builtin,
    };
    *cache = Some((Instant::now(), s.clone()));
    Some(s)
}

fn invalidate() {
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

fn int(v: &Value, k: &str) -> Option<i64> {
    match v.get(k)? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn num(v: &Value, k: &str) -> f64 {
    v.get(k).and_then(Value::as_f64).unwrap_or(0.0)
}

fn flag(v: &Value, k: &str) -> bool {
    v.get(k).and_then(Value::as_bool).unwrap_or(false)
}

// ---- pure translators (unit-tested) -----------------------------------------

/// yabai space index -> desktop number (the ws-id): user desktops in index
/// order, 1-based; native fullscreen spaces have no desktop number.
pub fn desktop_numbers(spaces: &Value) -> BTreeMap<i64, i64> {
    let mut idx: Vec<i64> = spaces
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| !flag(s, "is-native-fullscreen"))
        .filter_map(|s| int(s, "index"))
        .collect();
    idx.sort_unstable();
    idx.into_iter().zip(1..).collect()
}

/// The yabai space index of desktop `desk` (the inverse of
/// [`desktop_numbers`]).
pub fn space_index(spaces: &Value, desk: i64) -> Option<i64> {
    desktop_numbers(spaces)
        .into_iter()
        .find_map(|(i, d)| (d == desk).then_some(i))
}

/// yabai display index -> unique display name: "Built-in Display", and
/// "External Display" (numbered left to right when there are several).
pub fn display_names(s: &Snap) -> BTreeMap<i64, String> {
    let mut rows: Vec<(f64, i64, bool)> = s
        .displays
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| {
            let index = int(d, "index")?;
            let builtin = int(d, "id").and_then(|id| s.builtin.get(&id).copied()) == Some(true);
            let x = d.get("frame").map_or(0.0, |f| num(f, "x"));
            Some((x, index, builtin))
        })
        .collect();
    rows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let externals = rows.iter().filter(|r| !r.2).count();
    let mut n = 0;
    rows.into_iter()
        .map(|(_, index, builtin)| {
            let name = if builtin {
                "Built-in Display".to_string()
            } else if externals > 1 {
                n += 1;
                format!("External Display {n}")
            } else {
                "External Display".to_string()
            };
            (index, name)
        })
        .collect()
}

/// Hyprland-shaped `workspaces`: one row per desktop, `[{id, name, monitor,
/// windows}]`, named from bsctl's names file (default: the number).
pub fn workspaces_from(s: &Snap, names: &BTreeMap<i64, String>) -> Value {
    let desks = desktop_numbers(&s.spaces);
    let dnames = display_names(s);
    let mut counts: BTreeMap<i64, i64> = BTreeMap::new();
    for w in s.windows.as_array().into_iter().flatten() {
        if let Some(d) = int(w, "space").and_then(|i| desks.get(&i)) {
            *counts.entry(*d).or_default() += 1;
        }
    }
    let mut rows: Vec<(i64, Value)> = s
        .spaces
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|sp| {
            let desk = *desks.get(&int(sp, "index")?)?;
            let monitor = int(sp, "display")
                .and_then(|d| dnames.get(&d).cloned())
                .unwrap_or_default();
            Some((
                desk,
                json!({
                    "id": desk,
                    "name": names.get(&desk).cloned().unwrap_or_else(|| desk.to_string()),
                    "monitor": monitor,
                    "windows": counts.get(&desk).copied().unwrap_or(0),
                }),
            ))
        })
        .collect();
    rows.sort_by_key(|r| r.0);
    Value::Array(rows.into_iter().map(|r| r.1).collect())
}

/// Hyprland-shaped `monitors`: `[{id, name, x, y, width, height, focused,
/// activeWorkspace: {id}, disabled}]`, real geometry from yabai's frames.
pub fn monitors_from(s: &Snap) -> Value {
    let desks = desktop_numbers(&s.spaces);
    let dnames = display_names(s);
    let visible = |display: i64| -> Option<i64> {
        s.spaces.as_array()?.iter().find_map(|sp| {
            (int(sp, "display") == Some(display) && flag(sp, "is-visible"))
                .then(|| desks.get(&int(sp, "index")?).copied())
                .flatten()
        })
    };
    Value::Array(
        s.displays
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|d| {
                let index = int(d, "index")?;
                let f = d.get("frame").cloned().unwrap_or(Value::Null);
                Some(json!({
                    "id": index,
                    "name": dnames.get(&index).cloned().unwrap_or_default(),
                    "x": num(&f, "x") as i64,
                    "y": num(&f, "y") as i64,
                    "width": num(&f, "w") as i64,
                    "height": num(&f, "h") as i64,
                    "focused": flag(d, "has-focus"),
                    "activeWorkspace": {"id": visible(index)},
                    "disabled": false,
                }))
            })
            .collect(),
    )
}

/// Hyprland-shaped `clients`: `[{pid, address, workspace: {id}}]` for every
/// window on a desktop (fullscreen-space windows have no ws-id).
pub fn clients_from(s: &Snap) -> Value {
    let desks = desktop_numbers(&s.spaces);
    Value::Array(
        s.windows
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|w| {
                let desk = desks.get(&int(w, "space")?)?;
                Some(json!({
                    "pid": int(w, "pid"),
                    "address": int(w, "id")?.to_string(),
                    "workspace": {"id": desk},
                }))
            })
            .collect(),
    )
}

/// Hyprland-shaped `activeworkspace`: the focused desktop.
pub fn active_from(s: &Snap) -> Option<Value> {
    let desks = desktop_numbers(&s.spaces);
    let desk = s.spaces.as_array()?.iter().find_map(|sp| {
        flag(sp, "has-focus")
            .then(|| desks.get(&int(sp, "index")?).copied())
            .flatten()
    })?;
    Some(json!({ "id": desk }))
}

/// A planned action: yabai argv lists to run in order, or a keystroke.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    Yabai(Vec<Vec<String>>),
    /// Post Ctrl+<digit> (macOS "Switch to Desktop N"); `fallback` is a
    /// window on that desktop to focus when keystrokes can't be posted.
    SwitchDesktop {
        n: i64,
        fallback: Option<String>,
    },
    Nothing,
}

/// What an action does under yabai. `focused` is the focused window id
/// (window moves act on it); pure, so the policy is unit-tested.
pub fn plan(a: &Action, s: &Snap, focused: Option<&str>) -> Result<Plan, String> {
    let desk_of = |t: &WsToken| match t {
        WsToken::Id(id) => Ok(*id),
        WsToken::Name(n) => Err(format!(
            "named workspace {n:?} has no desktop (bsctl uses numbered desktops)"
        )),
    };
    let index_of = |desk: i64| {
        space_index(&s.spaces, desk).ok_or_else(|| {
            format!("no desktop {desk} (add desktops in Mission Control: Ctrl+Up, then +)")
        })
    };
    let v = |xs: &[&str]| xs.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    Ok(match a {
        Action::Focus(t) => {
            let desk = desk_of(t)?;
            let index = index_of(desk)?;
            let fallback = s.windows.as_array().and_then(|ws| {
                ws.iter()
                    .find(|w| int(w, "space") == Some(index) && !flag(w, "is-minimized"))
                    .and_then(|w| int(w, "id"))
                    .map(|id| id.to_string())
            });
            if !(1..=9).contains(&desk) {
                // No Ctrl+digit shortcut exists past 9: a window is the way in.
                return fallback
                    .map(|id| Plan::Yabai(vec![v(&["-m", "window", "--focus", &id])]))
                    .ok_or_else(|| {
                        format!("desktop {desk} is empty and has no Ctrl+number shortcut")
                    });
            }
            Plan::SwitchDesktop { n: desk, fallback }
        }
        Action::FocusMonitor(name) => {
            let index = display_names(s)
                .into_iter()
                .find_map(|(i, n)| (&n == name).then_some(i))
                .ok_or_else(|| format!("no display named {name:?}"))?;
            Plan::Yabai(vec![v(&["-m", "display", "--focus", &index.to_string()])])
        }
        Action::FocusWindow(addr) => Plan::Yabai(vec![v(&["-m", "window", "--focus", addr])]),
        Action::MoveWindow(t, follow) => {
            let index = index_of(desk_of(t)?)?.to_string();
            let id = focused.ok_or("no focused window to move")?;
            let mut runs = vec![v(&["-m", "window", id, "--space", &index])];
            if *follow {
                // Focusing the moved window is what switches to its desktop.
                runs.push(v(&["-m", "window", "--focus", id]));
            }
            Plan::Yabai(runs)
        }
        Action::MoveWorkspace(..) => {
            return Err(
                "macOS moves a desktop to another display only in Mission Control \
                 (Ctrl+Up, then drag the desktop onto the other display's row); \
                 yabai can't without disabling SIP"
                    .into(),
            );
        }
        Action::Rename(..) => Plan::Nothing,
    })
}

// ---- the ipc surface -----------------------------------------------------------

/// [`crate::ipc::json`] under yabai.
pub fn json(query: &str) -> Option<Value> {
    let s = snap()?;
    match query.trim() {
        "workspaces" => Some(workspaces_from(&s, &ws::load_names())),
        "monitors" | "monitors all" => Some(monitors_from(&s)),
        "clients" => Some(clients_from(&s)),
        "activeworkspace" => active_from(&s),
        _ => None,
    }
}

/// [`crate::ipc::act`] under yabai.
pub fn act(a: &Action) -> i32 {
    if let Action::Rename(id, name) = a {
        return ws::set_name(*id, name.as_deref());
    }
    let Some(s) = snap() else {
        eprintln!("bsctl ws: yabai is not reachable (is it running?)");
        return 1;
    };
    let focused = cli_json(&["-m", "query", "--windows", "--window"])
        .and_then(|w| int(&w, "id"))
        .map(|id| id.to_string());
    let code = match plan(a, &s, focused.as_deref()) {
        Ok(Plan::Yabai(runs)) => runs.iter().fold(0, |code, argv| {
            if code != 0 {
                return code;
            }
            let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
            cli_run(&refs)
        }),
        Ok(Plan::SwitchDesktop { n, fallback }) => switch_desktop(n, fallback.as_deref()),
        Ok(Plan::Nothing) => 0,
        Err(msg) => {
            eprintln!("bsctl ws: {msg}");
            1
        }
    };
    invalidate();
    ws::note_changed();
    code
}

/// Post the "Switch to Desktop N" shortcut; when this process may not post
/// keystrokes (no Accessibility permission for the app that launched it),
/// focus a window on that desktop instead.
fn switch_desktop(n: i64, fallback: Option<&str>) -> i32 {
    // BSCTL_NO_KEYSTROKES=1: never press keys (the test suites set it — a
    // trusted terminal would otherwise switch the developer's desktops).
    let keys = std::env::var_os("BSCTL_NO_KEYSTROKES").is_none_or(|v| v.is_empty() || v == "0");
    if keys && post_ctrl_digit(n) {
        return 0;
    }
    match fallback {
        Some(id) => cli_run(&["-m", "window", "--focus", id]),
        None => {
            eprintln!(
                "bsctl ws: can't switch to empty desktop {n}: the app running bsctl needs \
                 Accessibility permission to press Ctrl+{n} (System Settings → Privacy & \
                 Security → Accessibility)"
            );
            1
        }
    }
}

/// [`crate::ipc::client_for_pids`] under yabai — the AeroSpace rule: the
/// first window of an ancestor pid, narrowed through kitty when one kitty
/// process serves several windows.
pub fn client_for_pids(pids: &[i64]) -> Option<(i64, Option<String>, Option<i64>)> {
    crate::aerospace::pick_client(&clients_from(&snap()?), pids)
}

// ---- macOS system calls ---------------------------------------------------------

#[cfg(target_os = "macos")]
mod sys_macos {
    use std::ffi::c_void;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGDisplayIsBuiltin(display: u32) -> u32;
        fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
        fn CGEventCreateKeyboardEvent(source: *const c_void, key: u16, down: bool) -> *mut c_void;
        fn CGEventSetFlags(event: *mut c_void, flags: u64);
        fn CGEventPost(tap: u32, event: *mut c_void);
    }
    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> u8;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: *const c_void);
    }

    /// Only a connected display is asked: for an id it doesn't know,
    /// CoreGraphics returns garbage rather than false.
    pub fn is_builtin(display: i64) -> bool {
        let Ok(d) = u32::try_from(display) else {
            return false;
        };
        let mut ids = [0u32; 32];
        let mut n = 0u32;
        let ok = unsafe { CGGetActiveDisplayList(32, ids.as_mut_ptr(), &mut n) } == 0;
        ok && ids[..n as usize].contains(&d) && unsafe { CGDisplayIsBuiltin(d) } != 0
    }

    /// Ctrl+<digit> down/up at the HID tap. false = not permitted (posting
    /// needs Accessibility; without it macOS silently drops the events, so
    /// trust is checked first) or no such digit.
    pub fn ctrl_digit(n: i64) -> bool {
        // ANSI keycodes for the top-row digits 1-9.
        const KEYS: [u16; 9] = [18, 19, 20, 21, 23, 22, 26, 28, 25];
        const CONTROL: u64 = 0x0004_0000; // kCGEventFlagMaskControl
        let Some(&key) = usize::try_from(n - 1).ok().and_then(|i| KEYS.get(i)) else {
            return false;
        };
        if unsafe { AXIsProcessTrusted() } == 0 {
            return false;
        }
        // The full chord, as a keyboard sends it: Control down, digit
        // down/up with the Control flag, Control up. A lone flagged digit is
        // not matched against the system's desktop shortcuts.
        const LEFT_CONTROL: u16 = 59;
        for (k, down, flags) in [
            (LEFT_CONTROL, true, CONTROL),
            (key, true, CONTROL),
            (key, false, CONTROL),
            (LEFT_CONTROL, false, 0),
        ] {
            unsafe {
                let e = CGEventCreateKeyboardEvent(std::ptr::null(), k, down);
                if e.is_null() {
                    return false;
                }
                CGEventSetFlags(e, flags);
                CGEventPost(0, e); // kCGHIDEventTap
                CFRelease(e);
            }
        }
        true
    }
}

#[cfg(target_os = "macos")]
fn display_is_builtin(id: i64) -> bool {
    sys_macos::is_builtin(id)
}
#[cfg(not(target_os = "macos"))]
fn display_is_builtin(_id: i64) -> bool {
    false
}

#[cfg(target_os = "macos")]
fn post_ctrl_digit(n: i64) -> bool {
    sys_macos::ctrl_digit(n)
}
#[cfg(not(target_os = "macos"))]
fn post_ctrl_digit(_n: i64) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two displays: built-in (index 1, desktops 1-2 + a fullscreen app)
    /// and an external above-left of it (index 2, desktop 3).
    fn snap_fixture() -> Snap {
        Snap {
            displays: json!([
                {"id": 1, "index": 1, "has-focus": true, "frame": {"x": 0.0, "y": 0.0, "w": 1512.0, "h": 982.0}},
                {"id": 3, "index": 2, "has-focus": false, "frame": {"x": -199.0, "y": -1080.0, "w": 1920.0, "h": 1080.0}}
            ]),
            spaces: json!([
                {"index": 1, "display": 1, "is-visible": true, "has-focus": true, "is-native-fullscreen": false},
                {"index": 2, "display": 1, "is-visible": false, "has-focus": false, "is-native-fullscreen": false},
                {"index": 3, "display": 1, "is-visible": false, "has-focus": false, "is-native-fullscreen": true},
                {"index": 4, "display": 2, "is-visible": true, "has-focus": false, "is-native-fullscreen": false}
            ]),
            windows: json!([
                {"id": 101, "pid": 500, "app": "kitty", "space": 1},
                {"id": 102, "pid": 500, "app": "kitty", "space": 4},
                {"id": 103, "pid": 600, "app": "Safari", "space": 1},
                {"id": 104, "pid": 700, "app": "Keynote", "space": 3},
                {"id": 105, "pid": 800, "app": "Notes", "space": 2, "is-minimized": true}
            ]),
            builtin: [(1, true), (3, false)].into(),
        }
    }

    #[test]
    fn desktops_skip_fullscreen_spaces() {
        let s = snap_fixture();
        let d = desktop_numbers(&s.spaces);
        assert_eq!(d, [(1, 1), (2, 2), (4, 3)].into());
        assert_eq!(space_index(&s.spaces, 3), Some(4));
        assert_eq!(space_index(&s.spaces, 4), None);
    }

    #[test]
    fn displays_are_named_builtin_and_external() {
        let s = snap_fixture();
        let n = display_names(&s);
        assert_eq!(n[&1], "Built-in Display");
        assert_eq!(n[&2], "External Display");
        // two externals are numbered left to right
        let mut two = s.clone();
        two.displays = json!([
            {"id": 7, "index": 1, "frame": {"x": 1920.0}},
            {"id": 8, "index": 2, "frame": {"x": 0.0}}
        ]);
        two.builtin = BTreeMap::new();
        let n = display_names(&two);
        assert_eq!(n[&2], "External Display 1");
        assert_eq!(n[&1], "External Display 2");
    }

    #[test]
    fn workspaces_are_desktops_counted_and_named() {
        let names: BTreeMap<i64, String> = [(3, "infra".to_string())].into();
        let w = workspaces_from(&snap_fixture(), &names);
        assert_eq!(
            w,
            json!([
                {"id": 1, "name": "1", "monitor": "Built-in Display", "windows": 2},
                {"id": 2, "name": "2", "monitor": "Built-in Display", "windows": 1},
                {"id": 3, "name": "infra", "monitor": "External Display", "windows": 1}
            ])
        );
    }

    #[test]
    fn monitors_carry_geometry_focus_and_visible_desktop() {
        let m = monitors_from(&snap_fixture());
        assert_eq!(m[0]["name"], "Built-in Display");
        assert_eq!(m[0]["focused"], true);
        assert_eq!(m[0]["activeWorkspace"]["id"], 1);
        assert_eq!(m[1]["x"], -199);
        assert_eq!(m[1]["activeWorkspace"]["id"], 3);
        // the shared numbering is leftmost first: the external is display 1
        let ds = ws::displays_from(&m);
        assert_eq!(ds[0].name, "External Display");
        assert_eq!(ds[1].active_ws, Some(1));
    }

    #[test]
    fn clients_and_active_desktop() {
        let s = snap_fixture();
        let c = clients_from(&s);
        let rows: Vec<(String, i64)> = c
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["address"].as_str().unwrap().to_string(),
                    r["workspace"]["id"].as_i64().unwrap(),
                )
            })
            .collect();
        // the fullscreen-space window (104) has no desktop
        assert_eq!(
            rows,
            [
                ("101".into(), 1),
                ("102".into(), 3),
                ("103".into(), 1),
                ("105".into(), 2)
            ]
        );
        assert_eq!(active_from(&s), Some(json!({"id": 1})));
    }

    #[test]
    fn plans() {
        let s = snap_fixture();
        let y = |xs: &[&[&str]]| {
            Plan::Yabai(
                xs.iter()
                    .map(|a| a.iter().map(|x| x.to_string()).collect())
                    .collect(),
            )
        };
        // switching uses the desktop shortcut, with a visible window as fallback
        assert_eq!(
            plan(&Action::Focus(WsToken::Id(3)), &s, None),
            Ok(Plan::SwitchDesktop {
                n: 3,
                fallback: Some("102".into())
            })
        );
        // a minimized window is no way in
        assert_eq!(
            plan(&Action::Focus(WsToken::Id(2)), &s, None),
            Ok(Plan::SwitchDesktop {
                n: 2,
                fallback: None
            })
        );
        assert!(plan(&Action::Focus(WsToken::Id(9)), &s, None).is_err()); // no such desktop
        assert!(plan(&Action::Focus(WsToken::Name("x".into())), &s, None).is_err());
        // moving the focused window targets the yabai index; --focus follows it
        assert_eq!(
            plan(&Action::MoveWindow(WsToken::Id(3), true), &s, Some("101")),
            Ok(y(&[
                &["-m", "window", "101", "--space", "4"],
                &["-m", "window", "--focus", "101"]
            ]))
        );
        assert_eq!(
            plan(&Action::MoveWindow(WsToken::Id(2), false), &s, Some("101")),
            Ok(y(&[&["-m", "window", "101", "--space", "2"]]))
        );
        assert!(plan(&Action::MoveWindow(WsToken::Id(2), false), &s, None).is_err());
        assert_eq!(
            plan(&Action::FocusMonitor("External Display".into()), &s, None),
            Ok(y(&[&["-m", "display", "--focus", "2"]]))
        );
        assert_eq!(
            plan(&Action::FocusWindow("103".into()), &s, None),
            Ok(y(&[&["-m", "window", "--focus", "103"]]))
        );
        // moving a whole desktop between displays is refused, with directions
        let e = plan(
            &Action::MoveWorkspace(3, "Built-in Display".into()),
            &s,
            None,
        )
        .unwrap_err();
        assert!(e.contains("Mission Control"));
        assert_eq!(plan(&Action::Rename(1, None), &s, None), Ok(Plan::Nothing));
    }
}
