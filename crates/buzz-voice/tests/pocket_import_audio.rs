use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

use buzz_voice::{
    imported::{write_pcm16_wav, PcmStats, PocketVoiceLibrary},
    pocket::{
        load_text_to_speech, load_voice_style, SynthesisOutcome, DEFAULT_VOICE, SAMPLE_RATE,
        VOICE_FILE_EXT,
    },
};

const PREVIEW_TEXT: &str = "This is an objective Pocket voice preview.";
const INTERRUPT_TEXT: &str =
    "This longer Pocket voice sentence should stop before it produces audio that could be played.";

fn required_path(name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("{name} must point to the required local test path"))
}

fn checked_in_voice() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../desktop/src-tauri/resources/pocket-voices/marius.wav")
}

fn evidence_dir() -> PathBuf {
    std::env::var_os("BUZZ_VOICE_EVIDENCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/buzz-voice-evidence")
        })
}

fn synthesize(model_dir: &Path, voice_path: &Path, text: &str) -> (Vec<f32>, PcmStats) {
    let engine = load_text_to_speech(
        model_dir
            .to_str()
            .expect("Pocket model path must be valid UTF-8"),
    )
    .expect("load Pocket model");
    let style = load_voice_style(voice_path).expect("load selected voice");
    let samples = engine
        .synth_chunk(text, "en", &style, 1)
        .expect("synthesize preview");
    let stats = PcmStats::analyze(&samples, SAMPLE_RATE);
    assert!(
        stats.is_non_silent(),
        "generated PCM must be non-silent: {stats:?}"
    );
    assert!(
        stats.duration_seconds > 0.2,
        "generated PCM is unexpectedly short: {stats:?}"
    );
    (samples, stats)
}

#[test]
#[ignore = "requires BUZZ_POCKET_MODEL_DIR and runs the installed Pocket ONNX model"]
fn objective_import_synthesis_delete_and_mary_fallback() {
    let model_dir = required_path("BUZZ_POCKET_MODEL_DIR");
    let temp = tempfile::tempdir().expect("temporary voice workspace");
    let source = temp.path().join("Imported Marius.wav");
    fs::copy(checked_in_voice(), &source).expect("copy checked-in voice fixture");

    let library_root = temp.path().join("library");
    let library = PocketVoiceLibrary::new(&library_root);
    let imported = library.import_path(&source).expect("import valid WAV");
    assert_eq!(
        library.find(&imported.key).expect("read selection"),
        Some(imported.clone())
    );

    drop(library);
    let relaunched = PocketVoiceLibrary::new(&library_root);
    let selected = relaunched
        .find(&imported.key)
        .expect("reload persisted selection")
        .expect("selected imported voice survived relaunch");
    let imported_path = relaunched
        .resolve_file(&selected)
        .expect("resolve persisted imported voice");
    let (imported_pcm, imported_stats) = synthesize(&model_dir, &imported_path, PREVIEW_TEXT);

    let evidence = evidence_dir();
    fs::create_dir_all(&evidence).expect("create evidence directory");
    let imported_wav = evidence.join("imported-preview.wav");
    write_pcm16_wav(&imported_wav, &imported_pcm, SAMPLE_RATE)
        .expect("write imported preview evidence");

    let engine = load_text_to_speech(
        model_dir
            .to_str()
            .expect("Pocket model path must be valid UTF-8"),
    )
    .expect("load Pocket model for interruption");
    let style = load_voice_style(&imported_path).expect("load imported style for interruption");
    let interrupted = Arc::new(AtomicBool::new(false));
    let interrupt_worker = Arc::clone(&interrupted);
    let interrupter = thread::spawn(move || {
        thread::sleep(Duration::from_millis(25));
        interrupt_worker.store(true, Ordering::Release);
    });
    let interrupt_check = Arc::clone(&interrupted);
    let outcome = engine
        .synth_chunk_interruptible(INTERRUPT_TEXT, "en", &style, 1, move || {
            interrupt_check.load(Ordering::Acquire)
        })
        .expect("interrupt synthesis");
    interrupter.join().expect("join interrupter");
    assert_eq!(
        outcome,
        SynthesisOutcome::Interrupted,
        "interrupted synthesis must discard partial PCM"
    );

    assert!(
        engine
            .synth_chunk("", "en", &style, 1)
            .expect("empty synthesis")
            .is_empty(),
        "empty input must not emit unintended audio"
    );

    relaunched
        .delete(&imported.key)
        .expect("delete imported voice");
    assert_eq!(
        relaunched.find(&imported.key).expect("reload after delete"),
        None
    );

    let mary_path = model_dir.join(format!("{DEFAULT_VOICE}.{VOICE_FILE_EXT}"));
    assert_eq!(
        mary_path.file_name().and_then(|name| name.to_str()),
        Some("reference_sample.wav"),
        "fallback must remain the deterministic Mary reference"
    );
    let (mary_pcm, mary_stats) = synthesize(&model_dir, &mary_path, PREVIEW_TEXT);
    let mary_wav = evidence.join("mary-fallback-preview.wav");
    write_pcm16_wav(&mary_wav, &mary_pcm, SAMPLE_RATE)
        .expect("write Mary fallback preview evidence");

    println!(
        "{}",
        serde_json::json!({
            "importedKey": imported.key,
            "persistence": "reloaded",
            "interruption": "partial PCM discarded",
            "emptyInputSamples": 0,
            "afterDelete": "pocket:mary",
            "importedPreview": {
                "path": imported_wav,
                "samples": imported_stats.sample_count,
                "sampleRate": imported_stats.sample_rate,
                "durationSeconds": imported_stats.duration_seconds,
                "peak": imported_stats.peak,
                "rms": imported_stats.rms,
                "nonSilentSamples": imported_stats.non_silent_samples,
            },
            "maryFallbackPreview": {
                "path": mary_wav,
                "samples": mary_stats.sample_count,
                "sampleRate": mary_stats.sample_rate,
                "durationSeconds": mary_stats.duration_seconds,
                "peak": mary_stats.peak,
                "rms": mary_stats.rms,
                "nonSilentSamples": mary_stats.non_silent_samples,
            }
        })
    );
}

#[cfg(target_os = "macos")]
mod blackhole {
    use std::{
        sync::{Arc, Mutex},
        time::Instant,
    };

    use cpal::{
        self,
        traits::{DeviceTrait, HostTrait, StreamTrait},
        SampleFormat,
    };

    use super::*;

    fn named_device(
        devices: impl Iterator<Item = cpal::Device>,
        expected_name: &str,
    ) -> cpal::Device {
        devices
            .filter_map(|device| {
                device
                    .description()
                    .ok()
                    .map(|description| (device, description.name().to_string()))
            })
            .find(|(_, name)| name == expected_name)
            .map(|(device, _)| device)
            .unwrap_or_else(|| panic!("audio device {expected_name:?} is not available"))
    }

    fn capture_f32(device: &cpal::Device, captured: Arc<Mutex<Vec<f32>>>) -> (cpal::Stream, u32) {
        let supported = device
            .default_input_config()
            .expect("read BlackHole input configuration");
        assert_eq!(
            supported.sample_format(),
            SampleFormat::F32,
            "BlackHole test expects Float32 input"
        );
        let sample_rate = supported.sample_rate();
        let config: cpal::StreamConfig = supported.into();
        let stream = device
            .build_input_stream(
                &config,
                move |data: &[f32], _| {
                    captured
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .extend_from_slice(data);
                },
                |error| panic!("BlackHole capture failed: {error}"),
                None,
            )
            .expect("build BlackHole input stream");
        (stream, sample_rate)
    }

    fn resample_linear(samples: &[f32], source_rate: u32, output_rate: u32) -> Vec<f32> {
        let output_len = ((samples.len() as u64 * u64::from(output_rate)
            + u64::from(source_rate) / 2)
            / u64::from(source_rate)) as usize;
        (0..output_len)
            .map(|index| {
                let source = index as f64 * f64::from(source_rate) / f64::from(output_rate);
                let left = source.floor() as usize;
                let fraction = (source - left as f64) as f32;
                let a = samples[left.min(samples.len() - 1)];
                let b = samples[(left + 1).min(samples.len() - 1)];
                a + (b - a) * fraction
            })
            .collect()
    }

    #[test]
    #[ignore = "requires BUZZ_POCKET_MODEL_DIR and BUZZ_VOICE_AUDIO_DEVICE=BlackHole 2ch"]
    fn blackhole_playback_captures_generated_non_silent_pcm() {
        let model_dir = required_path("BUZZ_POCKET_MODEL_DIR");
        let device_name = std::env::var("BUZZ_VOICE_AUDIO_DEVICE")
            .expect("BUZZ_VOICE_AUDIO_DEVICE must name a BlackHole loopback device");
        let temp = tempfile::tempdir().expect("temporary voice workspace");
        let source = temp.path().join("Imported Marius.wav");
        fs::copy(checked_in_voice(), &source).expect("copy checked-in voice fixture");
        let library = PocketVoiceLibrary::new(temp.path().join("library"));
        let imported = library.import_path(&source).expect("import voice");
        let selected = library
            .find(&imported.key)
            .expect("read selection")
            .expect("selected imported voice");
        let voice_path = library
            .resolve_file(&selected)
            .expect("resolve imported voice");
        let (samples, generated_stats) = synthesize(&model_dir, &voice_path, PREVIEW_TEXT);

        let host = cpal::default_host();
        let input = named_device(
            host.input_devices().expect("enumerate input devices"),
            &device_name,
        );
        let output = named_device(
            host.output_devices().expect("enumerate output devices"),
            &device_name,
        );
        let captured = Arc::new(Mutex::new(Vec::new()));
        let (input_stream, input_sample_rate) = capture_f32(&input, Arc::clone(&captured));
        input_stream.play().expect("start BlackHole capture");

        let supported_output = output
            .default_output_config()
            .expect("read BlackHole output configuration");
        assert_eq!(
            supported_output.sample_format(),
            SampleFormat::F32,
            "BlackHole test expects Float32 output"
        );
        let output_sample_rate = supported_output.sample_rate();
        let output_config: cpal::StreamConfig = supported_output.into();
        let output_channels = usize::from(output_config.channels);
        let playback = resample_linear(&samples, SAMPLE_RATE, output_sample_rate);
        let playback_len = playback.len();
        let mut playback_index = 0usize;
        let output_stream = output
            .build_output_stream(
                &output_config,
                move |data: &mut [f32], _| {
                    for frame in data.chunks_mut(output_channels) {
                        let sample = playback.get(playback_index).copied().unwrap_or(0.0);
                        frame.fill(sample);
                        playback_index = playback_index.saturating_add(1);
                    }
                },
                |error| panic!("BlackHole playback failed: {error}"),
                None,
            )
            .expect("build BlackHole output stream");
        output_stream.play().expect("start BlackHole playback");
        let started = Instant::now();
        let playback_duration =
            Duration::from_secs_f64(playback_len as f64 / f64::from(output_sample_rate));
        while started.elapsed() < playback_duration {
            thread::sleep(Duration::from_millis(20));
        }
        thread::sleep(Duration::from_millis(250));
        drop(output_stream);
        drop(input_stream);

        let captured_samples = captured
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let captured_stats = PcmStats::analyze(&captured_samples, input_sample_rate);
        assert!(
            captured_stats.is_non_silent(),
            "BlackHole must capture non-silent generated PCM: {captured_stats:?}"
        );
        assert!(
            captured_stats.non_silent_samples >= generated_stats.non_silent_samples / 4,
            "captured signal is unexpectedly sparse: generated={generated_stats:?}, captured={captured_stats:?}"
        );
        println!(
            "{}",
            serde_json::json!({
                "device": device_name,
                "generated": {
                    "samples": generated_stats.sample_count,
                    "peak": generated_stats.peak,
                    "rms": generated_stats.rms,
                },
                "captured": {
                    "samples": captured_stats.sample_count,
                    "sampleRate": captured_stats.sample_rate,
                    "peak": captured_stats.peak,
                    "rms": captured_stats.rms,
                    "nonSilentSamples": captured_stats.non_silent_samples,
                }
            })
        );
    }
}
