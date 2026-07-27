use std::{
    collections::VecDeque,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender},
        Arc, Mutex,
    },
};

use crate::huddle::pocket::{load_voice_style, VoiceStyle, DEFAULT_VOICE, VOICE_FILE_EXT};

#[derive(Debug)]
pub(super) struct PendingVoiceChange {
    pub(super) generation: u64,
    acknowledged: tokio::sync::oneshot::Sender<()>,
}

pub(super) type VoiceChangeAck = Arc<Mutex<Option<PendingVoiceChange>>>;
pub(super) type WorkerVoiceState = (Arc<Mutex<String>>, Arc<AtomicU64>, VoiceChangeAck);
pub(super) type WorkerCancelSignals = (Arc<AtomicBool>, Arc<AtomicBool>);
pub(super) type CancelTextState<'a> = (
    &'a mpsc::Receiver<QueuedText>,
    &'a mut VecDeque<QueuedText>,
    &'a mut Option<QueuedText>,
);
pub(super) type CancelSignals<'a> = (&'a AtomicBool, &'a AtomicBool);

#[derive(Debug)]
pub(super) struct QueuedText {
    pub(super) generation: u64,
    pub(super) text: String,
}

#[derive(Clone, Debug)]
pub(crate) struct TtsTextSender {
    pub(super) text_tx: SyncSender<QueuedText>,
    pub(super) generation: u64,
}

impl TtsTextSender {
    pub(crate) fn send(&self, text: String) -> Result<(), String> {
        self.text_tx
            .send(QueuedText {
                generation: self.generation,
                text,
            })
            .map_err(|error| error.to_string())
    }
}

pub(super) fn begin_voice_change(
    selected_voice: &Mutex<String>,
    voice_generation: &AtomicU64,
    voice_cancel: &AtomicBool,
    voice_change_ack: &VoiceChangeAck,
    voice: &str,
) -> Option<tokio::sync::oneshot::Receiver<()>> {
    let mut pending_ack = voice_change_ack
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut selected = selected_voice
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if selected.as_str() == voice {
        return None;
    }

    let (sender, receiver) = tokio::sync::oneshot::channel();
    voice_cancel.store(true, Ordering::Release);
    let generation = voice_generation.fetch_add(1, Ordering::AcqRel) + 1;
    if let Some(superseded) = pending_ack.replace(PendingVoiceChange {
        generation,
        acknowledged: sender,
    }) {
        let _ = superseded.acknowledged.send(());
    }
    *selected = voice.to_string();
    Some(receiver)
}

pub(super) fn acknowledge_voice_change(
    voice_change_ack: &VoiceChangeAck,
    voice_cancel: &AtomicBool,
) {
    let mut pending_ack = voice_change_ack
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if voice_cancel.load(Ordering::Acquire) {
        return;
    }
    if let Some(pending) = pending_ack.take() {
        let _ = pending.acknowledged.send(());
    }
}

pub(super) fn finish_voice_change_ack(voice_change_ack: &VoiceChangeAck) {
    if let Some(pending) = voice_change_ack
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
    {
        let _ = pending.acknowledged.send(());
    }
}

pub(super) fn reconcile_selected_voice(
    model_dir: &Path,
    selected_voice: &Mutex<String>,
    voice_name: &mut String,
    style: &mut VoiceStyle,
) -> bool {
    let requested_voice = selected_voice
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    if requested_voice == *voice_name {
        return true;
    }

    let requested_path = model_dir.join(format!("{requested_voice}.{VOICE_FILE_EXT}"));
    match load_voice_style(&requested_path) {
        Ok(requested_style) => {
            *style = requested_style;
            *voice_name = requested_voice;
            true
        }
        Err(error) => {
            eprintln!(
                "buzz-desktop: Pocket voice {requested_voice} is unavailable ({error}); falling back to Mary"
            );
            let fallback_path = model_dir.join(format!("{DEFAULT_VOICE}.{VOICE_FILE_EXT}"));
            match load_voice_style(&fallback_path) {
                Ok(fallback_style) => {
                    *style = fallback_style;
                    *voice_name = DEFAULT_VOICE.to_string();
                    *selected_voice
                        .lock()
                        .unwrap_or_else(|lock_error| lock_error.into_inner()) =
                        DEFAULT_VOICE.to_string();
                    true
                }
                Err(fallback_error) => {
                    eprintln!("buzz-desktop: Mary voice fallback is unavailable: {fallback_error}");
                    false
                }
            }
        }
    }
}

pub(super) fn retain_cancelled_text(
    deferred_text: &mut VecDeque<QueuedText>,
    current_text: &mut Option<QueuedText>,
    text_rx: &mpsc::Receiver<QueuedText>,
    preserve_generation: Option<u64>,
) {
    if let Some(generation) = preserve_generation {
        deferred_text.retain(|text| text.generation >= generation);
        if let Some(text) = current_text.take() {
            if text.generation >= generation {
                deferred_text.push_front(text);
            }
        }
        while let Ok(text) = text_rx.try_recv() {
            if text.generation >= generation {
                deferred_text.push_back(text);
            }
        }
    } else {
        deferred_text.clear();
        current_text.take();
        while text_rx.try_recv().is_ok() {}
    }
}
