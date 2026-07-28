//! Pocket callback-to-playback PCM assembly.

use super::{
    apply_fade_out, clamp_to_full_scale, FADE_OUT_SAMPLES, SAMPLE_RATE, SENTENCE_LEAD_IN_SAMPLES,
};

/// Number of samples retained until the next Pocket callback.
///
/// sherpa-onnx's Pocket callback yields independent Mimi decoder blocks despite
/// the generic Rust wrapper's cumulative-progress documentation. Retaining a
/// short suffix lets us determine which block is actually final and apply the
/// utterance fade exactly once without delaying the preceding ~1.2 s block.
pub(super) const STREAM_TAIL_SAMPLES: usize = FADE_OUT_SAMPLES * 2;

#[derive(Debug, Default)]
pub(super) struct PocketStreamAssembler {
    pending: Vec<f32>,
    callback_samples: usize,
    pub(super) callback_count: usize,
    pub(super) queued_samples: usize,
}

impl PocketStreamAssembler {
    /// Copy and enqueue one independent sherpa-onnx Pocket decoder block.
    ///
    /// `enqueue` must only queue/copy the buffer; playback itself is expected
    /// to happen asynchronously (rodio's `Player::append` has that contract).
    pub(super) fn push<F>(&mut self, samples: &[f32], mut enqueue: F) -> Result<(), String>
    where
        F: FnMut(Vec<f32>) -> Result<(), String>,
    {
        if samples.is_empty() {
            return Ok(());
        }

        self.callback_count += 1;
        self.callback_samples += samples.len();
        self.pending.extend_from_slice(samples);

        let emit_end = self.pending.len().saturating_sub(STREAM_TAIL_SAMPLES);
        if emit_end == 0 {
            return Ok(());
        }

        let emit_start = if self.queued_samples == 0 {
            match leading_audio_start(&self.pending[..emit_end]) {
                Some(start) => start,
                // Do not mark silent prefix PCM as playback. Keep it until a
                // later callback contains a usable onset.
                None => return Ok(()),
            }
        } else {
            0
        };

        let mut buffer =
            Vec::with_capacity(SENTENCE_LEAD_IN_SAMPLES + emit_end.saturating_sub(emit_start));
        if self.queued_samples == 0 {
            buffer.extend(std::iter::repeat_n(0.0_f32, SENTENCE_LEAD_IN_SAMPLES));
        }
        buffer.extend(
            self.pending[emit_start..emit_end]
                .iter()
                .map(|sample| sample.clamp(-1.0, 1.0)),
        );
        if buffer.len() == SENTENCE_LEAD_IN_SAMPLES && self.queued_samples == 0 {
            return Ok(());
        }

        enqueue(buffer)?;
        self.queued_samples += emit_end - emit_start;
        self.pending.drain(..emit_end);
        Ok(())
    }

    /// Queue the retained final suffix, or the complete waveform when an older
    /// sherpa-onnx build produced no callbacks.
    pub(super) fn finish<F>(
        &mut self,
        complete_samples: &[f32],
        silence_buf_len: usize,
        mut enqueue: F,
    ) -> Result<(), String>
    where
        F: FnMut(Vec<f32>) -> Result<(), String>,
    {
        if self.callback_count == 0 {
            self.pending.extend_from_slice(complete_samples);
            self.callback_samples = complete_samples.len();
        } else if self.callback_samples != complete_samples.len() {
            return Err(format!(
                "Pocket TTS callback contract mismatch: callbacks yielded {} samples, final audio contained {}",
                self.callback_samples,
                complete_samples.len()
            ));
        }

        let start = if self.queued_samples == 0 {
            leading_audio_start(&self.pending).unwrap_or(0)
        } else {
            0
        };
        let mut audio = clamp_to_full_scale(self.pending[start..].to_vec());
        apply_fade_out(&mut audio);

        if !audio.is_empty() {
            let trailing_silence_len = silence_buf_len.saturating_sub(SENTENCE_LEAD_IN_SAMPLES);
            let mut buffer = Vec::with_capacity(
                usize::from(self.queued_samples == 0) * SENTENCE_LEAD_IN_SAMPLES
                    + audio.len()
                    + trailing_silence_len,
            );
            if self.queued_samples == 0 {
                buffer.extend(std::iter::repeat_n(0.0_f32, SENTENCE_LEAD_IN_SAMPLES));
            }
            buffer.extend(audio);
            buffer.extend(std::iter::repeat_n(0.0_f32, trailing_silence_len));
            enqueue(buffer)?;
            self.queued_samples += self.pending.len().saturating_sub(start);
        }
        self.pending.clear();

        Ok(())
    }
}

/// Find the first sustained audio in a Pocket progress batch.
///
/// The stabilizing period prefix sometimes yields almost a second of leading
/// silence. A 10 ms RMS window rejects isolated numerical noise, while the
/// retained 50 ms of pre-roll protects soft word onsets.
fn leading_audio_start(samples: &[f32]) -> Option<usize> {
    const WINDOW: usize = SAMPLE_RATE as usize / 100;
    const RETAIN: usize = SAMPLE_RATE as usize / 20;

    // Use a fixed low noise floor. A threshold derived from the loudest sample
    // in the callback can classify a sustained quiet first word as silence when
    // a later vowel is much louder, recreating first-onset clipping.
    const THRESHOLD: f32 = 0.002;
    let threshold = THRESHOLD;
    let threshold_squared = threshold * threshold;
    let mut energy = 0.0_f32;

    for (index, sample) in samples.iter().enumerate() {
        energy += sample * sample;
        if index >= WINDOW {
            let expired = samples[index - WINDOW];
            energy -= expired * expired;
        }
        if index + 1 >= WINDOW && energy / WINDOW as f32 >= threshold_squared {
            return Some((index + 1 - WINDOW).saturating_sub(RETAIN));
        }
    }

    None
}
