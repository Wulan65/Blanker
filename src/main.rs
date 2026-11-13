#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(windows)]
mod screensaver {
    use std::ffi::c_void;
    use std::io;
    use std::mem;
    use std::ptr;
    use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};

    type Bool = i32;
    type Dword = u32;
    type Handle = *mut c_void;
    type Hinstance = Handle;
    type Hcursor = Handle;
    type Hicon = Handle;
    type Hbrush = Handle;
    type Hwnd = Handle;
    type Lparam = isize;
    type Lresult = isize;
    type Uint = u32;
    type Wparam = usize;

    const WS_EX_TOPMOST: Dword = 0x0000_0008;
    const WS_EX_TOOLWINDOW: Dword = 0x0000_0080;
    const WS_POPUP: Dword = 0x8000_0000;
    const SW_SHOW: i32 = 5;

    const WM_DESTROY: Uint = 0x0002;
    const WM_CLOSE: Uint = 0x0010;
    const WM_SETCURSOR: Uint = 0x0020;
    const WM_KEYDOWN: Uint = 0x0100;
    const WM_SYSKEYDOWN: Uint = 0x0104;
    const WM_APP: Uint = 0x8000;
    const WM_MOUSE_WAKE: Uint = WM_APP + 1;
    const WM_MOUSE_WAKE_COMPLETE: Uint = WM_APP + 2;
    const WH_MOUSE_LL: i32 = 14;
    const BLACK_BRUSH: i32 = 4;
    const SM_XVIRTUALSCREEN: i32 = 76;
    const SM_YVIRTUALSCREEN: i32 = 77;
    const SM_CXVIRTUALSCREEN: i32 = 78;
    const SM_CYVIRTUALSCREEN: i32 = 79;

    static WINDOW_HANDLE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
    static MOUSE_BUTTONS_HELD: AtomicUsize = AtomicUsize::new(0);
    static MOUSE_WAKE_PENDING: AtomicBool = AtomicBool::new(false);

    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    struct MouseHookData {
        point: Point,
        mouse_data: Dword,
        flags: Dword,
        time: Dword,
        extra_info: usize,
    }

    #[repr(C)]
    struct Message {
        hwnd: Hwnd,
        message: Uint,
        w_param: Wparam,
        l_param: Lparam,
        time: Dword,
        point: Point,
        private: Dword,
    }

    type WindowProc = Option<unsafe extern "system" fn(Hwnd, Uint, Wparam, Lparam) -> Lresult>;
    type HookProc = Option<unsafe extern "system" fn(i32, Wparam, Lparam) -> Lresult>;

    #[repr(C)]
    struct WindowClass {
        style: Uint,
        window_proc: WindowProc,
        class_extra: i32,
        window_extra: i32,
        instance: Hinstance,
        icon: Hicon,
        cursor: Hcursor,
        background: Hbrush,
        menu_name: *const u16,
        class_name: *const u16,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn CreateWindowExW(
            ex_style: Dword,
            class_name: *const u16,
            window_name: *const u16,
            style: Dword,
            x: i32,
            y: i32,
            width: i32,
            height: i32,
            parent: Hwnd,
            menu: Handle,
            instance: Hinstance,
            parameter: *mut c_void,
        ) -> Hwnd;
        fn DefWindowProcW(hwnd: Hwnd, message: Uint, w_param: Wparam, l_param: Lparam) -> Lresult;
        fn DispatchMessageW(message: *const Message) -> Lresult;
        fn DestroyWindow(hwnd: Hwnd) -> Bool;
        fn GetMessageW(message: *mut Message, hwnd: Hwnd, min: Uint, max: Uint) -> Bool;
        fn GetSystemMetrics(index: i32) -> i32;
        fn LoadCursorW(instance: Hinstance, cursor_name: *const u16) -> Hcursor;
        fn PostQuitMessage(exit_code: i32);
        fn PostMessageW(hwnd: Hwnd, message: Uint, w_param: Wparam, l_param: Lparam) -> Bool;
        fn RegisterClassW(window_class: *const WindowClass) -> u16;
        fn SetCursor(cursor: Hcursor) -> Hcursor;
        fn SetForegroundWindow(hwnd: Hwnd) -> Bool;
        fn ShowWindow(hwnd: Hwnd, command: i32) -> Bool;
        fn TranslateMessage(message: *const Message) -> Bool;
        fn UnregisterClassW(class_name: *const u16, instance: Hinstance) -> Bool;
        fn UpdateWindow(hwnd: Hwnd) -> Bool;
        fn SetWindowsHookExW(
            hook_id: i32,
            hook_proc: HookProc,
            instance: Hinstance,
            thread_id: Dword,
        ) -> Handle;
        fn UnhookWindowsHookEx(hook: Handle) -> Bool;
        fn CallNextHookEx(hook: Handle, code: i32, w_param: Wparam, l_param: Lparam) -> Lresult;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(module_name: *const u16) -> Hinstance;
    }

    fn should_wake(message: Uint) -> bool {
        matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN)
    }

    fn mouse_button(message: Uint, mouse_data: Dword) -> Option<(usize, bool)> {
        match message {
            0x0201 => Some((1, true)),
            0x0202 => Some((1, false)),
            0x0204 => Some((2, true)),
            0x0205 => Some((2, false)),
            0x0207 => Some((4, true)),
            0x0208 => Some((4, false)),
            0x020B | 0x020C => match mouse_data >> 16 {
                1 => Some((8, message == 0x020B)),
                2 => Some((16, message == 0x020B)),
                _ => None,
            },
            _ => None,
        }
    }

    unsafe extern "system" fn mouse_hook_proc(
        code: i32,
        w_param: Wparam,
        l_param: Lparam,
    ) -> Lresult {
        if code >= 0 {
            let data = unsafe { &*(l_param as *const MouseHookData) };
            if let Some((button, is_down)) = mouse_button(w_param as Uint, data.mouse_data) {
                let hwnd = WINDOW_HANDLE.load(Ordering::Acquire);
                if is_down {
                    MOUSE_BUTTONS_HELD.fetch_or(button, Ordering::AcqRel);
                    MOUSE_WAKE_PENDING.store(true, Ordering::Release);
                    unsafe {
                        PostMessageW(hwnd, WM_MOUSE_WAKE, 0, 0);
                    }
                    return 1;
                }

                if MOUSE_WAKE_PENDING.load(Ordering::Acquire) {
                    let remaining =
                        MOUSE_BUTTONS_HELD.fetch_and(!button, Ordering::AcqRel) & !button;
                    if remaining == 0 {
                        unsafe {
                            PostMessageW(hwnd, WM_MOUSE_WAKE_COMPLETE, 0, 0);
                        }
                    }
                    return 1;
                }
            }
        }

        unsafe { CallNextHookEx(ptr::null_mut(), code, w_param, l_param) }
    }

    unsafe extern "system" fn window_proc(
        hwnd: Hwnd,
        message: Uint,
        w_param: Wparam,
        l_param: Lparam,
    ) -> Lresult {
        if should_wake(message) || message == WM_CLOSE {
            unsafe {
                DestroyWindow(hwnd);
            }
            return 0;
        }

        if message == WM_MOUSE_WAKE {
            return 0;
        }

        if message == WM_MOUSE_WAKE_COMPLETE {
            if MOUSE_BUTTONS_HELD.load(Ordering::Acquire) == 0 {
                unsafe {
                    DestroyWindow(hwnd);
                }
            }
            return 0;
        }

        if message == WM_DESTROY {
            unsafe { PostQuitMessage(0) };
            return 0;
        }

        if message == WM_SETCURSOR {
            unsafe {
                SetCursor(ptr::null_mut());
            }
            return 1;
        }

        unsafe { DefWindowProcW(hwnd, message, w_param, l_param) }
    }

    fn run() -> io::Result<()> {
        let class_name: Vec<u16> = "BlankerWindow\0".encode_utf16().collect();
        let window_name: Vec<u16> = "Blanker\0".encode_utf16().collect();

        unsafe {
            let instance = GetModuleHandleW(ptr::null());
            if instance.is_null() {
                return Err(io::Error::last_os_error().into());
            }

            let window_class = WindowClass {
                style: 0,
                window_proc: Some(window_proc),
                class_extra: 0,
                window_extra: 0,
                instance,
                icon: ptr::null_mut(),
                cursor: ptr::null_mut(),
                background: GetStockObject(BLACK_BRUSH),
                menu_name: ptr::null(),
                class_name: class_name.as_ptr(),
            };

            if RegisterClassW(&window_class) == 0 {
                return Err(io::Error::last_os_error());
            }

            let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
            let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
            let width = GetSystemMetrics(SM_CXVIRTUALSCREEN);
            let height = GetSystemMetrics(SM_CYVIRTUALSCREEN);
            if width <= 0 || height <= 0 {
                UnregisterClassW(class_name.as_ptr(), instance);
                return Err(io::Error::other("Windows reported an invalid screen size."));
            }

            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                class_name.as_ptr(),
                window_name.as_ptr(),
                WS_POPUP,
                x,
                y,
                width,
                height,
                ptr::null_mut(),
                ptr::null_mut(),
                instance,
                ptr::null_mut(),
            );
            if hwnd.is_null() {
                let error = io::Error::last_os_error();
                UnregisterClassW(class_name.as_ptr(), instance);
                return Err(error);
            }

            let arrow_cursor = LoadCursorW(ptr::null_mut(), 32512usize as *const u16);
            SetCursor(ptr::null_mut());
            WINDOW_HANDLE.store(hwnd, Ordering::Release);
            MOUSE_BUTTONS_HELD.store(0, Ordering::Release);
            MOUSE_WAKE_PENDING.store(false, Ordering::Release);
            let mouse_hook = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook_proc), instance, 0);
            if mouse_hook.is_null() {
                let error = io::Error::last_os_error();
                WINDOW_HANDLE.store(ptr::null_mut(), Ordering::Release);
                DestroyWindow(hwnd);
                UnregisterClassW(class_name.as_ptr(), instance);
                return Err(error);
            }
            ShowWindow(hwnd, SW_SHOW);
            UpdateWindow(hwnd);
            SetForegroundWindow(hwnd);

            let mut message: Message = mem::zeroed();
            loop {
                let result = GetMessageW(&mut message, ptr::null_mut(), 0, 0);
                if result == -1 {
                    let error = io::Error::last_os_error();
                    UnhookWindowsHookEx(mouse_hook);
                    WINDOW_HANDLE.store(ptr::null_mut(), Ordering::Release);
                    DestroyWindow(hwnd);
                    SetCursor(arrow_cursor);
                    UnregisterClassW(class_name.as_ptr(), instance);
                    return Err(error);
                }
                if result == 0 {
                    break;
                }
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }

            UnhookWindowsHookEx(mouse_hook);
            WINDOW_HANDLE.store(ptr::null_mut(), Ordering::Release);
            SetCursor(arrow_cursor);
            UnregisterClassW(class_name.as_ptr(), instance);
        }
        Ok(())
    }

    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn GetStockObject(object: i32) -> Hbrush;
    }

    pub fn start() -> io::Result<()> {
        run()
    }

    pub fn show_error(error: &io::Error) {
        let message: Vec<u16> = format!("Blanker failed: {error}\0")
            .encode_utf16()
            .collect();
        let title: Vec<u16> = "Blanker\0".encode_utf16().collect();
        unsafe {
            MessageBoxW(ptr::null_mut(), message.as_ptr(), title.as_ptr(), 0x10);
        }
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(hwnd: Hwnd, text: *const u16, caption: *const u16, kind: Uint) -> i32;
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn wakes_only_for_keyboard_or_mouse_button_presses() {
            for message in [WM_KEYDOWN, WM_SYSKEYDOWN] {
                assert!(should_wake(message));
            }

            for message in [0x0200, 0x0101, 0x0202] {
                assert!(!should_wake(message));
            }
        }

        #[test]
        fn recognizes_mouse_button_down_and_release_events() {
            for (down, up, data) in [
                (0x0201, 0x0202, 0),
                (0x0204, 0x0205, 0),
                (0x0207, 0x0208, 0),
                (0x020B, 0x020C, 1 << 16),
                (0x020B, 0x020C, 2 << 16),
            ] {
                let (down_button, down_state) = mouse_button(down, data).unwrap();
                let (up_button, up_state) = mouse_button(up, data).unwrap();
                assert_eq!(down_button, up_button);
                assert!(down_state);
                assert!(!up_state);
            }

            assert_eq!(mouse_button(0x0200, 0), None);
            assert_eq!(mouse_button(0x020B, 0), None);
        }
    }
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    match screensaver::start() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            screensaver::show_error(&error);
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("Blanker currently supports Windows only.");
    std::process::ExitCode::FAILURE
}
