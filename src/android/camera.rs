//! Android Camera2 NDK → PipeWire Video/Source bridge.
//!
//! Opens the back-facing camera via the NDK Camera2 C API and streams NV12
//! frames directly to a PipeWire Video/Source node, making the camera visible
//! to Linux apps (Firefox, Snapshot, etc.) running inside proot.

use crate::android::utils::application_context::get_application_context;
use crate::core::config;
use pipewire_cam::PipeWireCamera;
use std::sync::{Arc, Mutex, OnceLock};

// ---- Constants ----

const CAM_WIDTH: i32 = 640;
const CAM_HEIGHT: i32 = 480;
const MAX_IMAGES: i32 = 2;

const AIMAGE_FORMAT_YUV_420_888: i32 = 0x23;
const ACAMERA_LENS_FACING: u32 = 0x00080005;
const ACAMERA_LENS_FACING_BACK: u8 = 1;
const TEMPLATE_PREVIEW: i32 = 1;

// ---- Opaque NDK types ----

#[repr(C)]
struct ACameraManager(u8);
#[repr(C)]
struct ACameraDevice(u8);
#[repr(C)]
struct ACaptureRequest(u8);
#[repr(C)]
struct ACameraCaptureSession(u8);
#[repr(C)]
struct ACaptureSessionOutputContainer(u8);
#[repr(C)]
struct ACaptureSessionOutput(u8);
#[repr(C)]
struct ACameraOutputTarget(u8);
#[repr(C)]
struct ACameraMetadata(u8);
#[repr(C)]
struct AImageReader(u8);
#[repr(C)]
struct AImage(u8);
#[repr(C)]
struct ANativeWindow(u8);

#[repr(C)]
struct ACameraIdList {
    num_cameras: i32,
    camera_ids: *const *const libc::c_char,
}

#[repr(C)]
struct ACameraMetadata_const_entry {
    tag: u32,
    type_: u8,
    count: u32,
    data: *const u8,
}

#[repr(C)]
struct ACameraDevice_StateCallbacks {
    context: *mut libc::c_void,
    on_disconnected: Option<unsafe extern "C" fn(*mut libc::c_void, *mut ACameraDevice)>,
    on_error: Option<unsafe extern "C" fn(*mut libc::c_void, *mut ACameraDevice, i32)>,
}

#[repr(C)]
struct ACameraCaptureSession_stateCallbacks {
    context: *mut libc::c_void,
    on_closed: Option<unsafe extern "C" fn(*mut libc::c_void, *mut ACameraCaptureSession)>,
    on_ready: Option<unsafe extern "C" fn(*mut libc::c_void, *mut ACameraCaptureSession)>,
    on_active: Option<unsafe extern "C" fn(*mut libc::c_void, *mut ACameraCaptureSession)>,
}

#[repr(C)]
struct AImageReader_ImageListener {
    context: *mut libc::c_void,
    on_image_available: Option<unsafe extern "C" fn(*mut libc::c_void, *mut AImageReader)>,
}

// ---- FFI ----

#[link(name = "camera2ndk")]
#[link(name = "mediandk")]
unsafe extern "C" {
    fn ACameraManager_create() -> *mut ACameraManager;
    fn ACameraManager_delete(mgr: *mut ACameraManager);
    fn ACameraManager_getCameraIdList(mgr: *mut ACameraManager, list: *mut *mut ACameraIdList)
        -> i32;
    fn ACameraManager_deleteCameraIdList(list: *mut ACameraIdList);
    fn ACameraManager_getCameraCharacteristics(
        mgr: *mut ACameraManager,
        id: *const libc::c_char,
        meta: *mut *mut ACameraMetadata,
    ) -> i32;
    fn ACameraManager_openCamera(
        mgr: *mut ACameraManager,
        id: *const libc::c_char,
        cbs: *mut ACameraDevice_StateCallbacks,
        dev: *mut *mut ACameraDevice,
    ) -> i32;

    fn ACameraMetadata_free(meta: *mut ACameraMetadata);
    fn ACameraMetadata_getConstEntry(
        meta: *const ACameraMetadata,
        tag: u32,
        entry: *mut ACameraMetadata_const_entry,
    ) -> i32;

    fn ACameraDevice_close(dev: *mut ACameraDevice) -> i32;
    fn ACameraDevice_createCaptureRequest(
        dev: *mut ACameraDevice,
        tpl: i32,
        req: *mut *mut ACaptureRequest,
    ) -> i32;
    fn ACameraDevice_createCaptureSession(
        dev: *mut ACameraDevice,
        outputs: *const ACaptureSessionOutputContainer,
        cbs: *const ACameraCaptureSession_stateCallbacks,
        session: *mut *mut ACameraCaptureSession,
    ) -> i32;

    fn ACaptureRequest_free(req: *mut ACaptureRequest);
    fn ACaptureRequest_addTarget(
        req: *mut ACaptureRequest,
        target: *mut ACameraOutputTarget,
    ) -> i32;

    fn ACaptureSessionOutputContainer_create(
        out: *mut *mut ACaptureSessionOutputContainer,
    ) -> i32;
    fn ACaptureSessionOutputContainer_free(c: *mut ACaptureSessionOutputContainer);
    fn ACaptureSessionOutputContainer_add(
        c: *mut ACaptureSessionOutputContainer,
        output: *mut ACaptureSessionOutput,
    ) -> i32;

    fn ACaptureSessionOutput_create(
        window: *mut ANativeWindow,
        out: *mut *mut ACaptureSessionOutput,
    ) -> i32;
    fn ACaptureSessionOutput_free(out: *mut ACaptureSessionOutput);

    fn ACameraOutputTarget_create(
        window: *mut ANativeWindow,
        out: *mut *mut ACameraOutputTarget,
    ) -> i32;
    fn ACameraOutputTarget_free(target: *mut ACameraOutputTarget);

    fn ACameraCaptureSession_setRepeatingRequest(
        session: *mut ACameraCaptureSession,
        cbs: *mut libc::c_void,
        n: i32,
        requests: *mut *mut ACaptureRequest,
        seq: *mut i32,
    ) -> i32;
    fn ACameraCaptureSession_close(session: *mut ACameraCaptureSession);

    fn AImageReader_new(
        w: i32, h: i32, fmt: i32, max: i32,
        out: *mut *mut AImageReader,
    ) -> i32;
    fn AImageReader_delete(reader: *mut AImageReader);
    fn AImageReader_getWindow(reader: *mut AImageReader, window: *mut *mut ANativeWindow) -> i32;
    fn AImageReader_setImageListener(
        reader: *mut AImageReader,
        listener: *mut AImageReader_ImageListener,
    ) -> i32;
    fn AImageReader_acquireLatestImage(reader: *mut AImageReader, image: *mut *mut AImage) -> i32;

    fn AImage_delete(image: *mut AImage);
    fn AImage_getWidth(image: *const AImage, w: *mut i32) -> i32;
    fn AImage_getHeight(image: *const AImage, h: *mut i32) -> i32;
    fn AImage_getPlaneData(
        image: *const AImage, plane: i32,
        data: *mut *mut u8, len: *mut i32,
    ) -> i32;
    fn AImage_getPlanePixelStride(image: *const AImage, plane: i32, stride: *mut i32) -> i32;
    fn AImage_getPlaneRowStride(image: *const AImage, plane: i32, stride: *mut i32) -> i32;
}

// ---- Global state ----

/// Shared frame buffer: camera callback writes, PipeWire process callback reads.
static PW_FRAME: OnceLock<Arc<Mutex<Option<Vec<u8>>>>> = OnceLock::new();
/// PipeWire camera handle (kept alive for the process lifetime).
static PW_CAMERA: OnceLock<Mutex<Option<PipeWireCamera>>> = OnceLock::new();

// ---- NDK callbacks ----

unsafe extern "C" fn on_disconnected(_ctx: *mut libc::c_void, _dev: *mut ACameraDevice) {
    tracing::warn!("[camera] Camera disconnected");
}

unsafe extern "C" fn on_error(_ctx: *mut libc::c_void, _dev: *mut ACameraDevice, err: i32) {
    tracing::error!("[camera] Camera error: {err}");
}

unsafe extern "C" fn on_image_available(_ctx: *mut libc::c_void, reader: *mut AImageReader) {
    let mut image: *mut AImage = std::ptr::null_mut();
    if unsafe { AImageReader_acquireLatestImage(reader, &mut image) } != 0 || image.is_null() {
        return;
    }
    if let Some(frame) = extract_nv12(image) {
        // Push frame to PipeWire
        if let Some(pw_cam) = PW_CAMERA.get() {
            if let Ok(guard) = pw_cam.lock() {
                if let Some(ref cam) = *guard {
                    cam.push_frame(&frame);
                }
            }
        }
    }
    unsafe { AImage_delete(image) };
}

// ---- YUV_420_888 → NV12 conversion ----

fn extract_nv12(image: *mut AImage) -> Option<Vec<u8>> {
    unsafe {
        let mut w = 0i32;
        let mut h = 0i32;
        if AImage_getWidth(image, &mut w) != 0 || AImage_getHeight(image, &mut h) != 0 {
            return None;
        }
        let (w, h) = (w as usize, h as usize);

        let mut y_ptr: *mut u8 = std::ptr::null_mut();
        let mut y_len = 0i32;
        let mut y_rs = 0i32;
        if AImage_getPlaneData(image, 0, &mut y_ptr, &mut y_len) != 0 { return None; }
        if AImage_getPlaneRowStride(image, 0, &mut y_rs) != 0 { return None; }
        let y = std::slice::from_raw_parts(y_ptr, y_len as usize);

        let mut u_ptr: *mut u8 = std::ptr::null_mut();
        let mut u_len = 0i32;
        let mut u_rs = 0i32;
        let mut u_ps = 0i32;
        if AImage_getPlaneData(image, 1, &mut u_ptr, &mut u_len) != 0 { return None; }
        if AImage_getPlaneRowStride(image, 1, &mut u_rs) != 0 { return None; }
        if AImage_getPlanePixelStride(image, 1, &mut u_ps) != 0 { return None; }
        let u = std::slice::from_raw_parts(u_ptr, u_len as usize);

        let mut v_ptr: *mut u8 = std::ptr::null_mut();
        let mut v_len = 0i32;
        let mut v_rs = 0i32;
        let mut v_ps = 0i32;
        if AImage_getPlaneData(image, 2, &mut v_ptr, &mut v_len) != 0 { return None; }
        if AImage_getPlaneRowStride(image, 2, &mut v_rs) != 0 { return None; }
        if AImage_getPlanePixelStride(image, 2, &mut v_ps) != 0 { return None; }
        let v = std::slice::from_raw_parts(v_ptr, v_len as usize);

        Some(yuv420_to_nv12(
            w, h,
            y, y_rs as usize,
            u, u_rs as usize, u_ps as usize,
            v, v_rs as usize, v_ps as usize,
        ))
    }
}

fn yuv420_to_nv12(
    w: usize, h: usize,
    y: &[u8], y_rs: usize,
    u: &[u8], u_rs: usize, u_ps: usize,
    v: &[u8], v_rs: usize, v_ps: usize,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(w * h * 3 / 2);
    for row in 0..h {
        let base = row * y_rs;
        let end = (base + w).min(y.len());
        out.extend_from_slice(&y[base..end]);
        out.resize(out.len() + w - (end - base), 16u8);
    }
    for row in 0..h / 2 {
        for col in 0..w / 2 {
            let ui = row * u_rs + col * u_ps;
            let vi = row * v_rs + col * v_ps;
            out.push(if ui < u.len() { u[ui] } else { 128 });
            out.push(if vi < v.len() { v[vi] } else { 128 });
        }
    }
    out
}

// ---- Camera startup ----

fn find_back_camera(mgr: *mut ACameraManager) -> Option<std::ffi::CString> {
    unsafe {
        let mut id_list: *mut ACameraIdList = std::ptr::null_mut();
        if ACameraManager_getCameraIdList(mgr, &mut id_list) != 0 || id_list.is_null() {
            return None;
        }
        let n = (*id_list).num_cameras;
        let mut result = None;
        for i in 0..n {
            let id_ptr = *(*id_list).camera_ids.offset(i as isize);
            let id_cstr = std::ffi::CStr::from_ptr(id_ptr).to_owned();
            let mut meta: *mut ACameraMetadata = std::ptr::null_mut();
            if ACameraManager_getCameraCharacteristics(mgr, id_ptr, &mut meta) != 0 {
                continue;
            }
            let mut entry = ACameraMetadata_const_entry {
                tag: 0,
                type_: 0,
                count: 0,
                data: std::ptr::null(),
            };
            let ok = ACameraMetadata_getConstEntry(meta, ACAMERA_LENS_FACING, &mut entry) == 0
                && !entry.data.is_null()
                && *entry.data == ACAMERA_LENS_FACING_BACK;
            ACameraMetadata_free(meta);
            if ok {
                result = Some(id_cstr);
                break;
            }
            if result.is_none() {
                result = Some(id_cstr);
            }
        }
        ACameraManager_deleteCameraIdList(id_list);
        result
    }
}

fn start_camera() -> Result<(), String> {
    unsafe {
        let mgr = ACameraManager_create();
        if mgr.is_null() {
            return Err("ACameraManager_create returned null".into());
        }

        let camera_id = match find_back_camera(mgr) {
            Some(id) => id,
            None => {
                ACameraManager_delete(mgr);
                return Err("No cameras found".into());
            }
        };
        tracing::info!("[camera] Opening {:?}", camera_id);

        let mut dev_cbs = ACameraDevice_StateCallbacks {
            context: std::ptr::null_mut(),
            on_disconnected: Some(on_disconnected),
            on_error: Some(on_error),
        };
        let mut device: *mut ACameraDevice = std::ptr::null_mut();
        if ACameraManager_openCamera(mgr, camera_id.as_ptr(), &mut dev_cbs, &mut device) != 0 {
            ACameraManager_delete(mgr);
            return Err("ACameraManager_openCamera failed".into());
        }

        let mut reader: *mut AImageReader = std::ptr::null_mut();
        if AImageReader_new(CAM_WIDTH, CAM_HEIGHT, AIMAGE_FORMAT_YUV_420_888, MAX_IMAGES, &mut reader) != 0 {
            ACameraDevice_close(device);
            ACameraManager_delete(mgr);
            return Err("AImageReader_new failed".into());
        }
        let mut listener = AImageReader_ImageListener {
            context: std::ptr::null_mut(),
            on_image_available: Some(on_image_available),
        };
        AImageReader_setImageListener(reader, &mut listener);

        let mut window: *mut ANativeWindow = std::ptr::null_mut();
        if AImageReader_getWindow(reader, &mut window) != 0 {
            AImageReader_delete(reader);
            ACameraDevice_close(device);
            ACameraManager_delete(mgr);
            return Err("AImageReader_getWindow failed".into());
        }

        let mut container: *mut ACaptureSessionOutputContainer = std::ptr::null_mut();
        ACaptureSessionOutputContainer_create(&mut container);
        let mut sess_out: *mut ACaptureSessionOutput = std::ptr::null_mut();
        ACaptureSessionOutput_create(window, &mut sess_out);
        ACaptureSessionOutputContainer_add(container, sess_out);

        let sess_cbs = ACameraCaptureSession_stateCallbacks {
            context: std::ptr::null_mut(),
            on_closed: None,
            on_ready: None,
            on_active: None,
        };
        let mut session: *mut ACameraCaptureSession = std::ptr::null_mut();
        if ACameraDevice_createCaptureSession(device, container, &sess_cbs, &mut session) != 0 {
            ACaptureSessionOutput_free(sess_out);
            ACaptureSessionOutputContainer_free(container);
            AImageReader_delete(reader);
            ACameraDevice_close(device);
            ACameraManager_delete(mgr);
            return Err("createCaptureSession failed".into());
        }

        let mut request: *mut ACaptureRequest = std::ptr::null_mut();
        ACameraDevice_createCaptureRequest(device, TEMPLATE_PREVIEW, &mut request);
        let mut target: *mut ACameraOutputTarget = std::ptr::null_mut();
        ACameraOutputTarget_create(window, &mut target);
        ACaptureRequest_addTarget(request, target);

        let mut seq = 0i32;
        if ACameraCaptureSession_setRepeatingRequest(
            session,
            std::ptr::null_mut(),
            1,
            &mut request,
            &mut seq,
        ) != 0
        {
            ACaptureRequest_free(request);
            ACameraOutputTarget_free(target);
            ACameraCaptureSession_close(session);
            ACaptureSessionOutput_free(sess_out);
            ACaptureSessionOutputContainer_free(container);
            AImageReader_delete(reader);
            ACameraDevice_close(device);
            ACameraManager_delete(mgr);
            return Err("setRepeatingRequest failed".into());
        }

        let _ = &sess_cbs;

        tracing::info!("[camera] Streaming {}×{} NV12", CAM_WIDTH, CAM_HEIGHT);
        std::thread::park(); // keep objects alive

        // Unreachable cleanup
        ACaptureRequest_free(request);
        ACameraOutputTarget_free(target);
        ACameraCaptureSession_close(session);
        ACaptureSessionOutput_free(sess_out);
        ACaptureSessionOutputContainer_free(container);
        AImageReader_delete(reader);
        ACameraDevice_close(device);
        ACameraManager_delete(mgr);
        Ok(())
    }
}

// ---- PipeWire connection (with retry) ----

fn connect_pipewire() {
    let ctx = get_application_context();
    let native_lib_dir = ctx.native_library_dir.to_string_lossy().to_string();
    let data_dir = ctx.data_dir.to_string_lossy().to_string();
    let pw_socket = format!("{}/tmp/pipewire-0", config::ARCH_FS_ROOT);

    loop {
        tracing::info!("[camera] Connecting to PipeWire at {pw_socket}...");

        match PipeWireCamera::start(&pw_socket, &native_lib_dir, &data_dir) {
            Some(camera) => {
                tracing::info!("[camera] PipeWire camera stream connected");
                if let Some(pw) = PW_CAMERA.get() {
                    if let Ok(mut guard) = pw.lock() {
                        *guard = Some(camera);
                    }
                }
                return;
            }
            None => {
                tracing::warn!("[camera] PipeWire not ready, retrying in 2s...");
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        }
    }
}

// ---- Public entry point ----

pub fn start() {
    if PW_FRAME.set(Arc::new(Mutex::new(None))).is_err() {
        tracing::warn!("[camera] Already started");
        return;
    }
    let _ = PW_CAMERA.set(Mutex::new(None));

    // Thread 1: Connect to PipeWire (retries until daemon is ready)
    std::thread::Builder::new()
        .name("cam-pipewire".into())
        .spawn(connect_pipewire)
        .ok();

    // Thread 2: Start camera capture
    std::thread::Builder::new()
        .name("cam-capture".into())
        .spawn(|| {
            if let Err(e) = start_camera() {
                tracing::error!("[camera] {e}");
            }
        })
        .ok();
}
