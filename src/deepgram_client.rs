use crate::config::AppConfig;
use crate::logger;
use anyhow::{Context, Result};
use deepgram::Deepgram;
use deepgram::common::options::{Encoding, Endpointing, Options};
use deepgram::common::stream_response::StreamResponse;
use std::any::Any;
use std::panic::{self, AssertUnwindSafe};
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

pub struct DeepgramSession {
    audio_tx: UnboundedSender<Vec<u8>>,
    stop_tx: UnboundedSender<()>,
    thread: Option<JoinHandle<()>>,
    stop_requested: bool,
}

impl DeepgramSession {
    pub fn start(
        config: AppConfig,
        sample_rate: u32,
        transcript_tx: crossbeam_channel::Sender<String>,
        status_tx: crossbeam_channel::Sender<String>,
    ) -> Self {
        let (audio_tx, audio_rx) = unbounded_channel();
        let (stop_tx, stop_rx) = unbounded_channel();
        let thread = std::thread::spawn(move || {
            let panic_status_tx = status_tx.clone();
            let result = panic::catch_unwind(AssertUnwindSafe(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(err) => {
                        logger::info(format!("Tokio runtime creation failed: {err:#}"));
                        let _ = status_tx.send(format!("Error: Tokio runtime failed: {err}"));
                        return;
                    }
                };

                runtime.block_on(async move {
                    if let Err(err) = run(
                        config,
                        sample_rate,
                        audio_rx,
                        stop_rx,
                        transcript_tx,
                        status_tx.clone(),
                    )
                    .await
                    {
                        logger::info(format!("Deepgram error: {err:#}"));
                        let _ = status_tx.send(format!("Error: Deepgram: {err}"));
                    }
                });
            }));
            if let Err(payload) = result {
                let message = panic_payload_message(payload.as_ref());
                logger::info(format!("Deepgram worker panic: {message}"));
                let _ = panic_status_tx.send(format!("Error: Deepgram worker panic: {message}"));
            }
        });

        Self {
            audio_tx,
            stop_tx,
            thread: Some(thread),
            stop_requested: false,
        }
    }

    pub fn audio_sender(&self) -> UnboundedSender<Vec<u8>> {
        self.audio_tx.clone()
    }

    pub fn request_stop(&mut self) {
        if !self.stop_requested {
            self.stop_requested = true;
            let _ = self.stop_tx.send(());
            logger::info("Deepgram stop signal sent");
        }
    }

    pub fn join_if_finished(&mut self) -> bool {
        let Some(thread) = self.thread.as_ref() else {
            return true;
        };
        if !thread.is_finished() {
            return false;
        }
        if let Some(thread) = self.thread.take() {
            match thread.join() {
                Ok(()) => logger::info("Deepgram worker joined"),
                Err(payload) => {
                    logger::info(format!(
                        "Deepgram worker panicked: {}",
                        panic_payload_message(payload.as_ref())
                    ));
                }
            }
        }
        true
    }
}

impl Drop for DeepgramSession {
    fn drop(&mut self) {
        self.request_stop();
        if self
            .thread
            .as_ref()
            .is_some_and(|thread| thread.is_finished())
        {
            let _ = self.join_if_finished();
        }
    }
}

async fn run(
    config: AppConfig,
    sample_rate: u32,
    mut audio_rx: UnboundedReceiver<Vec<u8>>,
    mut stop_rx: UnboundedReceiver<()>,
    transcript_tx: crossbeam_channel::Sender<String>,
    status_tx: crossbeam_channel::Sender<String>,
) -> Result<()> {
    logger::info(format!(
        "Deepgram connecting model={} language={} sample_rate={} keyterms={}",
        config.deepgram_model,
        config.deepgram_language,
        sample_rate,
        config.deepgram_keyterms.len()
    ));
    let _ = status_tx.send("Connecting...".to_string());

    let dg = Deepgram::new(config.deepgram_api_key.clone())
        .context("failed to create Deepgram client")?;
    let options = Options::builder()
        .query_params(config.deepgram_query_params())
        .build();
    let mut handle = dg
        .transcription()
        .stream_request_with_options(options)
        .encoding(Encoding::Linear16)
        .sample_rate(sample_rate)
        .channels(1)
        .endpointing(Endpointing::CustomDurationMs(300))
        .interim_results(true)
        .keep_alive()
        .handle()
        .await
        .context("failed to open Deepgram streaming handle")?;

    logger::info(format!(
        "Deepgram connected request_id={}",
        handle.request_id()
    ));
    let _ = status_tx.send("Listening...".to_string());
    let mut sent_chunks: usize = 0;
    let mut last_final = String::new();

    loop {
        if stop_rx.try_recv().is_ok() {
            logger::info("Deepgram stop requested");
            if let Err(err) = handle.finalize().await {
                logger::info(format!("Deepgram finalize failed: {err:#}"));
            }
            if let Err(err) = handle.close_stream().await {
                logger::info(format!("Deepgram close failed: {err:#}"));
            }
            break;
        }

        for _ in 0..32 {
            let Ok(chunk) = audio_rx.try_recv() else {
                break;
            };
            if chunk.is_empty() {
                continue;
            }
            handle
                .send_data(chunk)
                .await
                .context("failed to send audio chunk to Deepgram")?;
            sent_chunks += 1;
            if sent_chunks == 1 || sent_chunks % 100 == 0 {
                logger::info(format!("Deepgram chunks sent={sent_chunks}"));
            }
        }

        match tokio::time::timeout(Duration::from_millis(15), handle.receive()).await {
            Ok(Some(Ok(response))) => {
                handle_response(response, &transcript_tx, &status_tx, &mut last_final)
            }
            Ok(Some(Err(err))) => logger::info(format!("Deepgram receive error: {err:#}")),
            Ok(None) => {
                logger::info("Deepgram stream ended by remote");
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(5)).await,
        }
    }

    let _ = status_tx.send("Idle".to_string());
    logger::info(format!("Deepgram run loop ended chunks_sent={sent_chunks}"));
    Ok(())
}

fn handle_response(
    response: StreamResponse,
    transcript_tx: &crossbeam_channel::Sender<String>,
    status_tx: &crossbeam_channel::Sender<String>,
    last_final: &mut String,
) {
    match response {
        StreamResponse::TranscriptResponse {
            is_final,
            speech_final,
            channel,
            ..
        } => {
            if !(is_final || speech_final) {
                return;
            }
            let Some(alt) = channel.alternatives.first() else {
                return;
            };
            let text = alt.transcript.trim();
            if text.is_empty() || text == last_final {
                return;
            }
            *last_final = text.to_string();
            let text = format!("{text} ");
            logger::info(format!("Transcript: {text}"));
            let _ = transcript_tx.send(text);
        }
        StreamResponse::SpeechStartedResponse { .. } => {
            let _ = status_tx.send("Speech detected".to_string());
        }
        StreamResponse::UtteranceEndResponse { .. } => {
            let _ = status_tx.send("Listening...".to_string());
        }
        StreamResponse::TerminalResponse { .. } => {
            logger::info("Deepgram terminal response received");
        }
        _ => {}
    }
}
fn panic_payload_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "non-string panic payload".to_string()
}
