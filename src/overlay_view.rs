#![allow(unsafe_op_in_unsafe_fn)]

use crate::logger;
use crate::util::{pcwstr, wide};
use iced::widget::{canvas, container};
use iced::window;
use iced::{
    Background, Color, Element, Length, Point, Rectangle, Renderer, Shadow, Size, Task, Theme,
    Vector, mouse,
};
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
const BAR_COUNT: usize = 56;
const BAR_MIN_HEIGHT: f32 = 3.0;
const IDLE_BAR_VALUE: f32 = 0.06;
const SHIMMER_SPEED: f32 = 0.12;
const SHIMMER_SPREAD: f32 = 0.55;
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

/// Visual lifecycle of the dictate pill. State is carried by color and
/// motion; the pill itself renders no text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PillState {
    /// Microphone hot. Bars follow the live input level.
    Listening,
    /// Transcribing or polishing. Calm shimmer, distinct from listening.
    Working,
    /// Last operation failed.
    Error,
    /// Text actions and other quiet states.
    Idle,
}

/// Everything the dictate pill renders: a level snapshot, the live level,
/// the lifecycle state, and an animation frame counter.
pub struct PillContent<'a> {
    pub levels: &'a [f32],
    pub audio_level: f32,
    pub state: PillState,
    pub frame: u64,
}

pub fn view<'a, Message: 'a>(content: PillContent<'a>) -> Element<'a, Message> {
    let program = VoiceProgram {
        bars: content.levels.to_vec(),
        level: content.audio_level,
        state: content.state,
        frame: content.frame,
    };
    let visualizer = canvas(program).width(Length::Fill).height(Length::Fill);
    container(visualizer)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding([8, 20])
        .clip(true)
        .style(move |_| container::Style {
            text_color: Some(Color::from_rgb(0.96, 0.99, 1.0)),
            background: Some(Background::Color(Color::from_rgba(
                0.025, 0.035, 0.055, 0.95,
            ))),
            border: iced::border::rounded(CORNER_RADIUS)
                .width(BORDER_WIDTH)
                .color(content.state.border_color()),
            shadow: Shadow {
                color: Color::TRANSPARENT,
                offset: Vector::ZERO,
                blur_radius: 0.0,
            },
            ..Default::default()
        })
        .into()
}

impl PillState {
    fn border_color(self) -> Color {
        let background = Color::from_rgba(0.025, 0.035, 0.055, 0.95);
        match self {
            Self::Listening => Color::from_rgba(0.0, 0.88, 1.0, 0.88),
            Self::Working => Color::from_rgba(0.0, 0.88, 1.0, 0.45),
            Self::Error => Color::from_rgba(1.0, 0.32, 0.28, 0.85),
            Self::Idle => background,
        }
    }
}

/// Canvas program drawing the voice waveform: center-mirrored rounded bars
/// whose height and alpha follow the audio level while listening, a slow
/// traveling shimmer while working, and red bars on error.
#[derive(Debug, Clone)]
struct VoiceProgram {
    bars: Vec<f32>,
    level: f32,
    state: PillState,
    frame: u64,
}

impl<Message> canvas::Program<Message> for VoiceProgram {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut values = if self.bars.len() > BAR_COUNT {
            self.bars[self.bars.len() - BAR_COUNT..].to_vec()
        } else {
            self.bars.clone()
        };
        if self.state == PillState::Listening
            && let Some(last) = values.last_mut()
        {
            *last = self.level.clamp(0.0, 1.0).max(*last);
        }
        while values.len() < BAR_COUNT {
            values.insert(0, 0.0);
        }
        let specs = bar_specs(&values, bounds.width, BAR_COUNT);
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        for (index, spec) in specs.iter().enumerate() {
            let value = match self.state {
                PillState::Listening | PillState::Error => spec.value,
                PillState::Working => 0.16 + 0.30 * work_shimmer(index, BAR_COUNT, self.frame),
                PillState::Idle => IDLE_BAR_VALUE,
            };
            let height = BAR_MIN_HEIGHT + value * (bounds.height - BAR_MIN_HEIGHT);
            let color = match self.state {
                PillState::Listening | PillState::Working => {
                    Color::from_rgba(0.0, 0.88, 1.0, 0.30 + 0.65 * value)
                }
                PillState::Error => Color::from_rgba(1.0, 0.32, 0.28, 0.35 + 0.55 * value),
                PillState::Idle => Color::from_rgba(0.86, 0.95, 1.0, 0.22),
            };
            let bar = canvas::Path::rounded_rectangle(
                Point::new(spec.x, (bounds.height - height) / 2.0),
                Size::new(spec.width, height),
                (spec.width / 2.0).into(),
            );
            frame.fill(&bar, color);
        }
        vec![frame.into_geometry()]
    }
}

/// Horizontal layout of one [`BarSpec`] per bar: even pitch across the area
/// with neighbor-averaged values so motion stays fluid instead of jagged.
#[derive(Debug, Clone, PartialEq)]
pub struct BarSpec {
    pub x: f32,
    pub width: f32,
    pub value: f32,
}

pub fn bar_specs(values: &[f32], area_width: f32, bar_count: usize) -> Vec<BarSpec> {
    if bar_count == 0 || area_width <= 0.0 {
        return Vec::new();
    }
    let pitch = area_width / bar_count as f32;
    let width = (pitch * 0.55).clamp(2.0, 4.0);
    let start = values.len().saturating_sub(bar_count);
    let window = &values[start..];
    let pad = bar_count.saturating_sub(window.len());
    (0..bar_count)
        .map(|index| {
            let value = if index < pad || window.is_empty() {
                0.0
            } else {
                let position = index - pad;
                let at = |offset: usize| window[offset.min(window.len() - 1)];
                (at(position.saturating_sub(1)) + at(position) + at(position + 1)) / 3.0
            };
            BarSpec {
                x: index as f32 * pitch + (pitch - width) / 2.0,
                width,
                value: value.clamp(0.0, 1.0),
            }
        })
        .collect()
}

/// Slow traveling wave driving the [`PillState::Working`] shimmer. Pure and
/// deterministic in bar index and animation frame.
pub fn work_shimmer(bar: usize, bar_count: usize, frame: u64) -> f32 {
    if bar_count == 0 {
        return 0.0;
    }
    let phase = frame as f32 * SHIMMER_SPEED + bar as f32 * SHIMMER_SPREAD;
    0.5 + 0.5 * phase.sin()
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
    fn bar_specs_cover_the_area_with_even_pitch() {
        let values = vec![0.5; 8];
        let specs = super::bar_specs(&values, 160.0, 8);
        assert_eq!(specs.len(), 8);
        for pair in specs.windows(2) {
            let pitch = pair[1].x - pair[0].x;
            assert!((pitch - 20.0).abs() < 0.001);
            assert_eq!(pair[0].width, pair[1].width);
        }
        for spec in &specs {
            assert!(spec.x >= 0.0);
            assert!(spec.x + spec.width <= 160.0 + 0.001);
        }
    }

    #[test]
    fn bar_specs_pad_short_histories_and_clamp() {
        let specs = super::bar_specs(&[2.0, -1.0], 100.0, 4);
        assert_eq!(specs.len(), 4);
        assert_eq!(specs[0].value, 0.0);
        assert_eq!(specs[1].value, 0.0);
        for spec in &specs {
            assert!((0.0..=1.0).contains(&spec.value));
        }
        assert!(super::bar_specs(&[0.5], 100.0, 0).is_empty());
        assert!(super::bar_specs(&[0.5], 0.0, 4).is_empty());
    }

    #[test]
    fn bar_specs_smooth_neighbors() {
        let specs = super::bar_specs(&[0.0, 0.3, 0.6, 0.9], 80.0, 4);
        assert!((specs[0].value - 0.1).abs() < 0.0001);
        assert!((specs[1].value - 0.3).abs() < 0.0001);
        assert!((specs[2].value - 0.6).abs() < 0.0001);
        assert!((specs[3].value - 0.8).abs() < 0.0001);
    }

    #[test]
    fn work_shimmer_stays_unit_and_moves_with_frame() {
        let first = super::work_shimmer(3, 8, 10);
        assert!((0.0..=1.0).contains(&first));
        assert_eq!(first, super::work_shimmer(3, 8, 10));
        assert_ne!(first, super::work_shimmer(3, 8, 11));
        assert_eq!(super::work_shimmer(0, 0, 10), 0.0);
    }
}
