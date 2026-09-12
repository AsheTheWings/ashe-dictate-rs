#![allow(unsafe_op_in_unsafe_fn)]

use crate::logger;
use crate::pill_renderer::{self, PillState};
use crate::util::{pcwstr, wide};
use anyhow::{Context, Result, anyhow};
use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::null_mut;
use std::slice;
use windows::Win32::Foundation::{
    COLORREF, HMODULE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateCompatibleDC, CreateDIBSection, CreateFontW,
    DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS, DT_CENTER, DT_NOPREFIX, DT_SINGLELINE,
    DT_VCENTER, DeleteDC, DeleteObject, DrawTextW, FF_DONTCARE, FW_NORMAL, HBITMAP, HDC, HGDIOBJ,
    MONITOR_DEFAULTTONEAREST, MonitorFromPoint, OUT_DEFAULT_PRECIS, SelectObject, SetBkMode,
    SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, HTTRANSPARENT, RegisterClassExW, SW_HIDE,
    SW_SHOWNOACTIVATE, ShowWindow, ULW_ALPHA, UpdateLayeredWindow, WINDOW_EX_STYLE, WM_ERASEBKGND,
    WM_NCHITTEST, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};

const CLASS_NAME: &str = "AsheWorkerLayeredOverlay";
const WINDOW_NAME: &str = "Ashe Worker Dictation Overlay";
const PROCESSING_FONT_PIXELS: f32 = 14.0;

/// A Win32 layered window whose shape comes exclusively from per-pixel alpha.
/// No chroma key, clipping region, or opaque backing surface participates.
pub struct NativeOverlay {
    hwnd: HWND,
    surface: Option<LayeredSurface>,
    visible: bool,
}

impl NativeOverlay {
    pub fn new(instance: HMODULE) -> Result<Self> {
        unsafe {
            let class_name = wide(CLASS_NAME);
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                hInstance: instance.into(),
                lpfnWndProc: Some(window_proc),
                lpszClassName: pcwstr(&class_name),
                ..Default::default()
            };
            let _ = RegisterClassExW(&class);
            let ex_style = WINDOW_EX_STYLE(
                WS_EX_LAYERED.0
                    | WS_EX_TRANSPARENT.0
                    | WS_EX_TOOLWINDOW.0
                    | WS_EX_NOACTIVATE.0
                    | WS_EX_TOPMOST.0,
            );
            let hwnd = CreateWindowExW(
                ex_style,
                pcwstr(&class_name),
                pcwstr(&wide(WINDOW_NAME)),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(instance.into()),
                Some(null_mut()),
            )
            .context("CreateWindowExW failed for layered overlay")?;
            logger::info(format!("Native layered overlay created hwnd={:p}", hwnd.0));
            Ok(Self {
                hwnd,
                surface: None,
                visible: false,
            })
        }
    }

    pub fn update(
        &mut self,
        x: f32,
        y: f32,
        visible: bool,
        bars: &[f32],
        state: PillState,
    ) -> Result<()> {
        unsafe { self.update_inner(x, y, visible, bars, state) }
    }

    unsafe fn update_inner(
        &mut self,
        x: f32,
        y: f32,
        visible: bool,
        bars: &[f32],
        state: PillState,
    ) -> Result<()> {
        if !visible {
            self.hide();
            return Ok(());
        }

        let destination = POINT {
            x: x.round() as i32,
            y: y.round() as i32,
        };
        let scale = monitor_scale_factor(destination);
        let width = (pill_renderer::WIDTH * scale).round().max(1.0) as u32;
        let height = (pill_renderer::HEIGHT * scale).round().max(1.0) as u32;
        let rgba = pill_renderer::render_rgba(width, height, bars, state)
            .ok_or_else(|| anyhow!("failed to allocate layered overlay frame"))?;

        if self
            .surface
            .as_ref()
            .is_none_or(|surface| surface.width != width || surface.height != height)
        {
            self.surface = Some(LayeredSurface::new(width, height)?);
        }
        let surface = self.surface.as_mut().expect("surface initialized");
        surface.copy_premultiplied_rgba(&rgba)?;
        if state == PillState::Working {
            surface.draw_processing_text(scale);
        }

        let source = POINT::default();
        let size = SIZE {
            cx: width as i32,
            cy: height as i32,
        };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: u8::MAX,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        UpdateLayeredWindow(
            self.hwnd,
            None,
            Some(&destination),
            Some(&size),
            Some(surface.dc),
            Some(&source),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        )
        .context("UpdateLayeredWindow failed")?;

        if !self.visible {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            self.visible = true;
            logger::info(format!(
                "Native layered overlay shown size={}x{} scale={scale:.2}",
                width, height
            ));
        }
        Ok(())
    }

    fn hide(&mut self) {
        if self.visible {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            self.visible = false;
            logger::info("Native layered overlay hidden");
        }
    }
}

impl Drop for NativeOverlay {
    fn drop(&mut self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

struct LayeredSurface {
    width: u32,
    height: u32,
    dc: HDC,
    bitmap: HBITMAP,
    previous_bitmap: HGDIOBJ,
    bits: *mut c_void,
}

impl LayeredSurface {
    unsafe fn new(width: u32, height: u32) -> Result<Self> {
        let byte_count = width
            .checked_mul(height)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| anyhow!("layered overlay dimensions overflow"))?;
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: byte_count,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = null_mut();
        let bitmap = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)
            .context("CreateDIBSection failed for layered overlay")?;
        if bits.is_null() {
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
            return Err(anyhow!("CreateDIBSection returned no pixel buffer"));
        }
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
            return Err(anyhow!("CreateCompatibleDC failed for layered overlay"));
        }
        let previous_bitmap = SelectObject(dc, HGDIOBJ(bitmap.0));
        if previous_bitmap.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
            let _ = DeleteDC(dc);
            return Err(anyhow!("SelectObject failed for layered overlay bitmap"));
        }
        Ok(Self {
            width,
            height,
            dc,
            bitmap,
            previous_bitmap,
            bits,
        })
    }

    unsafe fn copy_premultiplied_rgba(&mut self, rgba: &[u8]) -> Result<()> {
        let expected = self.width as usize * self.height as usize * 4;
        if rgba.len() != expected {
            return Err(anyhow!(
                "layered overlay frame has {} bytes, expected {expected}",
                rgba.len()
            ));
        }
        let bgra = slice::from_raw_parts_mut(self.bits.cast::<u8>(), expected);
        let (source_pixels, _) = rgba.as_chunks::<4>();
        let (destination_pixels, _) = bgra.as_chunks_mut::<4>();
        for (source, destination) in source_pixels.iter().zip(destination_pixels) {
            destination[0] = source[2];
            destination[1] = source[1];
            destination[2] = source[0];
            destination[3] = source[3];
        }
        Ok(())
    }

    unsafe fn draw_processing_text(&mut self, scale: f32) {
        let pixels = slice::from_raw_parts_mut(
            self.bits.cast::<u8>(),
            self.width as usize * self.height as usize * 4,
        );
        let alpha: Vec<u8> = pixels
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| pixel[3])
            .collect();
        let face = wide("Segoe UI");
        let font_height = -(PROCESSING_FONT_PIXELS * scale).round().max(1.0) as i32;
        let font = CreateFontW(
            font_height,
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            DEFAULT_PITCH.0 as u32 | FF_DONTCARE.0 as u32,
            pcwstr(&face),
        );
        if !font.is_invalid() {
            let previous_font = SelectObject(self.dc, HGDIOBJ(font.0));
            let _ = SetBkMode(self.dc, TRANSPARENT);
            let _ = SetTextColor(self.dc, COLORREF(0x00ff_ffff));
            let mut bounds = RECT {
                left: 0,
                top: 0,
                right: self.width as i32,
                bottom: self.height as i32,
            };
            let mut text: Vec<u16> = "processing...".encode_utf16().collect();
            let _ = DrawTextW(
                self.dc,
                &mut text,
                &mut bounds,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
            if !previous_font.is_invalid() {
                let _ = SelectObject(self.dc, previous_font);
            }
            let _ = DeleteObject(HGDIOBJ(font.0));
        }
        // GDI text rasterization does not own the alpha channel. Preserve the
        // exact tiny-skia alpha mask while retaining GDI's antialiased RGB.
        for (pixel, alpha) in pixels.as_chunks_mut::<4>().0.iter_mut().zip(alpha) {
            pixel[3] = alpha;
        }
    }
}

impl Drop for LayeredSurface {
    fn drop(&mut self) {
        unsafe {
            let _ = SelectObject(self.dc, self.previous_bitmap);
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            let _ = DeleteDC(self.dc);
        }
    }
}

fn monitor_scale_factor(point: POINT) -> f32 {
    unsafe {
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        let mut dpi_x = 96;
        let mut dpi_y = 96;
        if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y).is_err()
            || dpi_x == 0
        {
            1.0
        } else {
            dpi_x as f32 / 96.0
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
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_ERASEBKGND => LRESULT(1),
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}
