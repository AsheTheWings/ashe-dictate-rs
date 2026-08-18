#![allow(unsafe_op_in_unsafe_fn)]

use crate::injector;
use crate::logger;
use crate::overlay_view;
use crate::util::{pcwstr, wide};
use crossbeam_channel::{Receiver, Sender};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::null_mut;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
};
use windows::Win32::System::LibraryLoader::{
    FindResourceW, GetModuleHandleW, LoadResource, LockResource, SizeofResource,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey, UnregisterHotKey,
    VK_BACK, VK_ESCAPE, VK_RETURN,
};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

const TOGGLE_HOTKEY_ID: i32 = 1001;
const CANCEL_HOTKEY_ID: i32 = 1002;
const REVERT_SENTENCE_HOTKEY_ID: i32 = 1003;
const CLEAR_TRANSCRIPT_HOTKEY_ID: i32 = 1004;
const SUBMIT_HOTKEY_ID: i32 = 1005;
const FIX_GRAMMAR_HOTKEY_ID: i32 = 1006;
const ANSWER_QUESTION_HOTKEY_ID: i32 = 1007;
const PASTE_IMAGE_HOTKEY_ID: i32 = 1008;
const TIMER_SERVICE: usize = 2001;
const TIMER_INTERVAL_MS: u32 = 16;
const CURSOR_OVERLAY_GAP: i32 = 8;
const WM_TRAY: u32 = WM_APP + 1;
const MENU_TOGGLE: usize = 3001;
const MENU_RELOAD_CONFIG: usize = 3002;
const MENU_OPEN_LOG: usize = 3003;
const MENU_COPY_LOG_PATH: usize = 3004;
const MENU_ABOUT: usize = 3005;
const MENU_QUIT: usize = 3006;
const MENU_TOGGLE_ACTIVITY: usize = 3007;
const MENU_OPEN_ARTIFACTS: usize = 3008;
const MENU_OPEN_JOURNAL: usize = 3009;
const ICON_FILE_NAME: &str = "ashe-worker.ico";
const ICON_DATA_PATH: &str = "assets/ashe-worker.ico";
const APP_ICON_RESOURCE_ID: u16 = 1;

#[derive(Debug, Clone)]
pub enum Win32Event {
    ToggleRequested { target_hwnd: isize, x: i32, y: i32 },
    CancelRequested,
    RevertLastSentenceRequested,
    ClearTranscriptRequested,
    SubmitRequested,
    FixGrammarRequested { target_hwnd: isize, x: i32, y: i32 },
    AnswerQuestionRequested { target_hwnd: isize, x: i32, y: i32 },
    PasteImageRequested { target_hwnd: isize },
    PositionChanged { x: i32, y: i32 },
    ReloadConfigRequested,
    OpenLogRequested,
    CopyLogPathRequested,
    ToggleActivityRequested,
    OpenArtifactsRequested,
    OpenJournalRequested,
    AboutRequested,
    QuitRequested,
    PasteCompleted(Result<(), String>),
    PathPasteCompleted(Result<(), String>),
    ServiceStopped,
}

#[derive(Debug, Clone)]
pub enum Win32Command {
    SetActive(bool),
    SetFollowCursor(bool),
    SetTooltip(String),
    SetActivityStatus {
        running: bool,
        status: String,
    },
    ShowMessageBox {
        title: String,
        text: String,
    },
    OpenLog(String),
    OpenPath(String),
    CopyText(String),
    PasteText {
        target_hwnd: isize,
        text: String,
    },
    PastePath {
        target_hwnd: isize,
        text: String,
    },
    InjectText {
        target_hwnd: isize,
        text: String,
        append_after_selection: bool,
    },
    Shutdown,
}

struct ServiceState {
    event_tx: Sender<Win32Event>,
    command_rx: Receiver<Win32Command>,
    active: bool,
    follow_cursor: bool,
    cancel_hotkey_registered: bool,
    revert_sentence_hotkey_registered: bool,
    clear_transcript_hotkey_registered: bool,
    submit_hotkey_registered: bool,
    activity_running: bool,
    activity_status: String,
    last_artifacts_open: Option<Instant>,
}

pub fn spawn(
    event_tx: Sender<Win32Event>,
    command_rx: Receiver<Win32Command>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || run(event_tx, command_rx))
}

fn run(event_tx: Sender<Win32Event>, command_rx: Receiver<Win32Command>) {
    logger::info("Win32 service thread starting");
    let result = unsafe { run_message_loop(event_tx.clone(), command_rx) };
    if let Err(err) = result {
        logger::info(format!("Win32 service failed: {err:#}"));
    }
    let _ = event_tx.send(Win32Event::ServiceStopped);
    logger::info("Win32 service thread stopped");
}

unsafe fn run_message_loop(
    event_tx: Sender<Win32Event>,
    command_rx: Receiver<Win32Command>,
) -> anyhow::Result<()> {
    let instance = GetModuleHandleW(None)?;
    let class = wide("AsheWorkerServiceWindow");
    let app_icon = load_app_icon(0, 0);
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        hInstance: instance.into(),
        lpfnWndProc: Some(window_proc),
        lpszClassName: pcwstr(&class),
        hIcon: app_icon,
        hIconSm: load_app_icon(GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON)),
        ..Default::default()
    };
    let _ = RegisterClassExW(&wc);
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        pcwstr(&class),
        pcwstr(&wide("Ashe Worker Service")),
        WS_OVERLAPPEDWINDOW,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        None,
        None,
        Some(instance.into()),
        Some(null_mut()),
    )?;
    set_window_icons(hwnd);
    let state = Box::new(ServiceState {
        event_tx,
        command_rx,
        active: false,
        follow_cursor: false,
        cancel_hotkey_registered: false,
        revert_sentence_hotkey_registered: false,
        clear_transcript_hotkey_registered: false,
        submit_hotkey_registered: false,
        activity_running: false,
        activity_status: "activity tracking starting".to_string(),
        last_artifacts_open: None,
    });
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize);
    register_hotkey(hwnd);
    let timer_id = SetTimer(Some(hwnd), TIMER_SERVICE, TIMER_INTERVAL_MS, None);
    if timer_id == 0 {
        logger::info("Win32 service SetTimer failed");
    }
    add_tray(hwnd, "Ashe Worker - Idle - Win+Shift+H");
    let mut message = MSG::default();
    while GetMessageW(&mut message, None, 0, 0).into() {
        let _ = TranslateMessage(&message);
        DispatchMessageW(&message);
    }
    Ok(())
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ServiceState;
    let state = state_ptr.as_mut();
    match message {
        WM_HOTKEY => match wparam.0 as i32 {
            TOGGLE_HOTKEY_ID => {
                logger::info("Toggle hotkey pressed");
                if let Some(state) = state {
                    let (x, y) = active_input_position();
                    let target_hwnd = target_window(hwnd);
                    let _ = state
                        .event_tx
                        .send(Win32Event::ToggleRequested { target_hwnd, x, y });
                }
                return LRESULT(0);
            }
            CANCEL_HOTKEY_ID => {
                logger::info("Cancel hotkey pressed");
                if let Some(state) = state {
                    let _ = state.event_tx.send(Win32Event::CancelRequested);
                }
                return LRESULT(0);
            }
            REVERT_SENTENCE_HOTKEY_ID => {
                logger::info("Revert sentence hotkey pressed");
                if let Some(state) = state {
                    let _ = state.event_tx.send(Win32Event::RevertLastSentenceRequested);
                }
                return LRESULT(0);
            }
            CLEAR_TRANSCRIPT_HOTKEY_ID => {
                logger::info("Clear transcript hotkey pressed");
                if let Some(state) = state {
                    let _ = state.event_tx.send(Win32Event::ClearTranscriptRequested);
                }
                return LRESULT(0);
            }
            SUBMIT_HOTKEY_ID => {
                logger::info("Submit hotkey pressed");
                if let Some(state) = state {
                    let _ = state.event_tx.send(Win32Event::SubmitRequested);
                }
                return LRESULT(0);
            }
            FIX_GRAMMAR_HOTKEY_ID => {
                logger::info("Fix grammar hotkey pressed");
                if let Some(state) = state {
                    let (x, y) = active_input_position();
                    let target_hwnd = target_window(hwnd);
                    let _ =
                        state
                            .event_tx
                            .send(Win32Event::FixGrammarRequested { target_hwnd, x, y });
                }
                return LRESULT(0);
            }
            ANSWER_QUESTION_HOTKEY_ID => {
                logger::info("Answer question hotkey pressed");
                if let Some(state) = state {
                    let (x, y) = active_input_position();
                    let target_hwnd = target_window(hwnd);
                    let _ = state.event_tx.send(Win32Event::AnswerQuestionRequested {
                        target_hwnd,
                        x,
                        y,
                    });
                }
                return LRESULT(0);
            }
            PASTE_IMAGE_HOTKEY_ID => {
                logger::info("Paste image hotkey pressed");
                if let Some(state) = state {
                    let _ = state.event_tx.send(Win32Event::PasteImageRequested {
                        target_hwnd: target_window(hwnd),
                    });
                }
                return LRESULT(0);
            }
            _ => {}
        },
        WM_TIMER => {
            if let Some(state) = state {
                drain_commands(hwnd, state);
                if state.active || state.follow_cursor {
                    let (x, y) = active_input_position();
                    let _ = state.event_tx.send(Win32Event::PositionChanged { x, y });
                }
            }
            return LRESULT(0);
        }
        WM_TRAY => {
            if lparam.0 as u32 == WM_LBUTTONUP {
                if let Some(state) = state {
                    let now = Instant::now();
                    let should_open = state
                        .last_artifacts_open
                        .is_none_or(|last| now.duration_since(last) >= Duration::from_millis(750));
                    if should_open {
                        state.last_artifacts_open = Some(now);
                        let _ = state.event_tx.send(Win32Event::OpenArtifactsRequested);
                    }
                }
                return LRESULT(0);
            }
            if lparam.0 as u32 == WM_RBUTTONUP {
                if let Some(state) = state {
                    show_tray_menu(
                        hwnd,
                        state.active,
                        state.activity_running,
                        &state.activity_status,
                    );
                }
                return LRESULT(0);
            }
        }
        WM_COMMAND => {
            if let Some(state) = state {
                match wparam.0 & 0xffff {
                    MENU_TOGGLE => {
                        let (x, y) = active_input_position();
                        let _ = state.event_tx.send(Win32Event::ToggleRequested {
                            target_hwnd: target_window(hwnd),
                            x,
                            y,
                        });
                        return LRESULT(0);
                    }
                    MENU_RELOAD_CONFIG => {
                        let _ = state.event_tx.send(Win32Event::ReloadConfigRequested);
                        return LRESULT(0);
                    }
                    MENU_OPEN_LOG => {
                        let _ = state.event_tx.send(Win32Event::OpenLogRequested);
                        return LRESULT(0);
                    }
                    MENU_COPY_LOG_PATH => {
                        let _ = state.event_tx.send(Win32Event::CopyLogPathRequested);
                        return LRESULT(0);
                    }
                    MENU_TOGGLE_ACTIVITY => {
                        let _ = state.event_tx.send(Win32Event::ToggleActivityRequested);
                        return LRESULT(0);
                    }
                    MENU_OPEN_ARTIFACTS => {
                        let _ = state.event_tx.send(Win32Event::OpenArtifactsRequested);
                        return LRESULT(0);
                    }
                    MENU_OPEN_JOURNAL => {
                        let _ = state.event_tx.send(Win32Event::OpenJournalRequested);
                        return LRESULT(0);
                    }
                    MENU_ABOUT => {
                        let _ = state.event_tx.send(Win32Event::AboutRequested);
                        return LRESULT(0);
                    }
                    MENU_QUIT => {
                        let _ = state.event_tx.send(Win32Event::QuitRequested);
                        return LRESULT(0);
                    }
                    _ => {}
                }
            }
        }
        WM_DESTROY => {
            remove_tray(hwnd);
            let _ = UnregisterHotKey(Some(hwnd), TOGGLE_HOTKEY_ID);
            let _ = UnregisterHotKey(Some(hwnd), CANCEL_HOTKEY_ID);
            let _ = UnregisterHotKey(Some(hwnd), REVERT_SENTENCE_HOTKEY_ID);
            let _ = UnregisterHotKey(Some(hwnd), CLEAR_TRANSCRIPT_HOTKEY_ID);
            let _ = UnregisterHotKey(Some(hwnd), SUBMIT_HOTKEY_ID);
            let _ = UnregisterHotKey(Some(hwnd), FIX_GRAMMAR_HOTKEY_ID);
            let _ = UnregisterHotKey(Some(hwnd), ANSWER_QUESTION_HOTKEY_ID);
            let _ = UnregisterHotKey(Some(hwnd), PASTE_IMAGE_HOTKEY_ID);
            if !state_ptr.is_null() {
                let _ = Box::from_raw(state_ptr);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            PostQuitMessage(0);
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, message, wparam, lparam)
}

unsafe fn drain_commands(hwnd: HWND, state: &mut ServiceState) {
    while let Ok(command) = state.command_rx.try_recv() {
        match command {
            Win32Command::SetActive(active) => {
                state.active = active;
                set_cancel_hotkey(hwnd, state, active);
                set_transcript_edit_hotkeys(hwnd, state, active);
                set_submit_hotkey(hwnd, state, active);
            }
            Win32Command::SetFollowCursor(follow) => {
                state.follow_cursor = follow;
            }
            Win32Command::SetTooltip(tooltip) => set_tray_tooltip(hwnd, &tooltip),
            Win32Command::SetActivityStatus { running, status } => {
                state.activity_running = running;
                state.activity_status = status;
            }
            Win32Command::ShowMessageBox { title, text } => message_box(hwnd, &text, &title),
            Win32Command::OpenLog(path) => {
                if let Err(err) = Command::new("notepad.exe").arg(path).spawn() {
                    logger::info(format!("Open log failed: {err:#}"));
                    message_box(
                        hwnd,
                        &format!("Could not open log file: {err}"),
                        "Ashe Worker",
                    );
                }
            }
            Win32Command::OpenPath(path) => {
                let mut path = PathBuf::from(&path);
                while !path.exists() {
                    let Some(parent) = path.parent().map(Path::to_path_buf) else {
                        break;
                    };
                    if parent == path {
                        break;
                    }
                    path = parent;
                }
                if let Err(err) = Command::new("explorer.exe").arg(path).spawn() {
                    logger::info(format!("Open path failed: {err:#}"));
                    message_box(hwnd, &format!("Could not open path: {err}"), "Ashe Worker");
                }
            }
            Win32Command::CopyText(text) => {
                if let Err(err) = injector::copy_text(&text) {
                    logger::info(format!("Copy text failed: {err:#}"));
                    message_box(hwnd, &format!("Could not copy text: {err}"), "Ashe Worker");
                }
            }
            Win32Command::PasteText { target_hwnd, text } => {
                let hwnd = HWND(target_hwnd as *mut c_void);
                let result = injector::paste_text_to(hwnd, &text).map_err(|err| format!("{err:#}"));
                let _ = state.event_tx.send(Win32Event::PasteCompleted(result));
            }
            Win32Command::PastePath { target_hwnd, text } => {
                let hwnd = HWND(target_hwnd as *mut c_void);
                let result = injector::paste_text_to(hwnd, &text).map_err(|err| format!("{err:#}"));
                let _ = state.event_tx.send(Win32Event::PathPasteCompleted(result));
            }
            Win32Command::InjectText {
                target_hwnd,
                text,
                append_after_selection,
            } => {
                let hwnd = HWND(target_hwnd as *mut c_void);
                let result = injector::inject_text_to(hwnd, &text, append_after_selection)
                    .map_err(|err| format!("{err:#}"));
                let _ = state.event_tx.send(Win32Event::PasteCompleted(result));
            }
            Win32Command::Shutdown => {
                let _ = DestroyWindow(hwnd);
                break;
            }
        }
    }
}

unsafe fn register_hotkey(hwnd: HWND) {
    if let Err(err) = RegisterHotKey(
        Some(hwnd),
        TOGGLE_HOTKEY_ID,
        MOD_WIN | MOD_SHIFT | MOD_NOREPEAT,
        'H' as u32,
    ) {
        logger::info(format!("RegisterHotKey failed: {err:#}"));
        message_box(
            hwnd,
            "Win+Shift+H could not be registered. Another app may already be using it.",
            "Ashe Worker",
        );
    }
    if let Err(err) = RegisterHotKey(
        Some(hwnd),
        FIX_GRAMMAR_HOTKEY_ID,
        MOD_WIN | MOD_SHIFT | MOD_NOREPEAT,
        'G' as u32,
    ) {
        logger::info(format!("RegisterHotKey (fix grammar) failed: {err:#}"));
        message_box(
            hwnd,
            "Win+Shift+G could not be registered. Another app may already be using it.",
            "Ashe Worker",
        );
    }
    if let Err(err) = RegisterHotKey(
        Some(hwnd),
        ANSWER_QUESTION_HOTKEY_ID,
        MOD_WIN | MOD_SHIFT | MOD_NOREPEAT,
        'Q' as u32,
    ) {
        logger::info(format!("RegisterHotKey (answer question) failed: {err:#}"));
        message_box(
            hwnd,
            "Win+Shift+Q could not be registered. Another app may already be using it.",
            "Ashe Worker",
        );
    }
    if let Err(error) = RegisterHotKey(
        Some(hwnd),
        PASTE_IMAGE_HOTKEY_ID,
        MOD_CONTROL | MOD_ALT | MOD_NOREPEAT,
        'V' as u32,
    ) {
        logger::info(format!(
            "RegisterHotKey (paste image Ctrl+Alt+V) failed: {error:#}"
        ));
    } else {
        logger::info("Clipboard image hotkey registered as Ctrl+Alt+V");
    }
}

unsafe fn set_cancel_hotkey(hwnd: HWND, state: &mut ServiceState, active: bool) {
    if active == state.cancel_hotkey_registered {
        return;
    }
    if active {
        match RegisterHotKey(
            Some(hwnd),
            CANCEL_HOTKEY_ID,
            MOD_NOREPEAT,
            VK_ESCAPE.0 as u32,
        ) {
            Ok(()) => state.cancel_hotkey_registered = true,
            Err(err) => logger::info(format!("Escape cancel hotkey registration failed: {err:#}")),
        }
    } else {
        let _ = UnregisterHotKey(Some(hwnd), CANCEL_HOTKEY_ID);
        state.cancel_hotkey_registered = false;
    }
}

unsafe fn set_submit_hotkey(hwnd: HWND, state: &mut ServiceState, active: bool) {
    if active == state.submit_hotkey_registered {
        return;
    }
    if active {
        match RegisterHotKey(
            Some(hwnd),
            SUBMIT_HOTKEY_ID,
            MOD_NOREPEAT,
            VK_RETURN.0 as u32,
        ) {
            Ok(()) => state.submit_hotkey_registered = true,
            Err(err) => logger::info(format!("Enter submit hotkey registration failed: {err:#}")),
        }
    } else {
        let _ = UnregisterHotKey(Some(hwnd), SUBMIT_HOTKEY_ID);
        state.submit_hotkey_registered = false;
    }
}

unsafe fn set_transcript_edit_hotkeys(hwnd: HWND, state: &mut ServiceState, active: bool) {
    if active == state.revert_sentence_hotkey_registered
        && active == state.clear_transcript_hotkey_registered
    {
        return;
    }
    if active {
        match RegisterHotKey(
            Some(hwnd),
            REVERT_SENTENCE_HOTKEY_ID,
            MOD_NOREPEAT,
            VK_BACK.0 as u32,
        ) {
            Ok(()) => state.revert_sentence_hotkey_registered = true,
            Err(err) => logger::info(format!(
                "Backspace revert hotkey registration failed: {err:#}"
            )),
        }
        match RegisterHotKey(
            Some(hwnd),
            CLEAR_TRANSCRIPT_HOTKEY_ID,
            MOD_SHIFT | MOD_NOREPEAT,
            VK_BACK.0 as u32,
        ) {
            Ok(()) => state.clear_transcript_hotkey_registered = true,
            Err(err) => logger::info(format!(
                "Shift+Backspace clear hotkey registration failed: {err:#}"
            )),
        }
    } else {
        let _ = UnregisterHotKey(Some(hwnd), REVERT_SENTENCE_HOTKEY_ID);
        let _ = UnregisterHotKey(Some(hwnd), CLEAR_TRANSCRIPT_HOTKEY_ID);
        state.revert_sentence_hotkey_registered = false;
        state.clear_transcript_hotkey_registered = false;
    }
}

unsafe fn active_input_position() -> (i32, i32) {
    let mut cursor = POINT::default();
    if GetCursorPos(&mut cursor).is_err() {
        return (120, 120);
    }

    let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
    let scale = monitor_scale_factor(monitor);
    let overlay_width = overlay_view::WIDTH;
    let overlay_height = overlay_view::HEIGHT;
    let gap = CURSOR_OVERLAY_GAP as f32;
    let cursor_x = cursor.x as f32 / scale;
    let cursor_y = cursor.y as f32 / scale;
    let mut x = cursor_x + gap;
    let mut y = cursor_y + gap;

    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if GetMonitorInfoW(monitor, &mut info).as_bool() {
        let work_left = info.rcWork.left as f32 / scale;
        let work_top = info.rcWork.top as f32 / scale;
        let work_right = info.rcWork.right as f32 / scale;
        let work_bottom = info.rcWork.bottom as f32 / scale;
        if x + overlay_width > work_right {
            x = cursor_x - overlay_width - gap;
        }
        if y + overlay_height > work_bottom {
            y = cursor_y - overlay_height - gap;
        }
        x = clamp_to_work_area(x, work_left, work_right - overlay_width);
        y = clamp_to_work_area(y, work_top, work_bottom - overlay_height);
    }

    (x.round() as i32, y.round() as i32)
}

unsafe fn monitor_scale_factor(monitor: windows::Win32::Graphics::Gdi::HMONITOR) -> f32 {
    let mut dpi_x = 96;
    let mut dpi_y = 96;
    if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y).is_err() || dpi_x == 0 {
        return 1.0;
    }
    dpi_x as f32 / 96.0
}

fn clamp_to_work_area(value: f32, min: f32, max: f32) -> f32 {
    if min > max {
        min
    } else {
        value.clamp(min, max)
    }
}

unsafe fn target_window(service_hwnd: HWND) -> isize {
    let foreground = GetForegroundWindow();
    if foreground.0.is_null() || foreground == service_hwnd {
        0
    } else {
        foreground.0 as isize
    }
}

fn add_tray(hwnd: HWND, tooltip: &str) {
    unsafe {
        let mut data = tray_data(hwnd, tooltip);
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        data.uCallbackMessage = WM_TRAY;
        data.hIcon = load_app_icon(GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
        let _ = Shell_NotifyIconW(NIM_ADD, &data);
    }
}

unsafe fn set_window_icons(hwnd: HWND) {
    let big_icon = load_app_icon(GetSystemMetrics(SM_CXICON), GetSystemMetrics(SM_CYICON));
    let small_icon = load_app_icon(GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
    let _ = SendMessageW(
        hwnd,
        WM_SETICON,
        Some(WPARAM(ICON_BIG as usize)),
        Some(LPARAM(big_icon.0 as isize)),
    );
    let _ = SendMessageW(
        hwnd,
        WM_SETICON,
        Some(WPARAM(ICON_SMALL as usize)),
        Some(LPARAM(small_icon.0 as isize)),
    );
}

unsafe fn load_app_icon(width: i32, height: i32) -> HICON {
    if let Some(icon) = load_app_icon_from_resource(width, height) {
        return icon;
    }

    if let Some(path) = find_app_icon_path() {
        let wide_path = wide(&path.to_string_lossy());
        match LoadImageW(
            None,
            pcwstr(&wide_path),
            IMAGE_ICON,
            width,
            height,
            LR_LOADFROMFILE,
        ) {
            Ok(handle) => {
                logger::info(format!(
                    "Loaded app icon path={} width={width} height={height}",
                    path.display()
                ));
                return HICON(handle.0);
            }
            Err(err) => logger::info(format!(
                "Load app icon failed path={} width={width} height={height}: {err:#}",
                path.display()
            )),
        }
    } else {
        logger::info("App icon file not found; falling back to default Windows icon");
    }

    LoadIconW(None, IDI_APPLICATION).unwrap_or_default()
}

unsafe fn load_app_icon_from_resource(width: i32, height: i32) -> Option<HICON> {
    let module = GetModuleHandleW(None).ok()?;
    let group = FindResourceW(
        Some(module),
        int_resource(APP_ICON_RESOURCE_ID),
        RT_GROUP_ICON,
    );
    if group.is_invalid() {
        return None;
    }

    let group_data = LoadResource(Some(module), group).ok()?;
    let group_ptr = LockResource(group_data) as *const u8;
    let group_size = SizeofResource(Some(module), group) as usize;
    if group_ptr.is_null() || group_size == 0 {
        return None;
    }

    let icon_id = LookupIconIdFromDirectoryEx(group_ptr, true, width, height, LR_DEFAULTCOLOR);
    if icon_id == 0 {
        return None;
    }

    let icon = FindResourceW(Some(module), int_resource(icon_id as u16), RT_ICON);
    if icon.is_invalid() {
        return None;
    }

    let icon_data = LoadResource(Some(module), icon).ok()?;
    let icon_ptr = LockResource(icon_data) as *const u8;
    let icon_size = SizeofResource(Some(module), icon) as usize;
    if icon_ptr.is_null() || icon_size == 0 {
        return None;
    }

    let bits = std::slice::from_raw_parts(icon_ptr, icon_size);
    match CreateIconFromResourceEx(bits, true, 0x0003_0000, width, height, LR_DEFAULTCOLOR) {
        Ok(icon) => {
            logger::info(format!(
                "Loaded embedded app icon resource id={APP_ICON_RESOURCE_ID} width={width} height={height}"
            ));
            Some(icon)
        }
        Err(err) => {
            logger::info(format!(
                "Load embedded app icon failed id={APP_ICON_RESOURCE_ID} width={width} height={height}: {err:#}"
            ));
            None
        }
    }
}

fn int_resource(id: u16) -> windows::core::PCWSTR {
    windows::core::PCWSTR(id as usize as *const u16)
}

fn find_app_icon_path() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf));
    let current_dir = std::env::current_dir().ok();
    let manifest_dir = option_env!("CARGO_MANIFEST_DIR").map(PathBuf::from);

    let candidates = [
        exe_dir.as_ref().map(|dir| dir.join(ICON_FILE_NAME)),
        exe_dir.as_ref().map(|dir| dir.join(ICON_DATA_PATH)),
        current_dir.as_ref().map(|dir| dir.join(ICON_DATA_PATH)),
        manifest_dir.as_ref().map(|dir| dir.join(ICON_DATA_PATH)),
    ];

    candidates.into_iter().flatten().find(|path| path.exists())
}

fn set_tray_tooltip(hwnd: HWND, tooltip: &str) {
    unsafe {
        let mut data = tray_data(hwnd, tooltip);
        data.uFlags = NIF_TIP;
        let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
    }
}

fn remove_tray(hwnd: HWND) {
    unsafe {
        let data = tray_data(hwnd, "");
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    }
}

fn tray_data(hwnd: HWND, tooltip: &str) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        ..Default::default()
    };
    for (idx, value) in wide(tooltip)
        .iter()
        .copied()
        .take(data.szTip.len())
        .enumerate()
    {
        data.szTip[idx] = value;
    }
    data
}

fn show_tray_menu(hwnd: HWND, active: bool, activity_running: bool, activity_status: &str) {
    unsafe {
        let menu = CreatePopupMenu().unwrap_or_default();
        let label = if active {
            "Stop dictation"
        } else {
            "Start dictation"
        };
        let _ = AppendMenuW(menu, MF_STRING, MENU_TOGGLE, pcwstr(&wide(label)));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let activity_label = if activity_running {
            "Pause activity tracking"
        } else {
            "Resume activity tracking"
        };
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            MENU_TOGGLE_ACTIVITY,
            pcwstr(&wide(activity_label)),
        );
        let status = format!("Activity: {activity_status}");
        let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, pcwstr(&wide(&status)));
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            MENU_OPEN_ARTIFACTS,
            pcwstr(&wide("Open artifacts folder")),
        );
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            MENU_OPEN_JOURNAL,
            pcwstr(&wide("Open today's journal")),
        );
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            MENU_RELOAD_CONFIG,
            pcwstr(&wide("Reload config")),
        );
        let _ = AppendMenuW(menu, MF_STRING, MENU_OPEN_LOG, pcwstr(&wide("Open log")));
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            MENU_COPY_LOG_PATH,
            pcwstr(&wide("Copy log path")),
        );
        let _ = AppendMenuW(menu, MF_STRING, MENU_ABOUT, pcwstr(&wide("About")));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, MENU_QUIT, pcwstr(&wide("Quit")));
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, point.x, point.y, Some(0), hwnd, None);
        let _ = DestroyMenu(menu);
    }
}

fn message_box(hwnd: HWND, text: &str, title: &str) {
    unsafe {
        let _ = MessageBoxW(
            Some(hwnd),
            pcwstr(&wide(text)),
            pcwstr(&wide(title)),
            MB_OK | MB_ICONWARNING,
        );
    }
}
