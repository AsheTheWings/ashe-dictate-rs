#![allow(unsafe_op_in_unsafe_fn)]
use crate::logger;
use crate::util::{pcwstr, wide};
use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};
use iced::widget::{column, container, scrollable, text};
use iced::window;
use iced::{Background, Color, Element, Length, Point, Shadow, Size, Subscription, Task, Vector};
use std::mem::size_of;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::WindowsAndMessaging::*;

const TITLE: &str = "Ashe Dictate Rs - Iced";
const WIDTH: f32 = 430.0;
const HEIGHT: f32 = 180.0;
static OVERLAY_POSITION_SENDS: AtomicUsize = AtomicUsize::new(0);
static OVERLAY_NATIVE_STYLE_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
static OVERLAY_ACTIVE_INPUT_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
static OVERLAY_TICKS_WITHOUT_WINDOW: AtomicUsize = AtomicUsize::new(0);
static OVERLAY_POSITION_SEND_FAILURES: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Debug)]
pub enum OverlayCommand {
    Show { x: i32, y: i32 },
    Position { x: i32, y: i32 },
    Hide,
    Status(String),
    Transcript(String),
    Polished(String),
    Error(String),
}

pub struct Overlay {
    tx: Sender<OverlayCommand>,
    _thread: JoinHandle<()>,
}

impl Overlay {
    pub fn create() -> Result<Self> {
        let (tx, rx) = crossbeam_channel::unbounded();
        let thread = std::thread::spawn(move || {
            logger::info("Iced overlay thread entered");
            match catch_unwind(AssertUnwindSafe(|| run_iced(rx))) {
                Ok(()) => logger::info("Iced overlay thread finished"),
                Err(payload) => logger::info(format!(
                    "Iced overlay thread panicked: {}",
                    panic_payload_message(payload)
                )),
            }
        });
        logger::info("Iced overlay thread spawned");
        Ok(Self {
            tx,
            _thread: thread,
        })
    }

    pub fn show_near_active_input(&self) {
        let (x, y) = active_input_position();
        logger::info(format!("Overlay send Show requested x={x} y={y}"));
        self.apply_native_styles();
        match self.tx.send(OverlayCommand::Show { x, y }) {
            Ok(()) => logger::info("Overlay send Show succeeded"),
            Err(err) => logger::info(format!("Overlay send Show failed: {err}")),
        }
    }

    pub fn hide(&self) {
        logger::info("Overlay send Hide requested");
        match self.tx.send(OverlayCommand::Hide) {
            Ok(()) => logger::info("Overlay send Hide succeeded"),
            Err(err) => logger::info(format!("Overlay send Hide failed: {err}")),
        }
    }

    pub fn update_position(&self) {
        let (x, y) = active_input_position();
        let count = OVERLAY_POSITION_SENDS.fetch_add(1, Ordering::Relaxed) + 1;
        if should_log_sample(count) {
            logger::info(format!(
                "Overlay send Position requested count={count} x={x} y={y}"
            ));
        }
        self.apply_native_styles();
        if let Err(err) = self.tx.send(OverlayCommand::Position { x, y }) {
            let failure_count = OVERLAY_POSITION_SEND_FAILURES.fetch_add(1, Ordering::Relaxed) + 1;
            if should_log_sample(failure_count) {
                logger::info(format!(
                    "Overlay send Position failed count={count} failure_count={failure_count}: {err}"
                ));
            }
        }
    }

    pub fn set_status(&self, status: impl Into<String>) {
        let status = status.into();
        logger::info(format!("Overlay send Status requested status={status}"));
        if let Err(err) = self.tx.send(OverlayCommand::Status(status)) {
            logger::info(format!("Overlay send Status failed: {err}"));
        }
    }

    pub fn set_transcript(&self, transcript: impl Into<String>) {
        let transcript = transcript.into();
        logger::info(format!(
            "Overlay send Transcript requested len={}",
            transcript.len()
        ));
        if let Err(err) = self.tx.send(OverlayCommand::Transcript(transcript)) {
            logger::info(format!("Overlay send Transcript failed: {err}"));
        }
    }

    pub fn set_polished(&self, text: impl Into<String>) {
        let text = text.into();
        logger::info(format!(
            "Overlay send Polished requested len={}",
            text.len()
        ));
        if let Err(err) = self.tx.send(OverlayCommand::Polished(text)) {
            logger::info(format!("Overlay send Polished failed: {err}"));
        }
    }

    pub fn set_error(&self, error: impl Into<String>) {
        let error = error.into();
        logger::info(format!("Overlay send Error requested error={error}"));
        if let Err(err) = self.tx.send(OverlayCommand::Error(error)) {
            logger::info(format!("Overlay send Error failed: {err}"));
        }
    }

    fn apply_native_styles(&self) {
        unsafe {
            let count = OVERLAY_NATIVE_STYLE_ATTEMPTS.fetch_add(1, Ordering::Relaxed) + 1;
            let Ok(hwnd) = FindWindowW(None, pcwstr(&wide(TITLE))) else {
                if should_log_sample(count) {
                    logger::info(format!(
                        "Overlay native styles skipped count={count} reason=FindWindow error title={TITLE}"
                    ));
                }
                return;
            };
            if hwnd.0.is_null() {
                if should_log_sample(count) {
                    logger::info(format!(
                        "Overlay native styles skipped count={count} reason=window_not_found title={TITLE}"
                    ));
                }
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
                    "Overlay native styles updated count={count} hwnd={:p} old=0x{current:08x} new=0x{desired:08x}",
                    hwnd.0
                ));
            } else if should_log_sample(count) {
                logger::info(format!(
                    "Overlay native styles already applied count={count} hwnd={:p} style=0x{current:08x}",
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
                Ok(()) => {
                    if should_log_sample(count) {
                        logger::info(format!(
                            "Overlay native SetWindowPos succeeded count={count} hwnd={:p}",
                            hwnd.0
                        ));
                    }
                }
                Err(err) => logger::info(format!(
                    "Overlay native SetWindowPos failed count={count} hwnd={:p}: {err:#}",
                    hwnd.0
                )),
            }
        }
    }
}

#[derive(Debug, Clone)]
enum Message {
    Tick,
    WindowReady(Option<window::Id>),
}

struct OverlayApp {
    rx: Receiver<OverlayCommand>,
    window_id: Option<window::Id>,
    position: Option<Point>,
    status: String,
    transcript: String,
    polished: Option<String>,
    error: Option<String>,
    visible: bool,
}

fn run_iced(rx: Receiver<OverlayCommand>) {
    logger::info("Iced overlay run starting");
    let boot_rx = rx.clone();
    let result = iced::application(
        move || {
            logger::info("Iced overlay boot invoked");
            (
                OverlayApp {
                    rx: boot_rx.clone(),
                    window_id: None,
                    position: None,
                    status: "Ready".to_string(),
                    transcript: String::new(),
                    polished: None,
                    error: None,
                    visible: false,
                },
                window::latest().map(Message::WindowReady),
            )
        },
        update,
        view,
    )
    .subscription(subscription)
    .window(window_settings())
    .run();
    match result {
        Ok(()) => logger::info("Iced overlay exited cleanly"),
        Err(err) => logger::info(format!("Iced overlay exited with error: {err:#}")),
    }
}

fn window_settings() -> window::Settings {
    logger::info(format!(
        "Iced overlay window settings title={TITLE} width={WIDTH} height={HEIGHT} visible=false transparent=true always_on_top=true"
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

fn subscription(_app: &OverlayApp) -> Subscription<Message> {
    logger::info("Iced overlay subscription configured tick_ms=40");
    iced::time::every(Duration::from_millis(40)).map(|_| Message::Tick)
}

fn update(app: &mut OverlayApp, message: Message) -> Task<Message> {
    match message {
        Message::WindowReady(id) => {
            logger::info(format!(
                "Iced overlay WindowReady received id={id:?} visible={} position={:?}",
                app.visible, app.position
            ));
            app.window_id = id;
            if app.visible {
                if let Some(id) = app.window_id {
                    logger::info(format!(
                        "Iced overlay applying deferred visible state id={id:?} position={:?}",
                        app.position
                    ));
                    let mut tasks = vec![window::set_mode(id, window::Mode::Windowed)];
                    if let Some(position) = app.position {
                        tasks.push(window::move_to(id, position));
                    }
                    return Task::batch(tasks);
                }
            }
            Task::none()
        }
        Message::Tick => {
            let mut tasks = Vec::new();
            if app.window_id.is_none() {
                let count = OVERLAY_TICKS_WITHOUT_WINDOW.fetch_add(1, Ordering::Relaxed) + 1;
                if should_log_sample(count) {
                    logger::info(format!(
                        "Iced overlay tick without window id count={count}; requesting latest window"
                    ));
                }
                tasks.push(window::latest().map(Message::WindowReady));
            }
            while let Ok(command) = app.rx.try_recv() {
                match command {
                    OverlayCommand::Show { x, y } => {
                        logger::info(format!(
                            "Iced overlay command Show received x={x} y={y} window_id={:?}",
                            app.window_id
                        ));
                        app.visible = true;
                        app.position = Some(Point::new(x as f32, y as f32));
                        app.transcript.clear();
                        app.polished = None;
                        app.error = None;
                        app.status = "Listening...".to_string();
                        if let Some(id) = app.window_id {
                            logger::info(format!(
                                "Iced overlay Show applying immediately id={id:?} position={:?}",
                                app.position
                            ));
                            tasks.push(window::move_to(id, app.position.unwrap()));
                            tasks.push(window::set_mode(id, window::Mode::Windowed));
                        } else {
                            logger::info("Iced overlay Show deferred because window_id is missing");
                            tasks.push(window::latest().map(Message::WindowReady));
                        }
                    }
                    OverlayCommand::Position { x, y } => {
                        let count = OVERLAY_POSITION_SENDS.load(Ordering::Relaxed);
                        if should_log_sample(count) {
                            logger::info(format!(
                                "Iced overlay command Position received count={count} x={x} y={y} visible={} window_id={:?}",
                                app.visible, app.window_id
                            ));
                        }
                        app.position = Some(Point::new(x as f32, y as f32));
                        if app.visible {
                            if let Some(id) = app.window_id {
                                tasks.push(window::move_to(id, app.position.unwrap()));
                            }
                        }
                    }
                    OverlayCommand::Hide => {
                        logger::info(format!(
                            "Iced overlay command Hide received window_id={:?}",
                            app.window_id
                        ));
                        app.visible = false;
                        if let Some(id) = app.window_id {
                            logger::info(format!("Iced overlay Hide applying id={id:?}"));
                            tasks.push(window::set_mode(id, window::Mode::Hidden));
                        }
                    }
                    OverlayCommand::Status(status) => {
                        logger::info(format!(
                            "Iced overlay command Status received status={status}"
                        ));
                        app.status = status;
                    }
                    OverlayCommand::Transcript(transcript) => {
                        logger::info(format!(
                            "Iced overlay command Transcript received len={}",
                            transcript.len()
                        ));
                        app.transcript = transcript;
                        app.polished = None;
                        app.error = None;
                    }
                    OverlayCommand::Polished(text) => {
                        logger::info(format!(
                            "Iced overlay command Polished received len={}",
                            text.len()
                        ));
                        app.polished = Some(text);
                        app.error = None;
                        app.status = "Inserted".to_string();
                    }
                    OverlayCommand::Error(error) => {
                        logger::info(format!("Iced overlay command Error received error={error}"));
                        app.error = Some(error);
                    }
                }
            }
            Task::batch(tasks)
        }
    }
}

fn view(app: &OverlayApp) -> Element<'_, Message> {
    let body = app
        .polished
        .as_deref()
        .or_else(|| {
            let text = app.transcript.trim();
            (!text.is_empty()).then_some(text)
        })
        .unwrap_or("Speak naturally. Your dictated text will appear here.");
    let status = if let Some(error) = &app.error {
        format!("{} · {}", app.status, error)
    } else {
        app.status.clone()
    };
    let content = column![
        text(status).size(14),
        scrollable(text(body).size(17)).height(Length::Fill)
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

fn active_input_position() -> (i32, i32) {
    unsafe {
        let count = OVERLAY_ACTIVE_INPUT_ATTEMPTS.fetch_add(1, Ordering::Relaxed) + 1;
        let foreground = GetForegroundWindow();
        if !foreground.0.is_null() {
            let thread_id = GetWindowThreadProcessId(foreground, None);
            let mut info = GUITHREADINFO {
                cbSize: size_of::<GUITHREADINFO>() as u32,
                ..Default::default()
            };
            if GetGUIThreadInfo(thread_id, &mut info).is_ok() && !info.hwndCaret.0.is_null() {
                let mut point = POINT {
                    x: info.rcCaret.left,
                    y: info.rcCaret.bottom,
                };
                if ClientToScreen(info.hwndCaret, &mut point).as_bool() {
                    if should_log_sample(count) {
                        logger::info(format!(
                            "Overlay active input position from caret count={count} foreground={:p} caret={:p} x={} y={}",
                            foreground.0,
                            info.hwndCaret.0,
                            point.x + 10,
                            point.y + 16
                        ));
                    }
                    return (point.x + 10, point.y + 16);
                }
                if should_log_sample(count) {
                    logger::info(format!(
                        "Overlay active input ClientToScreen failed count={count} foreground={:p} caret={:p}",
                        foreground.0, info.hwndCaret.0
                    ));
                }
            } else if should_log_sample(count) {
                logger::info(format!(
                    "Overlay active input caret unavailable count={count} foreground={:p} thread_id={thread_id}",
                    foreground.0
                ));
            }
        } else if should_log_sample(count) {
            logger::info(format!(
                "Overlay active input foreground unavailable count={count}"
            ));
        }
        let mut point = POINT::default();
        if GetCursorPos(&mut point).is_ok() {
            if should_log_sample(count) {
                logger::info(format!(
                    "Overlay active input position from cursor count={count} x={} y={}",
                    point.x + 18,
                    point.y + 18
                ));
            }
            return (point.x + 18, point.y + 18);
        }
        if should_log_sample(count) {
            logger::info(format!(
                "Overlay active input position fallback default count={count}"
            ));
        }
    }
    (120, 120)
}

fn should_log_sample(count: usize) -> bool {
    count <= 5 || count % 25 == 0
}

fn panic_payload_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_string()
    }
}
