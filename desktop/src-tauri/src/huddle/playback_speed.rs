//! Pitch-preserving playback speed for generated speech.
//!
//! This stage sits between Pocket synthesis and rodio playback. It is
//! deliberately independent of Pocket's model parameters, and it does not use
//! rodio's speed control because rodio changes pitch and speed together.

use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use crate::app_state::AppState;

/// Slowest supported generated-speech playback speed.
pub const MIN_PLAYBACK_SPEED: f32 = 0.75;
/// Fastest supported generated-speech playback speed.
pub const MAX_PLAYBACK_SPEED: f32 = 1.5;
/// Default generated-speech playback speed.
pub const DEFAULT_PLAYBACK_SPEED: f32 = 1.0;

const SETTINGS_FILE: &str = "tts-playback-settings.json";
const UNITY_EPSILON: f32 = 0.000_1;

/// Lock-free shared control read by the TTS worker before each synthesis chunk.
#[derive(Clone, Debug)]
pub struct PlaybackSpeedControl(Arc<AtomicU32>);

impl Default for PlaybackSpeedControl {
    fn default() -> Self {
        Self(Arc::new(AtomicU32::new(DEFAULT_PLAYBACK_SPEED.to_bits())))
    }
}

impl PlaybackSpeedControl {
    /// Return the current generated-speech playback speed.
    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Acquire))
    }

    /// Update the in-memory speed after validation.
    pub fn set(&self, speed: f32) -> Result<(), String> {
        validate_speed(speed)?;
        self.0.store(speed.to_bits(), Ordering::Release);
        Ok(())
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct PersistedPlaybackSettings {
    speed: f32,
}

/// Load the global playback speed during app setup.
pub fn load_playback_speed(app: &AppHandle, control: &PlaybackSpeedControl) -> Result<(), String> {
    let path = settings_path(app)?;
    let speed = load_from_path(&path)?;
    control.set(speed)
}

/// Return the globally configured generated-speech playback speed.
#[tauri::command]
pub fn get_tts_playback_speed(state: State<'_, AppState>) -> f32 {
    state.tts_playback_speed.get()
}

/// Persist and apply the global generated-speech playback speed.
#[tauri::command]
pub fn set_tts_playback_speed(
    speed: f32,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    validate_speed(speed)?;
    save_to_path(&settings_path(&app)?, speed)?;
    state.tts_playback_speed.set(speed)
}

fn settings_path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|directory| directory.join(SETTINGS_FILE))
        .map_err(|error| format!("resolve TTS playback settings directory: {error}"))
}

fn load_from_path(path: &Path) -> Result<f32, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DEFAULT_PLAYBACK_SPEED);
        }
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    let settings: PersistedPlaybackSettings = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    validate_speed(settings.speed)?;
    Ok(settings.speed)
}

fn save_to_path(path: &Path, speed: f32) -> Result<(), String> {
    validate_speed(speed)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    let payload = serde_json::to_vec_pretty(&PersistedPlaybackSettings { speed })
        .map_err(|error| format!("serialize TTS playback settings: {error}"))?;
    crate::managed_agents::storage::atomic_write_json(path, &payload)
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessorKind {
    Bypass,
    Signalsmith,
}

/// Stateful streaming pitch-preserving processor.
///
/// Calls to [`Self::process`] preserve input order. The current Pocket path
/// uses [`Self::process_complete_chunk`] because Pocket already hands the
/// player one complete synthesis chunk; a future progressive synthesizer can
/// feed deltas through `process` and pay only the one-time reported latency.
pub struct PlaybackSpeedProcessor {
    inner: Processor,
    speed: f32,
    input_samples: usize,
    output_samples: usize,
    cancelled: bool,
}

enum Processor {
    Bypass,
    Signalsmith(ssstretch::Stretch),
}

impl PlaybackSpeedProcessor {
    /// Select bypass at 1x or Signalsmith Stretch at a non-unity speed.
    pub fn new(speed: f32, sample_rate: u32) -> Result<Self, String> {
        validate_speed(speed)?;
        let inner = if (speed - DEFAULT_PLAYBACK_SPEED).abs() <= UNITY_EPSILON {
            Processor::Bypass
        } else {
            let mut stretch = ssstretch::Stretch::new();
            stretch.preset_default(1, sample_rate as f32);
            Processor::Signalsmith(stretch)
        };
        Ok(Self {
            inner,
            speed,
            input_samples: 0,
            output_samples: 0,
            cancelled: false,
        })
    }

    #[cfg(test)]
    fn kind(&self) -> ProcessorKind {
        match self.inner {
            Processor::Bypass => ProcessorKind::Bypass,
            Processor::Signalsmith(_) => ProcessorKind::Signalsmith,
        }
    }

    /// One-time output-side algorithmic latency in samples.
    pub fn output_latency(&self) -> usize {
        match &self.inner {
            Processor::Bypass => 0,
            Processor::Signalsmith(stretch) => stretch.output_latency().max(0) as usize,
        }
    }

    /// Process a progressive input delta without reordering prior deltas.
    pub fn process(&mut self, input: &[f32]) -> Result<Vec<f32>, String> {
        if input.is_empty() || self.cancelled {
            return Ok(Vec::new());
        }
        match &mut self.inner {
            Processor::Bypass => Ok(input.to_vec()),
            Processor::Signalsmith(stretch) => {
                self.input_samples = self.input_samples.saturating_add(input.len());
                let target_output =
                    (self.input_samples as f64 / self.speed as f64).round() as usize;
                let output_len = target_output.saturating_sub(self.output_samples);
                let input_len = i32_len(input.len())?;
                let output_len_i32 = i32_len(output_len)?;
                let inputs = [input.to_vec()];
                let mut outputs = [Vec::with_capacity(output_len)];
                stretch.process_vec(&inputs, input_len, &mut outputs, output_len_i32);
                self.output_samples = target_output;
                Ok(std::mem::take(&mut outputs[0]))
            }
        }
    }

    /// Process one already-buffered Pocket chunk and compensate DSP pre-roll.
    ///
    /// This returns exactly `input.len() / speed` samples and does not buffer
    /// any later Pocket chunk or the remainder of the response.
    pub fn process_complete_chunk(&mut self, input: &[f32]) -> Result<Vec<f32>, String> {
        if matches!(self.inner, Processor::Bypass) {
            return self.process(input);
        }

        let expected = (input.len() as f64 / self.speed as f64).round() as usize;
        let latency = self.output_latency();
        let mut output = self.process(input)?;
        output.extend(self.drain()?);

        let end = latency.saturating_add(expected);
        if output.len() < end {
            return Err(format!(
                "time stretcher produced {} samples, need {end}",
                output.len()
            ));
        }
        Ok(output[latency..end].to_vec())
    }

    /// Discard all pending processor output after barge-in.
    #[allow(dead_code)] // Used by progressive synthesis integrations; current Pocket chunks are atomic.
    pub fn cancel(&mut self) {
        self.cancelled = true;
        if let Processor::Signalsmith(stretch) = &mut self.inner {
            stretch.reset();
        }
    }

    fn drain(&mut self) -> Result<Vec<f32>, String> {
        if self.cancelled {
            return Ok(Vec::new());
        }
        let Processor::Signalsmith(stretch) = &mut self.inner else {
            return Ok(Vec::new());
        };
        let input_latency = stretch.input_latency().max(0) as usize;
        let output_latency = stretch.output_latency().max(0) as usize;
        let drain_output = (input_latency as f64 / self.speed as f64).ceil() as usize;

        let inputs = [vec![0.0; input_latency]];
        let mut processed = [Vec::with_capacity(drain_output)];
        stretch.process_vec(
            &inputs,
            i32_len(input_latency)?,
            &mut processed,
            i32_len(drain_output)?,
        );
        let mut flushed = [Vec::with_capacity(output_latency)];
        stretch.flush_vec(&mut flushed, i32_len(output_latency)?);
        processed[0].extend_from_slice(&flushed[0]);
        Ok(std::mem::take(&mut processed[0]))
    }
}

/// Validate a generated-speech playback speed.
pub fn validate_speed(speed: f32) -> Result<(), String> {
    if speed.is_finite() && (MIN_PLAYBACK_SPEED..=MAX_PLAYBACK_SPEED).contains(&speed) {
        Ok(())
    } else {
        Err(format!(
            "Speech playback speed must be between {MIN_PLAYBACK_SPEED} and {MAX_PLAYBACK_SPEED}"
        ))
    }
}

fn i32_len(length: usize) -> Result<i32, String> {
    i32::try_from(length).map_err(|_| "audio chunk is too large to process".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE: u32 = 24_000;

    #[test]
    fn selects_bypass_only_at_unity() {
        assert_eq!(
            PlaybackSpeedProcessor::new(1.0, SAMPLE_RATE)
                .expect("bypass")
                .kind(),
            ProcessorKind::Bypass
        );
        assert_eq!(
            PlaybackSpeedProcessor::new(1.25, SAMPLE_RATE)
                .expect("stretcher")
                .kind(),
            ProcessorKind::Signalsmith
        );
    }

    #[test]
    fn chunked_processing_preserves_length_pitch_and_order() {
        let input: Vec<f32> = (0..24_000)
            .map(|sample| {
                let frequency = if sample < 12_000 { 220.0 } else { 440.0 };
                (2.0 * std::f32::consts::PI * frequency * sample as f32 / SAMPLE_RATE as f32).sin()
            })
            .collect();
        let mut processor = PlaybackSpeedProcessor::new(1.25, SAMPLE_RATE).expect("processor");
        let output = processor
            .process_complete_chunk(&input)
            .expect("complete output");

        assert_eq!(output.len(), 19_200);
        let first_frequency = zero_crossing_frequency(&output[2_000..7_000], SAMPLE_RATE);
        let second_frequency = zero_crossing_frequency(&output[12_000..17_000], SAMPLE_RATE);
        assert!(
            (first_frequency - 220.0).abs() < 8.0,
            "first segment measured {first_frequency} Hz"
        );
        assert!(
            (second_frequency - 440.0).abs() <= 10.0,
            "second segment measured {second_frequency} Hz"
        );
    }

    #[test]
    fn cancellation_discards_subsequent_chunks_and_tail() {
        let mut processor = PlaybackSpeedProcessor::new(1.25, SAMPLE_RATE).expect("processor");
        assert!(!processor
            .process(&vec![0.25; 4_800])
            .expect("first chunk")
            .is_empty());
        processor.cancel();
        assert!(processor
            .process(&vec![0.5; 4_800])
            .expect("cancelled chunk")
            .is_empty());
        assert!(processor.drain().expect("cancelled tail").is_empty());
    }

    #[test]
    fn processor_preserves_sine_pitch() {
        let frequency = 220.0_f32;
        let input: Vec<f32> = (0..SAMPLE_RATE * 2)
            .map(|sample| {
                (2.0 * std::f32::consts::PI * frequency * sample as f32 / SAMPLE_RATE as f32).sin()
            })
            .collect();
        let mut processor = PlaybackSpeedProcessor::new(1.5, SAMPLE_RATE).expect("processor");
        let output = processor
            .process_complete_chunk(&input)
            .expect("complete output");
        let measured = zero_crossing_frequency(&output[2_000..], SAMPLE_RATE);
        assert!(
            (measured - frequency).abs() < 3.0,
            "expected {frequency} Hz, measured {measured} Hz"
        );
    }

    #[test]
    fn non_unity_latency_stays_below_75_ms() {
        let processor = PlaybackSpeedProcessor::new(1.25, SAMPLE_RATE).expect("processor");
        let latency_ms = processor.output_latency() as f64 * 1_000.0 / SAMPLE_RATE as f64;
        assert!(latency_ms <= 75.0, "algorithmic latency was {latency_ms}ms");
    }

    #[test]
    fn persisted_speed_round_trips_and_rejects_invalid_values() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join(SETTINGS_FILE);
        save_to_path(&path, 1.25).expect("save");
        assert_eq!(load_from_path(&path).expect("load"), 1.25);

        std::fs::write(&path, br#"{"speed":2.0}"#).expect("invalid fixture");
        assert!(load_from_path(&path).is_err());
    }

    fn zero_crossing_frequency(samples: &[f32], sample_rate: u32) -> f32 {
        let crossings = samples
            .windows(2)
            .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
            .count();
        crossings as f32 * sample_rate as f32 / samples.len() as f32
    }
}
