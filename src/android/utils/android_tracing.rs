use std::collections::VecDeque;
use std::ffi::CString;
use std::fmt::Write;
use std::sync::Mutex;
use std::time::Instant;
use tracing::field::{Field, Visit};
use tracing::Subscriber;
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;
use tracing_subscriber::Layer;

const DEBUG_LOG_MAX_LINES: usize = 500;

static DEBUG_LOG: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
static START_TIME: Mutex<Option<Instant>> = Mutex::new(None);

/// Returns the contents of the debug log buffer as a single string.
pub fn get_debug_log() -> String {
    let buf = DEBUG_LOG.lock().unwrap_or_else(|e| e.into_inner());
    let mut out = String::new();
    for line in buf.iter() {
        out.push_str(line);
        out.push('\n');
    }
    out
}

use ndk_sys::android_LogPriority;

const ANDROID_LOG_DEBUG: u32 = android_LogPriority::ANDROID_LOG_DEBUG.0;
const ANDROID_LOG_INFO: u32 = android_LogPriority::ANDROID_LOG_INFO.0;
const ANDROID_LOG_WARN: u32 = android_LogPriority::ANDROID_LOG_WARN.0;
const ANDROID_LOG_ERROR: u32 = android_LogPriority::ANDROID_LOG_ERROR.0;

#[derive(Debug)]
struct AndroidLogLayer;

impl<S: Subscriber> Layer<S> for AndroidLogLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let prio = match *metadata.level() {
            tracing::Level::ERROR => ANDROID_LOG_ERROR,
            tracing::Level::WARN => ANDROID_LOG_WARN,
            tracing::Level::INFO => ANDROID_LOG_INFO,
            _ => ANDROID_LOG_DEBUG,
        };

        let mut visitor = MessageVisitor(String::new());
        event.record(&mut visitor);

        let tag_str = metadata.target();
        let tag = CString::new(tag_str).unwrap_or_default();
        let msg = CString::new(visitor.0.as_str()).unwrap_or_default();
        unsafe {
            ndk_sys::__android_log_print(prio as i32, tag.as_ptr(), msg.as_ptr());
        }

        // Also write to the in-memory debug log buffer.
        let elapsed = {
            let mut guard = START_TIME.lock().unwrap_or_else(|e| e.into_inner());
            let start = guard.get_or_insert_with(Instant::now);
            start.elapsed()
        };
        let secs = elapsed.as_secs_f64();
        let level_char = match *metadata.level() {
            tracing::Level::ERROR => 'E',
            tracing::Level::WARN => 'W',
            tracing::Level::INFO => 'I',
            tracing::Level::DEBUG => 'D',
            tracing::Level::TRACE => 'T',
        };
        let line = format!("{secs:8.3} {level_char} {tag_str}: {}", visitor.0);
        if let Ok(mut buf) = DEBUG_LOG.lock() {
            if buf.len() >= DEBUG_LOG_MAX_LINES {
                buf.pop_front();
            }
            buf.push_back(line);
        }
    }
}

struct MessageVisitor(String);

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.0, "{:?}", value);
        } else {
            if !self.0.is_empty() {
                self.0.push(' ');
            }
            let _ = write!(self.0, "{}={:?}", field.name(), value);
        }
    }
}

pub fn init() {
    let subscriber = tracing_subscriber::registry()
        .with(AndroidLogLayer)
        .with(tracing_android_trace::AndroidTraceLayer::new());
    tracing::subscriber::set_global_default(subscriber).ok();
}
