//! PipeWire audio sink — receives audio from PipeWire and bridges to Android AAudio.

mod aaudio;
mod ring_buffer;
mod stream;

pub use stream::PipeWireAudioSink;
