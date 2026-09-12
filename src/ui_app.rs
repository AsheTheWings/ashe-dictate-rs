use crate::activity_pipeline::ActivityHandle;
use crate::audio::AudioCapture;
use crate::config::AppConfig;
use crate::deepgram_client::transcribe_pcm;
use crate::injector;
use crate::llm_client;
use crate::logger;
use crate::overlay_view;
use crate::paste_upload::PasteUploader;
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
const VISUALIZER_HISTORY_LEN: usize = 24;
type PolishResult = std::result::Result<String, String>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum DictationState {
    Idle,
    Starting,
    Listening,
    Transcribing,
    Polishing,
    Inserting,
    FixingGrammar,
    AnsweringQuestion,
}

#[derive(Clone, Copy)]
enum TextActionKind {
    FixGrammar,
    AnswerQuestion,
}

struct TextAction {
    kind: TextActionKind,
    target_hwnd: isize,
}

struct DictationSession {
    target_hwnd: isize,
    selected_context: Option<String>,
    raw_transcript: String,
}

impl DictationSession {
    fn displayed_transcript(&self) -> String {
        self.raw_transcript.clone()
    }
}

pub struct UiApp {
    config: AppConfig,
    state: DictationState,
    win32_tx: Sender<Win32Command>,
    win32_rx: Receiver<Win32Event>,
    _win32_thread: JoinHandle<()>,
    audio: Option<AudioCapture>,
    audio_rx: Option<tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>>,
    recorded_pcm: Vec<u8>,
    record_sample_rate: u32,
    audio_level: f32,
    level_history: Vec<f32>,
    anim_frame: u64,
    session: Option<DictationSession>,
    text_action: Option<TextAction>,
    window_id: Option<window::Id>,
    position: Option<Point>,
    target_position: Option<Point>,
    visible: bool,
    status: String,
    transcript: String,
    polished: Option<String>,
    error: Option<String>,
    activity: ActivityHandle,
    last_activity_status: String,
    paste_in_flight: bool,
    paste_uploader: PasteUploader,
}

#[derive(Debug, Clone)]
pub enum Message {
    Tick,
    WindowReady(Option<window::Id>),
    WindowCloseRequested(window::Id),
    TranscriptionCompleted(Result<String, String>),
    PolishCompleted(PolishResult),
    TextActionCompleted(PolishResult),
    PasteImageUploaded {
        target_hwnd: isize,
        result: PolishResult,
    },
}

impl UiApp {
    pub fn new() -> (Self, Task<Message>) {
        let config = AppConfig::load();
        logger::info(format!("Config loaded: {}", config.log_summary()));
        let (event_tx, win32_rx) = crossbeam_channel::unbounded();
        let (win32_tx, command_rx) = crossbeam_channel::unbounded();
        let win32_thread = win32_service::spawn(event_tx, command_rx);
        let activity = ActivityHandle::spawn(config.clone());
        let app = Self {
            config,
            state: DictationState::Idle,
            win32_tx,
            win32_rx,
            _win32_thread: win32_thread,
            audio: None,
            audio_rx: None,
            recorded_pcm: Vec::new(),
            record_sample_rate: 0,
            audio_level: 0.0,
            level_history: Vec::new(),
            anim_frame: 0,
            session: None,
            text_action: None,
            window_id: None,
            position: None,
            target_position: None,
            visible: false,
            status: "Ready".to_string(),
            transcript: String::new(),
            polished: None,
            error: None,
            activity,
            last_activity_status: String::new(),
            paste_in_flight: false,
            paste_uploader: PasteUploader::default(),
        };
        app.send_win32(Win32Command::SetTooltip(
            "Ashe Worker - Idle - Win+Shift+H".to_string(),
        ));
        (app, window::latest().map(Message::WindowReady))
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            iced::time::every(Duration::from_millis(OVERLAY_TICK_MS)).map(|_| Message::Tick),
            window::close_requests().map(Message::WindowCloseRequested),
        ])
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::WindowReady(id) => {
                logger::info(format!("Iced main WindowReady id={id:?}"));
                self.window_id = id;
                overlay_view::apply_native_styles();
                self.apply_window_state()
            }
            Message::WindowCloseRequested(id) => {
                logger::info(format!(
                    "Ignored overlay close request id={id:?}; use the tray Quit action to exit"
                ));
                Task::none()
            }
            Message::Tick => self.pump(),
            Message::TranscriptionCompleted(result) => self.finish_transcription(result),
            Message::PolishCompleted(result) => self.finish_polishing(result),
            Message::TextActionCompleted(result) => self.finish_text_action(result),
            Message::PasteImageUploaded {
                target_hwnd,
                result,
            } => self.finish_image_upload(target_hwnd, result),
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        // The pill is a text-free state lamp: lifecycle by color and motion,
        // status words live in the tray tooltip.
        let state = if self.error.is_some() {
            overlay_view::PillState::Error
        } else {
            match self.state {
                DictationState::Starting | DictationState::Listening => {
                    overlay_view::PillState::Listening
                }
                DictationState::Transcribing
                | DictationState::Polishing
                | DictationState::Inserting => overlay_view::PillState::Working,
                DictationState::Idle
                | DictationState::FixingGrammar
                | DictationState::AnsweringQuestion => overlay_view::PillState::Idle,
            }
        };
        overlay_view::view(overlay_view::PillContent {
            levels: &self.level_history,
            audio_level: self.audio_level,
            state,
            frame: self.anim_frame,
        })
    }

    fn pump(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        if self.window_id.is_none() {
            tasks.push(window::latest().map(Message::WindowReady));
        }
        while let Ok(event) = self.win32_rx.try_recv() {
            tasks.push(self.handle_win32_event(event));
        }
        self.pump_audio_capture();
        if self.visible {
            self.anim_frame = self.anim_frame.wrapping_add(1);
        }
        if self.visible && self.advance_overlay_position() {
            tasks.push(self.apply_window_state());
        }
        let activity = self.activity.status();
        let activity_key = format!(
            "{}:{}:{}",
            activity.running, activity.current_frames, activity.summary
        );
        if activity_key != self.last_activity_status {
            self.last_activity_status = activity_key;
            self.send_win32(Win32Command::SetActivityStatus {
                running: activity.running,
                status: activity.summary,
            });
        }
        Task::batch(tasks)
    }

    /// Drain captured PCM into the recording buffer while updating the voice
    /// visualizer level. Transcription happens once on stop, not streaming.
    fn pump_audio_capture(&mut self) {
        if !matches!(
            self.state,
            DictationState::Starting | DictationState::Listening
        ) {
            return;
        }
        let mut peak: f32 = 0.0;
        let mut drained = false;
        if let Some(rx) = self.audio_rx.as_mut() {
            while let Ok(chunk) = rx.try_recv() {
                if chunk.is_empty() {
                    continue;
                }
                drained = true;
                peak = peak.max(chunk_peak(&chunk));
                self.recorded_pcm.extend_from_slice(&chunk);
            }
        }
        if drained {
            self.audio_level = peak.max(self.audio_level * 0.55);
        } else {
            self.audio_level *= 0.90;
            if self.audio_level < 0.001 {
                self.audio_level = 0.0;
            }
        }
        self.level_history.push(self.audio_level);
        while self.level_history.len() > VISUALIZER_HISTORY_LEN {
            self.level_history.remove(0);
        }
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
                    return self.request_stop();
                }
                Task::none()
            }
            Win32Event::FixGrammarRequested { target_hwnd, x, y } => self.begin_text_action(
                TextActionKind::FixGrammar,
                target_hwnd,
                Point::new(x as f32, y as f32),
            ),
            Win32Event::AnswerQuestionRequested { target_hwnd, x, y } => self.begin_text_action(
                TextActionKind::AnswerQuestion,
                target_hwnd,
                Point::new(x as f32, y as f32),
            ),
            Win32Event::PasteImageRequested { target_hwnd } => self.begin_image_paste(target_hwnd),
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
                    "Ashe Worker - Log path copied - Win+Shift+H".to_string(),
                ));
                Task::none()
            }
            Win32Event::ToggleActivityRequested => {
                self.activity.toggle();
                Task::none()
            }
            Win32Event::OpenArtifactsRequested => {
                self.send_win32(Win32Command::OpenPath(
                    self.activity.artifacts_dir().display().to_string(),
                ));
                Task::none()
            }
            Win32Event::OpenJournalRequested => {
                self.send_win32(Win32Command::OpenPath(
                    self.activity.today_journal().display().to_string(),
                ));
                Task::none()
            }
            Win32Event::AboutRequested => {
                self.show_about();
                Task::none()
            }
            Win32Event::QuitRequested => self.quit(),
            Win32Event::PasteCompleted(result) => self.finish_insert(result),
            Win32Event::PathPasteCompleted(result) => {
                self.paste_in_flight = false;
                match result {
                    Ok(()) => logger::info("Remote clipboard image path pasted"),
                    Err(error) => logger::info(format!("Remote image path paste failed: {error}")),
                }
                Task::none()
            }
            Win32Event::ServiceStopped => {
                logger::info("Win32 service stopped event received");
                Task::none()
            }
        }
    }

    fn toggle(&mut self, target_hwnd: isize, position: Point) -> Task<Message> {
        match self.state {
            DictationState::Idle => self.start(target_hwnd, position),
            DictationState::Starting | DictationState::Listening => self.request_stop(),
            DictationState::Transcribing => {
                logger::info("Transcription already in progress");
                Task::none()
            }
            DictationState::Polishing | DictationState::Inserting => {
                logger::info("Polishing/inserting already in progress");
                Task::none()
            }
            DictationState::FixingGrammar | DictationState::AnsweringQuestion => {
                logger::info("Text action already in progress");
                Task::none()
            }
        }
    }

    fn cancel_operation(&mut self) -> Task<Message> {
        if self.state == DictationState::Idle {
            return Task::none();
        }
        logger::info("Cancel operation requested");
        if let Some(mut audio) = self.audio.take() {
            audio.stop();
        }
        self.audio_rx.take();
        self.recorded_pcm.clear();
        self.audio_level = 0.0;
        self.level_history.clear();
        self.session = None;
        self.state = DictationState::Idle;
        self.visible = false;
        self.status = "Cancelled".to_string();
        self.polished = None;
        self.error = None;
        self.send_win32(Win32Command::SetActive(false));
        self.send_win32(Win32Command::SetTooltip(
            "Ashe Worker - Cancelled - Win+Shift+H".to_string(),
        ));
        self.apply_window_state()
    }

    fn revert_last_sentence(&mut self) -> Task<Message> {
        if !matches!(
            self.state,
            DictationState::Starting | DictationState::Listening
        ) {
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
        self.transcript = session.displayed_transcript();
        self.polished = None;
        self.error = None;
        self.send_win32(Win32Command::SetTooltip(
            "Ashe Worker - Last sentence reverted - Win+Shift+H".to_string(),
        ));
        Task::none()
    }

    fn clear_transcript(&mut self) -> Task<Message> {
        if !matches!(
            self.state,
            DictationState::Starting | DictationState::Listening
        ) {
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
            "Ashe Worker - Transcript cleared - Win+Shift+H".to_string(),
        ));
        Task::none()
    }

    fn start(&mut self, target_hwnd: isize, position: Point) -> Task<Message> {
        if let Err(err) = self.config.validate_for_dictation() {
            logger::info(format!("Config validation failed: {err:#}"));
            self.send_win32(Win32Command::ShowMessageBox {
                title: "Ashe Worker".to_string(),
                text: format!("Cannot start dictation: {err}"),
            });
            return Task::none();
        }
        logger::info("Start dictation requested");
        self.state = DictationState::Starting;
        self.visible = true;
        self.position = Some(position);
        self.target_position = Some(position);
        self.status = "Starting...".to_string();
        self.transcript.clear();
        self.polished = None;
        self.error = None;
        let selected_context = self.capture_selected_context(target_hwnd);
        self.session = Some(DictationSession {
            target_hwnd,
            selected_context,
            raw_transcript: String::new(),
        });
        self.send_win32(Win32Command::SetActive(true));
        self.send_win32(Win32Command::SetTooltip(
            "Ashe Worker - Starting... - Win+Shift+H".to_string(),
        ));
        // Audio is buffered locally while recording. One pre-recorded
        // transcription request runs on stop for a better result than
        // committing streaming partials.
        let (audio_tx, audio_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        let audio = match AudioCapture::start(audio_tx, self.config.output_sample_rate) {
            Ok(audio) => audio,
            Err(err) => {
                logger::info(format!("Audio start failed: {err:#}"));
                let hide_task = self.finish_without_transcript();
                self.send_win32(Win32Command::ShowMessageBox {
                    title: "Ashe Worker".to_string(),
                    text: format!("Audio capture failed: {err}"),
                });
                return hide_task;
            }
        };
        self.record_sample_rate = audio.sample_rate();
        self.recorded_pcm.clear();
        self.audio_rx = Some(audio_rx);
        self.audio_level = 0.0;
        self.level_history.clear();
        self.audio = Some(audio);
        self.state = DictationState::Listening;
        self.status = "Listening...".to_string();
        self.send_win32(Win32Command::SetTooltip(
            "Ashe Worker - Listening... - Win+Shift+H".to_string(),
        ));
        self.apply_window_state()
    }

    fn request_stop(&mut self) -> Task<Message> {
        if matches!(
            self.state,
            DictationState::Idle
                | DictationState::Transcribing
                | DictationState::Polishing
                | DictationState::Inserting
                | DictationState::FixingGrammar
                | DictationState::AnsweringQuestion
        ) {
            return Task::none();
        }
        logger::info(format!(
            "Stop dictation requested bytes={}",
            self.recorded_pcm.len()
        ));
        // Drain anything captured between the last tick and the mic stop so
        // the tail of the utterance is not lost.
        if let Some(rx) = self.audio_rx.as_mut() {
            while let Ok(chunk) = rx.try_recv() {
                if !chunk.is_empty() {
                    self.recorded_pcm.extend_from_slice(&chunk);
                }
            }
        }
        self.audio_rx.take();
        if let Some(mut audio) = self.audio.take() {
            audio.stop();
        }
        self.state = DictationState::Transcribing;
        self.status = "Transcribing...".to_string();
        self.send_win32(Win32Command::SetTooltip(
            "Ashe Worker - Transcribing... - Win+Shift+H".to_string(),
        ));
        self.audio_level = 0.0;
        let pcm = std::mem::take(&mut self.recorded_pcm);
        let sample_rate = self.record_sample_rate;
        let config = self.config.clone();
        Task::perform(
            async move {
                transcribe_pcm(config, sample_rate, pcm)
                    .await
                    .map_err(|err| format!("{err:#}"))
            },
            Message::TranscriptionCompleted,
        )
    }

    fn finish_transcription(&mut self, result: Result<String, String>) -> Task<Message> {
        if self.state != DictationState::Transcribing {
            return Task::none();
        }
        match result {
            Ok(text) => {
                let transcript = text.trim().to_string();
                if transcript.is_empty() {
                    logger::info("Transcription returned no speech");
                    return self.finish_without_transcript();
                }
                logger::info(format!(
                    "Transcription completed chars={}",
                    transcript.len()
                ));
                if let Some(session) = self.session.as_mut() {
                    session.raw_transcript = transcript.clone();
                }
                self.transcript = transcript;
                self.polished = None;
                self.error = None;
                self.begin_polishing()
            }
            Err(err) => {
                logger::info(format!("Transcription failed: {err}"));
                self.error = Some("Transcription failed".to_string());
                self.status = "Transcription failed".to_string();
                self.send_win32(Win32Command::SetTooltip(
                    "Ashe Worker - Transcription error - Win+Shift+H".to_string(),
                ));
                self.hide_overlay_after_session()
            }
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
        self.status = "Polishing with Gemini...".to_string();
        self.send_win32(Win32Command::SetTooltip(
            "Ashe Worker - Polishing... - Win+Shift+H".to_string(),
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
                "Ashe Worker - Paste error - Win+Shift+H".to_string(),
            ));
        } else {
            self.send_win32(Win32Command::SetTooltip(
                "Ashe Worker - Inserted - Win+Shift+H".to_string(),
            ));
        }
        self.hide_overlay_after_session()
    }

    fn finish_without_transcript(&mut self) -> Task<Message> {
        self.send_win32(Win32Command::SetTooltip(
            "Ashe Worker - Idle - Win+Shift+H".to_string(),
        ));
        self.hide_overlay_after_session()
    }

    fn reload_config(&mut self) {
        if self.state != DictationState::Idle {
            self.send_win32(Win32Command::ShowMessageBox {
                title: "Ashe Worker".to_string(),
                text: "Stop dictation before reloading config.".to_string(),
            });
            return;
        }
        self.config = AppConfig::load();
        self.activity.shutdown();
        self.activity = ActivityHandle::spawn(self.config.clone());
        logger::info(format!("Config reloaded: {}", self.config.log_summary()));
        self.send_win32(Win32Command::SetTooltip(
            "Ashe Worker - Config reloaded - Win+Shift+H".to_string(),
        ));
    }

    fn show_about(&self) {
        self.send_win32(Win32Command::ShowMessageBox {
            title: "About Ashe Worker".to_string(),
            text: format!(
                "Ashe Worker\r\nVersion: {}\r\nBuild: {}\r\n\r\nDictate: Win+Shift+H\r\nGrammar: Win+Shift+G\r\nQuestion: Win+Shift+Q\r\nImage path: Ctrl+Alt+V\r\nActivity tracking: {}\r\nArtifacts: {}\r\nConfig: {}\r\nLog: {}",
                APP_VERSION,
                BUILD_ID,
                self.activity.status().summary,
                self.activity.artifacts_dir().display(),
                self.config.log_summary(),
                logger::log_path().display()
            ),
        });
    }

    fn begin_image_paste(&mut self, target_hwnd: isize) -> Task<Message> {
        if self.state != DictationState::Idle || self.paste_in_flight {
            logger::info("Clipboard image paste ignored while another action is active");
            return Task::none();
        }
        if target_hwnd == 0 {
            logger::info("Clipboard image paste ignored without a foreground target");
            return Task::none();
        }
        if let Err(error) = self.config.validate_for_paste() {
            logger::info(format!(
                "Clipboard image paste configuration invalid: {error:#}"
            ));
            return Task::none();
        }
        let png = match injector::capture_clipboard_png() {
            Ok(png) => png,
            Err(error) => {
                logger::info(format!("Clipboard image capture failed: {error:#}"));
                return Task::none();
            }
        };
        logger::info(format!("Clipboard image captured bytes={}", png.len()));
        self.paste_in_flight = true;
        let uploader = self.paste_uploader.clone();
        let config = self.config.clone();
        Task::perform(
            async move {
                uploader
                    .upload_png(config, png, chrono::Utc::now())
                    .await
                    .map_err(|error| format!("{error:#}"))
            },
            move |result| Message::PasteImageUploaded {
                target_hwnd,
                result,
            },
        )
    }

    fn finish_image_upload(&mut self, target_hwnd: isize, result: PolishResult) -> Task<Message> {
        match result {
            Ok(path) => {
                logger::info(format!("Clipboard image uploaded path={path}"));
                self.send_win32(Win32Command::PastePath {
                    target_hwnd,
                    text: path,
                });
            }
            Err(error) => {
                self.paste_in_flight = false;
                logger::info(format!("Clipboard image upload failed: {error}"));
            }
        }
        Task::none()
    }

    fn begin_text_action(
        &mut self,
        kind: TextActionKind,
        target_hwnd: isize,
        position: Point,
    ) -> Task<Message> {
        if self.state != DictationState::Idle {
            logger::info("Text action ignored; not idle");
            return Task::none();
        }
        let validation = match kind {
            TextActionKind::FixGrammar => self.config.validate_for_grammar(),
            TextActionKind::AnswerQuestion => self.config.validate_for_question(),
        };
        if let Err(err) = validation {
            logger::info(format!("LLM config validation failed: {err:#}"));
            self.send_win32(Win32Command::ShowMessageBox {
                title: "Ashe Worker".to_string(),
                text: format!("Cannot run text action: {err}"),
            });
            return Task::none();
        }
        let selected = match injector::capture_selected_text() {
            Ok(Some(text)) => text,
            Ok(None) => {
                logger::info("Text action: no text selected");
                self.send_win32(Win32Command::SetTooltip(
                    "Ashe Worker - No text selected - Win+Shift+H".to_string(),
                ));
                return Task::none();
            }
            Err(err) => {
                logger::info(format!("Text action selection capture failed: {err:#}"));
                self.send_win32(Win32Command::ShowMessageBox {
                    title: "Ashe Worker".to_string(),
                    text: format!("Could not capture selected text: {err}"),
                });
                return Task::none();
            }
        };

        let (state, status, tooltip) = match kind {
            TextActionKind::FixGrammar => (
                DictationState::FixingGrammar,
                "Fixing grammar...",
                "Ashe Worker - Fixing grammar... - Win+Shift+H",
            ),
            TextActionKind::AnswerQuestion => (
                DictationState::AnsweringQuestion,
                "Answering...",
                "Ashe Worker - Answering... - Win+Shift+H",
            ),
        };
        logger::info(format!("Text action started chars={}", selected.len()));
        self.state = state;
        self.text_action = Some(TextAction { kind, target_hwnd });
        self.visible = true;
        self.position = Some(position);
        self.target_position = Some(position);
        self.status = status.to_string();
        self.transcript = selected.clone();
        self.polished = None;
        self.error = None;
        self.send_win32(Win32Command::SetTooltip(tooltip.to_string()));
        self.send_win32(Win32Command::SetFollowCursor(true));

        let config = self.config.clone();
        let llm_task = Task::perform(
            async move {
                match kind {
                    TextActionKind::FixGrammar => llm_client::fix_grammar(config, selected).await,
                    TextActionKind::AnswerQuestion => {
                        llm_client::answer_question(config, selected).await
                    }
                }
                .map_err(|err| format!("{err:#}"))
            },
            Message::TextActionCompleted,
        );
        Task::batch([self.apply_window_state(), llm_task])
    }

    fn finish_text_action(&mut self, result: PolishResult) -> Task<Message> {
        if !matches!(
            self.state,
            DictationState::FixingGrammar | DictationState::AnsweringQuestion
        ) {
            return Task::none();
        }
        let Some(action) = self.text_action.as_ref() else {
            return self.finish_without_transcript();
        };
        let append = matches!(action.kind, TextActionKind::AnswerQuestion);
        let target_hwnd = action.target_hwnd;
        let text = match result {
            Ok(text) => text,
            Err(err) => {
                logger::info(format!("Text action failed: {err}"));
                self.error = Some("LLM request failed".to_string());
                self.send_win32(Win32Command::SetTooltip(
                    "Ashe Worker - LLM error - Win+Shift+H".to_string(),
                ));
                return self.hide_overlay_after_session();
            }
        };
        if text.trim().is_empty() {
            logger::info("Text action returned empty result");
            self.send_win32(Win32Command::SetTooltip(
                "Ashe Worker - Empty result - Win+Shift+H".to_string(),
            ));
            return self.hide_overlay_after_session();
        }
        logger::info("Text action completed");
        let inject_text = if append {
            format!("\n\n{text}")
        } else {
            text.clone()
        };
        self.polished = Some(text);
        self.state = DictationState::Inserting;
        self.status = "Inserting...".to_string();
        self.send_win32(Win32Command::InjectText {
            target_hwnd,
            text: inject_text,
            append_after_selection: append,
        });
        Task::none()
    }

    fn capture_selected_context(&self, target_hwnd: isize) -> Option<String> {
        match injector::capture_optional_selected_text(target_hwnd) {
            Ok(context) => context,
            Err(err) => {
                logger::info(format!("Selection context capture failed: {err:#}"));
                None
            }
        }
    }

    fn quit(&mut self) -> Task<Message> {
        logger::info("Quit requested");
        self.activity.shutdown();
        if let Some(mut audio) = self.audio.take() {
            audio.stop();
        }
        self.audio_rx.take();
        self.send_win32(Win32Command::Shutdown);
        if let Some(id) = self.window_id {
            window::close(id)
        } else {
            Task::none()
        }
    }

    fn hide_overlay_after_session(&mut self) -> Task<Message> {
        self.session = None;
        self.text_action = None;
        self.state = DictationState::Idle;
        self.visible = false;
        self.target_position = self.position;
        self.send_win32(Win32Command::SetActive(false));
        self.send_win32(Win32Command::SetFollowCursor(false));
        self.apply_window_state()
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

/// Peak normalized amplitude (0.0..=1.0) of a little-endian mono 16-bit PCM
/// chunk. Feeds the overlay voice visualizer while recording.
fn chunk_peak(chunk: &[u8]) -> f32 {
    let (pairs, _) = chunk.as_chunks::<2>();
    let mut peak: f32 = 0.0;
    for pair in pairs {
        let value = i16::from_le_bytes(*pair) as f32 / 32768.0;
        peak = peak.max(value.abs());
    }
    peak.clamp(0.0, 1.0)
}

impl Drop for UiApp {
    fn drop(&mut self) {
        if let Some(mut audio) = self.audio.take() {
            audio.stop();
        }
        let _ = self.win32_tx.send(Win32Command::Shutdown);
    }
}

#[cfg(test)]
mod tests {
    use super::{DictationSession, chunk_peak};

    fn session() -> DictationSession {
        DictationSession {
            target_hwnd: 0,
            selected_context: None,
            raw_transcript: String::new(),
        }
    }

    #[test]
    fn completed_transcript_is_shown_verbatim() {
        let mut session = session();
        session.raw_transcript = "Hello world.".to_string();
        assert_eq!(session.displayed_transcript(), "Hello world.");
    }

    #[test]
    fn chunk_peak_measures_silence_and_full_scale() {
        assert_eq!(chunk_peak(&[0, 0, 0, 0]), 0.0);
        assert_eq!(chunk_peak(&[]), 0.0);
        let full_scale = chunk_peak(&[0xFF, 0x7F]);
        assert!(full_scale > 0.99 && full_scale <= 1.0);
        assert_eq!(chunk_peak(&[0x00, 0x80]), 1.0);
    }

    #[test]
    fn chunk_peak_ignores_a_trailing_odd_byte() {
        assert_eq!(chunk_peak(&[0xFF]), 0.0);
        assert_eq!(chunk_peak(&[0, 0, 0xFF]), 0.0);
    }
}
