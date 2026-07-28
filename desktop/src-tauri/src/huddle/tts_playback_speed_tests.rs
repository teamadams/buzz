use super::*;

/// Regression guard: playback-speed processing must not shorten the device
/// cushion that production prepends to every sentence.
#[test]
fn playback_speed_preserves_production_sentence_lead_in() {
    let mut first = true;
    let buf = build_sentence_append_buffer(&mut first, vec![0.5; 2_400], 2_400);
    let processed =
        process_complete_chunk_preserving_lead_in(&buf, SENTENCE_LEAD_IN_SAMPLES, 1.5, SAMPLE_RATE)
            .expect("process production-shaped sentence buffer");

    assert_eq!(SENTENCE_LEAD_IN_SAMPLES, 480);
    assert!(
        processed[..SENTENCE_LEAD_IN_SAMPLES]
            .iter()
            .all(|&sample| sample == 0.0),
        "the fixed device cushion must remain pure zero"
    );
    assert!(
        processed[SENTENCE_LEAD_IN_SAMPLES..]
            .iter()
            .any(|sample| sample.abs() > 0.1),
        "speech energy must remain after the fixed device cushion"
    );
}
