#![allow(unsafe_op_in_unsafe_fn)]

use crate::logger;
use crate::util::{pcwstr, wide};
use iced::widget::{Id, operation};
use iced::widget::{column, container, scrollable, text};
use iced::window;
use iced::{Background, Color, Element, Length, Point, Shadow, Size, Task, Vector};
#[cfg(target_os = "windows")]
use std::mem::size_of;
#[cfg(target_os = "windows")]
use windows::Win32::Foundation::{COLORREF, HWND};
#[cfg(target_os = "windows")]
use windows::Win32::UI::WindowsAndMessaging::*;

pub const TITLE: &str = "Ashe Worker";
pub const WIDTH: f32 = 430.0;
pub const HEIGHT: f32 = 216.0;
const CORNER_RADIUS: u32 = 0;
const BORDER_WIDTH: f32 = 2.0;
const TRANSCRIPT_FONT_SIZE: u32 = 15;
const TRANSCRIPT_SCROLL_ID: &str = "overlay-transcript-scroll";
#[cfg(target_os = "windows")]
const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
#[cfg(target_os = "windows")]
const DWMWCP_DONOTROUND: i32 = 1;

#[cfg(target_os = "windows")]
#[link(name = "dwmapi")]
unsafe extern "system" {
    fn DwmSetWindowAttribute(
        hwnd: HWND,
        dwattribute: u32,
        pvattribute: *const core::ffi::c_void,
        cbattribute: u32,
    ) -> i32;
}

pub fn window_settings() -> window::Settings {
    logger::info(format!(
        "Iced main window settings title={TITLE} width={WIDTH} height={HEIGHT} visible=false transparent=true always_on_top=true"
    ));
    window::Settings {
        size: Size::new(WIDTH, HEIGHT),
        position: window::Position::Specific(Point::new(120.0, 120.0)),
        visible: false,
        resizable: false,
        decorations: false,
        transparent: true,
        level: window::Level::AlwaysOnTop,
        platform_specific: platform_specific_settings(),
        ..Default::default()
    }
}

#[cfg(target_os = "windows")]
fn platform_specific_settings() -> window::settings::PlatformSpecific {
    window::settings::PlatformSpecific {
        skip_taskbar: true,
        ..Default::default()
    }
}

#[cfg(not(target_os = "windows"))]
fn platform_specific_settings() -> window::settings::PlatformSpecific {
    window::settings::PlatformSpecific::default()
}

pub fn view<'a, Message: 'a>(
    status: &str,
    transcript: &str,
    polished: Option<&str>,
    error: Option<&str>,
) -> Element<'a, Message> {
    let body = polished
        .or_else(|| {
            let text = transcript.trim();
            (!text.is_empty()).then_some(text)
        })
        .unwrap_or("Speak naturally. Your dictated text will appear here.");
    let listening = status.starts_with("Listening") || status == "Speech detected";
    let status = if let Some(error) = error {
        format!("{status} · {error}")
    } else {
        status.to_string()
    };
    let body_text = text(body.to_string())
        .size(TRANSCRIPT_FONT_SIZE)
        .width(Length::Fill)
        .wrapping(text::Wrapping::WordOrGlyph);
    let transcript = scrollable(body_text)
        .id(transcript_scroll_id())
        .direction(scrollable::Direction::Vertical(
            scrollable::Scrollbar::hidden(),
        ))
        .height(Length::Fill)
        .width(Length::Fill)
        .style(|_, status| {
            let mut style = scrollable::default(&iced::Theme::Dark, status);
            style.container.background = None;
            style.vertical_rail.background = None;
            style.vertical_rail.border =
                iced::border::rounded(3).width(0).color(Color::TRANSPARENT);
            style.vertical_rail.scroller.background =
                Background::Color(Color::from_rgba(0.86, 0.95, 1.0, 0.22));
            style.vertical_rail.scroller.border = iced::border::rounded(3);
            style.horizontal_rail.background = None;
            style.horizontal_rail.scroller.background = Background::Color(Color::TRANSPARENT);
            style.gap = None;
            style
        });
    let content = column![text(status).size(14), transcript].spacing(10);
    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(16)
        .clip(true)
        .style(move |_| {
            let background = Color::from_rgba(0.025, 0.035, 0.055, 0.95);
            let border = if listening {
                Color::from_rgba(0.0, 0.88, 1.0, 0.88)
            } else {
                background
            };
            container::Style {
                text_color: Some(Color::from_rgb(0.96, 0.99, 1.0)),
                background: Some(Background::Color(background)),
                border: iced::border::rounded(CORNER_RADIUS)
                    .width(BORDER_WIDTH)
                    .color(border),
                shadow: Shadow {
                    color: Color::TRANSPARENT,
                    offset: Vector::ZERO,
                    blur_radius: 0.0,
                },
                ..Default::default()
            }
        })
        .into()
}

pub fn scroll_transcript_to_end<Message>() -> Task<Message> {
    operation::snap_to_end(transcript_scroll_id())
}

fn transcript_scroll_id() -> Id {
    Id::new(TRANSCRIPT_SCROLL_ID)
}

pub fn apply_native_styles() {
    #[cfg(not(target_os = "windows"))]
    {
        return;
    }

    #[cfg(target_os = "windows")]
    unsafe {
        let Ok(hwnd) = FindWindowW(None, pcwstr(&wide(TITLE))) else {
            logger::info(format!(
                "Overlay native styles skipped reason=FindWindow error title={TITLE}"
            ));
            return;
        };
        if hwnd.0.is_null() {
            logger::info(format!(
                "Overlay native styles skipped reason=window_not_found title={TITLE}"
            ));
            return;
        }
        let corner_preference = DWMWCP_DONOTROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner_preference as *const _ as *const core::ffi::c_void,
            size_of::<i32>() as u32,
        );
        let current = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let desired = current
            | WS_EX_TOOLWINDOW.0
            | WS_EX_NOACTIVATE.0
            | WS_EX_TRANSPARENT.0
            | WS_EX_LAYERED.0;
        if desired != current {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, desired as isize);
            logger::info(format!(
                "Overlay native styles updated hwnd={:p} old=0x{current:08x} new=0x{desired:08x}",
                hwnd.0
            ));
        }
        if let Err(err) = SetLayeredWindowAttributes(hwnd, COLORREF(0), u8::MAX, LWA_ALPHA) {
            logger::info(format!(
                "Overlay native SetLayeredWindowAttributes failed hwnd={:p}: {err:#}",
                hwnd.0
            ));
        }
        match SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        ) {
            Ok(()) => logger::info(format!(
                "Overlay native SetWindowPos succeeded hwnd={:p}",
                hwnd.0
            )),
            Err(err) => logger::info(format!(
                "Overlay native SetWindowPos failed hwnd={:p}: {err:#}",
                hwnd.0
            )),
        }
    }
}

pub fn apply_window_state<Message: 'static>(
    id: window::Id,
    position: Option<Point>,
    visible: bool,
) -> Task<Message> {
    let mut tasks = Vec::new();
    if let Some(position) = position {
        tasks.push(window::move_to(id, position));
    }
    if visible {
        tasks.push(window::set_mode(id, window::Mode::Windowed));
    } else {
        tasks.push(window::set_mode(id, window::Mode::Hidden));
    }
    Task::batch(tasks)
}
