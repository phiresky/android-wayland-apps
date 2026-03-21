//! PipeWire audio sink -> Android AAudio bridge.

use crate::android::utils::application_context::get_application_context;
use crate::core::config;
use pipewire_audio::PipeWireAudioSink;
use std::sync::{Mutex, OnceLock};

static PW_AUDIO: OnceLock<Mutex<Option<PipeWireAudioSink>>> = OnceLock::new();

/// Start the PipeWire audio sink on a background thread.
/// Safe to call multiple times — only the first call takes effect.
pub fn start() {
    if PW_AUDIO.set(Mutex::new(None)).is_err() {
        tracing::warn!("[audio] Already started");
        return;
    }
    let _ = std::thread::Builder::new()
        .name("audio-connect".into())
        .spawn(connect_pipewire_audio);
}

fn connect_pipewire_audio() {
    let ctx = get_application_context();
    let native_lib_dir = ctx.native_library_dir.to_string_lossy().to_string();
    let data_dir = ctx.data_dir.to_string_lossy().to_string();
    let pw_socket = format!("{}/tmp/pipewire-0", config::ARCH_FS_ROOT);

    tracing::info!("[audio] Connecting to PipeWire audio at {pw_socket}...");
    match PipeWireAudioSink::start(&pw_socket, &native_lib_dir, &data_dir) {
        Some(sink) => {
            tracing::info!("[audio] PipeWire audio sink started");
            if let Some(pw) = PW_AUDIO.get() {
                if let Ok(mut guard) = pw.lock() {
                    *guard = Some(sink);
                }
            }
        }
        None => {
            tracing::error!("[audio] Failed to start PipeWire audio sink");
        }
    }
}
