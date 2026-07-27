use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[test]
fn selecting_a_voice_immediately_raises_cancel_and_retains_the_engine_handle() {
    let model_dir = tempfile::tempdir().expect("temp model dir");
    let active = Arc::new(AtomicBool::new(false));
    let cancel = Arc::new(AtomicBool::new(false));
    let pipeline = TtsPipeline::new_with_voice(
        model_dir.path().to_path_buf(),
        active,
        Arc::clone(&cancel),
        "reference_sample",
        None,
    )
    .expect("pipeline handle");

    pipeline.select_voice("marius");

    assert!(cancel.load(Ordering::Acquire));
    assert_eq!(
        pipeline
            .voice
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_str(),
        "marius"
    );
}
