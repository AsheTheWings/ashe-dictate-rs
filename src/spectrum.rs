//! Live voice spectrum for the dictate pill.
//!
//! Frequency analysis follows the established recipe used by tools like
//! lookas: Hann window, FFT via [`spectrum_analyzer`], log-spaced voice
//! bands, a time-domain noise gate, relative normalization, and asymmetric
//! attack/release smoothing. The pill renderer draws the resulting bands.

use spectrum_analyzer::scaling::divide_by_N_sqrt;
use spectrum_analyzer::windows::hann_window;
use spectrum_analyzer::{FrequencyLimit, samples_fft_to_spectrum};

/// FFT window in samples. At 48 kHz this is ~85 ms, about 12 Hz per bin,
/// fine enough to feed every log band with real data.
pub const FFT_SIZE: usize = 4096;
/// Frequency bars drawn by the pill overlay.
pub const BAND_COUNT: usize = 64;
/// Voice range feeding the bars.
pub const FMIN: f32 = 80.0;
/// Voice range feeding the bars.
pub const FMAX: f32 = 8000.0;
/// Time-domain peak below which a frame counts as silence.
const SILENCE_GATE: f32 = 0.015;
/// Per-tick decay of displayed energy at ~60 fps. Attacks are instant.
const RELEASE: f32 = 0.80;

pub struct SpectrumAnalyzer {
    sample_rate: u32,
    ring: Vec<f32>,
    displayed: Vec<f32>,
}

impl SpectrumAnalyzer {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            ring: Vec::new(),
            displayed: vec![0.0; BAND_COUNT],
        }
    }

    pub fn reset(&mut self) {
        self.ring.clear();
        self.displayed.fill(0.0);
    }

    /// Append mono samples in -1.0..=1.0. Only the latest window matters.
    pub fn push_samples(&mut self, samples: &[f32]) {
        self.ring.extend_from_slice(samples);
        if self.ring.len() > FFT_SIZE {
            self.ring.drain(..self.ring.len() - FFT_SIZE);
        }
    }

    /// Recompute the displayed bands from the latest window.
    pub fn update(&mut self) {
        let mut window = vec![0.0f32; FFT_SIZE];
        let len = self.ring.len().min(FFT_SIZE);
        window[FFT_SIZE - len..].copy_from_slice(&self.ring[self.ring.len() - len..]);
        let peak = window
            .iter()
            .fold(0.0f32, |max, sample| max.max(sample.abs()));
        let fmax = FMAX.min(self.sample_rate as f32 / 2.0 - 1.0);
        let target = if peak < SILENCE_GATE || fmax <= FMIN {
            vec![0.0; BAND_COUNT]
        } else {
            match samples_fft_to_spectrum(
                &hann_window(&window),
                self.sample_rate,
                FrequencyLimit::Range(FMIN, fmax),
                Some(&divide_by_N_sqrt),
            ) {
                Ok(spectrum) => {
                    let edges = log_band_edges(BAND_COUNT, FMIN, fmax);
                    let mut bands = map_bins_to_bands(
                        spectrum
                            .data()
                            .iter()
                            .map(|(freq, val)| (freq.val(), val.val())),
                        &edges,
                    );
                    normalize_relative(&mut bands);
                    bands
                }
                Err(_) => vec![0.0; BAND_COUNT],
            }
        };
        smooth_bands(&mut self.displayed, &target, RELEASE);
    }

    pub fn bars(&self) -> &[f32] {
        &self.displayed
    }
}

/// Log-spaced `(low, high)` band edges in Hertz, matching how pitch is
/// perceived instead of a linear bin grid.
pub fn log_band_edges(bands: usize, fmin: f32, fmax: f32) -> Vec<(f32, f32)> {
    if bands == 0 || fmax <= fmin {
        return Vec::new();
    }
    let low = fmin.max(1.0).log10();
    let high = fmax.log10();
    (0..bands)
        .map(|index| {
            let lo = 10.0f32.powf(low + (high - low) * index as f32 / bands as f32);
            let hi = 10.0f32.powf(low + (high - low) * (index + 1) as f32 / bands as f32);
            (lo, hi)
        })
        .collect()
}

/// Collapse FFT `(frequency, magnitude)` bins into per-band peak energy.
pub fn map_bins_to_bands(
    bins: impl IntoIterator<Item = (f32, f32)>,
    edges: &[(f32, f32)],
) -> Vec<f32> {
    let mut bands = vec![0.0f32; edges.len()];
    for (freq, mag) in bins {
        if mag <= 0.0 {
            continue;
        }
        if let Some(index) = edges.iter().position(|(lo, hi)| freq >= *lo && freq < *hi) {
            bands[index] = bands[index].max(mag);
        }
    }
    bands
}

/// Scale bands relative to the frame peak with a square-root lift so quiet
/// voice energy stays visible next to loud vowels. A gentle treble tilt
/// compensates the natural downward slope of voice spectra so consonants
/// on the right of the pill read next to bass-heavy vowels on the left.
const TREBLE_TILT: f32 = 0.5;

pub fn normalize_relative(bands: &mut [f32]) {
    let denom = bands.len().saturating_sub(1).max(1) as f32;
    for (index, value) in bands.iter_mut().enumerate() {
        *value *= 1.0 + TREBLE_TILT * index as f32 / denom;
    }
    let max = bands.iter().fold(0.0f32, |peak, value| peak.max(*value));
    if max <= 0.0 {
        bands.fill(0.0);
        return;
    }
    for value in bands.iter_mut() {
        *value = (*value / max).sqrt().clamp(0.0, 1.0);
    }
}

/// Instant attack, exponential release: transients land at full height while
/// decay trails off smoothly instead of flickering.
pub fn smooth_bands(displayed: &mut [f32], target: &[f32], release: f32) {
    for (shown, wanted) in displayed.iter_mut().zip(target.iter()) {
        *shown = wanted.max(*shown * release);
    }
}

/// Convert a little-endian mono 16-bit PCM chunk to -1.0..=1.0 samples,
/// ignoring a trailing odd byte.
pub fn pcm_chunk_to_mono(chunk: &[u8]) -> Vec<f32> {
    let (pairs, _) = chunk.as_chunks::<2>();
    pairs
        .iter()
        .map(|pair| i16::from_le_bytes(*pair) as f32 / 32768.0)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        log_band_edges, map_bins_to_bands, normalize_relative, pcm_chunk_to_mono, smooth_bands,
    };

    #[test]
    fn band_edges_span_the_voice_range_logarithmically() {
        let edges = log_band_edges(4, 100.0, 10_000.0);
        assert_eq!(edges.len(), 4);
        assert!((edges[0].0 - 100.0).abs() < 0.001);
        assert!((edges[3].1 - 10_000.0).abs() < 1.0);
        let widths: Vec<f32> = edges.iter().map(|(lo, hi)| hi - lo).collect();
        assert!(widths[0] < widths[1] && widths[1] < widths[2] && widths[2] < widths[3]);
        assert!(log_band_edges(0, 100.0, 10_000.0).is_empty());
        assert!(log_band_edges(4, 10_000.0, 100.0).is_empty());
    }

    #[test]
    fn bins_land_in_their_band_with_peak_energy() {
        let edges = vec![(100.0, 200.0), (200.0, 400.0)];
        let bands = map_bins_to_bands(
            [(150.0, 0.4), (160.0, 0.9), (300.0, 0.2), (50.0, 5.0)],
            &edges,
        );
        assert_eq!(bands, vec![0.9, 0.2]);
    }

    #[test]
    fn normalization_is_relative_with_a_lift_for_quiet_bands() {
        let mut bands = vec![0.25, 1.0, 0.0];
        normalize_relative(&mut bands);
        assert_eq!(bands[1], 1.0);
        assert_eq!(bands[2], 0.0);
        // Tilted [0.25, 1.25, 0.0] peaks at 1.25: sqrt(0.25 / 1.25).
        assert!((bands[0] - 0.4472).abs() < 0.0001);
        let mut silent = vec![0.0, 0.0];
        normalize_relative(&mut silent);
        assert_eq!(silent, vec![0.0, 0.0]);
    }

    #[test]
    fn treble_tilt_breaks_ties_toward_the_right() {
        let mut bands = vec![1.0, 1.0, 1.0];
        normalize_relative(&mut bands);
        assert_eq!(bands[2], 1.0);
        assert!(bands[0] < bands[1] && bands[1] < bands[2]);
    }

    #[test]
    fn smoothing_attacks_instantly_and_releases_gradually() {
        let mut shown = vec![0.0, 0.8];
        smooth_bands(&mut shown, &[0.6, 0.2], 0.5);
        assert_eq!(shown[0], 0.6);
        assert_eq!(shown[1], 0.4);
    }

    #[test]
    fn pcm_conversion_measures_silence_and_full_scale() {
        assert_eq!(pcm_chunk_to_mono(&[0, 0, 0, 0]), vec![0.0, 0.0]);
        assert!(pcm_chunk_to_mono(&[]).is_empty());
        let full_scale = pcm_chunk_to_mono(&[0xFF, 0x7F])[0];
        assert!(full_scale > 0.99 && full_scale <= 1.0);
        assert_eq!(pcm_chunk_to_mono(&[0x00, 0x80])[0], -1.0);
        assert_eq!(pcm_chunk_to_mono(&[0, 0, 0xFF]), vec![0.0]);
    }
}
