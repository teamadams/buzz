//! Focused tests for Pocket TTS callback streaming and playback queueing.

use super::*;
use std::sync::mpsc;

fn streaming_test_chunk(value: f32, len: usize) -> Vec<f32> {
    vec![value; len]
}

#[test]
fn pocket_streaming_preserves_independent_callback_order() {
    let first = streaming_test_chunk(0.25, 1000);
    let second = streaming_test_chunk(0.5, 1000);
    let complete = [first.as_slice(), second.as_slice()].concat();
    let mut assembler = PocketStreamAssembler::default();
    let mut queued = Vec::<Vec<f32>>::new();

    assembler
        .push(&first, |buffer| {
            queued.push(buffer);
            Ok(())
        })
        .expect("first callback");
    assembler
        .push(&second, |buffer| {
            queued.push(buffer);
            Ok(())
        })
        .expect("second callback");
    assembler
        .finish(&complete, 2400, |buffer| {
            queued.push(buffer);
            Ok(())
        })
        .expect("finish stream");

    assert_eq!(assembler.callback_count, 2);
    assert_eq!(assembler.queued_samples, complete.len());
    assert_eq!(queued.len(), 3, "two streamed blocks plus retained tail");

    let output = queued.concat();
    let speech = &output[SENTENCE_LEAD_IN_SAMPLES..SENTENCE_LEAD_IN_SAMPLES + complete.len()];
    assert!(
        speech[..1000].iter().all(|sample| *sample == 0.25),
        "first callback must remain first"
    );
    assert!(
        speech[1000..1000 + (1000 - FADE_OUT_SAMPLES)]
            .iter()
            .all(|sample| *sample == 0.5),
        "second callback must follow the first without duplication"
    );
}

#[test]
fn pocket_streaming_queues_before_generation_finishes() {
    let mut assembler = PocketStreamAssembler::default();
    let (sender, receiver) = mpsc::channel();

    assembler
        .push(&streaming_test_chunk(0.25, 1000), |buffer| {
            sender
                .send(buffer)
                .map_err(|error| format!("send test PCM: {error}"))
        })
        .expect("stream first callback");

    let queued = receiver
        .try_recv()
        .expect("playback queue receives PCM before finish");
    assert_eq!(
        queued.len(),
        SENTENCE_LEAD_IN_SAMPLES + 1000 - STREAM_TAIL_SAMPLES
    );
}

#[test]
fn pocket_streaming_preserves_quiet_speech_before_a_loud_segment() {
    const PREFIX_SILENCE: usize = 5000;
    const QUIET_SPEECH: usize = 5000;
    const LOUD_SPEECH: usize = 5000;

    let callback = [
        vec![0.0; PREFIX_SILENCE],
        vec![0.005; QUIET_SPEECH],
        vec![0.5; LOUD_SPEECH],
    ]
    .concat();
    let mut assembler = PocketStreamAssembler::default();
    let mut queued = Vec::<Vec<f32>>::new();

    assembler
        .push(&callback, |buffer| {
            queued.push(buffer);
            Ok(())
        })
        .expect("stream callback");

    let output = queued.concat();
    let speech = &output[SENTENCE_LEAD_IN_SAMPLES..];
    let quiet_start = speech
        .iter()
        .position(|sample| *sample == 0.005)
        .expect("quiet onset is retained");
    assert_eq!(
        speech[quiet_start..]
            .iter()
            .take(QUIET_SPEECH)
            .filter(|sample| **sample == 0.005)
            .count(),
        QUIET_SPEECH,
        "a later loud segment must not trim sustained quiet speech"
    );
}

#[test]
fn pocket_streaming_cancellation_stops_queueing() {
    let mut assembler = PocketStreamAssembler::default();
    let mut queued = Vec::<Vec<f32>>::new();
    let cancelled = true;

    let error = assembler
        .push(&streaming_test_chunk(0.25, 1000), |buffer| {
            if cancelled {
                return Err("Pocket TTS streaming cancelled".to_string());
            }
            queued.push(buffer);
            Ok(())
        })
        .expect_err("cancelled append must stop generation");

    assert_eq!(error, "Pocket TTS streaming cancelled");
    assert_eq!(assembler.queued_samples, 0);
    assert!(queued.is_empty());
}

#[test]
fn pocket_streaming_surfaces_playback_queue_errors() {
    let mut assembler = PocketStreamAssembler::default();

    let error = assembler
        .push(&streaming_test_chunk(0.25, 1000), |_| {
            Err("audio device disconnected".to_string())
        })
        .expect_err("queue failure must be returned");

    assert_eq!(error, "audio device disconnected");
}

#[test]
fn pocket_streaming_rejects_callback_contract_mismatch() {
    let mut assembler = PocketStreamAssembler::default();
    assembler
        .push(&streaming_test_chunk(0.25, 1000), |_| Ok(()))
        .expect("callback");

    let error = assembler
        .finish(&streaming_test_chunk(0.25, 1200), 2400, |_| Ok(()))
        .expect_err("mismatched callback samples must not be duplicated");

    assert!(error.contains("callback contract mismatch"));
}
