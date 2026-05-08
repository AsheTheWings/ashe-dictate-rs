#![allow(unsafe_op_in_unsafe_fn)]
use crate::logger;
use crate::util::{pcwstr, wide};
use anyhow::Result;
use std::ptr::null_mut;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, Ellipse, EndPaint, PAINTSTRUCT, SelectObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

const DOT_SIZE: i32 = 18;

pub struct Overlay {
    hwnd: HWND,
}

impl Overlay {
    pub fn create() -> Result<Self> {
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = wide("AsheDictateRsOverlay");
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                hInstance: instance.into(),
                lpfnWndProc: Some(window_proc),
                lpszClassName: pcwstr(&class),
                ..Default::default()
            };
            let _ = RegisterClassExW(&wc);
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST
                    | WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE,
                pcwstr(&class),
                pcwstr(&wide("Ashe Dictate RS Indicator")),
                WS_POPUP,
                0,
                0,
                DOT_SIZE,
                DOT_SIZE,
                None,
                None,
                Some(instance.into()),
                Some(null_mut()),
            )?;
            SetLayeredWindowAttributes(hwnd, rgb(0, 0, 0), 235, LWA_ALPHA)?;
            logger::info("Overlay created");
            Ok(Self { hwnd })
        }
    }

    pub fn show_near_cursor(&self) {
        self.update_position();
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
    }

    pub fn hide(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    pub fn update_position(&self) {
        unsafe {
            let mut point = POINT::default();
            if GetCursorPos(&mut point).is_ok() {
                let _ = SetWindowPos(
                    self.hwnd,
                    Some(HWND_TOPMOST),
                    point.x + 18,
                    point.y + 18,
                    DOT_SIZE,
                    DOT_SIZE,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let brush = CreateSolidBrush(rgb(34, 197, 94));
            let old = SelectObject(hdc, brush.into());
            let _ = Ellipse(hdc, 0, 0, DOT_SIZE, DOT_SIZE);
            let _ = SelectObject(hdc, old);
            let _ = DeleteObject(brush.into());
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | ((g as u32) << 8) | ((b as u32) << 16))
}
