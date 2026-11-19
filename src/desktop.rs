use raw_window_handle::{
    HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle,
};
use serde::Serialize;
use std::{io, num::NonZeroIsize, ptr::null_mut};
use tao::{event_loop::EventLoopWindowTarget, monitor::MonitorHandle};
use windows_sys::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
        Graphics::Gdi::ScreenToClient,
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::*,
    },
    core::BOOL,
    w,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Display {
    pub id: String,
    pub label: String,
    pub primary: bool,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

pub fn displays<T>(target: &EventLoopWindowTarget<T>) -> Vec<Display> {
    let primary: Option<MonitorHandle> = target.primary_monitor();
    let mut displays: Vec<_> = target
        .available_monitors()
        .filter_map(|monitor| {
            let size = monitor.size();
            let position = monitor.position();
            let id = monitor.name()?;
            let primary = Some(&monitor) == primary.as_ref();
            let label = format!(
                "Экран {} · {} × {}",
                id.trim_start_matches(r"\\.\DISPLAY"),
                size.width,
                size.height
            );
            (size.width > 0 && size.height > 0).then_some(Display {
                id,
                label,
                primary,
                x: position.x,
                y: position.y,
                width: size.width as i32,
                height: size.height as i32,
            })
        })
        .collect();
    displays.sort_by(|left, right| (!left.primary, &left.id).cmp(&(!right.primary, &right.id)));
    displays
}

pub fn targets<'a>(displays: &'a [Display], choice: &str) -> Vec<&'a Display> {
    displays
        .iter()
        .filter(|display| {
            choice == "all" || (choice == "primary" && display.primary) || display.id == choice
        })
        .collect()
}

#[derive(Clone, Copy)]
pub struct Desktop {
    parent: HWND,
    after: HWND,
}

impl Desktop {
    pub fn find() -> io::Result<Self> {
        unsafe {
            let progman = FindWindowW(w!("Progman"), null_mut());
            if progman.is_null() {
                return Err(io::ErrorKind::NotFound.into());
            }
            if let Some(desktop) = Self::locate(progman) {
                return Ok(desktop);
            }
            // Explorer's private message creates the wallpaper WorkerW on demand.
            SendMessageTimeoutW(progman, 0x052c, 0x0d, 1, SMTO_ABORTIFHUNG, 1000, null_mut());
            Self::locate(progman).ok_or_else(|| io::ErrorKind::NotFound.into())
        }
    }

    unsafe fn locate(progman: HWND) -> Option<Self> {
        unsafe {
            if GetWindowLongPtrW(progman, GWL_EXSTYLE) as u32 & WS_EX_NOREDIRECTIONBITMAP != 0 {
                let icons = FindWindowExW(progman, null_mut(), w!("SHELLDLL_DefView"), null_mut());
                let backdrop = FindWindowExW(progman, null_mut(), w!("WorkerW"), null_mut());
                return (!icons.is_null() && !backdrop.is_null()).then_some(Self {
                    parent: progman,
                    after: icons,
                });
            }
            let mut worker: HWND = null_mut();
            EnumWindows(
                Some(find_classic_worker),
                (&mut worker as *mut HWND) as LPARAM,
            );
            (!worker.is_null()).then_some(Self {
                parent: worker,
                after: HWND_TOP,
            })
        }
    }

    pub fn create(self, display: &Display) -> io::Result<BackdropWindow> {
        unsafe {
            let mut origin = POINT {
                x: display.x,
                y: display.y,
            };
            if ScreenToClient(self.parent, &mut origin) == 0 {
                return Err(io::Error::last_os_error());
            }
            let instance = GetModuleHandleW(null_mut());
            let class_name = w!("KadrDesktopBackdrop");
            let mut class: WNDCLASSW = std::mem::zeroed();
            class.lpfnWndProc = Some(backdrop_proc);
            class.hInstance = instance;
            class.lpszClassName = class_name;
            // The class is registered once per process. Subsequent calls reuse it.
            RegisterClassW(&class);
            let raised = !self.after.is_null();
            let mut ex_style = WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW;
            if raised {
                ex_style |= WS_EX_LAYERED | WS_EX_TRANSPARENT;
            }
            let window = CreateWindowExW(
                ex_style,
                class_name,
                w!("Кадр"),
                WS_CHILD | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                origin.x,
                origin.y,
                display.width,
                display.height,
                self.parent,
                null_mut(),
                instance,
                null_mut(),
            );
            if window.is_null() {
                return Err(io::Error::last_os_error());
            }
            let backdrop = BackdropWindow {
                window,
                desktop: self,
            };
            if raised && SetLayeredWindowAttributes(window, 0, 255, LWA_ALPHA) == 0 {
                return Err(io::Error::last_os_error());
            }
            if SetWindowPos(
                window,
                self.after,
                0,
                0,
                0,
                0,
                SWP_NOSIZE | SWP_NOMOVE | SWP_NOACTIVATE,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(backdrop)
        }
    }
}

unsafe extern "system" fn find_classic_worker(window: HWND, param: LPARAM) -> BOOL {
    unsafe {
        if FindWindowExW(window, null_mut(), w!("SHELLDLL_DefView"), null_mut()).is_null() {
            return 1;
        }
        let worker = FindWindowExW(null_mut(), window, w!("WorkerW"), null_mut());
        if worker.is_null() {
            return 1;
        }
        *(param as *mut HWND) = worker;
        0
    }
}

unsafe extern "system" fn backdrop_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_NCHITTEST => HTTRANSPARENT as LRESULT,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

pub struct BackdropWindow {
    window: HWND,
    desktop: Desktop,
}

impl BackdropWindow {
    pub fn healthy(&self) -> bool {
        unsafe {
            IsWindow(self.window) != 0
                && GetParent(self.window) == self.desktop.parent
                && (self.desktop.after.is_null() || IsWindow(self.desktop.after) != 0)
        }
    }

    pub fn show(&self, visible: bool) {
        unsafe {
            ShowWindow(
                self.window,
                if visible { SW_SHOWNOACTIVATE } else { SW_HIDE },
            );
        }
    }
}

impl HasWindowHandle for BackdropWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let hwnd = NonZeroIsize::new(self.window as isize).ok_or(HandleError::Unavailable)?;
        let raw = RawWindowHandle::Win32(Win32WindowHandle::new(hwnd));
        // The HWND is owned by this wrapper until it is dropped.
        Ok(unsafe { WindowHandle::borrow_raw(raw) })
    }
}

impl Drop for BackdropWindow {
    fn drop(&mut self) {
        unsafe {
            if IsWindow(self.window) != 0 {
                DestroyWindow(self.window);
            }
        }
    }
}
