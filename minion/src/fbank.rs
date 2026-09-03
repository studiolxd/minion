//! Mel filterbank features, matching Kaldi's `fbank`.
//!
//! The speaker model was trained on features produced by Kaldi, so this has
//! to reproduce them closely: the same pre-emphasis, the same window shape,
//! the same mel spacing. A frontend that is merely reasonable produces
//! embeddings that look plausible and compare badly, which is a hard fault
//! to see. The defaults here are Kaldi's, and the parameters come from the
//! model's own config: 80 bins, 25 ms windows, 10 ms apart.

use std::f32::consts::PI;
use std::sync::Arc;

use rustfft::{num_complex::Complex, Fft, FftPlanner};

const SAMPLE_RATE: f32 = 16_000.0;
const FRAME_LENGTH_MS: usize = 25;
const FRAME_SHIFT_MS: usize = 10;
const NUM_MEL_BINS: usize = 80;
const PREEMPHASIS: f32 = 0.97;
const LOW_FREQ: f32 = 20.0;
const EPSILON: f32 = 1.1920929e-7; // Kaldi's floor before the logarithm

pub const FRAME_SIZE: usize = (SAMPLE_RATE as usize * FRAME_LENGTH_MS) / 1000; // 400
pub const FRAME_SHIFT: usize = (SAMPLE_RATE as usize * FRAME_SHIFT_MS) / 1000; // 160
pub const NUM_BINS: usize = NUM_MEL_BINS;

fn mel_from_hz(hz: f32) -> f32 {
    1127.0 * (1.0 + hz / 700.0).ln()
}

fn hz_from_mel(mel: f32) -> f32 {
    700.0 * ((mel / 1127.0).exp() - 1.0)
}

/// Precomputed windowing and mel weights.
///
/// Built once and reused: the filterbank does not change between calls, and
/// rebuilding it per utterance would dominate the cost.
pub struct FilterBank {
    fft: Arc<dyn Fft<f32>>,
    fft_size: usize,
    window: Vec<f32>,
    /// For each mel bin: where it starts, and its weights from there.
    filters: Vec<(usize, Vec<f32>)>,
}

impl FilterBank {
    pub fn new() -> Self {
        let fft_size = FRAME_SIZE.next_power_of_two(); // 512
        let fft = FftPlanner::new().plan_fft_forward(fft_size);

        // Povey window: Hann raised to 0.85, which is what Kaldi uses by
        // default and what the model was trained with.
        let window = (0..FRAME_SIZE)
            .map(|i| {
                let hann = 0.5 - 0.5 * (2.0 * PI * i as f32 / (FRAME_SIZE - 1) as f32).cos();
                hann.powf(0.85)
            })
            .collect();

        let num_bins = fft_size / 2 + 1;
        let nyquist = SAMPLE_RATE / 2.0;
        let mel_low = mel_from_hz(LOW_FREQ);
        let mel_high = mel_from_hz(nyquist);
        let mel_step = (mel_high - mel_low) / (NUM_MEL_BINS + 1) as f32;
        let hz_per_bin = SAMPLE_RATE / fft_size as f32;

        let mut filters = Vec::with_capacity(NUM_MEL_BINS);
        for bin in 0..NUM_MEL_BINS {
            // Triangular filter over three mel-spaced points. The triangle
            // is straight in the mel domain, not in Hz: Kaldi interpolates
            // between the mel values, and since the mapping is logarithmic
            // the two differ by a few per cent across every bin — a small,
            // systematic mismatch with the frontend the model was trained
            // on, which is exactly the kind that hides.
            let left_mel = mel_low + bin as f32 * mel_step;
            let centre_mel = left_mel + mel_step;
            let right_mel = left_mel + 2.0 * mel_step;
            let left = hz_from_mel(left_mel);
            let right = hz_from_mel(right_mel);

            let mut start = None;
            let mut weights = Vec::new();
            for k in 0..num_bins {
                let hz = k as f32 * hz_per_bin;
                let weight = if hz > left && hz < right {
                    let mel = mel_from_hz(hz);
                    if mel <= centre_mel {
                        (mel - left_mel) / mel_step
                    } else {
                        (right_mel - mel) / mel_step
                    }
                } else {
                    0.0
                };
                if weight > 0.0 {
                    if start.is_none() {
                        start = Some(k);
                    }
                    weights.push(weight);
                } else if start.is_some() && !weights.is_empty() {
                    break;
                }
            }
            filters.push((start.unwrap_or(0), weights));
        }

        Self { fft, fft_size, window, filters }
    }

    /// Turns samples into log mel energies, one row per frame.
    ///
    /// Returns a flat buffer of `frames × 80`, which is the layout the
    /// speaker model expects.
    pub fn compute(&self, samples: &[f32]) -> (Vec<f32>, usize) {
        if samples.len() < FRAME_SIZE {
            return (Vec::new(), 0);
        }
        // Kaldi's snip_edges: only whole frames, no padding at the end.
        let frames = 1 + (samples.len() - FRAME_SIZE) / FRAME_SHIFT;
        let mut out = Vec::with_capacity(frames * NUM_MEL_BINS);
        let mut buffer = vec![Complex { re: 0.0, im: 0.0 }; self.fft_size];

        for frame in 0..frames {
            let start = frame * FRAME_SHIFT;
            let slice = &samples[start..start + FRAME_SIZE];

            // Remove the DC offset, then pre-emphasise, then window —
            // Kaldi's order, and it matters.
            let mean: f32 = slice.iter().sum::<f32>() / FRAME_SIZE as f32;
            let mut windowed = Vec::with_capacity(FRAME_SIZE);
            let mut previous = slice[0] - mean;
            for (i, sample) in slice.iter().enumerate() {
                let centred = sample - mean;
                let emphasised = centred - PREEMPHASIS * previous;
                previous = centred;
                windowed.push(emphasised * self.window[i]);
            }

            buffer.iter_mut().for_each(|c| *c = Complex { re: 0.0, im: 0.0 });
            for (i, value) in windowed.iter().enumerate() {
                buffer[i] = Complex { re: *value, im: 0.0 };
            }
            self.fft.process(&mut buffer);

            let power: Vec<f32> = buffer[..self.fft_size / 2 + 1]
                .iter()
                .map(|c| c.re * c.re + c.im * c.im)
                .collect();

            for (start_bin, weights) in &self.filters {
                let energy: f32 = weights
                    .iter()
                    .enumerate()
                    .map(|(i, w)| w * power.get(start_bin + i).copied().unwrap_or(0.0))
                    .sum();
                out.push(energy.max(EPSILON).ln());
            }
        }

        (out, frames)
    }
}

/// Subtracts the mean of each coefficient across time.
///
/// Cepstral mean normalisation, as wespeaker applies before the model. It
/// is what makes the embedding depend on the voice rather than on the
/// microphone and the room.
pub fn normalise_mean(feats: &mut [f32], frames: usize) {
    if frames == 0 {
        return;
    }
    for bin in 0..NUM_MEL_BINS {
        let mean: f32 = (0..frames).map(|f| feats[f * NUM_MEL_BINS + bin]).sum::<f32>()
            / frames as f32;
        for frame in 0..frames {
            feats[frame * NUM_MEL_BINS + bin] -= mean;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_advance_by_ten_milliseconds() {
        let bank = FilterBank::new();
        // One second of audio: 400-sample frames every 160 samples.
        let (feats, frames) = bank.compute(&vec![0.1; 16_000]);
        assert_eq!(frames, 1 + (16_000 - 400) / 160);
        assert_eq!(feats.len(), frames * NUM_BINS);
    }

    #[test]
    fn audio_shorter_than_a_frame_yields_nothing() {
        let bank = FilterBank::new();
        let (feats, frames) = bank.compute(&vec![0.1; 100]);
        assert_eq!(frames, 0);
        assert!(feats.is_empty());
    }

    #[test]
    fn a_tone_lights_up_its_own_band_and_not_the_rest() {
        let bank = FilterBank::new();
        // 1 kHz sine, half a second.
        let samples: Vec<f32> = (0..8000)
            .map(|i| (2.0 * PI * 1000.0 * i as f32 / SAMPLE_RATE).sin())
            .collect();
        let (feats, frames) = bank.compute(&samples);
        assert!(frames > 0);

        // The loudest bin should sit near 1 kHz, which in mel terms is
        // roughly a third of the way up an 80-bin bank.
        let first_frame = &feats[..NUM_BINS];
        let loudest = first_frame
            .iter()
            .enumerate()
            // total_cmp, not partial_cmp().unwrap(): a NaN anywhere in
            // the bank would otherwise abort the process.
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i)
            .unwrap();
        assert!(
            (20..45).contains(&loudest),
            "a 1 kHz tone should peak in the middle bins, peaked at {loudest}"
        );
    }

    #[test]
    fn mean_normalisation_centres_each_coefficient() {
        let bank = FilterBank::new();
        let (mut feats, frames) = bank.compute(&vec![0.2; 16_000]);
        normalise_mean(&mut feats, frames);
        for bin in 0..NUM_BINS {
            let mean: f32 =
                (0..frames).map(|f| feats[f * NUM_BINS + bin]).sum::<f32>() / frames as f32;
            assert!(mean.abs() < 1e-3, "bin {bin} should be centred, got {mean}");
        }
    }
}
