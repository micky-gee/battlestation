# yabai + skhd (macOS window manager and keybinds)

On macOS, bsctl's workspaces are native Mission Control desktops (`ctl/src/lib.rs`,
"Window managers"). yabai tiles kitty and answers bsctl's queries; skhd
binds the global keys. Both configs and their launchd agents are stowed from
`stow/home-darwin/{yabai,skhd}`; the formulae are in `packages/brew.txt`.

Why native desktops rather than AeroSpace: AeroSpace emulates workspaces by
parking windows off-screen, which, with "Displays have separate Spaces" on,
loses track of windows that change display (they resize or vanish from the
Accessibility API). Native desktops leave placement to macOS.

## Install

The tap must be trusted before brew will build from it, and yabai is built
from HEAD: the 7.1.25 release predates its macOS 27 fixes.

```sh
brew trust asmvik/formulae
brew install --HEAD asmvik/formulae/yabai
brew install asmvik/formulae/skhd
```

## Permissions (by hand, once)

System Settings → Privacy & Security → Accessibility → **+** →
`/opt/homebrew/bin/yabai`, and again for `/opt/homebrew/bin/skhd`. launchd
retries both agents every few seconds, so they start once allowed (logs:
`/tmp/battlestation-{yabai,skhd}.log`). A HEAD rebuild is a new binary to
macOS: re-allow yabai after `brew reinstall`/`upgrade`.

Optionally allow `~/Applications/Battlestation.app` too: board clicks can
then switch to empty desktops (see below).

## Desktops and switching

Create the desktops once in Mission Control (Ctrl+↑, then **+** on each
display's row); scripts can't create them with SIP enabled. bsctl numbers
them as Mission Control does, left to right across displays, so adding or
removing one renumbers the rest.

Switching uses macOS's own "Switch to Desktop N" shortcuts (yabai's switch
is ignored by macOS 27 with SIP enabled). Enable Ctrl+1…9: System Settings
→ Keyboard → Keyboard Shortcuts → Mission Control → Switch to Desktop 1–9,
or:

```sh
for n in 1 2 3 4 5 6 7 8 9; do
  key=$((117 + n)); code=$(echo "18 19 20 21 23 22 26 28 25" | cut -d' ' -f$n)
  defaults write com.apple.symbolichotkeys AppleSymbolicHotKeys -dict-add $key \
    "<dict><key>enabled</key><true/><key>value</key><dict><key>parameters</key><array><integer>$((48 + n))</integer><integer>$code</integer><integer>262144</integer></array><key>type</key><string>standard</string></dict></dict>"
done
/System/Library/PrivateFrameworks/SystemAdministration.framework/Resources/activateSettings -u
```

bsctl presses that shortcut for `ws focus`, which needs Accessibility for
the app that runs it (skhd for keybinds, Battlestation.app for board
clicks). Without it, a desktop that has a window on it is still reached by
focusing that window; an empty one is not.

To move a whole desktop to the other display, drag it between the
display rows in Mission Control; yabai can't without disabling SIP.

## Troubleshooting

- Mission Control shows "No windows" for a desktop that has windows:
  `killall Dock` (it owns Mission Control; seen after leaving AeroSpace).
- Restart after a config edit: `launchctl kickstart -k gui/$(id -u)/local.battlestation.yabai`
  (same for skhd; skhd also reloads its config on save).
