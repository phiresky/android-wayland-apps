//! Raw FFI bindings for libpipewire-0.3 — only the functions we need.

#![allow(non_camel_case_types, dead_code)]

use std::ffi::{c_char, c_int, c_uint, c_void};

// Opaque types
pub enum pw_main_loop {}
pub enum pw_loop {}
pub enum pw_context {}
pub enum pw_core {}
pub enum pw_stream {}
pub enum pw_properties {}
pub enum pw_thread_loop {}

// Stream direction
pub const PW_DIRECTION_OUTPUT: c_uint = 1;

// Stream flags
pub const PW_STREAM_FLAG_MAP_BUFFERS: u32 = 1 << 2;
pub const PW_STREAM_FLAG_DRIVER: u32 = 1 << 3;

// IDs
pub const PW_ID_ANY: u32 = 0xffffffff;

// Stream states
pub const PW_STREAM_STATE_ERROR: c_int = -1;
pub const PW_STREAM_STATE_UNCONNECTED: c_int = 0;
pub const PW_STREAM_STATE_CONNECTING: c_int = 1;
pub const PW_STREAM_STATE_PAUSED: c_int = 2;
pub const PW_STREAM_STATE_STREAMING: c_int = 3;

// SPA constants
pub const SPA_PARAM_EnumFormat: u32 = 3;
pub const SPA_PARAM_Buffers: u32 = 5;
pub const SPA_PARAM_Meta: u32 = 6;

// Property keys
pub const PW_KEY_MEDIA_TYPE: *const c_char = b"media.type\0".as_ptr() as *const c_char;
pub const PW_KEY_MEDIA_CATEGORY: *const c_char = b"media.category\0".as_ptr() as *const c_char;
pub const PW_KEY_MEDIA_ROLE: *const c_char = b"media.role\0".as_ptr() as *const c_char;
pub const PW_KEY_MEDIA_CLASS: *const c_char = b"media.class\0".as_ptr() as *const c_char;
pub const PW_KEY_NODE_NAME: *const c_char = b"node.name\0".as_ptr() as *const c_char;
pub const PW_KEY_NODE_DESCRIPTION: *const c_char = b"node.description\0".as_ptr() as *const c_char;
pub const PW_KEY_REMOTE_NAME: *const c_char = b"remote.name\0".as_ptr() as *const c_char;

// SPA pod
#[repr(C)]
pub struct spa_pod {
    pub size: u32,
    pub type_: u32,
}

// Buffer structures
#[repr(C)]
pub struct spa_data {
    pub type_: u32,
    pub flags: u32,
    pub fd: i64,
    pub mapoffset: u32,
    pub maxsize: u32,
    pub data: *mut c_void,
    pub chunk: *mut spa_chunk,
}

#[repr(C)]
pub struct spa_chunk {
    pub offset: u32,
    pub size: u32,
    pub stride: i32,
    pub flags: i32,
}

#[repr(C)]
pub struct spa_buffer {
    pub n_metas: u32,
    pub n_datas: u32,
    pub metas: *mut c_void,
    pub datas: *mut spa_data,
}

#[repr(C)]
pub struct pw_buffer {
    pub buffer: *mut spa_buffer,
    pub user_data: *mut c_void,
    pub size: u64,
    pub requested: u64,
}

// Stream events
#[repr(C)]
pub struct pw_stream_events {
    pub version: u32,
    pub destroy: Option<unsafe extern "C" fn(data: *mut c_void)>,
    pub state_changed: Option<
        unsafe extern "C" fn(
            data: *mut c_void,
            old: c_int,
            state: c_int,
            error: *const c_char,
        ),
    >,
    pub control_info: Option<unsafe extern "C" fn(data: *mut c_void, id: u32, control: *const c_void)>,
    pub io_changed: Option<unsafe extern "C" fn(data: *mut c_void, id: u32, area: *mut c_void, size: u32)>,
    pub param_changed: Option<
        unsafe extern "C" fn(data: *mut c_void, id: u32, param: *const spa_pod),
    >,
    pub add_buffer: Option<unsafe extern "C" fn(data: *mut c_void, buffer: *mut pw_buffer)>,
    pub remove_buffer: Option<unsafe extern "C" fn(data: *mut c_void, buffer: *mut pw_buffer)>,
    pub process: Option<unsafe extern "C" fn(data: *mut c_void)>,
    pub drained: Option<unsafe extern "C" fn(data: *mut c_void)>,
    pub command: Option<unsafe extern "C" fn(data: *mut c_void, command: *const c_void)>,
    pub trigger_done: Option<unsafe extern "C" fn(data: *mut c_void)>,
}

pub const PW_VERSION_STREAM_EVENTS: u32 = 2;

// Hook / listener
#[repr(C)]
pub struct spa_callbacks {
    pub funcs: *const c_void,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct spa_hook {
    pub link: spa_list,
    pub cb: spa_callbacks,
    pub removed: Option<unsafe extern "C" fn(hook: *mut spa_hook)>,
    pub priv_: *mut c_void,
}

#[repr(C)]
pub struct spa_list {
    pub next: *mut spa_list,
    pub prev: *mut spa_list,
}

unsafe impl Send for spa_hook {}
unsafe impl Sync for spa_hook {}

unsafe extern "C" {
    // Init
    pub fn pw_init(argc: *mut c_int, argv: *mut *mut *mut c_char);
    pub fn pw_deinit();

    // Thread loop
    pub fn pw_thread_loop_new(name: *const c_char, props: *const c_void) -> *mut pw_thread_loop;
    pub fn pw_thread_loop_destroy(loop_: *mut pw_thread_loop);
    pub fn pw_thread_loop_start(loop_: *mut pw_thread_loop) -> c_int;
    pub fn pw_thread_loop_stop(loop_: *mut pw_thread_loop);
    pub fn pw_thread_loop_get_loop(loop_: *mut pw_thread_loop) -> *mut pw_loop;
    pub fn pw_thread_loop_lock(loop_: *mut pw_thread_loop);
    pub fn pw_thread_loop_unlock(loop_: *mut pw_thread_loop);
    pub fn pw_thread_loop_signal(loop_: *mut pw_thread_loop, wait_for_accept: bool);

    // Context
    pub fn pw_context_new(
        loop_: *mut pw_loop,
        props: *mut pw_properties,
        user_data_size: usize,
    ) -> *mut pw_context;
    pub fn pw_context_destroy(context: *mut pw_context);
    pub fn pw_context_connect(
        context: *mut pw_context,
        props: *mut pw_properties,
        user_data_size: usize,
    ) -> *mut pw_core;

    // Properties
    pub fn pw_properties_new(
        key: *const c_char,
        value: *const c_char,
        ...
    ) -> *mut pw_properties;

    // Stream
    pub fn pw_stream_new(
        core: *mut pw_core,
        name: *const c_char,
        props: *mut pw_properties,
    ) -> *mut pw_stream;
    pub fn pw_stream_destroy(stream: *mut pw_stream);
    pub fn pw_stream_add_listener(
        stream: *mut pw_stream,
        listener: *mut spa_hook,
        events: *const pw_stream_events,
        data: *mut c_void,
    );
    pub fn pw_stream_connect(
        stream: *mut pw_stream,
        direction: c_uint,
        target_id: u32,
        flags: u32,
        params: *mut *const spa_pod,
        n_params: u32,
    ) -> c_int;
    pub fn pw_stream_disconnect(stream: *mut pw_stream) -> c_int;
    pub fn pw_stream_dequeue_buffer(stream: *mut pw_stream) -> *mut pw_buffer;
    pub fn pw_stream_queue_buffer(stream: *mut pw_stream, buffer: *mut pw_buffer) -> c_int;
    pub fn pw_stream_update_params(
        stream: *mut pw_stream,
        params: *mut *const spa_pod,
        n_params: u32,
    ) -> c_int;
    pub fn pw_stream_trigger_process(stream: *mut pw_stream);
    pub fn pw_stream_set_active(stream: *mut pw_stream, active: bool) -> c_int;
}
