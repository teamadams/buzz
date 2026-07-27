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
enum ProcessorKind {
    Bypass,
    Signalsmith,
}

#[cfg(test)]
fn processor_kind(speed: f32) -> ProcessorKind {
    if (speed - DEFAULT_PLAYBACK_SPEED).abs() <= UNITY_EPSILON {
        ProcessorKind::Bypass
    } else {
        ProcessorKind::Signalsmith
    }
}

/// Pitch-preserve one already-buffered Pocket chunk.
///
/// The returned buffer has exactly `input.len() / speed` samples. Signalsmith
/// starts a reset processor `input_latency` samples before the supplied audio
/// and emits another `output_latency` samples of pre-roll. Both components are
/// removed after draining, so the returned chunk retains its beginning and
/// tail without buffering any later Pocket chunk.
pub fn process_complete_chunk(
    input: &[f32],
    speed: f32,
    sample_rate: u32,
) -> Result<Vec<f32>, String> {
    validate_speed(speed)?;
    if input.is_empty() || (speed - DEFAULT_PLAYBACK_SPEED).abs() <= UNITY_EPSILON {
        return Ok(input.to_vec());
    }

    let expected = (input.len() as f64 / speed as f64).round() as usize;
    let mut stretch = ssstretch::Stretch::new();
    stretch.preset_default(1, sample_rate as f32);
    let input_latency = stretch.input_latency().max(0) as usize;
    let output_latency = stretch.output_latency().max(0) as usize;
    let reset_pre_roll = (input_latency as f64 / speed as f64).ceil() as usize;

    let inputs = [input.to_vec()];
    let mut output = [Vec::with_capacity(expected)];
    stretch.process_vec(
        &inputs,
        i32_len(input.len())?,
        &mut output,
        i32_len(expected)?,
    );

    let latency_input = [vec![0.0; input_latency]];
    let mut latency_output = [Vec::with_capacity(reset_pre_roll)];
    stretch.process_vec(
        &latency_input,
        i32_len(input_latency)?,
        &mut latency_output,
        i32_len(reset_pre_roll)?,
    );
    output[0].extend_from_slice(&latency_output[0]);

    let mut flushed = [Vec::with_capacity(output_latency)];
    stretch.flush_vec(&mut flushed, i32_len(output_latency)?);
    output[0].extend_from_slice(&flushed[0]);

    let start = reset_pre_roll.saturating_add(output_latency);
    let end = start.saturating_add(expected);
    if output[0].len() < end {
        return Err(format!(
            "time stretcher produced {} samples, need {end}",
            output[0].len()
        ));
    }
    Ok(output[0][start..end].to_vec())
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
        assert_eq!(processor_kind(1.0), ProcessorKind::Bypass);
        assert_eq!(processor_kind(1.25), ProcessorKind::Signalsmith);
    }

    #[test]
    fn complete_processing_preserves_length_pitch_and_order() {
        let input: Vec<f32> = (0..24_000)
            .map(|sample| {
                let frequency = if sample < 8_000 {
                    220.0
                } else if sample < 16_000 {
                    440.0
                } else {
                    660.0
                };
                (2.0 * std::f32::consts::PI * frequency * sample as f32 / SAMPLE_RATE as f32).sin()
            })
            .collect();
        let output = process_complete_chunk(&input, 1.25, SAMPLE_RATE).expect("complete output");

        assert_eq!(output.len(), 19_200);
        let first_frequency = zero_crossing_frequency(&output[1_500..5_000], SAMPLE_RATE);
        let second_frequency = zero_crossing_frequency(&output[7_500..11_000], SAMPLE_RATE);
        let third_frequency = zero_crossing_frequency(&output[14_000..18_000], SAMPLE_RATE);
        assert!(
            (first_frequency - 220.0).abs() < 8.0,
            "first segment measured {first_frequency} Hz"
        );
        assert!(
            (second_frequency - 440.0).abs() <= 10.0,
            "second segment measured {second_frequency} Hz"
        );
        assert!(
            (third_frequency - 660.0).abs() <= 12.0,
            "third segment measured {third_frequency} Hz"
        );
    }

    #[test]
    fn processor_preserves_sine_pitch() {
        let frequency = 220.0_f32;
        let input: Vec<f32> = (0..SAMPLE_RATE * 2)
            .map(|sample| {
                (2.0 * std::f32::consts::PI * frequency * sample as f32 / SAMPLE_RATE as f32).sin()
            })
            .collect();
        let output = process_complete_chunk(&input, 1.5, SAMPLE_RATE).expect("complete output");
        assert!(
            root_mean_square(&output[..480]) > 0.2,
            "full reset pre-roll was not removed"
        );
        assert!(
            root_mean_square(&output[output.len() - 480..]) > 0.2,
            "speech tail was truncated"
        );
        let measured = zero_crossing_frequency(&output[2_000..], SAMPLE_RATE);
        assert!(
            (measured - frequency).abs() < 3.0,
            "expected {frequency} Hz, measured {measured} Hz"
        );
    }

    #[test]
    fn complete_processing_preserves_onset_timing() {
        let input: Vec<f32> = (0..SAMPLE_RATE)
            .map(|sample| {
                if (4_800..12_000).contains(&sample) || sample >= 16_800 {
                    (2.0 * std::f32::consts::PI * 220.0 * sample as f32 / SAMPLE_RATE as f32).sin()
                } else {
                    0.0
                }
            })
            .collect();
        let output = process_complete_chunk(&input, 1.25, SAMPLE_RATE).expect("complete output");
        let active_windows = active_window_starts(&output, 240, 0.15);

        let first_onset = *active_windows.first().expect("first tone onset");
        let second_onset = active_windows
            .windows(2)
            .find_map(|pair| (pair[1] > pair[0] + 480).then_some(pair[1]))
            .expect("second tone onset");
        assert!(
            (3_200..=4_400).contains(&first_onset),
            "first onset shifted to sample {first_onset}"
        );
        assert!(
            (12_800..=14_000).contains(&second_onset),
            "second onset shifted to sample {second_onset}"
        );
    }

    #[test]
    fn non_unity_latency_stays_below_75_ms() {
        let mut stretch = ssstretch::Stretch::new();
        stretch.preset_default(1, SAMPLE_RATE as f32);
        let latency_ms = stretch.output_latency().max(0) as f64 * 1_000.0 / SAMPLE_RATE as f64;
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

    fn root_mean_square(samples: &[f32]) -> f32 {
        (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32).sqrt()
    }

    fn active_window_starts(samples: &[f32], window: usize, threshold: f32) -> Vec<usize> {
        samples
            .chunks_exact(window)
            .enumerate()
            .filter_map(|(index, chunk)| {
                (root_mean_square(chunk) > threshold).then_some(index * window)
            })
            .collect()
    }
}
