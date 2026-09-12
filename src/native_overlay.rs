#![allow(unsafe_op_in_unsafe_fn)]

use crate::logger;
use crate::pill_renderer::{self, PillState, TypingBarContent};
use crate::util::{pcwstr, wide};
use anyhow::{Context, Result, anyhow};
use cosmic_text::{Attrs, Buffer, Color as TextColor, Family, FontSystem, Metrics, Shaping};
use cosmic_text::{SwashCache, Wrap};
use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::null_mut;
use std::slice;
use windows::Win32::Foundation::{COLORREF, HMODULE, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HBITMAP, HDC,
    HGDIOBJ, MONITOR_DEFAULTTONEAREST, MonitorFromPoint, SelectObject,
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
const MAIN_FONT_PIXELS: f32 = 14.0;
const TOP_BAR_FONT_PIXELS: f32 = 11.0;

/// A Win32 layered window whose shape comes exclusively from per-pixel alpha.
/// No chroma key, clipping region, or opaque backing surface participates.
pub struct NativeOverlay {
    hwnd: HWND,
    surface: Option<LayeredSurface>,
    text: TextRasterizer,
    visible: bool,
}

pub struct OverlayFrame<'a> {
    pub x: f32,
    pub y: f32,
    pub visible: bool,
    pub bars: &'a [f32],
    pub state: PillState,
    pub main_text: Option<&'a str>,
    pub typing_bar: Option<&'a TypingBarContent>,
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
                text: TextRasterizer::new(),
                visible: false,
            })
        }
    }

    pub fn update(&mut self, frame: OverlayFrame<'_>) -> Result<()> {
        unsafe { self.update_inner(frame) }
    }

    unsafe fn update_inner(&mut self, frame: OverlayFrame<'_>) -> Result<()> {
        if !frame.visible {
            self.hide();
            return Ok(());
        }

        let anchor = POINT {
            x: frame.x.round() as i32,
            y: frame.y.round() as i32,
        };
        let monitor_scale = monitor_scale_factor(anchor);
        let width = (pill_renderer::WIDTH * monitor_scale).round().max(1.0) as u32;
        let scale = width as f32 / pill_renderer::WIDTH;
        let main_top = (pill_renderer::MAIN_TOP * scale).round().max(0.0) as u32;
        let main_height = (pill_renderer::PILL_HEIGHT * scale).round().max(1.0) as u32;
        let height = main_top.saturating_add(main_height);
        let destination = POINT {
            x: anchor.x,
            y: anchor.y - main_top as i32,
        };
        let mut rgba = pill_renderer::render_rgba(
            width,
            height,
            frame.bars,
            frame.state,
            frame.main_text.is_none(),
            frame.typing_bar.is_some(),
        )
        .ok_or_else(|| anyhow!("failed to allocate layered overlay frame"))?;
        self.text.draw(
            &mut rgba,
            width,
            height,
            scale,
            frame.main_text,
            frame.typing_bar,
        );

        if self
            .surface
            .as_ref()
            .is_none_or(|surface| surface.width != width || surface.height != height)
        {
            self.surface = Some(LayeredSurface::new(width, height)?);
        }
        let surface = self.surface.as_mut().expect("surface initialized");
        surface.copy_premultiplied_rgba(&rgba)?;

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
}

struct TextRasterizer {
    fonts: FontSystem,
    cache: SwashCache,
}

impl TextRasterizer {
    fn new() -> Self {
        Self {
            fonts: FontSystem::new(),
            cache: SwashCache::new(),
        }
    }

    fn draw(
        &mut self,
        rgba: &mut [u8],
        width: u32,
        height: u32,
        scale: f32,
        main_text: Option<&str>,
        typing_bar: Option<&TypingBarContent>,
    ) {
        if let Some(text) = main_text.filter(|text| !text.is_empty()) {
            let rect = PixelRect::from_logical(
                0.0,
                pill_renderer::MAIN_TOP,
                pill_renderer::WIDTH,
                pill_renderer::PILL_HEIGHT,
                scale,
            );
            self.draw_text(
                rgba,
                width,
                height,
                text,
                MAIN_FONT_PIXELS * scale,
                rect,
                TextAlign::Center,
                u8::MAX,
            );
        }
        if let Some(content) = typing_bar {
            let count_left = pill_renderer::TOP_BAR_X + pill_renderer::TOP_BAR_TEXT_INSET;
            let count_rect = PixelRect::from_logical(
                count_left,
                pill_renderer::TOP_BAR_Y,
                pill_renderer::TOP_BAR_COUNT_WIDTH,
                pill_renderer::MAIN_TOP,
                scale,
            );
            self.draw_text(
                rgba,
                width,
                height,
                &content.count,
                TOP_BAR_FONT_PIXELS * scale,
                count_rect,
                TextAlign::Left,
                178,
            );
            let preview_left = count_left + pill_renderer::TOP_BAR_COUNT_WIDTH + 8.0;
            let preview_right = pill_renderer::TOP_BAR_X + pill_renderer::TOP_BAR_WIDTH
                - pill_renderer::TOP_BAR_TEXT_INSET;
            let preview_rect = PixelRect::from_logical(
                preview_left,
                pill_renderer::TOP_BAR_Y,
                (preview_right - preview_left).max(0.0),
                pill_renderer::MAIN_TOP,
                scale,
            );
            self.draw_text(
                rgba,
                width,
                height,
                &content.preview,
                TOP_BAR_FONT_PIXELS * scale,
                preview_rect,
                TextAlign::Right,
                204,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_text(
        &mut self,
        rgba: &mut [u8],
        width: u32,
        height: u32,
        text: &str,
        font_size: f32,
        clip: PixelRect,
        alignment: TextAlign,
        opacity: u8,
    ) {
        if text.is_empty() || clip.width <= 0 || clip.height <= 0 {
            return;
        }
        let line_height = (font_size * 1.35).max(1.0);
        let mut buffer = Buffer::new(
            &mut self.fonts,
            Metrics::new(font_size.max(1.0), line_height),
        );
        buffer.set_wrap(&mut self.fonts, Wrap::None);
        buffer.set_size(&mut self.fonts, None, Some(clip.height as f32));
        let attrs = Attrs::new().family(Family::Name("Segoe UI"));
        buffer.set_text(&mut self.fonts, text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.fonts, false);

        let mut raster = Vec::new();
        buffer.draw(
            &mut self.fonts,
            &mut self.cache,
            TextColor::rgb(255, 255, 255),
            |x, y, pixel_width, pixel_height, color| {
                raster.push(RasterPixel {
                    x,
                    y,
                    width: pixel_width,
                    height: pixel_height,
                    color,
                });
            },
        );
        let Some(bounds) = RasterBounds::for_pixels(&raster) else {
            return;
        };
        let origin_x = match alignment {
            TextAlign::Left => clip.left - bounds.left,
            TextAlign::Center => clip.left + (clip.width - bounds.width()) / 2 - bounds.left,
            TextAlign::Right => clip.right() - bounds.width() - bounds.left,
        };
        let origin_y = clip.top + (clip.height - bounds.height()) / 2 - bounds.top;

        for pixel in raster {
            for offset_y in 0..pixel.height as i32 {
                for offset_x in 0..pixel.width as i32 {
                    let x = origin_x + pixel.x + offset_x;
                    let y = origin_y + pixel.y + offset_y;
                    if clip.contains(x, y) {
                        composite_pixel(rgba, width, height, x, y, pixel.color, opacity);
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum TextAlign {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy)]
struct PixelRect {
    left: i32,
    top: i32,
    width: i32,
    height: i32,
}

impl PixelRect {
    fn from_logical(x: f32, y: f32, width: f32, height: f32, scale: f32) -> Self {
        Self {
            left: (x * scale).round() as i32,
            top: (y * scale).round() as i32,
            width: (width * scale).round() as i32,
            height: (height * scale).round() as i32,
        }
    }

    fn right(self) -> i32 {
        self.left + self.width
    }

    fn contains(self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right() && y >= self.top && y < self.top + self.height
    }
}

struct RasterPixel {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    color: TextColor,
}

struct RasterBounds {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl RasterBounds {
    fn for_pixels(pixels: &[RasterPixel]) -> Option<Self> {
        let first = pixels.first()?;
        let mut bounds = Self {
            left: first.x,
            top: first.y,
            right: first.x + first.width as i32,
            bottom: first.y + first.height as i32,
        };
        for pixel in &pixels[1..] {
            bounds.left = bounds.left.min(pixel.x);
            bounds.top = bounds.top.min(pixel.y);
            bounds.right = bounds.right.max(pixel.x + pixel.width as i32);
            bounds.bottom = bounds.bottom.max(pixel.y + pixel.height as i32);
        }
        Some(bounds)
    }

    fn width(&self) -> i32 {
        self.right - self.left
    }

    fn height(&self) -> i32 {
        self.bottom - self.top
    }
}

fn composite_pixel(
    rgba: &mut [u8],
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    color: TextColor,
    opacity: u8,
) {
    if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 {
        return;
    }
    let index = (y as usize * width as usize + x as usize) * 4;
    let source_alpha = (color.a() as u16 * opacity as u16 / 255) as u8;
    let inverse = 255_u16 - source_alpha as u16;
    let source = [color.r(), color.g(), color.b()];
    for channel in 0..3 {
        let source_premultiplied = source[channel] as u16 * source_alpha as u16 / 255;
        rgba[index + channel] =
            (source_premultiplied + rgba[index + channel] as u16 * inverse / 255).min(255) as u8;
    }
    rgba[index + 3] = (source_alpha as u16 + rgba[index + 3] as u16 * inverse / 255).min(255) as u8;
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

#[cfg(test)]
mod tests {
    use super::composite_pixel;
    use cosmic_text::Color;

    #[test]
    fn text_compositing_creates_premultiplied_alpha_on_transparency() {
        let mut rgba = [0_u8; 4];
        composite_pixel(&mut rgba, 1, 1, 0, 0, Color::rgba(255, 255, 255, 128), 128);
        assert_eq!(rgba, [64, 64, 64, 64]);
    }

    #[test]
    fn text_compositing_obeys_the_clip_surface_bounds() {
        let mut rgba = [0_u8; 4];
        composite_pixel(&mut rgba, 1, 1, 1, 0, Color::rgb(255, 255, 255), 255);
        assert_eq!(rgba, [0, 0, 0, 0]);
    }
}
