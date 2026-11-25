//! The way back to the UI thread from any other thread (as lwa_fm's shell
//! does it).
//!
//! Reactor messages can only be sent from the UI thread, and the core's
//! effects finish on Tokio's. A message-only window on the UI thread bridges
//! the two: [`wake`] posts to it, and its window procedure runs the installed
//! callback there. Wake-ups are coalesced, so a burst of deltas costs one
//! message.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DefWindowProcW, HWND_MESSAGE, PostMessageW, RegisterClassW, WM_APP, WNDCLASSW};

/// The message-only window, or 0 before [`install`].
static WINDOW: AtomicIsize = AtomicIsize::new(0);
/// A wake-up is posted and not yet delivered.
static PENDING: AtomicBool = AtomicBool::new(false);

thread_local! {
    static ON_WAKE: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
}

/// Makes the calling thread the one [`wake`] wakes; `on_wake` runs on it.
/// Returns whether the window could be made.
pub fn install(on_wake: impl Fn() + 'static) -> bool {
    ON_WAKE.with(|slot| *slot.borrow_mut() = Some(Rc::new(on_wake)));
    if WINDOW.load(Ordering::SeqCst) != 0 {
        return true;
    }
    let class_name: Vec<u16> = "DomovoiWake\0".encode_utf16().collect();
    // SAFETY: plain Win32 calls with valid arguments; the class and the window
    // live for the rest of the process, on this thread.
    let window = unsafe {
        let module = GetModuleHandleW(std::ptr::null());
        let class = WNDCLASSW { lpfnWndProc: Some(window_proc), hInstance: module, lpszClassName: class_name.as_ptr(), ..std::mem::zeroed() };
        if RegisterClassW(&raw const class) == 0 {
            return false;
        }
        CreateWindowExW(0, class_name.as_ptr(), class_name.as_ptr(), 0, 0, 0, 0, 0, HWND_MESSAGE, std::ptr::null_mut(), module, std::ptr::null())
    };
    if window.is_null() {
        return false;
    }
    WINDOW.store(window as isize, Ordering::SeqCst);
    true
}

/// Asks the UI thread to run its callback soon. Safe from any thread; calls
/// made before the callback ran are folded into one.
pub fn wake() {
    let window = WINDOW.load(Ordering::SeqCst);
    if window == 0 || PENDING.swap(true, Ordering::SeqCst) {
        return;
    }
    // SAFETY: posting to a window handle is allowed from any thread; a stale
    // handle only makes the call fail.
    let posted = unsafe { PostMessageW(window as HWND, WM_APP, 0, 0) };
    if posted == 0 {
        PENDING.store(false, Ordering::SeqCst);
    }
}

unsafe extern "system" fn window_proc(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if message == WM_APP {
        // Cleared first: a wake-up arriving while the callback runs posts again.
        PENDING.store(false, Ordering::SeqCst);
        let on_wake = ON_WAKE.with(|slot| slot.borrow().clone());
        if let Some(on_wake) = on_wake {
            on_wake();
        }
        return 0;
    }
    // SAFETY: forwarding the arguments this procedure was called with.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}
