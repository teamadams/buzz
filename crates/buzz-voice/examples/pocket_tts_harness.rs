//! Standalone benchmark for Buzz's production Pocket TTS implementation.

use std::env;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command as ProcessCommand;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use buzz_voice::pocket::{
    load_text_to_speech, load_voice_style, DEFAULT_VOICE, SAMPLE_RATE, VOICE_FILE_EXT,
};

const DEFAULT_TEXT: &str = concat!(
    "This sentence has five words. Here are five more words. Five-word sentences are fine. ",
    "But several together become monotonous. Listen to what is happening. ",
    "The writing is getting boring. The sound of it drones. It’s like a stuck record. ",
    "The ear demands some variety. Now listen. ",
    "I vary the sentence length, and I create music. Music. The writing sings. ",
    "It has a pleasant rhythm, a lilt, a harmony. I use short sentences. ",
    "And I use sentences of medium length. ",
    "And sometimes, when I am certain the reader is rested, I will engage him with a sentence ",
    "of considerable length, a sentence that burns with energy and builds with all the impetus ",
    "of a crescendo, the roll of the drums, the crash of the cymbals–sounds that say listen to ",
    "this, it is important."
);
const DEFAULT_OUTPUT_NAME: &str = "buzz-pocket-tts.wav";
const DEFAULT_RUNS: usize = 2;
const SYNTH_STEPS: usize = 1;

#[derive(Debug, PartialEq)]
struct Args {
    model_dir: PathBuf,
    voice: PathBuf,
    text: String,
    output: PathBuf,
    runs: usize,
    play: bool,
}

enum Command {
    Run(Args),
    Help,
}

#[derive(Debug, PartialEq)]
struct RunMetrics {
    synthesis: Duration,
    audio: Duration,
}

impl RunMetrics {
    fn rtf(&self) -> f64 {
        self.synthesis.as_secs_f64() / self.audio.as_secs_f64()
    }
}

fn main() -> ExitCode {
    let process_started = Instant::now();
    match run(env::args().skip(1), process_started) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: impl IntoIterator<Item = String>, process_started: Instant) -> Result<(), String> {
    let args = match parse_args(args)? {
        Command::Run(args) => args,
        Command::Help => {
            print_help();
            return Ok(());
        }
    };

    println!("Model directory: {}", args.model_dir.display());
    println!("Reference voice: {}", args.voice.display());
    println!("Output WAV:      {}", args.output.display());
    println!("Runs:            {}", args.runs);

    let model_started = Instant::now();
    let model_dir = args.model_dir.to_str().ok_or_else(|| {
        format!(
            "model path is not valid UTF-8: {}",
            args.model_dir.display()
        )
    })?;
    let engine = load_text_to_speech(model_dir)?;
    let model_load = model_started.elapsed();
    println!("Model load:      {}", format_duration(model_load));

    let voice_started = Instant::now();
    let voice = load_voice_style(&args.voice)?;
    let voice_load = voice_started.elapsed();
    println!("Voice load:      {}", format_duration(voice_load));
    println!(
        "Baseline:        the production API returns only a complete PCM buffer, \
         so time to playable PCM equals synthesis time"
    );

    let mut final_audio = Vec::new();
    for run_number in 1..=args.runs {
        let synthesis_started = Instant::now();
        let audio = engine.synth_chunk(&args.text, "en", &voice, SYNTH_STEPS)?;
        let synthesis = synthesis_started.elapsed();
        let process_to_playable_pcm = process_started.elapsed();
        if audio.is_empty() {
            return Err("Pocket TTS returned an empty audio buffer".to_string());
        }

        let metrics = metrics_for(synthesis, audio.len())?;
        println!(
            "Run {run_number}: synthesis {}, audio {}, RTF {:.3}, \
             full-buffer time to playable PCM {}",
            format_duration(metrics.synthesis),
            format_duration(metrics.audio),
            metrics.rtf(),
            format_duration(metrics.synthesis),
        );
        if run_number == 1 {
            println!(
                "First playable PCM from process start: {} \
                 (model load + voice load + full-buffer synthesis)",
                format_duration(process_to_playable_pcm)
            );
        }
        final_audio = audio;
    }

    let write_started = Instant::now();
    write_wav(&args.output, &final_audio)?;
    println!(
        "WAV ready:       {} in {}",
        args.output.display(),
        format_duration(write_started.elapsed())
    );

    if args.play {
        play_audio(&args.output)?;
    }

    Ok(())
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let mut model_dir = None;
    let mut voice = None;
    let mut text = None;
    let mut output = None;
    let mut runs = DEFAULT_RUNS;
    let mut play = cfg!(target_os = "macos");
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "--model-dir" => {
                model_dir = Some(PathBuf::from(required_value(&mut args, &arg)?));
            }
            "--voice" => {
                voice = Some(PathBuf::from(required_value(&mut args, &arg)?));
            }
            "--text" => {
                text = Some(required_value(&mut args, &arg)?);
            }
            "-o" | "--output" => {
                output = Some(PathBuf::from(required_value(&mut args, &arg)?));
            }
            "--runs" => {
                let value = required_value(&mut args, &arg)?;
                runs = value
                    .parse()
                    .map_err(|_| format!("invalid run count: {value}"))?;
                if runs == 0 {
                    return Err("--runs must be at least 1".to_string());
                }
            }
            "--no-play" => play = false,
            _ => return Err(format!("unknown option: {arg}")),
        }
    }

    let model_dir = model_dir.ok_or_else(|| "--model-dir PATH is required".to_string())?;
    let voice =
        voice.unwrap_or_else(|| model_dir.join(format!("{DEFAULT_VOICE}.{VOICE_FILE_EXT}")));
    let text = text.unwrap_or_else(|| DEFAULT_TEXT.to_string());
    if text.trim().is_empty() {
        return Err("--text must not be empty".to_string());
    }

    Ok(Command::Run(Args {
        model_dir,
        voice,
        text,
        output: output.unwrap_or_else(|| env::temp_dir().join(DEFAULT_OUTPUT_NAME)),
        runs,
        play,
    }))
}

fn required_value(args: &mut impl Iterator<Item = String>, option: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("{option} requires a value"))
}

fn metrics_for(synthesis: Duration, sample_count: usize) -> Result<RunMetrics, String> {
    if sample_count == 0 {
        return Err("cannot calculate timing for empty audio".to_string());
    }
    Ok(RunMetrics {
        synthesis,
        audio: Duration::from_secs_f64(sample_count as f64 / SAMPLE_RATE as f64),
    })
}

fn format_duration(duration: Duration) -> String {
    format!("{:.1} ms", duration.as_secs_f64() * 1_000.0)
}

fn write_wav(path: &Path, samples: &[f32]) -> Result<(), String> {
    let path_str = path
        .to_str()
        .ok_or_else(|| format!("output path is not valid UTF-8: {}", path.display()))?;
    if sherpa_onnx::write(path_str, samples, SAMPLE_RATE as i32) {
        Ok(())
    } else {
        Err(format!("could not write WAV to {}", path.display()))
    }
}

#[cfg(target_os = "macos")]
fn play_audio(path: &Path) -> Result<(), String> {
    println!("Playing with afplay. Pass --no-play to skip playback.");
    let status = ProcessCommand::new("/usr/bin/afplay")
        .arg(path)
        .status()
        .map_err(|error| format!("could not start afplay: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("afplay exited with status {status}"))
    }
}

#[cfg(not(target_os = "macos"))]
fn play_audio(_path: &Path) -> Result<(), String> {
    Err("automatic playback is only available on macOS".to_string())
}

fn print_help() {
    println!(
        "\
Benchmark Buzz's production Pocket TTS engine without starting Desktop.

Usage:
  cargo run --release -p buzz-voice --example pocket_tts_harness -- --model-dir PATH [OPTIONS]

Options:
  --model-dir PATH   Pocket model directory (required)
  --voice PATH       Reference voice WAV
  --text TEXT        Text to synthesize
  -o, --output PATH  Output WAV path
  --runs N           Loaded-engine synthesis runs (default: {DEFAULT_RUNS})
  --no-play          Do not play the final WAV on macOS
  -h, --help         Show this help

Defaults:
  voice:  MODEL_DIR/{DEFAULT_VOICE}.{VOICE_FILE_EXT}
  text:   {DEFAULT_TEXT}
  output: the system temporary directory/{DEFAULT_OUTPUT_NAME}
  play:   enabled on macOS"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_requires_explicit_model_directory() {
        let error = match parse_args(Vec::<String>::new()) {
            Ok(_) => panic!("missing model directory should fail"),
            Err(error) => error,
        };
        assert_eq!(error, "--model-dir PATH is required");
    }

    #[test]
    fn parse_uses_stable_defaults() {
        let command =
            parse_args(["--model-dir".into(), "/models/pocket".into()]).expect("valid arguments");
        let Command::Run(args) = command else {
            panic!("expected run command");
        };
        assert_eq!(args.model_dir, PathBuf::from("/models/pocket"));
        assert_eq!(
            args.voice,
            PathBuf::from("/models/pocket/reference_sample.wav")
        );
        assert_eq!(args.text, DEFAULT_TEXT);
        assert_eq!(args.output, env::temp_dir().join(DEFAULT_OUTPUT_NAME));
        assert_eq!(args.runs, 2);
        assert_eq!(args.play, cfg!(target_os = "macos"));
    }

    #[test]
    fn parse_accepts_all_overrides() {
        let command = parse_args([
            "--model-dir".into(),
            "/models".into(),
            "--voice".into(),
            "/voices/alex.wav".into(),
            "--text".into(),
            "Hello there.".into(),
            "--output".into(),
            "/tmp/custom.wav".into(),
            "--runs".into(),
            "4".into(),
            "--no-play".into(),
        ])
        .expect("valid arguments");
        let Command::Run(args) = command else {
            panic!("expected run command");
        };
        assert_eq!(args.voice, PathBuf::from("/voices/alex.wav"));
        assert_eq!(args.text, "Hello there.");
        assert_eq!(args.output, PathBuf::from("/tmp/custom.wav"));
        assert_eq!(args.runs, 4);
        assert!(!args.play);
    }

    #[test]
    fn parse_rejects_zero_runs_and_empty_text() {
        assert!(parse_args([
            "--model-dir".into(),
            "/models".into(),
            "--runs".into(),
            "0".into(),
        ])
        .is_err());
        assert!(parse_args([
            "--model-dir".into(),
            "/models".into(),
            "--text".into(),
            "  ".into(),
        ])
        .is_err());
    }

    #[test]
    fn timing_math_reports_conventional_rtf() {
        let metrics = metrics_for(Duration::from_millis(500), SAMPLE_RATE as usize * 2)
            .expect("non-empty audio");
        assert_eq!(metrics.audio, Duration::from_secs(2));
        assert!((metrics.rtf() - 0.25).abs() < f64::EPSILON);
        assert!(metrics_for(Duration::from_millis(1), 0).is_err());
    }

    #[test]
    fn wav_writer_produces_readable_pcm() {
        let path = env::temp_dir().join(format!(
            "buzz-pocket-tts-harness-test-{}.wav",
            std::process::id()
        ));
        let samples = [0.0, 0.25, -0.25, 0.0];
        write_wav(&path, &samples).expect("write test WAV");
        let path_str = path.to_str().expect("UTF-8 test path");
        let wave = sherpa_onnx::Wave::read(path_str).expect("read test WAV");
        assert_eq!(wave.sample_rate(), SAMPLE_RATE as i32);
        assert_eq!(wave.samples().len(), samples.len());
        std::fs::remove_file(path).expect("remove test WAV");
    }
}
