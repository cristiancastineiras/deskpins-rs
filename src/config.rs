use crate::{install, w};
use std::{fs, path::Path, path::PathBuf};
use windows_sys::Win32::System::WindowsProgramming::{
    GetPrivateProfileSectionW, GetPrivateProfileStringW, WritePrivateProfileStringW,
};

pub struct Config {
    /// 0xRRGGBB
    pub color: u32,
    /// (text as written, parsed (modifiers, vk)) for [PinMode, TogglePin]
    pub hotkeys: [(String, Option<(u32, u32)>); 2],
    pub autopin: bool,
    pub delay: u32,
    /// (title pattern, class pattern)
    pub rules: Vec<(String, String)>,
}

const DEFAULT: &str = "\
; DeskPins-rs settings / configuración
; Save and close Notepad to apply. / Guarda y cierra el Bloc de notas para aplicar.
; Comments must be on their own line. / Los comentarios van en su propia línea.

[General]
; Pin color, hex RRGGBB / Color del pin
PinColor=FF0000

[Hotkeys]
; Ctrl, Alt, Shift, Win + key (A-Z, 0-9, F1-F24, Pause, Insert, Home...). Empty = off / Vacío = desactivado
PinMode=Ctrl+F11
TogglePin=Ctrl+F12

[AutoPin]
Enabled=0
; ms to wait after a window appears / ms de espera tras aparecer una ventana
Delay=200
; RuleN=TitlePattern|ClassPattern   (* and ?, case-insensitive / sin distinguir mayúsculas)
; Rule1=*Calculator*|*
; Rule2=*|Notepad
";

pub fn user_path() -> PathBuf {
    PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join(r"deskpins-rs\deskpins.ini")
}

/// Portable copies keep settings beside the exe; installed ones in %APPDATA%.
pub fn path() -> PathBuf {
    if install::mode() == install::Mode::Portable {
        install::exe().with_file_name("deskpins.ini")
    } else {
        user_path()
    }
}

fn get(ini: &[u16], sec: &str, key: &str, def: &str) -> String {
    let mut b = [0u16; 1024];
    let n = unsafe {
        GetPrivateProfileStringW(w(sec).as_ptr(), w(key).as_ptr(), w(def).as_ptr(), b.as_mut_ptr(), b.len() as u32, ini.as_ptr())
    };
    String::from_utf16_lossy(&b[..n as usize]).trim().into()
}

pub fn set(path: &Path, sec: &str, key: &str, val: &str) {
    let p = w(&path.to_string_lossy());
    unsafe { WritePrivateProfileStringW(w(sec).as_ptr(), w(key).as_ptr(), w(val).as_ptr(), p.as_ptr()) };
}

pub fn load(path: &Path) -> Config {
    if !path.exists() {
        if let Some(d) = path.parent() {
            let _ = fs::create_dir_all(d);
        }
        // UTF-16LE + BOM so the profile API reads/writes it as Unicode.
        let bytes: Vec<u8> = [0xFF, 0xFE].into_iter().chain(DEFAULT.encode_utf16().flat_map(u16::to_le_bytes)).collect();
        let _ = fs::write(path, bytes);
    }
    let ini = w(&path.to_string_lossy());
    let hk = |key, def| {
        let s = get(&ini, "Hotkeys", key, def);
        let p = parse_hotkey(&s);
        (s, p)
    };
    let mut sec = vec![0u16; 32767];
    let n = unsafe { GetPrivateProfileSectionW(w("AutoPin").as_ptr(), sec.as_mut_ptr(), sec.len() as u32, ini.as_ptr()) };
    let rules = String::from_utf16_lossy(&sec[..n as usize])
        .split('\0')
        .filter_map(|line| {
            let (k, v) = line.split_once('=')?;
            k.trim().to_ascii_lowercase().starts_with("rule").then(|| {
                let (t, c) = v.split_once('|').unwrap_or((v, "*"));
                (t.trim().to_string(), c.trim().to_string())
            })
        })
        .collect();
    Config {
        color: u32::from_str_radix(get(&ini, "General", "PinColor", "FF0000").trim_start_matches('#'), 16).unwrap_or(0xFF0000) & 0xFFFFFF,
        hotkeys: [hk("PinMode", "Ctrl+F11"), hk("TogglePin", "Ctrl+F12")],
        autopin: get(&ini, "AutoPin", "Enabled", "0") == "1",
        delay: get(&ini, "AutoPin", "Delay", "200").parse().unwrap_or(200u32).min(10_000),
        rules,
    }
}

impl Config {
    pub fn matches(&self, title: &str, class: &str) -> bool {
        self.rules.iter().any(|(t, c)| wild(t, title) && wild(c, class))
    }
}

/// "Ctrl+Alt+F11" -> (MOD_CONTROL|MOD_ALT, VK_F11). None if empty or invalid.
pub fn parse_hotkey(s: &str) -> Option<(u32, u32)> {
    let (mut mods, mut vk) = (0, 0);
    for part in s.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        let p = part.to_ascii_uppercase();
        match p.as_str() {
            "ALT" => mods |= 1,
            "CTRL" | "CONTROL" => mods |= 2,
            "SHIFT" => mods |= 4,
            "WIN" => mods |= 8,
            _ if vk != 0 => return None,
            _ => vk = key(&p)?,
        }
    }
    (vk != 0).then_some((mods, vk))
}

fn key(p: &str) -> Option<u32> {
    let b = p.as_bytes();
    if b.len() == 1 && b[0].is_ascii_alphanumeric() {
        return Some(b[0] as u32);
    }
    if let Some(n) = p.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()) {
        return (1..=24).contains(&n).then_some(0x70 + n - 1);
    }
    if let Some(n) = p.strip_prefix("NUM").and_then(|n| n.parse::<u32>().ok()) {
        return (n <= 9).then_some(0x60 + n);
    }
    Some(match p {
        "BACKSPACE" => 0x08, "TAB" => 0x09, "ENTER" => 0x0D, "PAUSE" => 0x13, "ESC" => 0x1B,
        "SPACE" => 0x20, "PAGEUP" => 0x21, "PAGEDOWN" => 0x22, "END" => 0x23, "HOME" => 0x24,
        "LEFT" => 0x25, "UP" => 0x26, "RIGHT" => 0x27, "DOWN" => 0x28,
        "INSERT" => 0x2D, "DELETE" => 0x2E, "SCROLLLOCK" => 0x91,
        _ => return None,
    })
}

/// Case-insensitive `*`/`?` wildcard match. Empty pattern matches anything.
pub fn wild(pat: &str, s: &str) -> bool {
    if pat.is_empty() {
        return true;
    }
    let p: Vec<char> = pat.to_lowercase().chars().collect();
    let s: Vec<char> = s.to_lowercase().chars().collect();
    let (mut i, mut j, mut star, mut mark) = (0, 0, None, 0);
    while j < s.len() {
        if i < p.len() && (p[i] == '?' || p[i] == s[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == '*' {
            star = Some(i);
            mark = j;
            i += 1;
        } else if let Some(st) = star {
            i = st + 1;
            mark += 1;
            j = mark;
        } else {
            return false;
        }
    }
    p[i..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards() {
        assert!(wild("*calc*", "Calculadora - Calc"));
        assert!(wild("note?ad", "NOTEPAD"));
        assert!(wild("*", ""));
        assert!(wild("", "anything"));
        assert!(wild("a*b*c", "aXXbYYc"));
        assert!(!wild("a*b*c", "aXXbYY"));
        assert!(!wild("abc", "abcd"));
    }

    #[test]
    fn hotkeys() {
        assert_eq!(parse_hotkey("Ctrl+F11"), Some((2, 0x7A)));
        assert_eq!(parse_hotkey("ctrl + alt + shift + win + p"), Some((15, b'P' as u32)));
        assert_eq!(parse_hotkey("Win+Num5"), Some((8, 0x65)));
        assert_eq!(parse_hotkey(""), None);
        assert_eq!(parse_hotkey("Ctrl"), None);
        assert_eq!(parse_hotkey("Ctrl+F25"), None);
        assert_eq!(parse_hotkey("A+B"), None);
    }
}
