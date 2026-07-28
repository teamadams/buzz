use super::{enqueue_agent_tts_text, normalize_agent_tts_text, MAX_TTS_TEXT_LEN};

#[tokio::test]
async fn assistant_plain_text_routes_unchanged_into_voice_pipeline_boundary() {
    let (sender, receiver) = std::sync::mpsc::channel();
    let text = "A newly submitted assistant reply.".to_string();

    enqueue_agent_tts_text(text.clone(), move |queued| {
        sender.send(queued).map_err(|error| error.to_string())
    })
    .await
    .expect("route assistant text");

    assert_eq!(receiver.recv().expect("queued text"), text);
}

#[test]
fn assistant_text_truncation_is_unicode_safe_before_voice_routing() {
    let input = "🦀".repeat(MAX_TTS_TEXT_LEN + 1);
    let output = normalize_agent_tts_text(input);
    assert_eq!(
        output.chars().count(),
        MAX_TTS_TEXT_LEN + "... message truncated.".chars().count()
    );
    assert!(output.ends_with("... message truncated."));
}
