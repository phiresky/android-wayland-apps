//! PipeWire Audio/Sink stream — receives audio from PipeWire apps and plays via AAudio.

use crate::aaudio;
use crate::ring_buffer::RingBuffer;
use pipewire as pw;
use pw::spa;
use pw::spa::pod::Pod;
use std::sync::Arc;

const SAMPLE_RATE: u32 = 48000;
const CHANNELS: u32 = 2;
/// Ring buffer size in frames (~85ms at 48kHz).
const RING_BUFFER_FRAMES: usize = 4096;

/// Handle to a running PipeWire audio sink.
/// Dropping this does NOT stop the background thread (it runs until the process exits).
pub struct PipeWireAudioSink {
    _thread: std::thread::JoinHandle<()>,
}

unsafe impl Send for PipeWireAudioSink {}

impl PipeWireAudioSink {
    /// Create and start a PipeWire audio sink connected to the daemon at `socket_path`.
    ///
    /// `socket_path` — PipeWire daemon Unix socket (e.g. "{ARCH_FS_ROOT}/tmp/pipewire-0")
    /// `module_dir` — where libpipewire-module-*.so and libspa-*.so are (app's native lib dir)
    /// `data_dir` — writable dir for SPA plugin symlinks and config
    pub fn start(socket_path: &str, module_dir: &str, data_dir: &str) -> Option<Self> {
        setup_spa_plugins(module_dir, data_dir);
        setup_pipewire_env(module_dir, data_dir, socket_path);

        let socket = socket_path.to_string();
        let thread = std::thread::Builder::new()
            .name("pw-audio-loop".into())
            .spawn(move || {
                if let Err(e) = run_pipewire_audio_loop(&socket) {
                    tracing::error!("[pw-audio] PipeWire loop error: {e}");
                }
            })
            .ok()?;

        Some(Self { _thread: thread })
    }
}

/// Set up SPA plugin symlinks in the data directory.
fn setup_spa_plugins(module_dir: &str, data_dir: &str) {
    let spa_dir = format!("{}/pw-spa-plugins", data_dir);

    let plugins = [
        ("support", "libspa-support.so"),
        ("videoconvert", "libspa-videoconvert.so"),
        ("audioconvert", "libspa-audioconvert.so"),
    ];

    for (subdir, lib_name) in &plugins {
        let dir = format!("{}/{}", spa_dir, subdir);
        let _ = std::fs::create_dir_all(&dir);
        let lib_src = format!("{}/{}", module_dir, lib_name);
        let lib_link = format!("{}/{}", dir, lib_name);
        let _ = std::fs::remove_file(&lib_link);
        let _ = std::os::unix::fs::symlink(&lib_src, &lib_link);
    }
}

/// Set PipeWire environment variables and write client config.
fn setup_pipewire_env(module_dir: &str, data_dir: &str, socket_path: &str) {
    let spa_dir = format!("{}/pw-spa-plugins", data_dir);
    let config_dir = format!("{}/pw-config", data_dir);
    let _ = std::fs::create_dir_all(&config_dir);
    let config_file = format!("{}/client.conf", config_dir);
    let _ = std::fs::write(
        &config_file,
        "context.properties = {}\n\
         context.spa-libs = {\n\
             support.* = support/libspa-support\n\
             videoconvert = videoconvert/libspa-videoconvert\n\
             audioconvert = audioconvert/libspa-audioconvert\n\
         }\n\
         context.modules = [\n\
             { name = libpipewire-module-protocol-native }\n\
             { name = libpipewire-module-client-node }\n\
             { name = libpipewire-module-adapter }\n\
             { name = libpipewire-module-metadata }\n\
         ]\n",
    );

    // SAFETY: called at startup, env vars are process-global.
    unsafe {
        std::env::set_var("PIPEWIRE_MODULE_DIR", module_dir);
        std::env::set_var("SPA_PLUGIN_DIR", &spa_dir);
        std::env::set_var("PIPEWIRE_REMOTE", socket_path);
        std::env::set_var("PIPEWIRE_CONFIG_DIR", &config_dir);
        std::env::set_var("PIPEWIRE_CONFIG_NAME", "client.conf");
        std::env::set_var("PIPEWIRE_DEBUG", "3");
    }
}

/// AAudio data callback — reads from ring buffer, fills output with audio or silence.
///
/// # Safety
/// Called from Android's audio thread. `user_data` must point to a valid `Arc<RingBuffer>`.
unsafe extern "C" fn aaudio_data_callback(
    _stream: *mut aaudio::AAudioStream,
    user_data: *mut libc::c_void,
    audio_data: *mut libc::c_void,
    num_frames: i32,
) -> i32 {
    let ring = unsafe { &*(user_data as *const RingBuffer) };
    let num_samples = num_frames as usize * CHANNELS as usize;
    let out = unsafe { std::slice::from_raw_parts_mut(audio_data as *mut f32, num_samples) };

    let read = ring.read(out);
    // Fill remainder with silence on underflow
    for sample in &mut out[read..] {
        *sample = 0.0;
    }

    aaudio::AAUDIO_CALLBACK_RESULT_CONTINUE
}

/// Start an AAudio output stream that reads from the given ring buffer.
/// Returns the raw stream pointer (must be closed when done).
fn start_aaudio(ring: &Arc<RingBuffer>) -> Result<*mut aaudio::AAudioStream, String> {
    let mut builder: *mut aaudio::AAudioStreamBuilder = std::ptr::null_mut();

    unsafe {
        aaudio::check(
            aaudio::AAudio_createStreamBuilder(&mut builder),
            "createStreamBuilder",
        )?;

        aaudio::AAudioStreamBuilder_setDirection(builder, aaudio::AAUDIO_DIRECTION_OUTPUT);
        aaudio::AAudioStreamBuilder_setSharingMode(builder, aaudio::AAUDIO_SHARING_MODE_SHARED);
        aaudio::AAudioStreamBuilder_setFormat(builder, aaudio::AAUDIO_FORMAT_PCM_FLOAT);
        aaudio::AAudioStreamBuilder_setSampleRate(builder, SAMPLE_RATE as i32);
        aaudio::AAudioStreamBuilder_setChannelCount(builder, CHANNELS as i32);
        aaudio::AAudioStreamBuilder_setPerformanceMode(
            builder,
            aaudio::AAUDIO_PERFORMANCE_MODE_LOW_LATENCY,
        );

        // Pass ring buffer pointer as callback user data.
        // The Arc keeps it alive for the lifetime of the stream.
        let ring_ptr = Arc::as_ptr(ring) as *mut libc::c_void;
        aaudio::AAudioStreamBuilder_setDataCallback(builder, aaudio_data_callback, ring_ptr);

        let mut stream: *mut aaudio::AAudioStream = std::ptr::null_mut();
        let result = aaudio::AAudioStreamBuilder_openStream(builder, &mut stream);
        aaudio::AAudioStreamBuilder_delete(builder);
        aaudio::check(result, "openStream")?;

        aaudio::check(
            aaudio::AAudioStream_requestStart(stream),
            "requestStart",
        )?;

        Ok(stream)
    }
}

fn run_pipewire_audio_loop(socket: &str) -> Result<(), pw::Error> {
    pw::init();

    tracing::info!("[pw-audio] Creating PipeWire main loop...");
    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|e| { tracing::error!("[pw-audio] MainLoop::new failed: {e}"); e })?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|e| { tracing::error!("[pw-audio] Context::new failed: {e}"); e })?;

    // Retry connection — PipeWire daemon may not be ready yet
    let _socket = socket; // env var PIPEWIRE_REMOTE handles the socket path
    let core = loop {
        match context.connect_rc(None) {
            Ok(core) => break core,
            Err(_) => {
                tracing::info!("[pw-audio] Waiting for PipeWire daemon...");
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        }
    };
    tracing::info!("[pw-audio] Connected to PipeWire daemon");

    // Create shared ring buffer
    let ring = Arc::new(RingBuffer::new(RING_BUFFER_FRAMES * CHANNELS as usize));

    // Start AAudio output
    let _aaudio_stream = match start_aaudio(&ring) {
        Ok(s) => {
            tracing::info!("[pw-audio] AAudio stream started");
            s
        }
        Err(e) => {
            tracing::error!("[pw-audio] Failed to start AAudio: {e}");
            return Err(pw::Error::CreationFailed);
        }
    };

    let stream = pw::stream::StreamBox::new(
        &core,
        "android-audio-sink",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Playback",
            *pw::keys::MEDIA_CLASS => "Audio/Sink",
            *pw::keys::NODE_NAME => "android-audio-sink",
            *pw::keys::NODE_DESCRIPTION => "Android Audio Output",
        },
    )?;

    let _listener = stream
        .add_local_listener_with_user_data(ring)
        .state_changed(|_, _, old, new| {
            tracing::info!("[pw-audio] State: {old:?} -> {new:?}");
        })
        .process(|stream, ring_buf| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }
            let data = &mut datas[0];
            let size = data.chunk().size() as usize;
            let Some(slice) = data.data() else {
                return;
            };
            let size = size.min(slice.len());
            let audio_bytes = &slice[..size];

            // Reinterpret bytes as f32 samples.
            // SAFETY: We negotiated F32LE format. On aarch64 (little-endian), f32 == F32LE.
            // PipeWire MAP_BUFFERS provides properly aligned memory.
            let (_, samples, _): (_, &[f32], _) = unsafe { audio_bytes.align_to::<f32>() };

            if !samples.is_empty() {
                ring_buf.write(samples);
            }
        })
        .register()?;

    // Build audio format params: F32LE stereo @ 48kHz
    let obj = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaType,
            Id,
            spa::param::format::MediaType::Audio
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaSubtype,
            Id,
            spa::param::format::MediaSubtype::Raw
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::AudioFormat,
            Choice,
            Enum,
            Id,
            spa::param::audio::AudioFormat::F32LE,
            spa::param::audio::AudioFormat::F32LE,
            spa::param::audio::AudioFormat::S16LE
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::AudioRate,
            Int,
            SAMPLE_RATE as i32
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::AudioChannels,
            Int,
            CHANNELS as i32
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

    tracing::info!("[pw-audio] Connecting stream (format pod {} bytes)...", values.len());
    stream.connect(
        spa::utils::Direction::Input,
        None,
        pw::stream::StreamFlags::MAP_BUFFERS | pw::stream::StreamFlags::RT_PROCESS,
        &mut params,
    )?;

    tracing::info!("[pw-audio] Stream connected, entering main loop");
    mainloop.run();

    // Clean up AAudio (only reached if mainloop exits)
    unsafe {
        aaudio::AAudioStream_requestStop(_aaudio_stream);
        aaudio::AAudioStream_close(_aaudio_stream);
    }

    Ok(())
}
