#![allow(unsafe_op_in_unsafe_fn)]

use crate::injector;
use crate::logger;
use crate::util::{pcwstr, wide};
use crossbeam_channel::{Receiver, Sender};
use std::ffi::c_void;
use std::process::Command;
use std::ptr::null_mut;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey, UnregisterHotKey,
};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

const HOTKEY_ID: i32 = 1001;
const TIMER_SERVICE: usize = 2001;
const WM_TRAY: u32 = WM_APP + 1;
const MENU_TOGGLE: usize = 3001;
const MENU_RELOAD_CONFIG: usize = 3002;
const MENU_OPEN_LOG: usize = 3003;
const MENU_COPY_LOG_PATH: usize = 3004;
const MENU_ABOUT: usize = 3005;
const MENU_QUIT: usize = 3006;

#[derive(Debug, Clone)]
pub enum Win32Event {
    ToggleRequested { target_hwnd: isize, x: i32, y: i32 },
    PositionChanged { x: i32, y: i32 },
    ReloadConfigRequested,
    OpenLogRequested,
    CopyLogPathRequested,
    AboutRequested,
    QuitRequested,
    PasteCompleted(Result<(), String>),
    ServiceStopped,
}

#[derive(Debug, Clone)]
pub enum Win32Command {
    SetActive(bool),
    SetTooltip(String),
    ShowMessageBox { title: String, text: String },
    OpenLog(String),
    CopyText(String),
    PasteText { target_hwnd: isize, text: String },
    Shutdown,
}

struct ServiceState {
    event_tx: Sender<Win32Event>,
    command_rx: Receiver<Win32Command>,
    active: bool,
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
    let class = wide("AsheDictateRsServiceWindow");
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        hInstance: instance.into(),
        lpfnWndProc: Some(window_proc),
        lpszClassName: pcwstr(&class),
        hIcon: LoadIconW(None, IDI_APPLICATION)?,
        ..Default::default()
    };
    let _ = RegisterClassExW(&wc);
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        pcwstr(&class),
        pcwstr(&wide("Ashe Dictate RS Service")),
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
    let state = Box::new(ServiceState {
        event_tx,
        command_rx,
        active: false,
    });
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize);
    register_hotkey(hwnd);
    let timer_id = SetTimer(Some(hwnd), TIMER_SERVICE, 80, None);
    if timer_id == 0 {
        logger::info("Win32 service SetTimer failed");
    }
    add_tray(hwnd, "Ashe Dictate RS - Idle - Ctrl+Shift+D");
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
        WM_HOTKEY => {
            if wparam.0 as i32 == HOTKEY_ID {
                logger::info("Hotkey pressed");
                if let Some(state) = state {
                    let (x, y) = active_input_position();
                    let target_hwnd = target_window(hwnd);
                    let _ = state
                        .event_tx
                        .send(Win32Event::ToggleRequested { target_hwnd, x, y });
                }
                return LRESULT(0);
            }
        }
        WM_TIMER => {
            if let Some(state) = state {
                drain_commands(hwnd, state);
                if state.active {
                    let (x, y) = active_input_position();
                    let _ = state.event_tx.send(Win32Event::PositionChanged { x, y });
                }
            }
            return LRESULT(0);
        }
        WM_TRAY => {
            if lparam.0 as u32 == WM_RBUTTONUP {
                if let Some(state) = state {
                    show_tray_menu(hwnd, state.active);
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
            let _ = UnregisterHotKey(Some(hwnd), HOTKEY_ID);
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
            Win32Command::SetActive(active) => state.active = active,
            Win32Command::SetTooltip(tooltip) => set_tray_tooltip(hwnd, &tooltip),
            Win32Command::ShowMessageBox { title, text } => message_box(hwnd, &text, &title),
            Win32Command::OpenLog(path) => {
                if let Err(err) = Command::new("notepad.exe").arg(path).spawn() {
                    logger::info(format!("Open log failed: {err:#}"));
                    message_box(
                        hwnd,
                        &format!("Could not open log file: {err}"),
                        "Ashe Dictate RS",
                    );
                }
            }
            Win32Command::CopyText(text) => {
                if let Err(err) = injector::copy_text(&text) {
                    logger::info(format!("Copy text failed: {err:#}"));
                    message_box(
                        hwnd,
                        &format!("Could not copy text: {err}"),
                        "Ashe Dictate RS",
                    );
                }
            }
            Win32Command::PasteText { target_hwnd, text } => {
                let hwnd = HWND(target_hwnd as *mut c_void);
                let result = injector::paste_text_to(hwnd, &text).map_err(|err| format!("{err:#}"));
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
        HOTKEY_ID,
        MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT,
        'D' as u32,
    ) {
        logger::info(format!("RegisterHotKey failed: {err:#}"));
        message_box(
            hwnd,
            "Ctrl+Shift+D could not be registered. Another app may already be using it.",
            "Ashe Dictate RS",
        );
    }
}

unsafe fn active_input_position() -> (i32, i32) {
    let foreground = GetForegroundWindow();
    if !foreground.0.is_null() {
        let thread_id = GetWindowThreadProcessId(foreground, None);
        let mut info = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        if GetGUIThreadInfo(thread_id, &mut info).is_ok() && !info.hwndCaret.0.is_null() {
            let mut point = POINT {
                x: info.rcCaret.left,
                y: info.rcCaret.bottom,
            };
            if ClientToScreen(info.hwndCaret, &mut point).as_bool() {
                return (point.x + 10, point.y + 16);
            }
        }
    }
    let mut point = POINT::default();
    if GetCursorPos(&mut point).is_ok() {
        return (point.x + 18, point.y + 18);
    }
    (120, 120)
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
        data.hIcon = LoadIconW(None, IDI_APPLICATION).unwrap_or_default();
        let _ = Shell_NotifyIconW(NIM_ADD, &data);
    }
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
    let mut data = NOTIFYICONDATAW::default();
    data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = hwnd;
    data.uID = 1;
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

fn show_tray_menu(hwnd: HWND, active: bool) {
    unsafe {
        let menu = CreatePopupMenu().unwrap_or_default();
        let label = if active {
            "Stop dictation"
        } else {
            "Start dictation"
        };
        let _ = AppendMenuW(menu, MF_STRING, MENU_TOGGLE, pcwstr(&wide(label)));
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
