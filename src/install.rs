use crate::{t, w, MAIN_CLASS};
use std::ffi::c_void;
use std::mem::{size_of, transmute, zeroed};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::{env, fs, process::Command};
use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::Com::*;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const UNINST: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\deskpins-rs";
const EXE: &str = "DeskPins.exe";

#[derive(PartialEq, Clone, Copy)]
pub enum Mode {
    Portable,
    User,
    All,
}

fn var(k: &str) -> PathBuf {
    env::var_os(k).map(PathBuf::from).unwrap_or_default()
}

pub fn dir(all: bool) -> PathBuf {
    if all { var("ProgramFiles") } else { var("LOCALAPPDATA").join("Programs") }.join("deskpins-rs")
}

fn lnk(all: bool) -> PathBuf {
    if all { var("ProgramData") } else { var("APPDATA") }.join(r"Microsoft\Windows\Start Menu\Programs\DeskPins-rs.lnk")
}

pub fn exe() -> PathBuf {
    env::current_exe().unwrap_or_default()
}

fn same(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

pub fn mode() -> Mode {
    let d = exe().parent().map(Path::to_path_buf).unwrap_or_default();
    if same(&d, &dir(false)) {
        Mode::User
    } else if same(&d, &dir(true)) {
        Mode::All
    } else {
        Mode::Portable
    }
}

pub fn msg(text: &str, flags: MESSAGEBOX_STYLE) -> i32 {
    unsafe { MessageBoxW(null_mut(), w(text).as_ptr(), w("DeskPins").as_ptr(), flags | MB_SETFOREGROUND) }
}

fn reg_sz(root: HKEY, key: &str, name: &str, val: &str) {
    let v = w(val);
    unsafe { RegSetKeyValueW(root, w(key).as_ptr(), w(name).as_ptr(), REG_SZ, v.as_ptr().cast(), (v.len() * 2) as u32) };
}

fn reg_dw(root: HKEY, key: &str, name: &str, val: u32) {
    unsafe { RegSetKeyValueW(root, w(key).as_ptr(), w(name).as_ptr(), REG_DWORD, (&val as *const u32).cast(), 4) };
}

pub fn autostart() -> bool {
    unsafe {
        RegGetValueW(HKEY_CURRENT_USER, w(RUN).as_ptr(), w("deskpins-rs").as_ptr(), RRF_RT_REG_SZ, null_mut(), null_mut(), null_mut()) == 0
    }
}

pub fn set_autostart(exe: Option<&Path>) {
    match exe {
        Some(p) => reg_sz(HKEY_CURRENT_USER, RUN, "deskpins-rs", &format!("\"{}\"", p.display())),
        None => unsafe {
            RegDeleteKeyValueW(HKEY_CURRENT_USER, w(RUN).as_ptr(), w("deskpins-rs").as_ptr());
        },
    }
}

fn is_admin() -> bool {
    unsafe { IsUserAnAdmin() != 0 }
}

/// Run this exe elevated with `args` and wait; true if it exited with 0.
fn elevate(args: &str) -> bool {
    let (verb, file, params) = (w("runas"), w(&exe().to_string_lossy()), w(args));
    unsafe {
        let mut sei: SHELLEXECUTEINFOW = zeroed();
        sei.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
        sei.fMask = SEE_MASK_NOCLOSEPROCESS;
        sei.lpVerb = verb.as_ptr();
        sei.lpFile = file.as_ptr();
        sei.lpParameters = params.as_ptr();
        sei.nShow = SW_SHOWNORMAL;
        if ShellExecuteExW(&mut sei) == 0 || sei.hProcess.is_null() {
            return false;
        }
        WaitForSingleObject(sei.hProcess, INFINITE);
        let mut code = 1;
        GetExitCodeProcess(sei.hProcess, &mut code);
        CloseHandle(sei.hProcess);
        code == 0
    }
}

/// Ask other DeskPins instances to quit (they unpin everything) and wait for them.
fn close_running() {
    let cls = w(MAIN_CLASS);
    unsafe {
        let me = GetCurrentProcessId();
        let mut h = FindWindowExW(null_mut(), null_mut(), cls.as_ptr(), null());
        while !h.is_null() {
            let mut pid = 0;
            GetWindowThreadProcessId(h, &mut pid);
            if pid != me {
                let proc = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
                PostMessageW(h, WM_CLOSE, 0, 0);
                if !proc.is_null() {
                    WaitForSingleObject(proc, 3000);
                    CloseHandle(proc);
                }
            }
            h = FindWindowExW(null_mut(), h, cls.as_ptr(), null());
        }
    }
}

/// Create a .lnk via IShellLinkW (raw vtable calls; windows-sys has no COM wrappers).
unsafe fn shortcut(lnk: &Path, target: &Path) -> bool {
    const CLSID_SHELL_LINK: GUID = GUID::from_u128(0x00021401_0000_0000_c000_000000000046);
    const IID_ISHELL_LINK_W: GUID = GUID::from_u128(0x000214f9_0000_0000_c000_000000000046);
    const IID_IPERSIST_FILE: GUID = GUID::from_u128(0x0000010b_0000_0000_c000_000000000046);
    type Qi = unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32;
    type Release = unsafe extern "system" fn(*mut c_void) -> u32;
    type SetStr = unsafe extern "system" fn(*mut c_void, *const u16) -> i32;
    type Save = unsafe extern "system" fn(*mut c_void, *const u16, i32) -> i32;
    let slot = |obj: *mut c_void, i: usize| *(*(obj as *const *const usize)).add(i);

    CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32);
    let mut sl = null_mut();
    if CoCreateInstance(&CLSID_SHELL_LINK, null_mut(), CLSCTX_INPROC_SERVER, &IID_ISHELL_LINK_W, &mut sl) != 0 {
        return false;
    }
    let dir = target.parent().unwrap_or(target);
    transmute::<usize, SetStr>(slot(sl, 20))(sl, w(&target.to_string_lossy()).as_ptr()); // SetPath
    transmute::<usize, SetStr>(slot(sl, 9))(sl, w(&dir.to_string_lossy()).as_ptr()); // SetWorkingDirectory
    let mut pf = null_mut();
    let ok = transmute::<usize, Qi>(slot(sl, 0))(sl, &IID_IPERSIST_FILE, &mut pf) == 0 && {
        let r = transmute::<usize, Save>(slot(pf, 6))(pf, w(&lnk.to_string_lossy()).as_ptr(), 1) == 0;
        transmute::<usize, Release>(slot(pf, 2))(pf);
        r
    };
    transmute::<usize, Release>(slot(sl, 2))(sl);
    ok
}

fn install_files(all: bool) -> Result<PathBuf, String> {
    let d = dir(all);
    let target = d.join(EXE);
    fs::create_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
    let src = exe();
    if !same(&src, &target) {
        fs::copy(&src, &target).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    let root = if all { HKEY_LOCAL_MACHINE } else { HKEY_CURRENT_USER };
    let tgt = target.display().to_string();
    reg_sz(root, UNINST, "DisplayName", "DeskPins-rs");
    reg_sz(root, UNINST, "DisplayVersion", env!("CARGO_PKG_VERSION"));
    reg_sz(root, UNINST, "Publisher", "DeskPins");
    reg_sz(root, UNINST, "DisplayIcon", &tgt);
    reg_sz(root, UNINST, "InstallLocation", &d.display().to_string());
    reg_sz(root, UNINST, "UninstallString", &format!("\"{tgt}\" --uninstall"));
    reg_dw(root, UNINST, "NoModify", 1);
    reg_dw(root, UNINST, "NoRepair", 1);
    reg_dw(root, UNINST, "EstimatedSize", (fs::metadata(&target).map(|m| m.len()).unwrap_or(0) / 1024) as u32 + 1);
    unsafe { shortcut(&lnk(all), &target) };
    Ok(target)
}

/// Install for the current user or all users (self-elevates). Returns the installed exe.
pub fn setup(all: bool) -> Result<PathBuf, String> {
    let target = if all && !is_admin() {
        if !elevate("--install-all --elevated") {
            return Err(t("Installation cancelled or failed.", "Instalación cancelada o fallida.").into());
        }
        dir(true).join(EXE)
    } else {
        install_files(all)?
    };
    set_autostart(Some(&target));
    // Old name's autostart entry would launch a second copy at login.
    unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, w(RUN).as_ptr(), w("DeskPins2").as_ptr()) };
    // Carry portable settings over to the installed location.
    let (old, new) = (exe().with_file_name("deskpins.ini"), crate::config::user_path());
    if old.exists() && !new.exists() {
        let _ = fs::create_dir_all(new.parent().unwrap());
        let _ = fs::copy(old, new);
    }
    Ok(target)
}

/// Delete only our exe (retrying ~60 s until it has exited), then the folder if it is empty.
fn remove_dir_later(d: &Path) {
    let (d, f) = (d.display(), d.join(EXE));
    let f = f.display();
    let _ = Command::new("cmd.exe")
        .raw_arg(format!("/d /c for /l %i in (1,1,30) do @(ping -n 3 127.0.0.1 >nul & del /f /q \"{f}\" 2>nul & if not exist \"{f}\" (rd \"{d}\" 2>nul & exit))"))
        .current_dir(env::temp_dir())
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .spawn();
}

fn uninstall(child: bool) -> i32 {
    let all = match mode() {
        Mode::Portable => {
            msg(t("This is a portable copy; nothing to uninstall.\nJust delete DeskPins.exe and deskpins.ini.",
                  "Esta es una copia portable; no hay nada que desinstalar.\nBorra DeskPins.exe y deskpins.ini."), MB_ICONINFORMATION);
            return 1;
        }
        m => m == Mode::All,
    };
    if !child && msg(t("Uninstall DeskPins?", "¿Desinstalar DeskPins?"), MB_YESNO | MB_ICONQUESTION) != IDYES {
        return 1;
    }
    if all && !is_admin() {
        if !elevate("--uninstall --elevated") {
            return 1;
        }
    } else {
        close_running();
        let root = if all { HKEY_LOCAL_MACHINE } else { HKEY_CURRENT_USER };
        unsafe { RegDeleteTreeW(root, w(UNINST).as_ptr()) };
        let _ = fs::remove_file(lnk(all));
        remove_dir_later(&dir(all));
    }
    if child {
        return 0;
    }
    set_autostart(None);
    msg(t("DeskPins was uninstalled.\nYour settings remain in %APPDATA%\\deskpins-rs.",
          "DeskPins se desinstaló.\nTu configuración sigue en %APPDATA%\\deskpins-rs."), MB_ICONINFORMATION);
    0
}

pub fn cli(args: &[String]) -> i32 {
    let child = args.iter().any(|a| a == "--elevated");
    match args[0].as_str() {
        a @ ("--install" | "--install-all") => {
            if !child {
                close_running();
            }
            match setup(a == "--install-all") {
                Ok(p) => {
                    if !child {
                        let _ = Command::new(&p).spawn();
                        msg(&format!("{}\n{}", t("DeskPins installed in:", "DeskPins instalado en:"), p.parent().unwrap().display()), MB_ICONINFORMATION);
                    }
                    0
                }
                Err(e) => {
                    if !child {
                        msg(&e, MB_ICONERROR);
                    }
                    1
                }
            }
        }
        "--uninstall" => uninstall(child),
        _ => {
            msg("DeskPins.exe [--install | --install-all | --uninstall]\n\n\
                 (none)         portable / portable\n\
                 --install      current user / usuario actual\n\
                 --install-all  all users / todos (admin)\n\
                 --uninstall", MB_ICONINFORMATION);
            2
        }
    }
}
