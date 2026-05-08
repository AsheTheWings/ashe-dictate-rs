use crate::audio::AudioCapture;
use crate::config::AppConfig;
use crate::deepgram_client::DeepgramSession;
use crate::llm_client;
use crate::logger;
use crate::overlay_view;
use crate::win32_service::{self, Win32Command, Win32Event};
use crossbeam_channel::{Receiver, Sender};
use iced::{Element, Point, Subscription, Task, window};
use std::thread::JoinHandle;
use std::time::Duration;

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const BUILD_ID: &str = env!("ASHE_BUILD_ID");
type PolishResult = std::result::Result<String, String>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum DictationState {
    Idle,
    Starting,
    Listening,
    Stopping,
    Polishing,
    Inserting,
}

struct DictationSession {
    target_hwnd: isize,
    raw_transcript: String,
}

pub struct UiApp {
    config: AppConfig,
    state: DictationState,
    win32_tx: Sender<Win32Command>,
    win32_rx: Receiver<Win32Event>,
    _win32_thread: JoinHandle<()>,
    transcript_tx: Sender<String>,
    transcript_rx: Receiver<String>,
    status_tx: Sender<String>,
    status_rx: Receiver<String>,
    audio: Option<AudioCapture>,
    deepgram: Option<DeepgramSession>,
    bridge_thread: Option<JoinHandle<()>>,
    session: Option<DictationSession>,
    window_id: Option<window::Id>,
    position: Option<Point>,
    visible: bool,
    status: String,
    transcript: String,
    polished: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Message {
    Tick,
    WindowReady(Option<window::Id>),
    PolishCompleted(PolishResult),
}

impl UiApp {
    pub fn new() -> (Self, Task<Message>) {
        let config = AppConfig::load();
        logger::info(format!("Config loaded: {}", config.log_summary()));
        let (event_tx, win32_rx) = crossbeam_channel::unbounded();
        let (win32_tx, command_rx) = crossbeam_channel::unbounded();
        let win32_thread = win32_service::spawn(event_tx, command_rx);
        let (transcript_tx, transcript_rx) = crossbeam_channel::unbounded();
        let (status_tx, status_rx) = crossbeam_channel::unbounded();
        let app = Self {
            config,
            state: DictationState::Idle,
            win32_tx,
            win32_rx,
            _win32_thread: win32_thread,
            transcript_tx,
            transcript_rx,
            status_tx,
            status_rx,
            audio: None,
            deepgram: None,
            bridge_thread: None,
            session: None,
            window_id: None,
            position: None,
            visible: false,
            status: "Ready".to_string(),
            transcript: String::new(),
            polished: None,
            error: None,
        };
        app.send_win32(Win32Command::SetTooltip(
            "Ashe Dictate RS - Idle - Ctrl+Shift+D".to_string(),
        ));
        (app, window::latest().map(Message::WindowReady))
    }

    pub fn subscription(&self) -> Subscription<Message> {
        iced::time::every(Duration::from_millis(40)).map(|_| Message::Tick)
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::WindowReady(id) => {
                logger::info(format!("Iced main WindowReady id={id:?}"));
                self.window_id = id;
                overlay_view::apply_native_styles();
                self.apply_window_state()
            }
            Message::Tick => self.pump(),
            Message::PolishCompleted(result) => self.finish_polishing(result),
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        overlay_view::view(
            &self.status,
            &self.transcript,
            self.polished.as_deref(),
            self.error.as_deref(),
        )
    }

    fn pump(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        if self.window_id.is_none() {
            tasks.push(window::latest().map(Message::WindowReady));
        }
        while let Ok(event) = self.win32_rx.try_recv() {
            tasks.push(self.handle_win32_event(event));
        }
        self.pump_transcripts_and_statuses();
        self.join_finished_threads();
        if self.state == DictationState::Stopping {
            self.complete_stop_if_ready(&mut tasks);
        }
        Task::batch(tasks)
    }

    fn handle_win32_event(&mut self, event: Win32Event) -> Task<Message> {
        match event {
            Win32Event::ToggleRequested { target_hwnd, x, y } => {
                self.toggle(target_hwnd, Point::new(x as f32, y as f32))
            }
            Win32Event::PositionChanged { x, y } => {
                self.position = Some(Point::new(x as f32, y as f32));
                if self.visible {
                    self.apply_window_state()
                } else {
                    Task::none()
                }
            }
            Win32Event::ReloadConfigRequested => {
                self.reload_config();
                Task::none()
            }
            Win32Event::OpenLogRequested => {
                self.send_win32(Win32Command::OpenLog(
                    logger::log_path().display().to_string(),
                ));
                Task::none()
            }
            Win32Event::CopyLogPathRequested => {
                self.send_win32(Win32Command::CopyText(
                    logger::log_path().display().to_string(),
                ));
                self.send_win32(Win32Command::SetTooltip(
                    "Ashe Dictate RS - Log path copied - Ctrl+Shift+D".to_string(),
                ));
                Task::none()
            }
            Win32Event::AboutRequested => {
                self.show_about();
                Task::none()
            }
            Win32Event::QuitRequested => self.quit(),
            Win32Event::PasteCompleted(result) => {
                self.finish_insert(result);
                Task::none()
            }
            Win32Event::ServiceStopped => {
                logger::info("Win32 service stopped event received");
                Task::none()
            }
        }
    }

    fn pump_transcripts_and_statuses(&mut self) {
        while let Ok(text) = self.transcript_rx.try_recv() {
            if let Some(session) = self.session.as_mut() {
                session.raw_transcript.push_str(&text);
                self.transcript = session.raw_transcript.clone();
                self.polished = None;
                self.error = None;
            } else {
                logger::info(format!("Transcript without active session: {text}"));
            }
        }
        while let Ok(status) = self.status_rx.try_recv() {
            logger::info(format!("Status: {status}"));
            self.status = status.clone();
            if !(self.state == DictationState::Stopping && status == "Idle") {
                self.send_win32(Win32Command::SetTooltip(format!(
                    "Ashe Dictate RS - {status} - Ctrl+Shift+D"
                )));
            }
            if (status.starts_with("Listening") || status == "Speech detected")
                && self.state == DictationState::Starting
            {
                self.state = DictationState::Listening;
            }
            if status.starts_with("Error:") {
                self.request_stop();
            }
        }
    }

    fn toggle(&mut self, target_hwnd: isize, position: Point) -> Task<Message> {
        match self.state {
            DictationState::Idle => self.start(target_hwnd, position),
            DictationState::Starting | DictationState::Listening => {
                self.request_stop();
                Task::none()
            }
            DictationState::Stopping => {
                logger::info("Stop already in progress");
                Task::none()
            }
            DictationState::Polishing | DictationState::Inserting => {
                logger::info("Polishing/inserting already in progress");
                Task::none()
            }
        }
    }

    fn start(&mut self, target_hwnd: isize, position: Point) -> Task<Message> {
        if let Err(err) = self.config.validate_for_dictation() {
            logger::info(format!("Config validation failed: {err:#}"));
            self.send_win32(Win32Command::ShowMessageBox {
                title: "Ashe Dictate RS".to_string(),
                text: format!("Cannot start dictation: {err}"),
            });
            return Task::none();
        }
        logger::info("Start dictation requested");
        self.state = DictationState::Starting;
        self.visible = true;
        self.position = Some(position);
        self.status = "Connecting...".to_string();
        self.transcript.clear();
        self.polished = None;
        self.error = None;
        self.session = Some(DictationSession {
            target_hwnd,
            raw_transcript: String::new(),
        });
        self.send_win32(Win32Command::SetActive(true));
        self.send_win32(Win32Command::SetTooltip(
            "Ashe Dictate RS - Connecting... - Ctrl+Shift+D".to_string(),
        ));
        let (audio_tx, mut audio_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        let audio = match AudioCapture::start(audio_tx, self.config.output_sample_rate) {
            Ok(audio) => audio,
            Err(err) => {
                logger::info(format!("Audio start failed: {err:#}"));
                self.finish_without_transcript();
                self.send_win32(Win32Command::ShowMessageBox {
                    title: "Ashe Dictate RS".to_string(),
                    text: format!("Audio capture failed: {err}"),
                });
                return Task::none();
            }
        };
        let deepgram = DeepgramSession::start(
            self.config.clone(),
            audio.sample_rate(),
            self.transcript_tx.clone(),
            self.status_tx.clone(),
        );
        let dg_audio = deepgram.audio_sender();
        self.bridge_thread = Some(std::thread::spawn(move || {
            while let Some(chunk) = audio_rx.blocking_recv() {
                if dg_audio.send(chunk).is_err() {
                    logger::info("Deepgram audio channel closed");
                    break;
                }
            }
            logger::info("Audio bridge thread exited");
        }));
        self.audio = Some(audio);
        self.deepgram = Some(deepgram);
        self.apply_window_state()
    }

    fn request_stop(&mut self) {
        if matches!(
            self.state,
            DictationState::Idle | DictationState::Polishing | DictationState::Inserting
        ) {
            return;
        }
        logger::info("Stop dictation requested");
        self.state = DictationState::Stopping;
        self.status = "Finalizing transcript...".to_string();
        self.send_win32(Win32Command::SetTooltip(
            "Ashe Dictate RS - Stopping... - Ctrl+Shift+D".to_string(),
        ));
        if let Some(mut audio) = self.audio.take() {
            audio.stop();
        }
        if let Some(deepgram) = self.deepgram.as_mut() {
            deepgram.request_stop();
        }
    }

    fn complete_stop_if_ready(&mut self, tasks: &mut Vec<Task<Message>>) {
        let deepgram_done = self
            .deepgram
            .as_mut()
            .map_or(true, DeepgramSession::join_if_finished);
        if deepgram_done {
            self.deepgram.take();
        }
        if self.bridge_thread.is_none() && deepgram_done {
            logger::info("Dictation stopped cleanly");
            tasks.push(self.begin_polishing());
        }
    }

    fn begin_polishing(&mut self) -> Task<Message> {
        let Some(session) = self.session.as_ref() else {
            self.finish_without_transcript();
            return Task::none();
        };
        let raw = session.raw_transcript.trim().to_string();
        if raw.is_empty() {
            logger::info("No transcript captured");
            self.finish_without_transcript();
            return Task::none();
        }
        self.state = DictationState::Polishing;
        self.status = "Polishing with Kimi...".to_string();
        self.send_win32(Win32Command::SetTooltip(
            "Ashe Dictate RS - Polishing... - Ctrl+Shift+D".to_string(),
        ));
        let config = self.config.clone();
        Task::perform(
            async move {
                llm_client::polish_transcript(config, raw)
                    .await
                    .map_err(|err| format!("{err:#}"))
            },
            Message::PolishCompleted,
        )
    }

    fn finish_polishing(&mut self, result: PolishResult) -> Task<Message> {
        if self.state != DictationState::Polishing {
            return Task::none();
        }
        let Some(session) = self.session.as_ref() else {
            self.finish_without_transcript();
            return Task::none();
        };
        let raw = session.raw_transcript.trim().to_string();
        let text = match result {
            Ok(text) => {
                logger::info("LLM polishing completed");
                text
            }
            Err(err) => {
                logger::info(format!("LLM polishing failed: {err}"));
                self.error = Some("LLM failed; inserting raw transcript".to_string());
                raw
            }
        };
        self.state = DictationState::Inserting;
        self.polished = Some(text.clone());
        self.status = "Inserting...".to_string();
        self.send_win32(Win32Command::PasteText {
            target_hwnd: session.target_hwnd,
            text,
        });
        Task::none()
    }

    fn finish_insert(&mut self, result: Result<(), String>) {
        if let Err(err) = result {
            logger::info(format!("Text injection failed: {err}"));
            self.error = Some("Paste failed".to_string());
            self.send_win32(Win32Command::SetTooltip(
                "Ashe Dictate RS - Paste error - Ctrl+Shift+D".to_string(),
            ));
        } else {
            self.send_win32(Win32Command::SetTooltip(
                "Ashe Dictate RS - Inserted - Ctrl+Shift+D".to_string(),
            ));
        }
        self.session = None;
        self.state = DictationState::Idle;
        self.visible = false;
        self.send_win32(Win32Command::SetActive(false));
    }

    fn finish_without_transcript(&mut self) {
        self.session = None;
        self.state = DictationState::Idle;
        self.visible = false;
        self.send_win32(Win32Command::SetActive(false));
        self.send_win32(Win32Command::SetTooltip(
            "Ashe Dictate RS - Idle - Ctrl+Shift+D".to_string(),
        ));
    }

    fn reload_config(&mut self) {
        if self.state != DictationState::Idle {
            self.send_win32(Win32Command::ShowMessageBox {
                title: "Ashe Dictate RS".to_string(),
                text: "Stop dictation before reloading config.".to_string(),
            });
            return;
        }
        self.config = AppConfig::load();
        logger::info(format!("Config reloaded: {}", self.config.log_summary()));
        self.send_win32(Win32Command::SetTooltip(
            "Ashe Dictate RS - Config reloaded - Ctrl+Shift+D".to_string(),
        ));
    }

    fn show_about(&self) {
        self.send_win32(Win32Command::ShowMessageBox {
            title: "About Ashe Dictate RS".to_string(),
            text: format!(
                "Ashe Dictate RS\r\nVersion: {}\r\nBuild: {}\r\n\r\nHotkey: Ctrl+Shift+D\r\nConfig: {}\r\nLog: {}",
                APP_VERSION,
                BUILD_ID,
                self.config.log_summary(),
                logger::log_path().display()
            ),
        });
    }

    fn quit(&mut self) -> Task<Message> {
        logger::info("Quit requested");
        self.request_stop();
        self.send_win32(Win32Command::Shutdown);
        if let Some(id) = self.window_id {
            window::close(id)
        } else {
            Task::none()
        }
    }

    fn join_finished_threads(&mut self) {
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
    }

    fn apply_window_state(&self) -> Task<Message> {
        let Some(id) = self.window_id else {
            return Task::none();
        };
        overlay_view::apply_window_state(id, self.position, self.visible)
    }

    fn send_win32(&self, command: Win32Command) {
        if let Err(err) = self.win32_tx.send(command) {
            logger::info(format!("Win32 command send failed: {err}"));
        }
    }
}

impl Drop for UiApp {
    fn drop(&mut self) {
        self.request_stop();
        let _ = self.win32_tx.send(Win32Command::Shutdown);
    }
}
