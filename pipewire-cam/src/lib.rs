//! PipeWire camera source — presents Android camera frames as a PipeWire Video/Source node.
//!
//! Uses raw FFI bindings to libpipewire-0.3 (cross-compiled for Android/bionic).
//! Connects to the PipeWire daemon running inside proot via its Unix socket.

mod ffi;
mod spa_pod;
mod stream;

pub use stream::PipeWireCamera;
