use std::ffi::{CString, c_char, c_int};
use std::fmt::Write;
use tracing::field::{Field, Visit};
use tracing::{span, Event, Metadata, Subscriber};

#[link(name = "log")]
unsafe extern "C" {
    fn __android_log_print(prio: c_int, tag: *const c_char, fmt: *const c_char, ...) -> c_int;
}

const ANDROID_LOG_DEBUG: c_int = 3;
const ANDROID_LOG_INFO: c_int = 4;
const ANDROID_LOG_WARN: c_int = 5;
const ANDROID_LOG_ERROR: c_int = 6;

pub struct AndroidLogSubscriber;

impl AndroidLogSubscriber {
    pub fn init() {
        tracing::subscriber::set_global_default(Self).ok();
    }
}

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

impl Subscriber for AndroidLogSubscriber {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        *metadata.level() <= tracing::Level::INFO
    }

    fn max_level_hint(&self) -> Option<tracing::metadata::LevelFilter> {
        Some(tracing::metadata::LevelFilter::INFO)
    }

    fn new_span(&self, _attrs: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }

    fn record(&self, _span: &span::Id, _values: &span::Record<'_>) {}
    fn record_follows_from(&self, _span: &span::Id, _follows: &span::Id) {}
    fn enter(&self, _span: &span::Id) {}
    fn exit(&self, _span: &span::Id) {}

    fn event(&self, event: &Event<'_>) {
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
