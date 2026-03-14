//! PipeWire camera stream — creates a Video/Source node and pushes NV12 frames.

use std::ffi::{c_char, c_int, c_void, CString};
use std::ptr;
use std::sync::{Arc, Mutex};

use crate::ffi;
use crate::spa_pod::{self, PodBuilder};

const CAM_WIDTH: u32 = 640;
const CAM_HEIGHT: u32 = 480;
const FRAME_SIZE: u32 = CAM_WIDTH * CAM_HEIGHT * 3 / 2; // NV12

/// Shared state between camera capture and PipeWire stream.
struct StreamData {
    /// Latest NV12 frame from camera (shared with camera callback).
    frame: Arc<Mutex<Option<Vec<u8>>>>,
    /// The pw_stream pointer (set after creation).
    stream: *mut ffi::pw_stream,
    /// The thread loop (for signaling).
    thread_loop: *mut ffi::pw_thread_loop,
}

unsafe impl Send for StreamData {}
unsafe impl Sync for StreamData {}

/// PipeWire camera source handle.
pub struct PipeWireCamera {
    thread_loop: *mut ffi::pw_thread_loop,
    context: *mut ffi::pw_context,
    core: *mut ffi::pw_core,
    stream: *mut ffi::pw_stream,
    /// Shared frame buffer — camera writes here, PipeWire reads.
    frame: Arc<Mutex<Option<Vec<u8>>>>,
    /// Boxed stream data (must outlive the stream).
    _data: Box<StreamData>,
    /// Listener hook (must outlive the stream).
    _listener: Box<ffi::spa_hook>,
}

unsafe impl Send for PipeWireCamera {}

impl PipeWireCamera {
    /// Create and start a PipeWire camera source.
    ///
    /// `socket_path` is the PipeWire daemon's Unix socket (e.g. "{ARCH_FS_ROOT}/tmp/pipewire-0").
    /// `module_dir` is where libpipewire-module-*.so files are (the app's native lib dir).
    /// `data_dir` is a writable directory for creating SPA plugin symlink structure.
    ///
    /// Returns the shared frame buffer that the camera capture should write NV12 frames into.
    pub fn start(
        socket_path: &str,
        module_dir: &str,
        data_dir: &str,
    ) -> Option<(Self, Arc<Mutex<Option<Vec<u8>>>>)> {
        // Set env vars for PipeWire module/plugin discovery
        // SAFETY: called once at startup before other threads use these vars

        // Create SPA plugin directory structure — PipeWire expects
        // $SPA_PLUGIN_DIR/support/libspa-support.so (subdirectory by category)
        let spa_dir = format!("{}/pw-spa-plugins", data_dir);
        let support_dir = format!("{}/support", spa_dir);
        let _ = std::fs::create_dir_all(&support_dir);
        let spa_lib = format!("{}/libspa-support.so", module_dir);
        let spa_link = format!("{}/libspa-support.so", support_dir);
        // Always recreate — symlink target changes on app reinstall
        let _ = std::fs::remove_file(&spa_link);
        let _ = std::os::unix::fs::symlink(&spa_lib, &spa_link);

        // Create minimal PipeWire client config
        let config_dir = format!("{}/pw-config", data_dir);
        let _ = std::fs::create_dir_all(&config_dir);
        let config_file = format!("{}/client.conf", config_dir);
        let _ = std::fs::write(&config_file, concat!(
            "context.properties = {}\n",
            "context.spa-libs = {\n",
            "    support.* = support/libspa-support\n",
            "}\n",
            "context.modules = [\n",
            "    { name = libpipewire-module-protocol-native }\n",
            "    { name = libpipewire-module-client-node }\n",
            "]\n",
        ));

        unsafe {
            std::env::set_var("PIPEWIRE_MODULE_DIR", module_dir);
            std::env::set_var("SPA_PLUGIN_DIR", &spa_dir);
            std::env::set_var("PIPEWIRE_REMOTE", socket_path);
            std::env::set_var("PIPEWIRE_CONFIG_DIR", &config_dir);
            std::env::set_var("PIPEWIRE_CONFIG_NAME", "client.conf");
            std::env::set_var("PIPEWIRE_DEBUG", "3");
        }

        let frame = Arc::new(Mutex::new(None::<Vec<u8>>));

        unsafe {
            log::info!("[pw-cam] pw_init (module_dir={module_dir}, spa_dir={spa_dir}, remote={socket_path})");
            ffi::pw_init(ptr::null_mut(), ptr::null_mut());

            let loop_name = CString::new("pw-cam").ok()?;
            let thread_loop = ffi::pw_thread_loop_new(loop_name.as_ptr(), ptr::null());
            if thread_loop.is_null() {
                log::error!("[pw-cam] Failed to create thread loop");
                return None;
            }
            log::info!("[pw-cam] Thread loop created");

            let pw_loop = ffi::pw_thread_loop_get_loop(thread_loop);

            let null: *const c_char = ptr::null();
            let context = ffi::pw_context_new(pw_loop, ptr::null_mut(), 0);
            if context.is_null() {
                log::error!("[pw-cam] Failed to create context");
                ffi::pw_thread_loop_destroy(thread_loop);
                return None;
            }
            log::info!("[pw-cam] Context created");

            ffi::pw_thread_loop_lock(thread_loop);

            if ffi::pw_thread_loop_start(thread_loop) < 0 {
                log::error!("[pw-cam] Failed to start thread loop");
                ffi::pw_thread_loop_unlock(thread_loop);
                ffi::pw_context_destroy(context);
                ffi::pw_thread_loop_destroy(thread_loop);
                return None;
            }
            log::info!("[pw-cam] Thread loop started");

            // Pass remote.name in the connect properties
            let remote_key = CString::new("remote.name").ok()?;
            let remote_val = CString::new(socket_path).ok()?;
            let connect_props = ffi::pw_properties_new(
                remote_key.as_ptr(),
                remote_val.as_ptr(),
                null,
            );

            let core = ffi::pw_context_connect(context, connect_props, 0);
            if core.is_null() {
                log::error!("[pw-cam] Failed to connect to PipeWire daemon at {socket_path}");
                ffi::pw_thread_loop_unlock(thread_loop);
                ffi::pw_thread_loop_stop(thread_loop);
                ffi::pw_context_destroy(context);
                ffi::pw_thread_loop_destroy(thread_loop);
                return None;
            }
            log::info!("[pw-cam] Connected to PipeWire daemon");

            // Create stream with Video/Source properties
            let media_class_key = CString::new("media.class").ok()?;
            let media_class_val = CString::new("Video/Source").ok()?;
            let node_name_key = CString::new("node.name").ok()?;
            let node_name_val = CString::new("android-camera").ok()?;
            let node_desc_key = CString::new("node.description").ok()?;
            let node_desc_val = CString::new("Android Camera").ok()?;

            let stream_props = ffi::pw_properties_new(
                media_class_key.as_ptr(),
                media_class_val.as_ptr(),
                node_name_key.as_ptr(),
                node_name_val.as_ptr(),
                node_desc_key.as_ptr(),
                node_desc_val.as_ptr(),
                null,
            );

            let stream_name = CString::new("android-camera").ok()?;
            let stream = ffi::pw_stream_new(core, stream_name.as_ptr(), stream_props);
            if stream.is_null() {
                log::error!("[pw-cam] Failed to create stream");
                ffi::pw_thread_loop_unlock(thread_loop);
                ffi::pw_thread_loop_stop(thread_loop);
                ffi::pw_context_destroy(context);
                ffi::pw_thread_loop_destroy(thread_loop);
                return None;
            }

            // Create stream data
            let mut data = Box::new(StreamData {
                frame: Arc::clone(&frame),
                stream,
                thread_loop,
            });

            // Set up event listener
            let events = ffi::pw_stream_events {
                version: ffi::PW_VERSION_STREAM_EVENTS,
                destroy: None,
                state_changed: Some(on_state_changed),
                control_info: None,
                io_changed: None,
                param_changed: Some(on_param_changed),
                add_buffer: None,
                remove_buffer: None,
                process: Some(on_process),
                drained: None,
                command: None,
                trigger_done: None,
            };

            let mut listener = Box::new(std::mem::zeroed::<ffi::spa_hook>());

            ffi::pw_stream_add_listener(
                stream,
                &mut *listener,
                &events,
                &mut *data as *mut StreamData as *mut c_void,
            );

            // Build format params
            let format_pod = PodBuilder::build_video_enum_format(
                CAM_WIDTH,
                CAM_HEIGHT,
                30,
                1,
            );
            let format_ptr = format_pod.as_ptr() as *const ffi::spa_pod;
            let mut params = [format_ptr];

            // Connect as output (source)
            let res = ffi::pw_stream_connect(
                stream,
                ffi::PW_DIRECTION_OUTPUT,
                ffi::PW_ID_ANY,
                ffi::PW_STREAM_FLAG_MAP_BUFFERS | ffi::PW_STREAM_FLAG_DRIVER,
                params.as_mut_ptr(),
                1,
            );
            if res < 0 {
                log::error!("[pw-cam] Failed to connect stream: {res}");
                ffi::pw_stream_destroy(stream);
                ffi::pw_thread_loop_unlock(thread_loop);
                ffi::pw_thread_loop_stop(thread_loop);
                ffi::pw_context_destroy(context);
                ffi::pw_thread_loop_destroy(thread_loop);
                return None;
            }

            ffi::pw_thread_loop_unlock(thread_loop);

            log::info!("[pw-cam] PipeWire camera stream started");

            let frame_clone = Arc::clone(&frame);

            Some((
                Self {
                    thread_loop,
                    context,
                    core,
                    stream,
                    frame,
                    _data: data,
                    _listener: listener,
                },
                frame_clone,
            ))
        }
    }

    /// Push a new NV12 frame. Called from the camera capture callback.
    pub fn push_frame(&self, nv12_data: &[u8]) {
        if let Ok(mut frame) = self.frame.lock() {
            *frame = Some(nv12_data.to_vec());
        }
        // Wake the PipeWire thread to process the new frame
        unsafe {
            ffi::pw_thread_loop_lock(self.thread_loop);
            ffi::pw_stream_trigger_process(self.stream);
            ffi::pw_thread_loop_unlock(self.thread_loop);
        }
    }
}

impl Drop for PipeWireCamera {
    fn drop(&mut self) {
        unsafe {
            ffi::pw_thread_loop_lock(self.thread_loop);
            ffi::pw_stream_disconnect(self.stream);
            ffi::pw_stream_destroy(self.stream);
            ffi::pw_thread_loop_unlock(self.thread_loop);
            ffi::pw_thread_loop_stop(self.thread_loop);
            ffi::pw_context_destroy(self.context);
            ffi::pw_thread_loop_destroy(self.thread_loop);
            ffi::pw_deinit();
        }
    }
}

// --- Stream event callbacks ---

unsafe extern "C" fn on_state_changed(
    _data: *mut c_void,
    old: c_int,
    state: c_int,
    error: *const c_char,
) {
    let err_str = if error.is_null() {
        String::new()
    } else {
        unsafe { std::ffi::CStr::from_ptr(error) }.to_string_lossy().into_owned()
    };
    log::info!("[pw-cam] State: {old} -> {state} {err_str}");
}

unsafe extern "C" fn on_param_changed(
    data: *mut c_void,
    id: u32,
    _param: *const ffi::spa_pod,
) {
    if id != spa_pod::SPA_FORMAT_VIDEO_format && id != ffi::SPA_PARAM_EnumFormat {
        return;
    }

    unsafe {
        let stream_data = &*(data as *const StreamData);

        let buffers = PodBuilder::build_buffers(FRAME_SIZE, 2, 8);
        let buffers_ptr = buffers.as_ptr() as *const ffi::spa_pod;
        let mut params = [buffers_ptr];

        ffi::pw_stream_update_params(
            stream_data.stream,
            params.as_mut_ptr(),
            1,
        );
    }
}

unsafe extern "C" fn on_process(data: *mut c_void) {
    unsafe {
        let stream_data = &*(data as *const StreamData);

        let buf = ffi::pw_stream_dequeue_buffer(stream_data.stream);
        if buf.is_null() {
            return;
        }

        let pw_buf = &*buf;
        let spa_buf = &*pw_buf.buffer;

        if spa_buf.n_datas == 0 {
            ffi::pw_stream_queue_buffer(stream_data.stream, buf);
            return;
        }

        let d = &mut *spa_buf.datas;
        if d.data.is_null() || d.maxsize == 0 {
            ffi::pw_stream_queue_buffer(stream_data.stream, buf);
            return;
        }

        // Copy latest frame into the PipeWire buffer
        let frame_guard = stream_data.frame.lock().ok();
        let written = if let Some(ref guard) = frame_guard {
            if let Some(ref nv12) = **guard {
                let copy_len = nv12.len().min(d.maxsize as usize);
                std::ptr::copy_nonoverlapping(nv12.as_ptr(), d.data as *mut u8, copy_len);
                copy_len as u32
            } else { FRAME_SIZE }
        } else { FRAME_SIZE };

        let chunk = &mut *d.chunk;
        chunk.offset = 0;
        chunk.size = written;
        chunk.stride = CAM_WIDTH as i32;

        ffi::pw_stream_queue_buffer(stream_data.stream, buf);
    }
}
