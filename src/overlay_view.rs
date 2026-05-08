#![allow(unsafe_op_in_unsafe_fn)]

use crate::logger;
use crate::util::{pcwstr, wide};
use iced::widget::{column, container, scrollable, text};
use iced::window;
use iced::{Background, Color, Element, Length, Point, Shadow, Size, Task, Vector};
use windows::Win32::UI::WindowsAndMessaging::*;

pub const TITLE: &str = "Ashe Dictate Rs - Iced";
const WIDTH: f32 = 430.0;
const HEIGHT: f32 = 180.0;

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
        platform_specific: window::settings::PlatformSpecific {
            skip_taskbar: true,
            ..Default::default()
        },
        ..Default::default()
    }
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
    let status = if let Some(error) = error {
        format!("{status} · {error}")
    } else {
        status.to_string()
    };
    let content = column![
        text(status).size(14),
        scrollable(text(body.to_string()).size(17)).height(Length::Fill)
    ]
    .spacing(10);
    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(16)
        .style(|_| container::Style {
            text_color: Some(Color::from_rgb(0.96, 0.98, 1.0)),
            background: Some(Background::Color(Color::from_rgba(0.04, 0.05, 0.08, 0.94))),
            border: iced::border::rounded(16)
                .width(1)
                .color(Color::from_rgba(0.36, 0.45, 0.62, 0.55)),
            shadow: Shadow {
                color: Color::from_rgba(0.0, 0.0, 0.0, 0.35),
                offset: Vector::new(0.0, 8.0),
                blur_radius: 24.0,
            },
            ..Default::default()
        })
        .into()
}

pub fn apply_native_styles() {
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
