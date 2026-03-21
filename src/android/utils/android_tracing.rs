use std::collections::VecDeque;
use std::ffi::{CString, c_char, c_int};
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

#[link(name = "log")]
unsafe extern "C" {
    fn __android_log_print(prio: c_int, tag: *const c_char, fmt: *const c_char, ...) -> c_int;
}

const ANDROID_LOG_DEBUG: c_int = 3;
const ANDROID_LOG_INFO: c_int = 4;
const ANDROID_LOG_WARN: c_int = 5;
const ANDROID_LOG_ERROR: c_int = 6;

#[derive(Debug)]
struct AndroidLogLayer;

struct MessageVisitor(String);

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            write!(self.0, "{value:?}").ok();
        } else {
            write!(self.0, " {field}={value:?}").ok();
        }
    }
}

impl<S: Subscriber> Layer<S> for AndroidLogLayer {
    fn enabled(
        &self,
        metadata: &tracing::Metadata<'_>,
        _ctx: Context<'_, S>,
    ) -> bool {
        *metadata.level() <= tracing::Level::INFO
    }

    fn max_level_hint(&self) -> Option<tracing::metadata::LevelFilter> {
        Some(tracing::metadata::LevelFilter::INFO)
    }

    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let level = metadata.level();
        let prio = if *level == tracing::Level::ERROR {
            ANDROID_LOG_ERROR
        } else if *level == tracing::Level::WARN {
            ANDROID_LOG_WARN
        } else if *level == tracing::Level::INFO {
            ANDROID_LOG_INFO
        } else {
            ANDROID_LOG_DEBUG
        };

        let mut visitor = MessageVisitor(String::new());
        event.record(&mut visitor);

        let Ok(tag) = CString::new(metadata.target()) else { return };
        let Ok(msg) = CString::new(visitor.0) else { return };

        unsafe {
            __android_log_print(prio, tag.as_ptr(), c"%s".as_ptr(), msg.as_ptr());
        }
    }
}

#[derive(Debug)]
struct DebugLogLayer;

impl<S: Subscriber> Layer<S> for DebugLogLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let level = metadata.level();
        let target = metadata.target();

        let mut visitor = MessageVisitor(String::new());
        event.record(&mut visitor);

        let elapsed = {
            let start = START_TIME.lock().unwrap_or_else(|e| e.into_inner());
            start.map_or(0.0, |s| s.elapsed().as_secs_f64())
        };
        let secs = elapsed as u64;
        let millis = ((elapsed - secs as f64) * 1000.0) as u64;
        let line = format!("{:4}.{:03} {level} {target}: {}", secs, millis, visitor.0);
        let mut buf = DEBUG_LOG.lock().unwrap_or_else(|e| e.into_inner());
        if buf.len() >= DEBUG_LOG_MAX_LINES {
            buf.pop_front();
        }
        buf.push_back(line);
    }
}

pub fn init() {
    *START_TIME.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    tracing_subscriber::registry()
        .with(AndroidLogLayer)
        .with(DebugLogLayer)
        .with(tracing_android_trace::AndroidTraceLayer::new())
        .try_init()
        .ok();
}
