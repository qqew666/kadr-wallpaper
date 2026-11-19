use raw_window_handle::{
    HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle,
};
use serde::Serialize;
use std::{io, num::NonZeroIsize, ptr::null_mut};
use tao::{event_loop::EventLoopWindowTarget, monitor::MonitorHandle};
use windows_sys::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::{
            Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
            Gdi::ScreenToClient,
        },
        System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentProcessId},
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
pub struct FullscreenBounds(RECT);

impl FullscreenBounds {
    pub fn covers(self, display: &Display) -> bool {
        const EDGE_TOLERANCE: i32 = 2;
        self.0.left <= display.x + EDGE_TOLERANCE
            && self.0.top <= display.y + EDGE_TOLERANCE
            && self.0.right >= display.x + display.width - EDGE_TOLERANCE
            && self.0.bottom >= display.y + display.height - EDGE_TOLERANCE
    }
}

pub fn foreground_fullscreen() -> Option<FullscreenBounds> {
    unsafe {
        let window = GetForegroundWindow();
        if window.is_null()
            || window == GetDesktopWindow()
            || window == GetShellWindow()
            || IsWindowVisible(window) == 0
            || IsIconic(window) != 0
            || GetWindowLongPtrW(window, GWL_STYLE) as u32 & WS_CAPTION == WS_CAPTION
        {
            return None;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(window, &mut pid);
        if pid == 0 || pid == GetCurrentProcessId() {
            return None;
        }

        let mut class = [0u16; 32];
        let length = GetClassNameW(window, class.as_mut_ptr(), class.len() as i32);
        if matches!(
            String::from_utf16_lossy(&class[..length as usize]).as_str(),
            "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
        ) {
            return None;
        }

        let mut bounds = RECT::default();
        let result = DwmGetWindowAttribute(
            window,
            DWMWA_EXTENDED_FRAME_BOUNDS as u32,
            (&mut bounds as *mut RECT).cast(),
            size_of::<RECT>() as u32,
        );
        if result < 0 && GetWindowRect(window, &mut bounds) == 0 {
            return None;
        }
        (bounds.left < bounds.right && bounds.top < bounds.bottom)
            .then_some(FullscreenBounds(bounds))
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fullscreen_bounds_select_only_covered_displays() {
        let left = Display {
            id: "left".into(),
            label: "Left".into(),
            primary: false,
            x: -1920,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let primary = Display {
            id: "primary".into(),
            label: "Primary".into(),
            primary: true,
            x: 0,
            ..left.clone()
        };
        let fullscreen = FullscreenBounds(RECT {
            left: -1919,
            top: 0,
            right: 0,
            bottom: 1080,
        });
        assert!(fullscreen.covers(&left));
        assert!(!fullscreen.covers(&primary));

        let work_area = FullscreenBounds(RECT {
            bottom: 1032,
            ..fullscreen.0
        });
        assert!(!work_area.covers(&left));
        let spanned = FullscreenBounds(RECT {
            right: 1920,
            ..fullscreen.0
        });
        assert!(spanned.covers(&left));
        assert!(spanned.covers(&primary));
    }
}
