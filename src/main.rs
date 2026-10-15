#![windows_subsystem = "windows"]
//! DeskPins-rs: pin any window on top. Tray app, event-driven (no polling).

mod config;
mod install;

use config::Config;
use std::cell::RefCell;
use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering::Relaxed};
use windows_sys::core::{PCWSTR, PWSTR};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::{Dwm::*, Gdi::*};
use windows_sys::Win32::System::{LibraryLoader::GetModuleHandleW, Threading::*};
use windows_sys::Win32::UI::{Accessibility::*, HiDpi::*, Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*};

pub fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// Pick English or Spanish by the user's UI language.
pub fn t(en: &'static str, es: &'static str) -> &'static str {
    if unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() } & 0x3ff == 0x0a { es } else { en }
}

pub const MAIN_CLASS: &str = "deskpins-rs";
const PIN_CLASS: &str = "deskpins-rs.Pin";
const OVERLAY_CLASS: &str = "deskpins-rs.Overlay";

const WM_TRAY: u32 = WM_APP + 1;
const WM_SYNC: u32 = WM_APP + 2; // wparam = target hwnd
const WM_PICK: u32 = WM_APP + 3; // wparam, lparam = screen x, y
const WM_RELOAD: u32 = WM_APP + 4;

const CMD_PINMODE: usize = 1;
const CMD_UNPINALL: usize = 2;
const CMD_AUTOPIN: usize = 3;
const CMD_STARTUP: usize = 4;
const CMD_SETTINGS: usize = 5;
const CMD_INSTALL_USER: usize = 6;
const CMD_INSTALL_ALL: usize = 7;
const CMD_UNINSTALL: usize = 8;
const CMD_ABOUT: usize = 9;
const CMD_EXIT: usize = 10;

static MAIN: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static OVERLAY: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static DELAY: AtomicU32 = AtomicU32::new(200);
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

fn main_wnd() -> HWND {
    MAIN.load(Relaxed)
}

fn hinst() -> HINSTANCE {
    unsafe { GetModuleHandleW(null()) }
}

/// Original DeskPins pin bitmap. d = transparent, 0 = black, f/7/8 = light/base/dark pin color.
const PIN_ART: [&[u8; 16]; 16] = [
    b"ddddddddd0000ddd", b"dddddd000fff70dd", b"dddd00f0ff77770d", b"ddd0ff0ff7777770",
    b"ddd0ff0f77777780", b"dd0ff70f77777780", b"dd0f770777777880", b"dd0f77707777880d",
    b"dd0777770788800d", b"dd0777777000080d", b"ddd07777777880dd", b"ddd07777778880dd",
    b"dd00007788800ddd", b"d000dd00000ddddd", b"d00ddddddddddddd", b"dddddddddddddddd",
];

/// 32-bit premultiplied top-down DIB of the pin, nearest-neighbour scaled to `size`.
unsafe fn pin_dib(size: i32, rgb: u32) -> HBITMAP {
    let mut bmi: BITMAPINFO = zeroed();
    bmi.bmiHeader = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: size, biHeight: -size,
        biPlanes: 1, biBitCount: 32, biCompression: BI_RGB, ..zeroed()
    };
    let mut bits = null_mut();
    let bmp = CreateDIBSection(null_mut(), &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
    if bmp.is_null() {
        return bmp;
    }
    let px = std::slice::from_raw_parts_mut(bits as *mut u32, (size * size) as usize);
    let ch = |f: fn(u32) -> u32| (0..3).fold(0, |acc, i| acc | f((rgb >> (i * 8)) & 255) << (i * 8));
    let (light, dark) = (ch(|c| (c + 255) / 2), ch(|c| c / 2));
    for y in 0..size {
        for x in 0..size {
            px[(y * size + x) as usize] = match PIN_ART[(y * 16 / size) as usize][(x * 16 / size) as usize] {
                b'0' => 0xFF00_0000,
                b'f' => 0xFF00_0000 | light,
                b'7' => 0xFF00_0000 | rgb,
                b'8' => 0xFF00_0000 | dark,
                _ => 0,
            };
        }
    }
    bmp
}

unsafe fn pin_icon(rgb: u32) -> HICON {
    let size = GetSystemMetrics(SM_CXSMICON);
    let color = pin_dib(size, rgb);
    let mask_bits = vec![0u8; ((size as usize + 15) / 16 * 2) * size as usize];
    let mask = CreateBitmap(size, size, 1, 1, mask_bits.as_ptr().cast());
    let ii = ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
    let icon = CreateIconIndirect(&ii);
    DeleteObject(color);
    DeleteObject(mask);
    icon
}

unsafe fn render(wnd: HWND, size: i32, rgb: u32) {
    let bmp = pin_dib(size, rgb);
    let dc = CreateCompatibleDC(null_mut());
    let old = SelectObject(dc, bmp);
    let (sz, src) = (SIZE { cx: size, cy: size }, POINT { x: 0, y: 0 });
    let bf = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
    UpdateLayeredWindow(wnd, null_mut(), null(), &sz, dc, &src, 0, &bf, ULW_ALPHA);
    SelectObject(dc, old);
    DeleteDC(dc);
    DeleteObject(bmp);
}

unsafe fn cloaked(h: HWND) -> bool {
    let mut c = 0u32;
    DwmGetWindowAttribute(h, DWMWA_CLOAKED as _, (&mut c as *mut u32).cast(), 4) == 0 && c != 0
}

unsafe fn text(h: HWND, f: unsafe extern "system" fn(HWND, PWSTR, i32) -> i32) -> String {
    let mut b = [0u16; 512];
    let n = f(h, b.as_mut_ptr(), b.len() as i32);
    String::from_utf16_lossy(&b[..n.max(0) as usize])
}

unsafe fn forbidden(h: HWND) -> bool {
    let mut pid = 0;
    GetWindowThreadProcessId(h, &mut pid);
    pid == GetCurrentProcessId()
        || ["Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd"].contains(&text(h, GetClassNameW).as_str())
}

struct Pin {
    target: HWND,
    wnd: HWND,
    /// WinEvent hook for the target's process; only one pin per process owns it.
    hook: HWINEVENTHOOK,
    pid: u32,
    dpi: u32,
}

struct App {
    cfg: Config,
    ini: PathBuf,
    pins: Vec<Pin>,
    icon: HICON,
    autopin: HWINEVENTHOOK,
    relaunch: Option<PathBuf>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Borrow the app state; None if already borrowed (re-entrant message during a sent-message wait).
fn with<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| {
        let mut g = a.try_borrow_mut().ok()?;
        g.as_mut().map(f)
    })
}

impl App {
    unsafe fn tray(&self, op: NOTIFY_ICON_MESSAGE, info: Option<&str>) {
        fn copy(dst: &mut [u16], s: &str) {
            let n = dst.len() - 1;
            dst.iter_mut().zip(s.encode_utf16().take(n)).for_each(|(d, c)| *d = c);
        }
        let mut n: NOTIFYICONDATAW = zeroed();
        n.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        n.hWnd = main_wnd();
        n.uID = 1;
        n.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        n.uCallbackMessage = WM_TRAY;
        n.hIcon = self.icon;
        copy(&mut n.szTip, &format!("DeskPins · {} {}", self.pins.len(), t("pinned", "fijadas")));
        if let Some(s) = info {
            n.uFlags |= NIF_INFO;
            n.dwInfoFlags = NIIF_WARNING;
            copy(&mut n.szInfo, s);
            copy(&mut n.szInfoTitle, "DeskPins");
        }
        Shell_NotifyIconW(op, &n);
    }

    /// (Re)apply config: icon, hotkeys, pin colors, autopin hook.
    unsafe fn apply(&mut self) {
        if !self.icon.is_null() {
            DestroyIcon(self.icon);
        }
        self.icon = pin_icon(self.cfg.color);
        let mut bad = vec![];
        for (id, (txt, hk)) in self.cfg.hotkeys.iter().enumerate() {
            UnregisterHotKey(main_wnd(), id as i32);
            let ok = match hk {
                Some((m, vk)) => RegisterHotKey(main_wnd(), id as i32, m | MOD_NOREPEAT, *vk) != 0,
                None => txt.is_empty(),
            };
            if !ok {
                bad.push(txt.as_str());
            }
        }
        let warn = (!bad.is_empty()).then(|| format!("{} {}", t("Hotkey unavailable or invalid:", "Atajo no disponible o inválido:"), bad.join(", ")));
        self.tray(NIM_MODIFY, warn.as_deref());
        for i in (0..self.pins.len()).rev() {
            self.pins[i].dpi = 0;
            self.sync(i);
        }
        self.apply_autopin();
    }

    unsafe fn apply_autopin(&mut self) {
        DELAY.store(self.cfg.delay, Relaxed);
        if self.cfg.autopin && self.autopin.is_null() {
            self.autopin = SetWinEventHook(EVENT_OBJECT_SHOW, EVENT_OBJECT_SHOW, null_mut(), Some(on_show), 0, 0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);
        } else if !self.cfg.autopin && !self.autopin.is_null() {
            UnhookWinEvent(self.autopin);
            self.autopin = null_mut();
        }
    }

    fn find(&self, target: HWND) -> Option<usize> {
        self.pins.iter().position(|p| p.target == target)
    }

    unsafe fn pin(&mut self, target: HWND, activate: bool) {
        if target.is_null() || IsWindow(target) == 0 || forbidden(target) || self.find(target).is_some() {
            return;
        }
        if SetWindowPos(target, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE) == 0 {
            return self.tray(NIM_MODIFY, Some(t(
                "Can't pin that window: it runs as administrator. Run DeskPins as administrator to pin it.",
                "No se puede fijar esa ventana: se ejecuta como administrador. Ejecuta DeskPins como administrador.")));
        }
        // Owned by the target: stays right above it and hides/minimizes with it.
        let wnd = CreateWindowExW(WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
            w(PIN_CLASS).as_ptr(), null(), WS_POPUP, 0, 0, 0, 0, target, null_mut(), hinst(), null());
        let mut pid = 0;
        GetWindowThreadProcessId(target, &mut pid);
        let hook = if self.pins.iter().any(|p| p.pid == pid) {
            null_mut()
        } else {
            SetWinEventHook(EVENT_OBJECT_DESTROY, EVENT_OBJECT_UNCLOAKED, null_mut(), Some(on_event), pid, 0, WINEVENT_OUTOFCONTEXT)
        };
        self.pins.push(Pin { target, wnd, hook, pid, dpi: 0 });
        self.sync(self.pins.len() - 1);
        self.tray(NIM_MODIFY, None);
        if activate {
            SetForegroundWindow(target);
        }
    }

    unsafe fn unpin(&mut self, i: usize) {
        let p = self.pins.remove(i);
        if IsWindow(p.target) != 0 {
            SetWindowPos(p.target, HWND_NOTOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
        DestroyWindow(p.wnd);
        if !p.hook.is_null() {
            match self.pins.iter_mut().find(|q| q.pid == p.pid) {
                Some(q) => q.hook = p.hook,
                None => {
                    UnhookWinEvent(p.hook);
                }
            }
        }
        self.tray(NIM_MODIFY, None);
    }

    unsafe fn unpin_all(&mut self) {
        while let Some(i) = self.pins.len().checked_sub(1) {
            self.unpin(i);
        }
    }

    /// Bring pin `i` in line with its target: gone -> unpin, hidden -> hide, else keep topmost and on the caption.
    unsafe fn sync(&mut self, i: usize) {
        let t = self.pins[i].target;
        if IsWindow(t) == 0 {
            return self.unpin(i);
        }
        let wnd = self.pins[i].wnd;
        if IsWindowVisible(t) == 0 || IsIconic(t) != 0 || cloaked(t) {
            ShowWindow(wnd, SW_HIDE);
            return;
        }
        if GetWindowLongW(t, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST == 0 {
            SetWindowPos(t, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
        let p = &mut self.pins[i];
        let dpi = GetDpiForWindow(t).max(96);
        let size = (16 * dpi / 96) as i32;
        if p.dpi != dpi {
            p.dpi = dpi;
            render(wnd, size, self.cfg.color);
        }
        let mut wr: RECT = zeroed();
        GetWindowRect(t, &mut wr);
        let (mut b, rs) = (zeroed::<RECT>(), size_of::<RECT>() as u32);
        let (x, y) = if DwmGetWindowAttribute(t, DWMWA_CAPTION_BUTTON_BOUNDS as _, (&mut b as *mut RECT).cast(), rs) == 0 && b.right > b.left {
            // Just left of the real min/max/close buttons, vertically centred on them.
            (wr.left + b.left - size * 3 / 2, wr.top + b.top + (b.bottom - b.top - size) / 2)
        } else {
            let mut f = wr;
            DwmGetWindowAttribute(t, DWMWA_EXTENDED_FRAME_BOUNDS as _, (&mut f as *mut RECT).cast(), rs);
            let st = GetWindowLongW(t, GWL_STYLE) as u32;
            let btns = if st & WS_SYSMENU == 0 { 0 } else if st & (WS_MINIMIZEBOX | WS_MAXIMIZEBOX) != 0 { 3 } else { 1 };
            let cap = GetSystemMetricsForDpi(SM_CYCAPTION, dpi);
            (f.right - btns * GetSystemMetricsForDpi(SM_CXSIZE, dpi) - size * 3 / 2, f.top + ((cap - size) / 2).max(2))
        };
        SetWindowPos(wnd, null_mut(), x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER);
        if IsWindowVisible(wnd) == 0 {
            ShowWindow(wnd, SW_SHOWNOACTIVATE);
        }
    }

    unsafe fn shutdown(&mut self) {
        self.unpin_all();
        self.cfg.autopin = false;
        self.apply_autopin();
        self.tray(NIM_DELETE, None);
        DestroyIcon(self.icon);
    }
}

unsafe extern "system" fn on_event(_: HWINEVENTHOOK, _: u32, h: HWND, obj: i32, child: i32, _: u32, _: u32) {
    if obj != OBJID_WINDOW || child != CHILDID_SELF as i32 {
        return;
    }
    // Filter here (cheap) when we can; posting keeps all state changes out of the callback.
    if with(|a| a.find(h).is_some()).unwrap_or(true) {
        PostMessageW(main_wnd(), WM_SYNC, h as usize, 0);
    }
}

unsafe extern "system" fn on_show(_: HWINEVENTHOOK, _: u32, h: HWND, obj: i32, child: i32, _: u32, _: u32) {
    if obj == OBJID_WINDOW && child == CHILDID_SELF as i32 && GetAncestor(h, GA_ROOT) == h {
        // Timer id = hwnd: one pending check per window, rules evaluated after the delay.
        SetTimer(main_wnd(), h as usize, DELAY.load(Relaxed).max(1), None);
    }
}

unsafe fn pin_mode() {
    if !OVERLAY.load(Relaxed).is_null() {
        return;
    }
    let (x, y) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
    let (cx, cy) = (GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN));
    // Near-invisible window over all monitors: catches the click and shows the pin cursor.
    let o = CreateWindowExW(WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW, w(OVERLAY_CLASS).as_ptr(), null(),
        WS_POPUP, x, y, cx, cy, main_wnd(), null_mut(), hinst(), null());
    if o.is_null() {
        return;
    }
    OVERLAY.store(o, Relaxed);
    SetLayeredWindowAttributes(o, 0, 1, LWA_ALPHA);
    ShowWindow(o, SW_SHOW);
    SetForegroundWindow(o);
    SetCursor(LoadCursorW(hinst(), 2 as PCWSTR));
}

unsafe fn about() {
    let Some((n, hk, ini)) = with(|a| (a.pins.len(), a.cfg.hotkeys.clone(), a.ini.display().to_string())) else { return };
    let mode = match install::mode() {
        install::Mode::Portable => t("portable", "portable"),
        install::Mode::User => t("installed for this user", "instalado para este usuario"),
        install::Mode::All => t("installed for all users", "instalado para todos"),
    };
    let body = format!(
        "DeskPins {} ({mode})\n\n{}: {n}\n\n{}\t{}\n{}\t{}\n{}\n\n{}:\n{ini}\n\nMIT · {}",
        env!("CARGO_PKG_VERSION"),
        t("Pinned windows", "Ventanas fijadas"),
        hk[0].0, t("pin mode (click a window)", "modo fijar (clic en una ventana)"),
        hk[1].0, t("pin/unpin the active window", "fijar/soltar la ventana activa"),
        t("Click a pin to remove it. Esc/right-click cancels pin mode.", "Clic en un pin para quitarlo. Esc/clic derecho cancela."),
        t("Settings", "Configuración"),
        t("Based on DeskPins by Elias Fotinis", "Basado en DeskPins de Elias Fotinis"),
    );
    install::msg(&body, MB_ICONINFORMATION);
}

unsafe fn tray_menu() {
    let Some((autopin, n)) = with(|a| (a.cfg.autopin, a.pins.len())) else { return };
    let m = CreatePopupMenu();
    let add = |id: usize, s: &str, flags: MENU_ITEM_FLAGS| AppendMenuW(m, flags, id, w(s).as_ptr());
    let check = |b: bool| if b { MF_CHECKED } else { MF_STRING };
    add(CMD_PINMODE, t("&Pin a window", "&Fijar una ventana"), MF_STRING);
    add(CMD_UNPINALL, t("&Unpin all", "&Quitar todos los pines"), if n == 0 { MF_GRAYED } else { MF_STRING });
    add(0, "", MF_SEPARATOR);
    add(CMD_AUTOPIN, t("&Autopin rules", "&Autofijar por reglas"), check(autopin));
    add(CMD_STARTUP, t("Start with &Windows", "Iniciar con &Windows"), check(install::autostart()));
    add(CMD_SETTINGS, t("&Settings…", "&Configuración…"), MF_STRING);
    if install::mode() == install::Mode::Portable {
        add(CMD_INSTALL_USER, t("&Install for me", "&Instalar para mí"), MF_STRING);
        add(CMD_INSTALL_ALL, t("Install for all &users…", "Instalar para &todos…"), MF_STRING);
    } else {
        add(CMD_UNINSTALL, t("Uninstall…", "Desinstalar…"), MF_STRING);
    }
    add(0, "", MF_SEPARATOR);
    add(CMD_ABOUT, t("A&bout", "Acerca &de"), MF_STRING);
    add(CMD_EXIT, t("E&xit", "&Salir"), MF_STRING);
    SetMenuDefaultItem(m, CMD_PINMODE as u32, 0);
    let mut pt = zeroed();
    GetCursorPos(&mut pt);
    SetForegroundWindow(main_wnd());
    let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, pt.x, pt.y, 0, main_wnd(), null());
    DestroyMenu(m);
    PostMessageW(main_wnd(), WM_NULL, 0, 0);
    if cmd > 0 {
        command(cmd as usize);
    }
}

/// Runs outside any state borrow (may show modal UI).
unsafe fn command(cmd: usize) {
    match cmd {
        CMD_PINMODE => pin_mode(),
        CMD_UNPINALL => {
            with(|a| a.unpin_all());
        }
        CMD_AUTOPIN => {
            with(|a| {
                a.cfg.autopin = !a.cfg.autopin;
                config::set(&a.ini, "AutoPin", "Enabled", if a.cfg.autopin { "1" } else { "0" });
                a.apply_autopin();
            });
        }
        CMD_STARTUP => install::set_autostart((!install::autostart()).then(install::exe).as_deref()),
        CMD_SETTINGS => {
            if let Some(ini) = with(|a| a.ini.clone()) {
                let main = main_wnd() as usize;
                std::thread::spawn(move || {
                    let _ = std::process::Command::new("notepad.exe").arg(ini).status();
                    unsafe { PostMessageW(main as HWND, WM_RELOAD, 0, 0) };
                });
            }
        }
        CMD_INSTALL_USER | CMD_INSTALL_ALL => match install::setup(cmd == CMD_INSTALL_ALL) {
            Ok(p) => {
                with(|a| a.relaunch = Some(p));
                DestroyWindow(main_wnd());
            }
            Err(e) => {
                install::msg(&e, MB_ICONERROR);
            }
        },
        CMD_UNINSTALL => {
            let _ = std::process::Command::new(install::exe()).arg("--uninstall").spawn();
        }
        CMD_ABOUT => about(),
        CMD_EXIT => {
            DestroyWindow(main_wnd());
        }
        _ => {}
    }
}

unsafe extern "system" fn main_proc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let done = match m {
        WM_TRAY => {
            match lp as u32 {
                WM_LBUTTONUP => pin_mode(),
                WM_RBUTTONUP => tray_menu(),
                _ => {}
            }
            Some(())
        }
        WM_COMMAND => Some(command(wp & 0xffff)),
        WM_HOTKEY => match wp {
            0 => Some(pin_mode()),
            _ => with(|a| {
                let fg = GetForegroundWindow();
                match a.find(fg) {
                    Some(i) => a.unpin(i),
                    None => a.pin(fg, false),
                }
            }),
        },
        WM_PICK => {
            let pt = POINT { x: wp as i32, y: lp as i32 };
            let target = GetAncestor(WindowFromPoint(pt), GA_ROOT);
            with(|a| match a.find(target) {
                Some(i) => a.unpin(i),
                None => a.pin(target, true),
            })
        }
        WM_SYNC => with(|a| {
            if let Some(i) = a.find(wp as HWND) {
                a.sync(i)
            }
        }),
        WM_TIMER => {
            KillTimer(h, wp);
            let target = wp as HWND;
            with(|a| {
                if IsWindowVisible(target) != 0 && a.cfg.matches(&text(target, InternalGetWindowText), &text(target, GetClassNameW)) {
                    a.pin(target, false);
                }
            })
        }
        WM_RELOAD => with(|a| {
            a.cfg = config::load(&a.ini);
            a.apply();
        }),
        WM_DESTROY => {
            with(|a| a.shutdown());
            PostQuitMessage(0);
            Some(())
        }
        _ if m == TASKBAR_CREATED.load(Relaxed) && m != 0 => with(|a| a.tray(NIM_ADD, None)),
        _ => return DefWindowProcW(h, m, wp, lp),
    };
    if done.is_none() {
        // State busy (re-entrant call): retry from the queue.
        PostMessageW(h, m, wp, lp);
    }
    0
}

unsafe extern "system" fn pin_proc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match m {
        WM_LBUTTONDOWN => {
            if with(|a| if let Some(i) = a.pins.iter().position(|p| p.wnd == h) { a.unpin(i) }).is_none() {
                PostMessageW(h, m, wp, lp);
            }
            0
        }
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        _ => DefWindowProcW(h, m, wp, lp),
    }
}

unsafe extern "system" fn overlay_proc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match m {
        WM_LBUTTONDOWN => {
            let mut pt = zeroed::<POINT>();
            GetCursorPos(&mut pt);
            DestroyWindow(h);
            PostMessageW(main_wnd(), WM_PICK, pt.x as usize, pt.y as isize);
        }
        WM_RBUTTONDOWN | WM_KEYDOWN | WM_SYSKEYDOWN | WM_CANCELMODE => {
            DestroyWindow(h);
        }
        WM_ACTIVATE if wp & 0xffff == WA_INACTIVE as usize => {
            DestroyWindow(h);
        }
        WM_DESTROY => OVERLAY.store(null_mut(), Relaxed),
        _ => return DefWindowProcW(h, m, wp, lp),
    }
    0
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        std::process::exit(install::cli(&args));
    }
    unsafe { run() }
}

unsafe fn run() {
    let mutex = CreateMutexW(null(), 0, w("Local\\deskpins-rs").as_ptr());
    if GetLastError() == ERROR_ALREADY_EXISTS {
        // Already running: hand over foreground rights and start pin mode there.
        let h = FindWindowW(w(MAIN_CLASS).as_ptr(), null());
        if !h.is_null() {
            let mut pid = 0;
            GetWindowThreadProcessId(h, &mut pid);
            AllowSetForegroundWindow(pid);
            PostMessageW(h, WM_COMMAND, CMD_PINMODE, 0);
        }
        return;
    }
    type WndProc = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;
    let classes: [(&str, WndProc, usize); 3] = [(MAIN_CLASS, main_proc, 0), (PIN_CLASS, pin_proc, 3), (OVERLAY_CLASS, overlay_proc, 2)];
    for (name, proc, cursor) in classes {
        let name = w(name);
        let mut wc: WNDCLASSW = zeroed();
        wc.lpfnWndProc = Some(proc);
        wc.hInstance = hinst();
        wc.lpszClassName = name.as_ptr();
        wc.hCursor = if cursor > 0 { LoadCursorW(hinst(), cursor as PCWSTR) } else { LoadCursorW(null_mut(), IDC_ARROW) };
        RegisterClassW(&wc);
    }
    let main = CreateWindowExW(0, w(MAIN_CLASS).as_ptr(), w("DeskPins").as_ptr(), WS_POPUP, 0, 0, 0, 0,
        null_mut(), null_mut(), hinst(), null());
    MAIN.store(main, Relaxed);
    TASKBAR_CREATED.store(RegisterWindowMessageW(w("TaskbarCreated").as_ptr()), Relaxed);

    let ini = config::path();
    let mut app = App { cfg: config::load(&ini), ini, pins: vec![], icon: null_mut(), autopin: null_mut(), relaunch: None };
    app.icon = pin_icon(app.cfg.color);
    app.tray(NIM_ADD, None);
    app.apply();
    APP.with(|a| *a.borrow_mut() = Some(app));

    let mut msg = zeroed();
    while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    let relaunch = APP.with(|a| a.borrow_mut().take()).and_then(|a| a.relaunch);
    CloseHandle(mutex);
    if let Some(p) = relaunch {
        let _ = std::process::Command::new(p).spawn();
    }
}
