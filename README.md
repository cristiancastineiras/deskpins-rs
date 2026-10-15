# DeskPins-rs

Rust rewrite of [DeskPins](http://efotinis.neocities.org/deskpins/): pin any window on top. Single 280 KB exe, no runtime to install, ~0 % CPU when idle (event-driven, the original polled every 20 ms).

## Use

| Action | How |
|---|---|
| Pin a window | Click the tray pin (or `Ctrl+F11`), then click the window |
| Pin/unpin active window | `Ctrl+F12` |
| Unpin | Click the red pin on the window's title bar |
| Cancel pin mode | `Esc` or right-click |
| Menu | Right-click the tray pin |

## Portable or installed

- **Portable**: just run `DeskPins.exe`. Settings go to `deskpins.ini` next to it.
- **Install for me**: tray menu, or `DeskPins.exe --install` → `%LOCALAPPDATA%\Programs\deskpins-rs`, Start Menu, autostart, appears in *Apps > Installed apps*.
- **Install for all users**: tray menu, or `DeskPins.exe --install-all` (asks for admin) → `%ProgramFiles%\deskpins-rs`.
- **Uninstall**: Windows *Installed apps*, the tray menu, or `DeskPins.exe --uninstall`.

Installed copies keep settings in `%APPDATA%\deskpins-rs\deskpins.ini`. It never touches a DeskPins 1.x install.

## Settings

Tray → *Settings…* opens the INI in Notepad; changes apply when you close it: pin color, hotkeys, autopin rules (`RuleN=TitlePattern|ClassPattern`, wildcards `*` `?`).

## Build

```
cargo build --release      # target\release\DeskPins.exe
cargo test
```

Windows 10/11 x64, MSVC toolchain. MIT license (see LICENSE.txt), based on DeskPins by Elias Fotinis.
