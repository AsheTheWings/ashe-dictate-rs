use crate::audio::AudioCapture;
use crate::config::AppConfig;
use crate::deepgram_client::DeepgramSession;
use crate::injector;
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
const OVERLAY_TICK_MS: u64 = 16;
const OVERLAY_SMOOTHING: f32 = 0.28;
const OVERLAY_SNAP_DISTANCE: f32 = 1.0;
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
    selected_context: Option<String>,
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
    target_position: Option<Point>,
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
            target_position: None,
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
        iced::time::every(Duration::from_millis(OVERLAY_TICK_MS)).map(|_| Message::Tick)
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
        if self.pump_transcripts_and_statuses() {
            tasks.push(overlay_view::scroll_transcript_to_end());
        }
        self.join_finished_threads();
        if self.visible && self.advance_overlay_position() {
            tasks.push(self.apply_window_state());
        }
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
            Win32Event::CancelRequested => self.cancel_operation(),
            Win32Event::RevertLastSentenceRequested => self.revert_last_sentence(),
            Win32Event::ClearTranscriptRequested => self.clear_transcript(),
            Win32Event::SubmitRequested => {
                if matches!(
                    self.state,
                    DictationState::Starting | DictationState::Listening
                ) {
                    self.request_stop();
                }
                Task::none()
            }
            Win32Event::PositionChanged { x, y } => {
                self.target_position = Some(Point::new(x as f32, y as f32));
                Task::none()
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
            Win32Event::PasteCompleted(result) => self.finish_insert(result),
            Win32Event::ServiceStopped => {
                logger::info("Win32 service stopped event received");
                Task::none()
            }
        }
    }

    fn pump_transcripts_and_statuses(&mut self) -> bool {
        let mut transcript_updated = false;
        while let Ok(text) = self.transcript_rx.try_recv() {
            if let Some(session) = self.session.as_mut() {
                session.raw_transcript.push_str(&text);
                self.transcript = session.raw_transcript.clone();
                self.polished = None;
                self.error = None;
                transcript_updated = true;
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
        transcript_updated
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

    fn cancel_operation(&mut self) -> Task<Message> {
        if self.state == DictationState::Idle {
            return Task::none();
        }
        logger::info("Cancel operation requested");
        self.stop_workers_for_cancel();
        self.session = None;
        self.state = DictationState::Idle;
        self.visible = false;
        self.status = "Cancelled".to_string();
        self.polished = None;
        self.error = None;
        self.send_win32(Win32Command::SetActive(false));
        self.send_win32(Win32Command::SetTooltip(
            "Ashe Dictate RS - Cancelled - Ctrl+Shift+D".to_string(),
        ));
        self.apply_window_state()
    }

    fn revert_last_sentence(&mut self) -> Task<Message> {
        if !matches!(self.state, DictationState::Starting | DictationState::Listening) {
            return Task::none();
        }
        let Some(session) = self.session.as_mut() else {
            return Task::none();
        };
        let updated = remove_last_sentence(&session.raw_transcript);
        if updated == session.raw_transcript {
            return Task::none();
        }
        logger::info("Reverted last transcript sentence");
        session.raw_transcript = updated;
        self.transcript = session.raw_transcript.clone();
        self.polished = None;
        self.error = None;
        self.send_win32(Win32Command::SetTooltip(
            "Ashe Dictate RS - Last sentence reverted - Ctrl+Shift+D".to_string(),
        ));
        overlay_view::scroll_transcript_to_end()
    }

    fn clear_transcript(&mut self) -> Task<Message> {
        if !matches!(self.state, DictationState::Starting | DictationState::Listening) {
            return Task::none();
        }
        let Some(session) = self.session.as_mut() else {
            return Task::none();
        };
        if session.raw_transcript.is_empty() {
            return Task::none();
        }
        logger::info("Cleared transcript buffer");
        session.raw_transcript.clear();
        self.transcript.clear();
        self.polished = None;
        self.error = None;
        self.send_win32(Win32Command::SetTooltip(
            "Ashe Dictate RS - Transcript cleared - Ctrl+Shift+D".to_string(),
        ));
        overlay_view::scroll_transcript_to_end()
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
        self.target_position = Some(position);
        self.status = "Connecting...".to_string();
        self.transcript.clear();
        self.polished = None;
        self.error = None;
        let selected_context = self.capture_selected_context();
        self.session = Some(DictationSession {
            target_hwnd,
            selected_context,
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
                let hide_task = self.finish_without_transcript();
                self.send_win32(Win32Command::ShowMessageBox {
                    title: "Ashe Dictate RS".to_string(),
                    text: format!("Audio capture failed: {err}"),
                });
                return hide_task;
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
            return self.finish_without_transcript();
        };
        let raw = session.raw_transcript.trim().to_string();
        if raw.is_empty() {
            logger::info("No transcript captured");
            return self.finish_without_transcript();
        }
        self.state = DictationState::Polishing;
        self.status = "Polishing with Kimi...".to_string();
        self.send_win32(Win32Command::SetTooltip(
            "Ashe Dictate RS - Polishing... - Ctrl+Shift+D".to_string(),
        ));
        let config = self.config.clone();
        let context = session.selected_context.clone();
        Task::perform(
            async move {
                llm_client::polish_transcript(config, raw, context)
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
            return self.finish_without_transcript();
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

    fn finish_insert(&mut self, result: Result<(), String>) -> Task<Message> {
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
        self.hide_overlay_after_session()
    }

    fn finish_without_transcript(&mut self) -> Task<Message> {
        self.send_win32(Win32Command::SetTooltip(
            "Ashe Dictate RS - Idle - Ctrl+Shift+D".to_string(),
        ));
        self.hide_overlay_after_session()
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

    fn capture_selected_context(&self) -> Option<String> {
        match injector::capture_selected_text() {
            Ok(context) => context,
            Err(err) => {
                logger::info(format!("Selection context capture failed: {err:#}"));
                None
            }
        }
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

    fn hide_overlay_after_session(&mut self) -> Task<Message> {
        self.session = None;
        self.state = DictationState::Idle;
        self.visible = false;
        self.target_position = self.position;
        self.send_win32(Win32Command::SetActive(false));
        self.apply_window_state()
    }

    fn stop_workers_for_cancel(&mut self) {
        if let Some(mut audio) = self.audio.take() {
            audio.stop();
        }
        if let Some(mut deepgram) = self.deepgram.take() {
            deepgram.request_stop();
        }
    }

    fn advance_overlay_position(&mut self) -> bool {
        let Some(target) = self.target_position else {
            return false;
        };
        let Some(current) = self.position else {
            self.position = Some(target);
            return true;
        };
        let dx = target.x - current.x;
        let dy = target.y - current.y;
        if dx.hypot(dy) <= OVERLAY_SNAP_DISTANCE {
            if current != target {
                self.position = Some(target);
                return true;
            }
            return false;
        }
        self.position = Some(Point::new(
            current.x + dx * OVERLAY_SMOOTHING,
            current.y + dy * OVERLAY_SMOOTHING,
        ));
        true
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

fn remove_last_sentence(text: &str) -> String {
    let trimmed_end = text.trim_end();
    if trimmed_end.is_empty() {
        return String::new();
    }

    let mut chars = trimmed_end.char_indices().rev().peekable();
    while chars
        .peek()
        .is_some_and(|(_, ch)| matches!(ch, '.' | '!' | '?' | ';' | ':' | ','))
    {
        chars.next();
    }
    while chars.peek().is_some_and(|(_, ch)| ch.is_whitespace()) {
        chars.next();
    }

    for (idx, ch) in chars {
        if matches!(ch, '.' | '!' | '?' | '\n') {
            return trimmed_end[..idx + ch.len_utf8()].trim_end().to_string();
        }
    }

    String::new()
}

impl Drop for UiApp {
    fn drop(&mut self) {
        self.request_stop();
        let _ = self.win32_tx.send(Win32Command::Shutdown);
    }
}
