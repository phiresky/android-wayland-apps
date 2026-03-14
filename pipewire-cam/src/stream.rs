//! PipeWire camera stream — creates a Video/Source node and pushes NV12 frames.

use pipewire as pw;
use pw::spa;
use pw::spa::pod::Pod;
use std::sync::{Arc, Mutex};

const CAM_WIDTH: u32 = 640;
const CAM_HEIGHT: u32 = 480;
const FRAME_SIZE: usize = (CAM_WIDTH * CAM_HEIGHT * 3 / 2) as usize; // NV12

/// Shared frame buffer: camera capture writes, PipeWire process reads.
type FrameBuffer = Arc<Mutex<Option<Vec<u8>>>>;

/// PipeWire camera source handle.
pub struct PipeWireCamera {
    frame: FrameBuffer,
    // Keep the thread handle alive so the PipeWire loop keeps running
    _thread: std::thread::JoinHandle<()>,
}

unsafe impl Send for PipeWireCamera {}

impl PipeWireCamera {
    /// Create and start a PipeWire camera source.
    ///
    /// `socket_path` — PipeWire daemon Unix socket (e.g. "{ARCH_FS_ROOT}/tmp/pipewire-0")
    /// `module_dir` — where libpipewire-module-*.so are (app's native lib dir)
    /// `data_dir` — writable dir for SPA plugin symlinks and config
    pub fn start(
        socket_path: &str,
        module_dir: &str,
        data_dir: &str,
    ) -> Option<Self> {
        // Set up SPA plugin directory structure
        let spa_dir = format!("{}/pw-spa-plugins", data_dir);
        let support_dir = format!("{}/support", spa_dir);
        let _ = std::fs::create_dir_all(&support_dir);
        let spa_lib = format!("{}/libspa-support.so", module_dir);
        let spa_link = format!("{}/libspa-support.so", support_dir);
        let _ = std::fs::remove_file(&spa_link);
        let _ = std::os::unix::fs::symlink(&spa_lib, &spa_link);

        // Minimal PipeWire client config
        let config_dir = format!("{}/pw-config", data_dir);
        let _ = std::fs::create_dir_all(&config_dir);
        let config_file = format!("{}/client.conf", config_dir);
        let _ = std::fs::write(
            &config_file,
            "context.properties = {}\n\
             context.spa-libs = {\n\
                 support.* = support/libspa-support\n\
             }\n\
             context.modules = [\n\
                 { name = libpipewire-module-protocol-native }\n\
                 { name = libpipewire-module-client-node }\n\
             ]\n",
        );

        // SAFETY: called once at startup before other threads use these vars
        unsafe {
            std::env::set_var("PIPEWIRE_MODULE_DIR", module_dir);
            std::env::set_var("SPA_PLUGIN_DIR", &spa_dir);
            std::env::set_var("PIPEWIRE_REMOTE", socket_path);
            std::env::set_var("PIPEWIRE_CONFIG_DIR", &config_dir);
            std::env::set_var("PIPEWIRE_CONFIG_NAME", "client.conf");
        }

        let frame: FrameBuffer = Arc::new(Mutex::new(None));
        let frame_clone = Arc::clone(&frame);
        let socket = socket_path.to_string();

        let thread = std::thread::Builder::new()
            .name("pw-cam-loop".into())
            .spawn(move || {
                if let Err(e) = run_pipewire_loop(frame_clone, &socket) {
                    log::error!("[pw-cam] PipeWire loop error: {e}");
                }
            })
            .ok()?;

        Some(Self {
            frame,
            _thread: thread,
        })
    }

    /// Push a new NV12 frame. Called from the camera capture callback.
    pub fn push_frame(&self, nv12_data: &[u8]) {
        if let Ok(mut frame) = self.frame.lock() {
            *frame = Some(nv12_data.to_vec());
        }
    }
}

fn run_pipewire_loop(frame: FrameBuffer, _socket: &str) -> Result<(), pw::Error> {
    pw::init();

    log::info!("[pw-cam] Creating PipeWire main loop...");
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;

    // remote.name is read from PIPEWIRE_REMOTE env var
    let core = context.connect_rc(None)?;
    log::info!("[pw-cam] Connected to PipeWire daemon");

    let stream = pw::stream::StreamBox::new(
        &core,
        "android-camera",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_CLASS => "Video/Source",
            *pw::keys::NODE_NAME => "android-camera",
            *pw::keys::NODE_DESCRIPTION => "Android Camera",
        },
    )?;

    let _listener = stream
        .add_local_listener_with_user_data(frame)
        .state_changed(|_, _, old, new| {
            log::info!("[pw-cam] State: {old:?} -> {new:?}");
        })
        .process(|stream, frame_buf| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }
            let data = &mut datas[0];
            let Some(slice) = data.data() else {
                return;
            };

            // Copy latest camera frame into PipeWire buffer
            let written = if let Ok(guard) = frame_buf.lock() {
                if let Some(ref nv12) = *guard {
                    let copy_len = nv12.len().min(slice.len());
                    slice[..copy_len].copy_from_slice(&nv12[..copy_len]);
                    copy_len
                } else {
                    // No frame yet — zero fill
                    let fill = FRAME_SIZE.min(slice.len());
                    slice[..fill].fill(0);
                    fill
                }
            } else {
                0
            };

            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.size_mut() = written as _;
            *chunk.stride_mut() = CAM_WIDTH as _;
        })
        .register()?;

    // Build video format params: NV12 640x480 @ 30fps
    let obj = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaType,
            Id,
            spa::param::format::MediaType::Video
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaSubtype,
            Id,
            spa::param::format::MediaSubtype::Raw
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFormat,
            Id,
            spa::param::video::VideoFormat::NV12
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoSize,
            Rectangle,
            spa::utils::Rectangle {
                width: CAM_WIDTH,
                height: CAM_HEIGHT,
            }
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFramerate,
            Fraction,
            spa::utils::Fraction { num: 30, denom: 1 }
        ),
    );
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|_| pw::Error::CreationFailed)?
    .0
    .into_inner();

    let mut params = [Pod::from_bytes(&values).ok_or(pw::Error::CreationFailed)?];

    stream.connect(
        spa::utils::Direction::Output,
        None,
        pw::stream::StreamFlags::MAP_BUFFERS | pw::stream::StreamFlags::DRIVER,
        &mut params,
    )?;

    log::info!("[pw-cam] Stream connected, entering main loop");
    mainloop.run();

    Ok(())
}
