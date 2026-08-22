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

const FINALIZATION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptUpdate {
    Interim(String),
    Final(String),
}

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
        transcript_tx: crossbeam_channel::Sender<TranscriptUpdate>,
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
    transcript_tx: crossbeam_channel::Sender<TranscriptUpdate>,
    status_tx: crossbeam_channel::Sender<String>,
) -> Result<()> {
    logger::info(format!(
        "Deepgram connecting model={} language={} sample_rate={} keyterms={}",
        config.deepgram_model,
        config.deepgram_language,
        sample_rate,
        config.deepgram_keyterms.len()
    ));
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
    let mut finalization_deadline = None;

    loop {
        if finalization_deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
            logger::info("Deepgram finalization timed out");
            break;
        }
        let stop_requested = finalization_deadline.is_none() && stop_rx.try_recv().is_ok();
        if stop_requested {
            logger::info("Deepgram stop requested");
            while let Ok(chunk) = audio_rx.try_recv() {
                if chunk.is_empty() {
                    continue;
                }
                handle
                    .send_data(chunk)
                    .await
                    .context("failed to send final audio chunk to Deepgram")?;
                sent_chunks += 1;
            }
            if let Err(err) = handle.finalize().await {
                logger::info(format!("Deepgram finalize failed: {err:#}"));
            }
            if let Err(err) = handle.close_stream().await {
                logger::info(format!("Deepgram close failed: {err:#}"));
            }
            finalization_deadline = Some(tokio::time::Instant::now() + FINALIZATION_TIMEOUT);
        }

        if finalization_deadline.is_none() {
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
                if sent_chunks == 1 || sent_chunks.is_multiple_of(100) {
                    logger::info(format!("Deepgram chunks sent={sent_chunks}"));
                }
            }
        }

        let receive_timeout = if finalization_deadline.is_some() {
            Duration::from_millis(100)
        } else {
            Duration::from_millis(15)
        };
        match tokio::time::timeout(receive_timeout, handle.receive()).await {
            Ok(Some(Ok(response))) => {
                if handle_response(response, &transcript_tx, &status_tx) {
                    break;
                }
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
    transcript_tx: &crossbeam_channel::Sender<TranscriptUpdate>,
    status_tx: &crossbeam_channel::Sender<String>,
) -> bool {
    match response {
        StreamResponse::TranscriptResponse {
            is_final, channel, ..
        } => {
            let Some(alt) = channel.alternatives.first() else {
                return false;
            };
            let text = alt.transcript.trim().to_string();
            if is_final {
                logger::info(format!("Final transcript: {text}"));
                let _ = transcript_tx.send(TranscriptUpdate::Final(text));
            } else {
                let _ = transcript_tx.send(TranscriptUpdate::Interim(text));
            }
            false
        }
        StreamResponse::SpeechStartedResponse { .. } => {
            let _ = status_tx.send("Speech detected".to_string());
            false
        }
        StreamResponse::UtteranceEndResponse { .. } => {
            let _ = status_tx.send("Listening...".to_string());
            false
        }
        StreamResponse::TerminalResponse { .. } => {
            logger::info("Deepgram terminal response received");
            true
        }
        _ => false,
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

#[cfg(test)]
mod tests {
    use super::{TranscriptUpdate, handle_response};
    use deepgram::common::stream_response::{
        Alternatives, Channel, Metadata, ModelInfo, StreamResponse,
    };

    fn transcript_response(is_final: bool, speech_final: bool, text: &str) -> StreamResponse {
        StreamResponse::TranscriptResponse {
            type_field: "Results".to_string(),
            start: 0.0,
            duration: 1.0,
            is_final,
            speech_final,
            from_finalize: false,
            channel: Channel {
                alternatives: vec![Alternatives {
                    transcript: text.to_string(),
                    words: Vec::new(),
                    confidence: 0.9,
                    languages: Vec::new(),
                }],
            },
            metadata: Metadata {
                request_id: "request".to_string(),
                model_info: ModelInfo {
                    name: "nova-3".to_string(),
                    version: "test".to_string(),
                    arch: "test".to_string(),
                },
                model_uuid: "model".to_string(),
            },
            channel_index: vec![0, 1],
        }
    }

    #[test]
    fn provisional_nova_results_are_forwarded_as_replacements() {
        let (transcript_tx, transcript_rx) = crossbeam_channel::unbounded();
        let (status_tx, _status_rx) = crossbeam_channel::unbounded();

        assert!(!handle_response(
            transcript_response(false, true, "hello word"),
            &transcript_tx,
            &status_tx,
        ));
        assert_eq!(
            transcript_rx.try_recv().unwrap(),
            TranscriptUpdate::Interim("hello word".to_string())
        );
    }

    #[test]
    fn stable_nova_results_are_forwarded_as_final_segments() {
        let (transcript_tx, transcript_rx) = crossbeam_channel::unbounded();
        let (status_tx, _status_rx) = crossbeam_channel::unbounded();

        assert!(!handle_response(
            transcript_response(true, true, "Hello world."),
            &transcript_tx,
            &status_tx,
        ));
        assert_eq!(
            transcript_rx.try_recv().unwrap(),
            TranscriptUpdate::Final("Hello world.".to_string())
        );
    }
}
