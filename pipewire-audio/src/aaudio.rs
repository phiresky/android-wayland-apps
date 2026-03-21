//! Minimal FFI bindings to Android AAudio API.

#![allow(non_camel_case_types, dead_code)]

/// Opaque AAudio stream builder handle.
#[repr(C)]
pub struct AAudioStreamBuilder {
    _opaque: [u8; 0],
}

/// Opaque AAudio stream handle.
#[repr(C)]
pub struct AAudioStream {
    _opaque: [u8; 0],
}

pub type AAudioStream_dataCallback = unsafe extern "C" fn(
    stream: *mut AAudioStream,
    user_data: *mut libc::c_void,
    audio_data: *mut libc::c_void,
    num_frames: i32,
) -> i32;

// Direction
pub const AAUDIO_DIRECTION_OUTPUT: i32 = 0;

// Format
pub const AAUDIO_FORMAT_PCM_FLOAT: i32 = 2;

// Sharing mode
pub const AAUDIO_SHARING_MODE_SHARED: i32 = 0;

// Performance mode
pub const AAUDIO_PERFORMANCE_MODE_LOW_LATENCY: i32 = 12;

// Callback result
pub const AAUDIO_CALLBACK_RESULT_CONTINUE: i32 = 0;

// Result codes
pub const AAUDIO_OK: i32 = 0;

unsafe extern "C" {
    pub fn AAudio_createStreamBuilder(builder: *mut *mut AAudioStreamBuilder) -> i32;
    pub fn AAudioStreamBuilder_setDirection(builder: *mut AAudioStreamBuilder, direction: i32);
    pub fn AAudioStreamBuilder_setSharingMode(builder: *mut AAudioStreamBuilder, mode: i32);
    pub fn AAudioStreamBuilder_setSampleRate(builder: *mut AAudioStreamBuilder, sample_rate: i32);
    pub fn AAudioStreamBuilder_setChannelCount(builder: *mut AAudioStreamBuilder, count: i32);
    pub fn AAudioStreamBuilder_setFormat(builder: *mut AAudioStreamBuilder, format: i32);
    pub fn AAudioStreamBuilder_setPerformanceMode(builder: *mut AAudioStreamBuilder, mode: i32);
    pub fn AAudioStreamBuilder_setDataCallback(
        builder: *mut AAudioStreamBuilder,
        callback: AAudioStream_dataCallback,
        user_data: *mut libc::c_void,
    );
    pub fn AAudioStreamBuilder_openStream(
        builder: *mut AAudioStreamBuilder,
        stream: *mut *mut AAudioStream,
    ) -> i32;
    pub fn AAudioStreamBuilder_delete(builder: *mut AAudioStreamBuilder) -> i32;
    pub fn AAudioStream_requestStart(stream: *mut AAudioStream) -> i32;
    pub fn AAudioStream_requestStop(stream: *mut AAudioStream) -> i32;
    pub fn AAudioStream_close(stream: *mut AAudioStream) -> i32;
}

#[link(name = "aaudio")]
unsafe extern "C" {}

/// Check an AAudio result code, returning Err with the operation name on failure.
pub fn check(result: i32, op: &str) -> Result<(), String> {
    if result >= AAUDIO_OK {
        Ok(())
    } else {
        Err(format!("AAudio {op} failed: error {result}"))
    }
}
