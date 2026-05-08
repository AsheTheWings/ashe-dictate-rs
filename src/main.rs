#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(unsafe_op_in_unsafe_fn)]
mod audio;
mod config;
mod deepgram_client;
mod injector;
mod logger;
mod overlay;
mod util;

use anyhow::Result;
use audio::AudioCapture;
use config::AppConfig;
use crossbeam_channel::{Receiver, Sender};
use deepgram_client::DeepgramSession;
use overlay::Overlay;
use std::process::Command;
use std::ptr::null_mut;
use std::thread::JoinHandle;
use util::{pcwstr, wide};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
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
const TIMER_APP: usize = 2001;
const WM_TRAY: u32 = WM_APP + 1;
const MENU_TOGGLE: usize = 3001;
const MENU_RELOAD_CONFIG: usize = 3002;
const MENU_OPEN_LOG: usize = 3003;
const MENU_COPY_LOG_PATH: usize = 3004;
const MENU_ABOUT: usize = 3005;
const MENU_QUIT: usize = 3006;

#[derive(Clone, Copy, PartialEq, Eq)]
enum DictationState {
    Idle,
    Starting,
    Listening,
    Stopping,
}

struct App {
    hwnd: HWND,
    config: AppConfig,
    overlay: Overlay,
    state: DictationState,
    audio: Option<AudioCapture>,
    deepgram: Option<DeepgramSession>,
    bridge_thread: Option<JoinHandle<()>>,
    transcript_tx: Sender<String>,
    transcript_rx: Receiver<String>,
    status_tx: Sender<String>,
    status_rx: Receiver<String>,
}

impl App {
    fn new(hwnd: HWND) -> Result<Self> {
        let config = AppConfig::load();
        logger::info(format!("Config loaded: {}", config.log_summary()));
        let overlay = Overlay::create()?;
        let (transcript_tx, transcript_rx) = crossbeam_channel::unbounded();
        let (status_tx, status_rx) = crossbeam_channel::unbounded();
        Ok(Self {
            hwnd,
            config,
            overlay,
            state: DictationState::Idle,
            audio: None,
            deepgram: None,
            bridge_thread: None,
            transcript_tx,
            transcript_rx,
            status_tx,
            status_rx,
        })
    }

    fn initialize(&self) {
        unsafe {
            if let Err(err) = RegisterHotKey(
                Some(self.hwnd),
                HOTKEY_ID,
                MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT,
                'D' as u32,
            ) {
                logger::info(format!("RegisterHotKey failed: {err:#}"));
                message_box(
                    self.hwnd,
                    "Ctrl+Shift+D could not be registered. Another app may already be using it.",
                    "Ashe Dictate RS",
                );
            }
            let timer_id = SetTimer(Some(self.hwnd), TIMER_APP, 80, None);
            if timer_id == 0 {
                logger::info("SetTimer failed");
            }
        }
        add_tray(self.hwnd, "Ashe Dictate RS - Idle - Ctrl+Shift+D");
        if self.config.deepgram_api_key.is_empty() {
            message_box(
                self.hwnd,
                "DEEPGRAM_API_KEY was not found.",
                "Ashe Dictate RS",
            );
        }
    }

    fn toggle(&mut self) {
        match self.state {
            DictationState::Idle => self.start(),
            DictationState::Starting | DictationState::Listening => self.request_stop(),
            DictationState::Stopping => logger::info("Stop already in progress"),
        }
    }

    fn is_active(&self) -> bool {
        matches!(
            self.state,
            DictationState::Starting | DictationState::Listening | DictationState::Stopping
        )
    }

    fn should_show_overlay(&self) -> bool {
        matches!(self.state, DictationState::Starting | DictationState::Listening)
    }

    fn start(&mut self) {
        if self.state != DictationState::Idle {
            return;
        }
        if let Err(err) = self.config.validate_for_dictation() {
            logger::info(format!("Config validation failed: {err:#}"));
            message_box(
                self.hwnd,
                &format!("Cannot start dictation: {err}"),
                "Ashe Dictate RS",
            );
            return;
        }

        logger::info("Start dictation requested");
        self.state = DictationState::Starting;
        self.overlay.show_near_cursor();
        set_tray_tooltip(self.hwnd, "Ashe Dictate RS - Connecting... - Ctrl+Shift+D");

        let (audio_tx, mut audio_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        let audio = match AudioCapture::start(audio_tx, self.config.output_sample_rate) {
            Ok(audio) => audio,
            Err(err) => {
                logger::info(format!("Audio start failed: {err:#}"));
                self.state = DictationState::Idle;
                self.overlay.hide();
                set_tray_tooltip(self.hwnd, "Ashe Dictate RS - Audio error - Ctrl+Shift+D");
                message_box(
                    self.hwnd,
                    &format!("Audio capture failed: {err}"),
                    "Ashe Dictate RS",
                );
                return;
            }
        };

        let deepgram = DeepgramSession::start(
            self.config.clone(),
            audio.sample_rate(),
            self.transcript_tx.clone(),
            self.status_tx.clone(),
        );
        let dg_audio = deepgram.audio_sender();
        let bridge_thread = std::thread::spawn(move || {
            while let Some(chunk) = audio_rx.blocking_recv() {
                if dg_audio.send(chunk).is_err() {
                    logger::info("Deepgram audio channel closed");
                    break;
                }
            }
            logger::info("Audio bridge thread exited");
        });

        self.audio = Some(audio);
        self.deepgram = Some(deepgram);
        self.bridge_thread = Some(bridge_thread);
    }

    fn request_stop(&mut self) {
        if self.state == DictationState::Idle {
            return;
        }
        logger::info("Stop dictation requested");
        self.state = DictationState::Stopping;
        self.overlay.hide();
        set_tray_tooltip(self.hwnd, "Ashe Dictate RS - Stopping... - Ctrl+Shift+D");
        if let Some(mut audio) = self.audio.take() {
            audio.stop();
        }
        if let Some(deepgram) = self.deepgram.as_mut() {
            deepgram.request_stop();
        }
        self.complete_stop_if_ready();
    }

    fn pump(&mut self) {
        while let Ok(text) = self.transcript_rx.try_recv() {
            if let Err(err) = injector::paste_text(&text) {
                logger::info(format!("Text injection failed: {err:#}"));
                set_tray_tooltip(self.hwnd, "Ashe Dictate RS - Paste error - Ctrl+Shift+D");
            }
        }

        while let Ok(status) = self.status_rx.try_recv() {
            logger::info(format!("Status: {status}"));
            set_tray_tooltip(
                self.hwnd,
                &format!("Ashe Dictate RS - {status} - Ctrl+Shift+D"),
            );
            if status.starts_with("Listening") || status == "Speech detected" {
                if self.state == DictationState::Starting {
                    self.state = DictationState::Listening;
                }
            }
            if status.starts_with("Error:") {
                self.request_stop();
            }
        }

        if self.state == DictationState::Stopping {
            self.complete_stop_if_ready();
        }
    }

    fn complete_stop_if_ready(&mut self) {
        if self
            .bridge_thread
            .as_ref()
            .is_some_and(|thread| thread.is_finished())
        {
            if let Some(thread) = self.bridge_thread.take() {
                match thread.join() {
                    Ok(()) => logger::info("Audio bridge thread joined"),
                    Err(_) => logger::info("Audio bridge thread panicked"),
                }
            }
        }

        let deepgram_done = match self.deepgram.as_mut() {
            Some(deepgram) => deepgram.join_if_finished(),
            None => true,
        };
        if deepgram_done {
            self.deepgram.take();
        }

        let bridge_done = self.bridge_thread.is_none();
        if self.state == DictationState::Stopping && bridge_done && deepgram_done {
            self.state = DictationState::Idle;
            set_tray_tooltip(self.hwnd, "Ashe Dictate RS - Idle - Ctrl+Shift+D");
            logger::info("Dictation stopped cleanly");
        }
    }
    fn reload_config(&mut self) {
        if self.is_active() {
            message_box(
                self.hwnd,
                "Stop dictation before reloading config.",
                "Ashe Dictate RS",
            );
            return;
        }
        self.config = AppConfig::load();
        logger::info(format!("Config reloaded: {}", self.config.log_summary()));
        set_tray_tooltip(
            self.hwnd,
            "Ashe Dictate RS - Config reloaded - Ctrl+Shift+D",
        );
    }

    fn open_log(&self) {
        let path = logger::log_path();
        if let Err(err) = Command::new("notepad.exe").arg(&path).spawn() {
            logger::info(format!("Open log failed: {err:#}"));
            message_box(
                self.hwnd,
                &format!("Could not open log file: {err}"),
                "Ashe Dictate RS",
            );
        }
    }

    fn copy_log_path(&self) {
        let path = logger::log_path().display().to_string();
        match injector::copy_text(&path) {
            Ok(()) => set_tray_tooltip(
                self.hwnd,
                "Ashe Dictate RS - Log path copied - Ctrl+Shift+D",
            ),
            Err(err) => {
                logger::info(format!("Copy log path failed: {err:#}"));
                message_box(
                    self.hwnd,
                    &format!("Could not copy log path: {err}"),
                    "Ashe Dictate RS",
                );
            }
        }
    }

    fn show_about(&self) {
        message_box(
            self.hwnd,
            &format!(
                "Ashe Dictate RS\r\n\r\nHotkey: Ctrl+Shift+D\r\nConfig: {}\r\nLog: {}",
                self.config.log_summary(),
                logger::log_path().display()
            ),
            "About Ashe Dictate RS",
        );
    }
}

fn main() -> Result<()> {
    logger::init();
    logger::info(format!("Log path: {}", logger::log_path().display()));
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = wide("AsheDictateRsHiddenWindow");
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
            pcwstr(&wide("Ashe Dictate RS")),
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
        let app = Box::new(App::new(hwnd)?);
        app.initialize();
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(app) as isize);
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).into() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let app_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
    let app = unsafe { app_ptr.as_mut() };
    match message {
        WM_HOTKEY => {
            if wparam.0 as i32 == HOTKEY_ID {
                logger::info("Hotkey pressed");
                if let Some(app) = app {
                    app.toggle();
                }
                return LRESULT(0);
            }
        }
        WM_TIMER => {
            if let Some(app) = app {
                if app.should_show_overlay() {
                    app.overlay.update_position();
                }
                app.pump();
            }
            return LRESULT(0);
        }
        WM_TRAY => {
            if lparam.0 as u32 == WM_RBUTTONUP {
                show_tray_menu(hwnd, app.map(|app| app.is_active()).unwrap_or(false));
                return LRESULT(0);
            }
        }
        WM_COMMAND => match wparam.0 & 0xffff {
            MENU_TOGGLE => {
                if let Some(app) = app {
                    app.toggle();
                }
                return LRESULT(0);
            }
            MENU_RELOAD_CONFIG => {
                if let Some(app) = app {
                    app.reload_config();
                }
                return LRESULT(0);
            }
            MENU_OPEN_LOG => {
                if let Some(app) = app {
                    app.open_log();
                }
                return LRESULT(0);
            }
            MENU_COPY_LOG_PATH => {
                if let Some(app) = app {
                    app.copy_log_path();
                }
                return LRESULT(0);
            }
            MENU_ABOUT => {
                if let Some(app) = app {
                    app.show_about();
                }
                return LRESULT(0);
            }
            MENU_QUIT => {
                let _ = DestroyWindow(hwnd);
                return LRESULT(0);
            }
            _ => {}
        },
        WM_DESTROY => {
            if let Some(app) = app {
                app.request_stop();
            }
            remove_tray(hwnd);
            let _ = UnregisterHotKey(Some(hwnd), HOTKEY_ID);
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
            if !ptr.is_null() {
                let _ = unsafe { Box::from_raw(ptr) };
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            PostQuitMessage(0);
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
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
