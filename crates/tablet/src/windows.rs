//! Windows pointer hook for tablet data dropped by winit (such as the pen barrel button).
//!
//! winit 0.30 drops `POINTER_PEN_INFO::penFlags` (`PEN_FLAG_BARREL`) on Windows and turns pen
//! inputs into `WindowEvent::Touch`, mapping them to `PointerButton::Primary`. Windows Ink also
//! does not update `GetAsyncKeyState(VK_RBUTTON)` for stylus barrel buttons. This thread-local
//! `WH_GETMESSAGE` hook inspects `WM_POINTER*` messages on the UI thread's message queue to
//! track the physical barrel button state.

#![allow(unsafe_code, clippy::upper_case_acronyms)]

use std::sync::atomic::{AtomicBool, Ordering};

static BARREL_DOWN: AtomicBool = AtomicBool::new(false);

#[repr(C)]
#[derive(Default)]
struct POINT {
    x: i32,
    y: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct MOUSEMOVEPOINT {
    x: i32,
    y: i32,
    time: u32,
    extra_info: usize,
}

#[repr(C)]
struct MSG {
    hwnd: *mut std::ffi::c_void,
    message: u32,
    w_param: usize,
    l_param: isize,
    time: u32,
    pt: POINT,
}

#[repr(C)]
struct POINTER_INFO {
    pointer_type: u32,
    pointer_id: u32,
    frame_id: u32,
    pointer_flags: u32,
    source_device: *mut std::ffi::c_void,
    hwnd_target: *mut std::ffi::c_void,
    pt_pixel_location: POINT,
    pt_himetric_location: POINT,
    pt_pixel_location_raw: POINT,
    pt_himetric_location_raw: POINT,
    dw_time: u32,
    history_count: u32,
    input_data: i32,
    dw_key_states: u32,
    performance_count: u64,
    button_change_type: i32,
}

#[repr(C)]
struct POINTER_PEN_INFO {
    pointer_info: POINTER_INFO,
    pen_flags: u32,
    pen_mask: u32,
    pressure: u32,
    rotation: u32,
    tilt_x: i32,
    tilt_y: i32,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn SetWindowsHookExW(
        id_hook: i32,
        lpfn: unsafe extern "system" fn(i32, usize, isize) -> isize,
        h_mod: *mut std::ffi::c_void,
        dw_thread_id: u32,
    ) -> *mut std::ffi::c_void;
    fn UnhookWindowsHookEx(hhk: *mut std::ffi::c_void) -> i32;
    fn CallNextHookEx(hhk: *mut std::ffi::c_void, n_code: i32, w_param: usize, l_param: isize) -> isize;
    fn GetCurrentThreadId() -> u32;
    fn GetPointerPenInfo(pointer_id: u32, pen_info: *mut POINTER_PEN_INFO) -> i32;
    fn RegisterRawInputDevices(devices: *const RAWINPUTDEVICE, count: u32, size: u32) -> i32;
    fn GetLastError() -> u32;
    fn LoadCursorW(instance: *mut std::ffi::c_void, name: *const u16) -> *mut std::ffi::c_void;
    fn SetCursor(cursor: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    fn GetCursorPos(point: *mut POINT) -> i32;
    fn GetMouseMovePointsEx(cb_size: u32, lppt: *const MOUSEMOVEPOINT, lppt_buf: *mut MOUSEMOVEPOINT, n_buf_points: i32, resolution: u32) -> i32;
}

/// `GMMP_USE_DISPLAY_POINTS` (winuser.h): history in screen pixels.
const GMMP_USE_DISPLAY_POINTS: u32 = 1;
/// The most moves Windows keeps (`GetMouseMovePointsEx`).
const MOUSE_HISTORY: usize = 64;

/// The cursor's latest moves, newest first, in screen pixels: what Windows keeps of the moves it
/// merged into one `WM_MOUSEMOVE` while the window was busy (a frame painting a heavy brush; user
/// report: a circle came out a polygon). Empty if the system has none for the cursor's place (a
/// pen sending pointer messages, not mouse moves).
pub fn mouse_history() -> Vec<crate::MovePoint> {
    let mut now = POINT::default();
    // SAFETY: GetCursorPos writes the cursor's screen position into a valid POINT.
    if unsafe { GetCursorPos(&mut now) } == 0 {
        return Vec::new();
    }
    // The history is looked up by the low 16 bits of the coordinates (winuser.h's sample).
    let at = MOUSEMOVEPOINT { x: now.x & 0xFFFF, y: now.y & 0xFFFF, time: 0, extra_info: 0 };
    let mut buf = [MOUSEMOVEPOINT::default(); MOUSE_HISTORY];
    // SAFETY: `at` is a valid MOUSEMOVEPOINT, `buf` holds MOUSE_HISTORY of them, and both sizes
    // are given; the call writes at most that many and returns how many (or -1).
    let n = unsafe { GetMouseMovePointsEx(std::mem::size_of::<MOUSEMOVEPOINT>() as u32, &at, buf.as_mut_ptr(), MOUSE_HISTORY as i32, GMMP_USE_DISPLAY_POINTS) };
    let Ok(n) = usize::try_from(n) else { return Vec::new() };
    // Coordinates come back as 16-bit values: left of or above the main display is negative.
    let signed = |v: i32| if v > 32767 { v - 65536 } else { v };
    buf.iter().take(n).map(|p| crate::MovePoint { x: signed(p.x), y: signed(p.y), time: p.time }).collect()
}

/// `IDC_WAIT` (winuser.h): the system's busy cursor.
const IDC_WAIT: usize = 32514;

/// The cursor [`wait_cursor`] replaced, while it shows (as an address: the handle is only ever
/// passed back to `SetCursor` on the UI thread).
static BEFORE_WAIT: std::sync::Mutex<Option<usize>> = std::sync::Mutex::new(None);

/// Show the system's busy cursor at once (`true`), or the cursor it replaced again (`false`).
/// For work about to hold the UI thread: winit sets the cursor only while it handles messages, so
/// one set from a frame would never show during that frame. UI thread only.
pub fn wait_cursor(on: bool) {
    let Ok(mut before) = BEFORE_WAIT.lock() else { return };
    match (on, *before) {
        (true, None) => {
            // SAFETY: a null instance with a predefined `IDC_` id loads a shared system cursor,
            // which is never destroyed; SetCursor takes any cursor handle and returns the one it
            // replaced (null for none).
            let wait = unsafe { LoadCursorW(std::ptr::null_mut(), IDC_WAIT as *const u16) };
            if !wait.is_null() {
                // SAFETY: see above.
                *before = Some(unsafe { SetCursor(wait) } as usize);
            }
        }
        (false, Some(old)) => {
            // SAFETY: `old` is the handle SetCursor returned on this thread (null hides the
            // cursor again, as it was).
            unsafe { SetCursor(old as *mut std::ffi::c_void) };
            *before = None;
        }
        _ => {}
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RAWINPUTDEVICE {
    us_usage_page: u16,
    us_usage: u16,
    dw_flags: u32,
    hwnd_target: *mut std::ffi::c_void,
}

/// `HID_USAGE_PAGE_GENERIC` / `HID_USAGE_GENERIC_MOUSE` (hidusage.h).
const HID_USAGE_PAGE_GENERIC: u16 = 0x01;
const HID_USAGE_GENERIC_MOUSE: u16 = 0x02;
/// `RIDEV_REMOVE` (winuser.h): stop raw input from this device class (needs a null target).
const RIDEV_REMOVE: u32 = 0x0000_0001;

/// Stop the raw mouse input (`WM_INPUT`) winit registers this process for. winit asks for every
/// mouse's raw reports by default, for `DeviceEvent`s PhotoCraft doesn't use, and eframe has no
/// way to turn that off. A gaming mouse sends 1000-8000 reports a second, each queued as a
/// `WM_INPUT`, and Windows only sends `WM_PAINT` while the queue is empty: frames starved while
/// the mouse moved (user report: panning lagged with the mouse, not with a moved cursor; a pan at
/// 159 fps fell to 59 fps at 1 kHz and 14 fps at 8 kHz). Ordinary mouse messages, which egui
/// reads, are coalesced and unaffected. Call it on the UI thread once the event loop exists.
pub fn stop_raw_mouse_input() -> Result<(), crate::Error> {
    let device =
        RAWINPUTDEVICE { us_usage_page: HID_USAGE_PAGE_GENERIC, us_usage: HID_USAGE_GENERIC_MOUSE, dw_flags: RIDEV_REMOVE, hwnd_target: std::ptr::null_mut() };
    // SAFETY: one valid, initialised RAWINPUTDEVICE with its own size; RIDEV_REMOVE with a null
    // target is the documented way to unregister a device class. GetLastError has no
    // preconditions.
    let ok = unsafe { RegisterRawInputDevices(&device, 1, std::mem::size_of::<RAWINPUTDEVICE>() as u32) };
    if ok == 0 {
        // SAFETY: see above.
        let code = unsafe { GetLastError() };
        return Err(crate::Error::Platform(format!("RegisterRawInputDevices (remove the mouse) failed: error {code}")));
    }
    Ok(())
}

const WH_GETMESSAGE: i32 = 3;
const WM_POINTERUPDATE: u32 = 0x0245;
const WM_POINTERDOWN: u32 = 0x0246;
const WM_POINTERUP: u32 = 0x0247;
const WM_POINTERLEAVE: u32 = 0x024A;

/// `PEN_FLAG_BARREL` (winuser.h): the barrel button of the pen is pressed.
pub const PEN_FLAG_BARREL: u32 = 0x00000001;

/// `POINTER_MESSAGE_FLAG_SECONDBUTTON` (winuser.h): the secondary action button is pressed.
pub const POINTER_FLAG_SECONDBUTTON: u32 = 0x00000020;

/// Pure decode of whether the stylus barrel button is pressed.
///
/// Plain tip contact sets `POINTER_FLAG_INCONTACT` (`0x0004`) and optionally `POINTER_FLAG_FIRSTBUTTON`
/// (`0x0010`), which must never be confused with the barrel button (`POINTER_FLAG_SECONDBUTTON`, `0x0020`).
pub fn is_barrel_pressed(pen_flags: Option<u32>, pointer_message_flags: u32) -> bool {
    let second_button = (pointer_message_flags & POINTER_FLAG_SECONDBUTTON) != 0;
    match pen_flags {
        Some(pen) => (pen & PEN_FLAG_BARREL) != 0 || second_button,
        None => second_button,
    }
}

unsafe extern "system" fn get_msg_proc(code: i32, w_param: usize, l_param: isize) -> isize {
    if code >= 0 && l_param != 0 {
        // SAFETY: Win32 guarantees that for code >= 0, l_param points to a valid MSG struct
        // dispatched to this thread's message queue.
        let msg = unsafe { &*(l_param as *const MSG) };
        match msg.message {
            WM_POINTERDOWN | WM_POINTERUPDATE => {
                let pointer_id = (msg.w_param & 0xFFFF) as u32;
                let pointer_message_flags = (msg.w_param >> 16) as u32;
                // SAFETY: `pen_info` is a stack-allocated buffer initialized to zero, and `pointer_id`
                // is the pointer identifier from the valid WM_POINTER message.
                let pen_flags = unsafe {
                    let mut info: POINTER_PEN_INFO = std::mem::zeroed();
                    (GetPointerPenInfo(pointer_id, &mut info) != 0).then_some(info.pen_flags)
                };
                let is_barrel = is_barrel_pressed(pen_flags, pointer_message_flags);
                BARREL_DOWN.store(is_barrel, Ordering::Relaxed);
            }
            WM_POINTERUP | WM_POINTERLEAVE => {
                BARREL_DOWN.store(false, Ordering::Relaxed);
            }
            _ => {}
        }
    }
    // SAFETY: CallNextHookEx passes the parameters down the hook chain. MSDN notes the first
    // parameter is ignored and can be null.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, w_param, l_param) }
}

/// Keeps the thread-local pointer message hook installed; dropping it removes the hook.
/// Main-thread only (not `Send`).
pub struct Monitor {
    hook: *mut std::ffi::c_void,
}

impl Monitor {
    /// Installs the thread-local pointer message hook on the UI thread.
    pub fn install() -> Result<Self, crate::Error> {
        // SAFETY: GetCurrentThreadId has no preconditions. SetWindowsHookExW installs
        // a thread-specific WH_GETMESSAGE hook on the calling thread's message queue
        // using our extern "system" callback.
        let hook = unsafe {
            let thread_id = GetCurrentThreadId();
            SetWindowsHookExW(WH_GETMESSAGE, get_msg_proc, std::ptr::null_mut(), thread_id)
        };
        if hook.is_null() {
            return Err(crate::Error::Platform("SetWindowsHookExW returned null".into()));
        }
        Ok(Self { hook })
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        // SAFETY: `self.hook` is the non-null handle returned by `SetWindowsHookExW`,
        // unhooked exactly once here on the thread that created it.
        unsafe {
            UnhookWindowsHookEx(self.hook);
        }
        BARREL_DOWN.store(false, Ordering::Relaxed);
    }
}

/// Returns true if the stylus barrel button is physically pressed.
pub fn is_barrel_button_down() -> bool {
    BARREL_DOWN.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetRegisteredRawInputDevices(devices: *mut RAWINPUTDEVICE, count: *mut u32, size: u32) -> u32;
    }

    /// The raw input device classes this process is registered for.
    fn registered() -> Vec<(u16, u16)> {
        let size = std::mem::size_of::<RAWINPUTDEVICE>() as u32;
        let mut count = 0u32;
        // SAFETY: a null buffer asks for the count only.
        unsafe { GetRegisteredRawInputDevices(std::ptr::null_mut(), &mut count, size) };
        let empty = RAWINPUTDEVICE { us_usage_page: 0, us_usage: 0, dw_flags: 0, hwnd_target: std::ptr::null_mut() };
        let mut buf = vec![empty; count as usize];
        // SAFETY: `buf` holds `count` devices of `size` bytes each.
        let n = unsafe { GetRegisteredRawInputDevices(buf.as_mut_ptr(), &mut count, size) };
        buf.truncate(if n == u32::MAX { 0 } else { n as usize });
        buf.iter().map(|d| (d.us_usage_page, d.us_usage)).collect()
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetCursor() -> *mut std::ffi::c_void;
    }

    /// The busy cursor shows at once and the one it replaced comes back; a second `true` or
    /// `false` changes nothing.
    #[test]
    fn the_wait_cursor_shows_and_the_old_one_comes_back() {
        // SAFETY: GetCursor and LoadCursorW (a shared system cursor) have no preconditions.
        let (before, wait) = unsafe { (GetCursor(), LoadCursorW(std::ptr::null_mut(), IDC_WAIT as *const u16)) };
        wait_cursor(true);
        wait_cursor(true);
        // SAFETY: see above.
        assert_eq!(unsafe { GetCursor() }, wait);
        wait_cursor(false);
        // SAFETY: see above.
        assert_eq!(unsafe { GetCursor() }, before);
        wait_cursor(false);
        // SAFETY: see above.
        assert_eq!(unsafe { GetCursor() }, before);
    }

    /// winit's raw mouse registration (as it makes it) is removed; nothing else is touched.
    #[test]
    fn raw_mouse_input_is_unregistered() {
        let size = std::mem::size_of::<RAWINPUTDEVICE>() as u32;
        let mouse = (HID_USAGE_PAGE_GENERIC, HID_USAGE_GENERIC_MOUSE);
        let keyboard = (HID_USAGE_PAGE_GENERIC, 0x06);
        let devices =
            [mouse, keyboard].map(|(page, usage)| RAWINPUTDEVICE { us_usage_page: page, us_usage: usage, dw_flags: 0, hwnd_target: std::ptr::null_mut() });
        // SAFETY: two initialised devices of `size` bytes, no target window (the focus window).
        assert_ne!(unsafe { RegisterRawInputDevices(devices.as_ptr(), 2, size) }, 0);
        assert!(registered().contains(&mouse));
        stop_raw_mouse_input().unwrap();
        let now = registered();
        assert!(!now.contains(&mouse), "{now:?}");
        assert!(now.contains(&keyboard), "the keyboard stays registered: {now:?}");
        // Again, with nothing to remove: no error.
        let again = stop_raw_mouse_input();
        assert!(again.is_ok(), "{again:?}");
    }

    const POINTER_FLAG_INCONTACT: u32 = 0x0004;
    const POINTER_FLAG_FIRSTBUTTON: u32 = 0x0010;

    #[test]
    fn the_mouse_history_ends_where_the_cursor_is() {
        let history = mouse_history();
        assert!(history.len() <= MOUSE_HISTORY);
        if let Some(newest) = history.first() {
            let mut now = POINT::default();
            // SAFETY: GetCursorPos writes into a valid POINT.
            unsafe { GetCursorPos(&mut now) };
            assert!((newest.x - now.x).abs() <= 1 && (newest.y - now.y).abs() <= 1, "{newest:?} vs ({}, {})", now.x, now.y);
            assert!(history.windows(2).all(|w| w[0].time.wrapping_sub(w[1].time) < 1 << 31), "newest first: {history:?}");
        }
    }

    #[test]
    fn plain_pen_contact_is_not_treated_as_barrel() {
        // Plain tip touch on digitizer: INCONTACT is set, first button may be set, no barrel flag.
        assert!(!is_barrel_pressed(Some(0), POINTER_FLAG_INCONTACT));
        assert!(!is_barrel_pressed(Some(0), POINTER_FLAG_INCONTACT | POINTER_FLAG_FIRSTBUTTON));
        assert!(!is_barrel_pressed(None, POINTER_FLAG_INCONTACT));
    }

    #[test]
    fn pen_barrel_flag_is_detected() {
        assert!(is_barrel_pressed(Some(PEN_FLAG_BARREL), 0));
        assert!(is_barrel_pressed(Some(PEN_FLAG_BARREL), POINTER_FLAG_INCONTACT));
    }

    #[test]
    fn pointer_message_second_button_is_detected() {
        assert!(is_barrel_pressed(None, POINTER_FLAG_SECONDBUTTON));
        assert!(is_barrel_pressed(Some(0), POINTER_FLAG_SECONDBUTTON));
        assert!(is_barrel_pressed(Some(0), POINTER_FLAG_INCONTACT | POINTER_FLAG_SECONDBUTTON));
    }
}
