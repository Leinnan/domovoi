//! Windows-only pieces of the status bar icon, after the same workarounds in
//! open_wihaister's tunnel: a stable identity for the icon, keeping its menu
//! above the Windows 11 taskbar, shutting down cleanly when the session
//! ends, and not leaving a console window behind.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::Console::{FreeConsole, GetConsoleProcessList};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows_sys::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, EVENT_SYSTEM_MENUPOPUPSTART, GetClassNameW, GetMessageW, HWND_TOPMOST, MSG, OBJID_WINDOW,
    RegisterClassExW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos, TranslateMessage, WINEVENT_OUTOFCONTEXT, WM_CLOSE, WM_ENDSESSION,
    WM_QUERYENDSESSION, WNDCLASSEXW, WS_EX_TOOLWINDOW,
};

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A stable identity for the icon, so Windows remembers "always show" for
/// it. Derived from the executable's path: Windows binds a GUID to the first
/// unsigned binary that registers it, so a fixed one would leave every other
/// build without an icon.
pub fn tray_guid_for_exe(exe: &Path) -> u128 {
    const NAMESPACE: uuid::Uuid = uuid::Uuid::from_u128(0x7a1e_55c0_3b2d_4f6a_9c41_d2e8_60b7_f513);
    // Windows paths are case-insensitive.
    uuid::Uuid::new_v5(&NAMESPACE, exe.to_string_lossy().to_lowercase().as_bytes()).as_u128()
}

/// Whether the taskbar is dark ("Windows mode"), which is what the icon sits
/// on; apps may be light at the same time.
pub fn taskbar_is_dark() -> bool {
    let key = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
    let value = wide("SystemUsesLightTheme");
    let mut light: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: the strings are NUL-terminated and outlive the call; `light`
    // and `size` are valid for writes of the sizes given.
    let status = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), (&raw mut light).cast(), &raw mut size)
    };
    // Unknown: Windows' default taskbar is dark.
    status != 0 || light == 0
}

/// With `--tray` started by double-click or at login, the console window
/// Windows made for this process alone is let go (it may flash briefly).
/// Started from a terminal, the console is shared and kept.
pub fn detach_own_console() {
    let mut pids = [0u32; 2];
    // SAFETY: `pids` is valid for two entries.
    let attached = unsafe { GetConsoleProcessList(pids.as_mut_ptr(), 2) };
    if attached == 1 {
        // SAFETY: no handles of ours refer to the console after this; logs
        // also go to the log file.
        unsafe { FreeConsole() };
    }
}

/// Window class of Win32 popup menus.
const POPUP_MENU_CLASS: &str = "#32768";

/// Keeps the icon's menu above the Windows 11 taskbar: started from the
/// Start menu, the popup can land below it (`tauri-apps/tray-icon#303`).
/// A WinEvent hook raises each popup menu as it appears. Unhooks on drop.
pub struct MenuZOrderHook(HWINEVENTHOOK);

impl MenuZOrderHook {
    /// Must be called on the thread that owns the icon.
    pub fn install() -> Option<Self> {
        // SAFETY: the callback is a plain function that lives for the whole
        // program; an out-of-context hook needs no module handle.
        let hook = unsafe {
            SetWinEventHook(
                EVENT_SYSTEM_MENUPOPUPSTART,
                EVENT_SYSTEM_MENUPOPUPSTART,
                std::ptr::null_mut(),
                Some(on_menu_popup_start),
                GetCurrentProcessId(),
                GetCurrentThreadId(),
                WINEVENT_OUTOFCONTEXT,
            )
        };
        (!hook.is_null()).then_some(Self(hook))
    }
}

impl Drop for MenuZOrderHook {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a live hook from `SetWinEventHook`.
        unsafe {
            UnhookWinEvent(self.0);
        }
    }
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    // SAFETY: `buf` is valid for `buf.len()` UTF-16 units.
    let len = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    usize::try_from(len).ok().and_then(|len| buf.get(..len)).map(String::from_utf16_lossy).unwrap_or_default()
}

unsafe extern "system" fn on_menu_popup_start(_hook: HWINEVENTHOOK, _event: u32, hwnd: HWND, id_object: i32, _id_child: i32, _thread: u32, _time: u32) {
    if hwnd.is_null() || id_object != OBJID_WINDOW || class_name(hwnd) != POPUP_MENU_CLASS {
        return;
    }
    // SAFETY: `hwnd` is a live window the system just told us about; the
    // flags change only its z-order.
    unsafe {
        SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    }
}

/// How long sign-out is held while the server shuts down; Windows ends the
/// process once `WM_ENDSESSION` returns.
const ENDSESSION_WAIT: Duration = Duration::from_secs(8);

struct SessionEnd {
    request_shutdown: Box<dyn Fn() + Send + Sync>,
    finished: Arc<AtomicBool>,
}

static SESSION_END: OnceLock<SessionEnd> = OnceLock::new();

/// Shuts down cleanly when Windows asks: sign-out, an installer's Restart
/// Manager and `taskkill` without `/F` talk to top-level windows, and winit
/// ignores the session messages. A hidden window on its own thread turns
/// them into the normal shutdown. `finished` becomes true once the server
/// has stopped.
pub fn watch_session_end(request_shutdown: impl Fn() + Send + Sync + 'static, finished: Arc<AtomicBool>) {
    if SESSION_END.set(SessionEnd { request_shutdown: Box::new(request_shutdown), finished }).is_err() {
        return;
    }
    if let Err(e) = std::thread::Builder::new().name("session-end".into()).spawn(run_window) {
        tracing::warn!("Cannot watch for the Windows session ending: {e}");
    }
}

fn run_window() {
    let class = wide("DomovoiServerSessionEnd");
    // SAFETY: the class name outlives registration and the window; the
    // window procedure is a plain function; `message` outlives its uses.
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let mut window_class: WNDCLASSEXW = std::mem::zeroed();
        window_class.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
        window_class.lpfnWndProc = Some(window_proc);
        window_class.hInstance = instance;
        window_class.lpszClassName = class.as_ptr();
        if RegisterClassExW(&raw const window_class) == 0 {
            return;
        }
        // Top-level and never shown: a message-only window gets no broadcasts.
        let window = CreateWindowExW(WS_EX_TOOLWINDOW, class.as_ptr(), class.as_ptr(), 0, 0, 0, 0, 0, std::ptr::null_mut(), std::ptr::null_mut(), instance, std::ptr::null());
        if window.is_null() {
            return;
        }
        let mut message: MSG = std::mem::zeroed();
        while GetMessageW(&raw mut message, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&raw const message);
            DispatchMessageW(&raw const message);
        }
    }
}

fn shut_down_and_wait() {
    let Some(handler) = SESSION_END.get() else { return };
    (handler.request_shutdown)();
    let deadline = Instant::now() + ENDSESSION_WAIT;
    while !handler.finished.load(Ordering::Acquire) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
}

unsafe extern "system" fn window_proc(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        // Agree to end; the work happens in WM_ENDSESSION.
        WM_QUERYENDSESSION => 1,
        WM_ENDSESSION => {
            if wparam != 0 {
                tracing::info!("Windows is ending the session; shutting down");
                shut_down_and_wait();
            }
            0
        }
        WM_CLOSE => {
            tracing::info!("Close requested by another program; shutting down");
            if let Some(handler) = SESSION_END.get() {
                (handler.request_shutdown)();
            }
            0
        }
        // SAFETY: forwards what Windows passed to this procedure.
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}
