//! The native window behind the reactor window: when it becomes the active
//! window or stops being it, and when it is closed. Reactor reports a
//! window's size and place but not these, so the window is subclassed for
//! them (as lwa_fm does it).

use std::cell::RefCell;
use std::rc::Rc;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, WM_ACTIVATE, WM_NCDESTROY};

/// Any number: the subclass is told apart by its procedure and this.
const SUBCLASS_ID: usize = 0xD0_30;
/// `WA_INACTIVE`: the low word of `WM_ACTIVATE`'s `wParam` when the window is left.
const INACTIVE: usize = 0;

/// What the window tells its watcher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    /// It became the active window, or another did.
    Active(bool),
    /// It is going away for good.
    Closing,
}

thread_local! {
    /// The one window's watcher (this app has one window).
    static WATCHER: RefCell<Option<Rc<dyn Fn(Signal)>>> = const { RefCell::new(None) };
}

/// Stops the watching when dropped.
pub struct Watch(isize);

impl Drop for Watch {
    fn drop(&mut self) {
        WATCHER.with(|watcher| *watcher.borrow_mut() = None);
        // SAFETY: removing a subclass that may already be gone only fails.
        unsafe {
            RemoveWindowSubclass(self.0 as HWND, Some(subclass), SUBCLASS_ID);
        }
    }
}

/// Calls `on_signal` on the UI thread as `window` gains and loses the focus
/// and when it closes; at once with whether it is active now. Call on the
/// window's thread.
pub fn watch(window: isize, on_signal: impl Fn(Signal) + 'static) -> Watch {
    let on_signal: Rc<dyn Fn(Signal)> = Rc::new(on_signal);
    WATCHER.with(|watcher| *watcher.borrow_mut() = Some(Rc::clone(&on_signal)));
    // SAFETY: `window` is a live window of this thread; the procedure lives
    // for the whole process.
    let active = unsafe {
        SetWindowSubclass(window as HWND, Some(subclass), SUBCLASS_ID, 0);
        GetForegroundWindow() as isize == window
    };
    on_signal(Signal::Active(active));
    Watch(window)
}

unsafe extern "system" fn subclass(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, _data: usize) -> LRESULT {
    let signal = match message {
        WM_ACTIVATE => Some(Signal::Active((wparam & 0xFFFF) != INACTIVE)),
        WM_NCDESTROY => Some(Signal::Closing),
        _ => None,
    };
    if let Some(signal) = signal {
        // Taken out of the cell first: the watcher may drop its `Watch`.
        let watcher = WATCHER.with(|watcher| watcher.borrow().clone());
        if let Some(watcher) = watcher {
            watcher(signal);
        }
    }
    // SAFETY: passing on the message this procedure was called with.
    unsafe { DefSubclassProc(window, message, wparam, lparam) }
}
