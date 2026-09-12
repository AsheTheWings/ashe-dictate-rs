use crate::config::AppConfig;
use crate::logger;
use anyhow::{Context, Result, anyhow};
use deepgram::Deepgram;
use deepgram::common::audio_source::AudioSource;
use deepgram::common::batch_response::Response;
use deepgram::common::options::Options;

/// Transcribe a complete recording with one pre-recorded request.
///
/// Dictation buffers microphone audio locally while recording and calls this
/// once on stop. A single full-utterance request produces a more accurate
/// result than committing streaming partials as they arrive.
pub async fn transcribe_pcm(
    config: AppConfig,
    sample_rate: u32,
    pcm: Vec<u8>,
) -> Result<String> {
    if pcm.is_empty() {
        return Err(anyhow!("no audio captured"));
    }
    if !pcm.len().is_multiple_of(2) {
        return Err(anyhow!("captured PCM has an odd byte count"));
    }
    if sample_rate == 0 {
        return Err(anyhow!("capture sample rate must be greater than zero"));
    }
    let seconds = pcm.len() as f64 / f64::from(sample_rate) / 2.0;
    logger::info(format!(
        "Deepgram pre-recorded request model={} language={} sample_rate={} bytes={} seconds={:.1} keyterms={}",
        config.deepgram_model,
        config.deepgram_language,
        sample_rate,
        pcm.len(),
        seconds,
        config.deepgram_keyterms.len()
    ));
    let dg =
        Deepgram::new(config.deepgram_api_key.clone()).context("failed to create Deepgram client")?;
    let options: Options = Options::builder()
        .query_params(config.deepgram_query_params())
        .punctuate(true)
        .build();
    let wav = encode_wav_mono16(&pcm, sample_rate);
    let source = AudioSource::from_buffer_with_mime_type(wav, "audio/wav");
    let response = dg
        .transcription()
        .prerecorded(source, &options)
        .await
        .context("Deepgram pre-recorded transcription failed")?;
    let transcript = extract_transcript(&response);
    logger::info(format!(
        "Deepgram pre-recorded transcript chars={}",
        transcript.len()
    ));
    Ok(transcript)
}

/// Wrap little-endian mono 16-bit PCM in a 44-byte WAV header so the
/// pre-recorded endpoint decodes the buffered capture without extra
/// encoding parameters.
pub fn encode_wav_mono16(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
    let data_len = pcm.len() as u32;
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36u32.wrapping_add(data_len)).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate.wrapping_mul(2)).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(pcm);
    wav
}

/// Read the top transcript alternative from a pre-recorded response.
pub fn extract_transcript(response: &Response) -> String {
    response
        .results
        .channels
        .first()
        .and_then(|channel| channel.alternatives.first())
        .map(|alternative| alternative.transcript.trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{encode_wav_mono16, extract_transcript};
    use deepgram::common::batch_response::Response;

    #[test]
    fn wav_header_describes_mono16_capture() {
        let pcm = vec![0x01, 0x02, 0x03, 0x04];
        let wav = encode_wav_mono16(&pcm, 48_000);
        assert_eq!(wav.len(), 48);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(u16::from_le_bytes([wav[20], wav[21]]), 1);
        assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 1);
        assert_eq!(u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]), 48_000);
        assert_eq!(u16::from_le_bytes([wav[34], wav[35]]), 16);
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(
            u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]),
            4
        );
        assert_eq!(&wav[44..], &pcm[..]);
    }

    #[test]
    fn empty_pcm_still_produces_a_valid_header() {
        let wav = encode_wav_mono16(&[], 16_000);
        assert_eq!(wav.len(), 44);
        assert_eq!(
            u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]),
            0
        );
    }

    #[test]
    fn transcript_extraction_trims_the_top_alternative() {
        let response: Response = serde_json::from_value(serde_json::json!({
            "metadata": {
                "request_id": "00000000-0000-0000-0000-000000000000",
                "transaction_key": "test",
                "sha256": "test",
                "created": "2026-09-12T00:00:00.000Z",
                "duration": 1.0,
                "channels": 1
            },
            "results": {
                "channels": [{
                    "search": null,
                    "detected_language": null,
                    "alternatives": [{
                        "transcript": "  Hello world.  ",
                        "confidence": 0.9,
                        "words": []
                    }]
                }]
            }
        }))
        .unwrap();
        assert_eq!(extract_transcript(&response), "Hello world.");
    }

    #[test]
    fn transcript_extraction_defaults_to_empty_without_alternatives() {
        for channels in [
            serde_json::json!([]),
            serde_json::json!([{
                "search": null,
                "detected_language": null,
                "alternatives": []
            }]),
        ] {
            let response: Response = serde_json::from_value(serde_json::json!({
                "metadata": {
                    "request_id": "00000000-0000-0000-0000-000000000000",
                    "transaction_key": "test",
                    "sha256": "test",
                    "created": "2026-09-12T00:00:00.000Z",
                    "duration": 0.0,
                    "channels": 0
                },
                "results": { "channels": channels }
            }))
            .unwrap();
            assert!(extract_transcript(&response).is_empty());
        }
    }
}
