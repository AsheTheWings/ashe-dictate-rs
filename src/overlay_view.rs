#![allow(unsafe_op_in_unsafe_fn)]

use crate::logger;
use crate::util::{pcwstr, wide};
use iced::widget::{column, container, row, text};
use iced::window;
use iced::{Background, Color, Element, Length, Point, Shadow, Size, Task, Vector};
#[cfg(target_os = "windows")]
use std::mem::size_of;
#[cfg(target_os = "windows")]
use windows::Win32::Foundation::{COLORREF, HWND};
#[cfg(target_os = "windows")]
use windows::Win32::UI::WindowsAndMessaging::*;

pub const TITLE: &str = "Ashe Worker";
pub const WIDTH: f32 = 380.0;
pub const HEIGHT: f32 = 64.0;
const CORNER_RADIUS: u32 = 32;
const BORDER_WIDTH: f32 = 1.5;
const _: () = assert!(HEIGHT <= 96.0, "dictate overlay stays pill height");
const _: () = assert!(WIDTH <= 480.0, "dictate overlay stays compact");
const _: () = assert!(
    CORNER_RADIUS * 2 >= HEIGHT as u32,
    "pill ends stay fully rounded"
);
const _: () = assert!(HEIGHT < WIDTH, "pill stays wider than tall");
const STATUS_FONT_SIZE: u32 = 12;
const BODY_FONT_SIZE: u32 = 14;
const VISUALIZER_BARS: usize = 24;
const VISUALIZER_BAR_WIDTH: f32 = 3.0;
const VISUALIZER_MAX_HEIGHT: f32 = 26.0;
const VISUALIZER_MIN_HEIGHT: f32 = 4.0;
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
        // The overlay is owned by the tray application. An unsolicited WM_CLOSE for this
        // hidden helper window must not terminate the background worker; only the tray's
        // explicit Quit action owns process shutdown.
        exit_on_close_request: false,
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

/// Everything the dictate pill renders. Bundled so the pill signature stays
/// stable as recording, visualizer, and result states evolve.
pub struct DictateContent<'a> {
    pub status: &'a str,
    pub transcript: &'a str,
    pub polished: Option<&'a str>,
    pub error: Option<&'a str>,
    pub audio_level: f32,
    pub level_history: &'a [f32],
    pub recording: bool,
    pub elapsed_secs: u64,
}

pub fn view<'a, Message: 'a>(content: &DictateContent<'a>) -> Element<'a, Message> {
    let body = content
        .polished
        .or_else(|| {
            let text = content.transcript.trim();
            (!text.is_empty()).then_some(text)
        })
        .unwrap_or("Speak naturally; transcription runs when you stop.");
    let body = single_line_preview(body);
    let listening =
        content.recording
            || content.status.starts_with("Listening")
            || content.status == "Speech detected";
    let status = if let Some(error) = content.error {
        format!("{} · {error}", content.status)
    } else {
        content.status.to_string()
    };
    let dot = text("●".to_string()).size(11);
    let status_text = text(status).size(STATUS_FONT_SIZE).width(Length::Fill);
    let timer = text(format_elapsed(content.elapsed_secs)).size(STATUS_FONT_SIZE);
    let status_row = row![dot, status_text, timer].spacing(6).align_y(iced::Alignment::Center);
    let body_text = text(body).size(BODY_FONT_SIZE).width(Length::Fill);
    let text_column = column![status_row, body_text].spacing(2).width(Length::Fill);
    let visualizer = visualizer_row(content.level_history, content.audio_level, listening);
    let layout = row![text_column, visualizer]
        .spacing(12)
        .align_y(iced::Alignment::Center);
    container(layout)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding([10, 18])
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

fn visualizer_row<'a, Message: 'a>(
    history: &[f32],
    level: f32,
    active: bool,
) -> Element<'a, Message> {
    let mut bars = row![].spacing(2).align_y(iced::Alignment::Center);
    let start = history.len().saturating_sub(VISUALIZER_BARS);
    let mut values: Vec<f32> = history[start..].to_vec();
    if values.len() < VISUALIZER_BARS {
        let mut padded = vec![0.0; VISUALIZER_BARS - values.len()];
        padded.append(&mut values);
        values = padded;
    }
    if active
        && let Some(last) = values.last_mut()
    {
        *last = level.clamp(0.0, 1.0).max(*last);
    }
    for value in values {
        let normalized = value.clamp(0.0, 1.0);
        let height = VISUALIZER_MIN_HEIGHT + normalized * (VISUALIZER_MAX_HEIGHT - VISUALIZER_MIN_HEIGHT);
        let color = if active {
            Color::from_rgba(0.0, 0.88, 1.0, 0.35 + 0.6 * normalized)
        } else {
            Color::from_rgba(0.86, 0.95, 1.0, 0.18)
        };
        let bar = container(text(String::new()).size(1))
            .width(Length::Fixed(VISUALIZER_BAR_WIDTH))
            .height(Length::Fixed(height))
            .style(move |_| container::Style {
                background: Some(Background::Color(color)),
                border: iced::border::rounded(2),
                ..Default::default()
            });
        bars = bars.push(bar);
    }
    container(bars)
        .width(Length::Shrink)
        .height(Length::Fixed(VISUALIZER_MAX_HEIGHT + 4.0))
        .style(|_| container::Style::default())
        .into()
}

fn single_line_preview(body: &str) -> String {
    let first_line = body.split(['\r', '\n']).next().unwrap_or("").trim();
    const MAX_CHARS: usize = 72;
    let truncated: String = first_line.chars().take(MAX_CHARS + 1).collect();
    if truncated.chars().count() > MAX_CHARS {
        format!("{}…", truncated.chars().take(MAX_CHARS).collect::<String>())
    } else {
        truncated
    }
}

fn format_elapsed(total_secs: u64) -> String {
    format!("{:02}:{:02}", total_secs / 60, total_secs % 60)
}

/// Retained for existing call sites. The pill shows a single-line preview
/// with no scrollable transcript, so there is nothing to scroll.
pub fn scroll_transcript_to_end<Message>() -> Task<Message> {
    Task::none()
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
        // The OS window stays a transparent rectangle (DONOTROUND) while the
        // pill shape is drawn by the iced container's rounded border.
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

#[cfg(test)]
mod tests {
    #[test]
    fn overlay_close_request_does_not_exit_tray_application() {
        assert!(!super::window_settings().exit_on_close_request);
    }

    #[test]
    fn body_preview_stays_single_line() {
        assert_eq!(
            super::single_line_preview("hello\nworld"),
            "hello".to_string()
        );
        let long = "a".repeat(100);
        assert!(super::single_line_preview(&long).ends_with('…'));
    }

    #[test]
    fn elapsed_formats_as_minutes_and_seconds() {
        assert_eq!(super::format_elapsed(0), "00:00".to_string());
        assert_eq!(super::format_elapsed(75), "01:15".to_string());
    }
}
