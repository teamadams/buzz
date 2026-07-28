//! Local Pocket reference-voice import storage and WAV canonicalization.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager};

use crate::managed_agents::storage::atomic_write_json_restricted;

const MAX_SOURCE_BYTES: u64 = 25 * 1024 * 1024;
const MIN_SAMPLE_RATE: u32 = 8_000;
const MAX_SAMPLE_RATE: u32 = 96_000;
const MIN_DURATION_SECONDS: f64 = 2.0;
const MAX_DURATION_SECONDS: f64 = 30.0;
const CANONICAL_SAMPLE_RATE: u32 = 32_000;
const REGISTRY_VERSION: u32 = 1;
const REGISTRY_FILE: &str = "registry.json";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImportedVoice {
    pub key: String,
    pub display_name: String,
    pub content_hash: String,
    pub file_name: String,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportedVoiceRegistry {
    version: u32,
    voices: Vec<ImportedVoice>,
}

pub fn voices_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|path| path.join("tts").join("pocket-voices"))
        .map_err(|error| format!("could not locate local voice storage: {error}"))
}

fn registry_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(voices_dir(app)?.join(REGISTRY_FILE))
}

fn ensure_storage_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path)
        .map_err(|error| format!("could not create local voice storage: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("could not restrict local voice storage: {error}"))?;
    }
    Ok(())
}

pub fn load_registry(app: &AppHandle) -> Result<Vec<ImportedVoice>, String> {
    let path = registry_path(app)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes =
        fs::read(&path).map_err(|error| format!("could not read imported voices: {error}"))?;
    let registry: ImportedVoiceRegistry = serde_json::from_slice(&bytes)
        .map_err(|error| format!("imported voice registry is invalid: {error}"))?;
    if registry.version > REGISTRY_VERSION {
        return Err(format!(
            "imported voice registry version {} is newer than this Buzz build supports",
            registry.version
        ));
    }
    Ok(registry
        .voices
        .into_iter()
        .filter(|voice| {
            valid_hash(&voice.content_hash)
                && voice.key == format!("pocket:imported:{}", voice.content_hash)
                && voice.file_name == format!("{}.wav", voice.content_hash)
        })
        .filter(|voice| resolve_file(app, voice).is_ok())
        .collect())
}

fn save_registry(app: &AppHandle, voices: &[ImportedVoice]) -> Result<(), String> {
    let dir = voices_dir(app)?;
    ensure_storage_dir(&dir)?;
    let payload = serde_json::to_vec_pretty(&ImportedVoiceRegistry {
        version: REGISTRY_VERSION,
        voices: voices.to_vec(),
    })
    .map_err(|error| format!("could not encode imported voice registry: {error}"))?;
    atomic_write_json_restricted(&dir.join(REGISTRY_FILE), &payload)
        .map_err(|error| format!("could not save imported voice registry: {error}"))
}

pub fn resolve_file(app: &AppHandle, voice: &ImportedVoice) -> Result<PathBuf, String> {
    if !valid_hash(&voice.content_hash) || voice.file_name != format!("{}.wav", voice.content_hash)
    {
        return Err("Imported voice registry contains an invalid file identity".to_string());
    }
    let path = voices_dir(app)?.join(&voice.file_name);
    if !is_regular_file_without_symlink(&path) {
        return Err(format!("Imported voice {} is missing", voice.display_name));
    }
    let bytes =
        fs::read(&path).map_err(|error| format!("could not verify imported voice: {error}"))?;
    if hex::encode(Sha256::digest(bytes)) != voice.content_hash {
        return Err(format!(
            "Imported voice {} does not match its content identity",
            voice.display_name
        ));
    }
    Ok(path)
}

pub async fn pick_and_import(app: &AppHandle) -> Result<Option<ImportedVoice>, String> {
    use tauri_plugin_dialog::DialogExt;

    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .add_filter("WAV audio", &["wav"])
        .pick_file(move |path| {
            let _ = sender.send(path);
        });
    let Some(file_path) = receiver
        .await
        .map_err(|_| "voice picker closed unexpectedly".to_string())?
    else {
        return Ok(None);
    };
    let path = file_path
        .as_path()
        .ok_or("the selected voice path is invalid")?
        .to_path_buf();
    let app = app.clone();
    tokio::task::spawn_blocking(move || import_path(&app, &path))
        .await
        .map_err(|error| format!("voice import task failed: {error}"))?
        .map(Some)
}

pub fn import_path(app: &AppHandle, source: &Path) -> Result<ImportedVoice, String> {
    let metadata =
        fs::metadata(source).map_err(|error| format!("could not inspect selected WAV: {error}"))?;
    if metadata.len() > MAX_SOURCE_BYTES {
        return Err("Voice WAV must be 25 MB or smaller".to_string());
    }
    let source_bytes =
        fs::read(source).map_err(|error| format!("could not read selected WAV: {error}"))?;
    let samples = decode_wav(&source_bytes)?;
    let canonical_samples = resample_linear(&samples.samples, samples.sample_rate);
    let canonical = encode_pcm16_wav(&canonical_samples, CANONICAL_SAMPLE_RATE);
    let hash = hex::encode(Sha256::digest(&canonical));
    let key = format!("pocket:imported:{hash}");
    let file_name = format!("{hash}.wav");
    let display_name = source
        .file_stem()
        .and_then(|name| name.to_str())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("Imported voice")
        .chars()
        .take(80)
        .collect::<String>();

    let dir = voices_dir(app)?;
    ensure_storage_dir(&dir)?;
    let file_path = dir.join(&file_name);
    let file_created = !file_path.exists();
    if file_created {
        atomic_write_json_restricted(&file_path, &canonical)
            .map_err(|error| format!("could not save imported voice audio: {error}"))?;
    } else {
        if !is_regular_file_without_symlink(&file_path) {
            return Err("Imported voice storage contains an unsafe file entry".to_string());
        }
        let existing = fs::read(&file_path)
            .map_err(|error| format!("could not verify imported voice audio: {error}"))?;
        if hex::encode(Sha256::digest(&existing)) != hash {
            return Err("Imported voice storage contains mismatched audio data".to_string());
        }
    }

    let mut imported = ImportedVoice {
        key,
        display_name,
        content_hash: hash,
        file_name,
    };
    let mut voices = load_registry(app)?;
    if let Some(existing) = voices
        .iter()
        .find(|voice| voice.content_hash == imported.content_hash)
    {
        imported = existing.clone();
    } else {
        voices.push(imported.clone());
    }
    if let Err(error) = save_registry(app, &voices) {
        if file_created {
            let _ = fs::remove_file(&file_path);
        }
        return Err(error);
    }
    Ok(imported)
}

pub fn delete(app: &AppHandle, key: &str) -> Result<(), String> {
    let mut voices = load_registry(app)?;
    let index = voices
        .iter()
        .position(|voice| voice.key == key)
        .ok_or_else(|| format!("Unknown imported voice: {key}"))?;
    let previous_voices = voices.clone();
    let removed = voices.remove(index);
    save_registry(app, &voices)?;
    let path = voices_dir(app)?.join(removed.file_name);
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            save_registry(app, &previous_voices).map_err(|rollback_error| {
                format!(
                    "Imported voice audio could not be deleted ({error}), and its registry entry \
                     could not be restored ({rollback_error})"
                )
            })?;
            Err(format!(
                "Imported voice audio could not be deleted: {error}"
            ))
        }
    }
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_regular_file_without_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.file_type().is_file() && !metadata.file_type().is_symlink())
}

#[derive(Debug)]
struct DecodedWav {
    sample_rate: u32,
    samples: Vec<f32>,
}

fn decode_wav(bytes: &[u8]) -> Result<DecodedWav, String> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("Selected file is not a valid RIFF/WAVE file".to_string());
    }
    let mut offset = 12usize;
    let mut format = None;
    let mut data = None;
    while offset.checked_add(8).is_some_and(|end| end <= bytes.len()) {
        let id = &bytes[offset..offset + 4];
        let size =
            u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap_or([0; 4])) as usize;
        let start = offset + 8;
        let end = start.checked_add(size).ok_or("WAV chunk size overflow")?;
        if end > bytes.len() {
            return Err("Selected WAV contains a truncated chunk".to_string());
        }
        if id == b"fmt " {
            format = Some(&bytes[start..end]);
        } else if id == b"data" {
            data = Some(&bytes[start..end]);
        }
        offset = end + (size & 1);
    }
    let format = format.ok_or("Selected WAV has no format chunk")?;
    let data = data.ok_or("Selected WAV has no audio data")?;
    if format.len() < 16 {
        return Err("Selected WAV has an invalid format chunk".to_string());
    }
    let encoding = u16::from_le_bytes(format[0..2].try_into().unwrap_or([0; 2]));
    let encoding = if encoding == 0xfffe && format.len() >= 40 {
        u16::from_le_bytes(format[24..26].try_into().unwrap_or([0; 2]))
    } else {
        encoding
    };
    let channels = u16::from_le_bytes(format[2..4].try_into().unwrap_or([0; 2]));
    let sample_rate = u32::from_le_bytes(format[4..8].try_into().unwrap_or([0; 4]));
    let block_align = u16::from_le_bytes(format[12..14].try_into().unwrap_or([0; 2])) as usize;
    let bits = u16::from_le_bytes(format[14..16].try_into().unwrap_or([0; 2]));
    if channels != 1 {
        return Err("Voice WAV must be mono".to_string());
    }
    if !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&sample_rate) {
        return Err("Voice WAV sample rate must be between 8 and 96 kHz".to_string());
    }
    let bytes_per_sample = usize::from(bits.div_ceil(8));
    if block_align != bytes_per_sample || block_align == 0 || data.len() % block_align != 0 {
        return Err("Voice WAV has invalid sample alignment".to_string());
    }
    if !matches!((encoding, bits), (1, 8 | 16 | 24 | 32) | (3, 32)) {
        return Err("Voice WAV must contain PCM or 32-bit float audio".to_string());
    }
    let frames = data.len() / block_align;
    let duration = frames as f64 / f64::from(sample_rate);
    if !(MIN_DURATION_SECONDS..=MAX_DURATION_SECONDS).contains(&duration) {
        return Err("Voice WAV must be between 2 and 30 seconds long".to_string());
    }

    let mut samples = Vec::with_capacity(frames);
    for chunk in data.chunks_exact(block_align) {
        let sample = match (encoding, bits) {
            (1, 8) => (f32::from(chunk[0]) - 128.0) / 128.0,
            (1, 16) => f32::from(i16::from_le_bytes([chunk[0], chunk[1]])) / 32768.0,
            (1, 24) => {
                let raw = i32::from_le_bytes([
                    chunk[0],
                    chunk[1],
                    chunk[2],
                    if chunk[2] & 0x80 == 0 { 0 } else { 0xff },
                ]);
                raw as f32 / 8_388_608.0
            }
            (1, 32) => {
                i32::from_le_bytes(chunk.try_into().map_err(|_| "invalid PCM sample")?) as f32
                    / 2_147_483_648.0
            }
            (3, 32) => f32::from_le_bytes(
                chunk
                    .try_into()
                    .map_err(|_| "invalid floating-point sample")?,
            ),
            _ => unreachable!(),
        };
        if !sample.is_finite() {
            return Err("Voice WAV contains non-finite samples".to_string());
        }
        samples.push(sample.clamp(-1.0, 1.0));
    }
    let peak = samples
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    let rms =
        (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32).sqrt();
    if peak < 0.001 || rms < 0.0001 {
        return Err("Voice WAV is silent or too quiet to clone".to_string());
    }
    Ok(DecodedWav {
        sample_rate,
        samples,
    })
}

fn resample_linear(samples: &[f32], source_rate: u32) -> Vec<f32> {
    if source_rate == CANONICAL_SAMPLE_RATE {
        return samples.to_vec();
    }
    let output_len = ((samples.len() as u64 * u64::from(CANONICAL_SAMPLE_RATE)
        + u64::from(source_rate) / 2)
        / u64::from(source_rate)) as usize;
    (0..output_len)
        .map(|index| {
            let source = index as f64 * f64::from(source_rate) / f64::from(CANONICAL_SAMPLE_RATE);
            let left = source.floor() as usize;
            let fraction = (source - left as f64) as f32;
            let a = samples[left.min(samples.len() - 1)];
            let b = samples[(left + 1).min(samples.len() - 1)];
            a + (b - a) * fraction
        })
        .collect()
}

fn encode_pcm16_wav(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(sample_rate: u32, seconds: usize, amplitude: f32) -> Vec<u8> {
        let samples = (0..sample_rate as usize * seconds)
            .map(|index| {
                amplitude
                    * (std::f32::consts::TAU * 220.0 * index as f32 / sample_rate as f32).sin()
            })
            .collect::<Vec<_>>();
        encode_pcm16_wav(&samples, sample_rate)
    }

    #[test]
    fn validates_and_canonicalizes_supported_wav() {
        let decoded = decode_wav(&fixture(44_100, 2, 0.5)).expect("valid WAV");
        let canonical = resample_linear(&decoded.samples, decoded.sample_rate);
        assert_eq!(canonical.len(), 64_000);
        let encoded = encode_pcm16_wav(&canonical, CANONICAL_SAMPLE_RATE);
        let reparsed = decode_wav(&encoded).expect("canonical WAV");
        assert_eq!(reparsed.sample_rate, 32_000);
        assert_eq!(reparsed.samples.len(), 64_000);
    }

    #[test]
    fn rejects_silent_and_short_wav() {
        assert!(decode_wav(&fixture(32_000, 2, 0.0))
            .expect_err("silence rejected")
            .contains("silent"));
        assert!(decode_wav(&fixture(32_000, 1, 0.5))
            .expect_err("short rejected")
            .contains("between 2 and 30"));
    }

    #[test]
    fn rejects_stereo_out_of_range_and_overlong_wav() {
        let mut stereo_header = fixture(32_000, 2, 0.5);
        stereo_header[22..24].copy_from_slice(&2_u16.to_le_bytes());
        assert!(decode_wav(&stereo_header)
            .expect_err("stereo rejected")
            .contains("mono"));

        let mut low_rate_header = fixture(32_000, 2, 0.5);
        low_rate_header[24..28].copy_from_slice(&4_000_u32.to_le_bytes());
        assert!(decode_wav(&low_rate_header)
            .expect_err("low sample rate rejected")
            .contains("between 8 and 96"));

        assert!(decode_wav(&fixture(8_000, 31, 0.5))
            .expect_err("overlong rejected")
            .contains("between 2 and 30"));
    }
}
