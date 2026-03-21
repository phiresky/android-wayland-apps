//! Android Camera2 NDK → PipeWire Video/Source bridge.
//!
//! Opens the back-facing camera via the NDK Camera2 C API and streams NV12
//! frames directly to a PipeWire Video/Source node, making the camera visible
//! to Linux apps (Firefox, Snapshot, etc.) running inside proot.

use crate::android::utils::application_context::get_application_context;
use crate::core::config;
use pipewire_cam::PipeWireCamera;
use std::sync::{Arc, Mutex, OnceLock};

use ndk_sys::{
    ACameraManager, ACameraDevice, ACaptureRequest, ACameraCaptureSession,
    ACaptureSessionOutputContainer, ACaptureSessionOutput, ACameraOutputTarget,
    ACameraMetadata, ACameraIdList, ACameraMetadata_const_entry,
    ACameraDevice_StateCallbacks, ACameraCaptureSession_stateCallbacks,
    AImageReader, AImageReader_ImageListener, AImage, ANativeWindow,
};

// ---- Constants ----

const CAM_WIDTH: i32 = 640;
const CAM_HEIGHT: i32 = 480;
const MAX_IMAGES: i32 = 2;

const AIMAGE_FORMAT_YUV_420_888: i32 = 0x23;
const ACAMERA_LENS_FACING: u32 = 0x00080005;
const ACAMERA_LENS_FACING_BACK: u8 = 1;
const TEMPLATE_PREVIEW: ndk_sys::ACameraDevice_request_template =
    ndk_sys::ACameraDevice_request_template::TEMPLATE_PREVIEW;

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
    if unsafe { ndk_sys::AImageReader_acquireLatestImage(reader, &mut image) } != ndk_sys::media_status_t::AMEDIA_OK || image.is_null() {
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
    unsafe { ndk_sys::AImage_delete(image) };
}

// ---- YUV_420_888 → NV12 conversion ----

fn extract_nv12(image: *mut AImage) -> Option<Vec<u8>> {
    unsafe {
        let mut w = 0i32;
        let mut h = 0i32;
        if ndk_sys::AImage_getWidth(image, &mut w) != ndk_sys::media_status_t::AMEDIA_OK || ndk_sys::AImage_getHeight(image, &mut h) != ndk_sys::media_status_t::AMEDIA_OK {
            return None;
        }
        let (w, h) = (w as usize, h as usize);

        let mut y_ptr: *mut u8 = std::ptr::null_mut();
        let mut y_len = 0i32;
        let mut y_rs = 0i32;
        if ndk_sys::AImage_getPlaneData(image, 0, &mut y_ptr, &mut y_len) != ndk_sys::media_status_t::AMEDIA_OK { return None; }
        if ndk_sys::AImage_getPlaneRowStride(image, 0, &mut y_rs) != ndk_sys::media_status_t::AMEDIA_OK { return None; }
        let y = std::slice::from_raw_parts(y_ptr, y_len as usize);

        let mut u_ptr: *mut u8 = std::ptr::null_mut();
        let mut u_len = 0i32;
        let mut u_rs = 0i32;
        let mut u_ps = 0i32;
        if ndk_sys::AImage_getPlaneData(image, 1, &mut u_ptr, &mut u_len) != ndk_sys::media_status_t::AMEDIA_OK { return None; }
        if ndk_sys::AImage_getPlaneRowStride(image, 1, &mut u_rs) != ndk_sys::media_status_t::AMEDIA_OK { return None; }
        if ndk_sys::AImage_getPlanePixelStride(image, 1, &mut u_ps) != ndk_sys::media_status_t::AMEDIA_OK { return None; }
        let u = std::slice::from_raw_parts(u_ptr, u_len as usize);

        let mut v_ptr: *mut u8 = std::ptr::null_mut();
        let mut v_len = 0i32;
        let mut v_rs = 0i32;
        let mut v_ps = 0i32;
        if ndk_sys::AImage_getPlaneData(image, 2, &mut v_ptr, &mut v_len) != ndk_sys::media_status_t::AMEDIA_OK { return None; }
        if ndk_sys::AImage_getPlaneRowStride(image, 2, &mut v_rs) != ndk_sys::media_status_t::AMEDIA_OK { return None; }
        if ndk_sys::AImage_getPlanePixelStride(image, 2, &mut v_ps) != ndk_sys::media_status_t::AMEDIA_OK { return None; }
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
        if ndk_sys::ACameraManager_getCameraIdList(mgr, &mut id_list) != ndk_sys::camera_status_t::ACAMERA_OK || id_list.is_null() {
            return None;
        }
        let n = (*id_list).numCameras;
        let mut result = None;
        for i in 0..n {
            let id_ptr = *(*id_list).cameraIds.offset(i as isize);
            let id_cstr = std::ffi::CStr::from_ptr(id_ptr).to_owned();
            let mut meta: *mut ACameraMetadata = std::ptr::null_mut();
            if ndk_sys::ACameraManager_getCameraCharacteristics(mgr, id_ptr, &mut meta) != ndk_sys::camera_status_t::ACAMERA_OK {
                continue;
            }
            let mut entry: ACameraMetadata_const_entry = std::mem::zeroed();
            let ok = ndk_sys::ACameraMetadata_getConstEntry(meta, ACAMERA_LENS_FACING, &mut entry) == ndk_sys::camera_status_t::ACAMERA_OK
                && !entry.data.u8_.is_null()
                && *entry.data.u8_ == ACAMERA_LENS_FACING_BACK;
            ndk_sys::ACameraMetadata_free(meta);
            if ok {
                result = Some(id_cstr);
                break;
            }
            if result.is_none() {
                result = Some(id_cstr);
            }
        }
        ndk_sys::ACameraManager_deleteCameraIdList(id_list);
        result
    }
}

fn start_camera() -> Result<(), String> {
    unsafe {
        let mgr = ndk_sys::ACameraManager_create();
        if mgr.is_null() {
            return Err("ACameraManager_create returned null".into());
        }

        let camera_id = match find_back_camera(mgr) {
            Some(id) => id,
            None => {
                ndk_sys::ACameraManager_delete(mgr);
                return Err("No cameras found".into());
            }
        };
        tracing::info!("[camera] Opening {:?}", camera_id);

        let mut dev_cbs = ACameraDevice_StateCallbacks {
            context: std::ptr::null_mut(),
            onDisconnected: Some(on_disconnected),
            onError: Some(on_error),
        };
        let mut device: *mut ACameraDevice = std::ptr::null_mut();
        if ndk_sys::ACameraManager_openCamera(mgr, camera_id.as_ptr(), &mut dev_cbs, &mut device) != ndk_sys::camera_status_t::ACAMERA_OK {
            ndk_sys::ACameraManager_delete(mgr);
            return Err("ACameraManager_openCamera failed".into());
        }

        let mut reader: *mut AImageReader = std::ptr::null_mut();
        if ndk_sys::AImageReader_new(CAM_WIDTH, CAM_HEIGHT, AIMAGE_FORMAT_YUV_420_888, MAX_IMAGES, &mut reader) != ndk_sys::media_status_t::AMEDIA_OK {
            ndk_sys::ACameraDevice_close(device);
            ndk_sys::ACameraManager_delete(mgr);
            return Err("AImageReader_new failed".into());
        }
        let mut listener = AImageReader_ImageListener {
            context: std::ptr::null_mut(),
            onImageAvailable: Some(on_image_available),
        };
        ndk_sys::AImageReader_setImageListener(reader, &mut listener);

        let mut window: *mut ANativeWindow = std::ptr::null_mut();
        if ndk_sys::AImageReader_getWindow(reader, &mut window) != ndk_sys::media_status_t::AMEDIA_OK {
            ndk_sys::AImageReader_delete(reader);
            ndk_sys::ACameraDevice_close(device);
            ndk_sys::ACameraManager_delete(mgr);
            return Err("AImageReader_getWindow failed".into());
        }

        let mut container: *mut ACaptureSessionOutputContainer = std::ptr::null_mut();
        ndk_sys::ACaptureSessionOutputContainer_create(&mut container);
        let mut sess_out: *mut ACaptureSessionOutput = std::ptr::null_mut();
        ndk_sys::ACaptureSessionOutput_create(window, &mut sess_out);
        ndk_sys::ACaptureSessionOutputContainer_add(container, sess_out);

        let sess_cbs = ACameraCaptureSession_stateCallbacks {
            context: std::ptr::null_mut(),
            onClosed: None,
            onReady: None,
            onActive: None,
        };
        let mut session: *mut ACameraCaptureSession = std::ptr::null_mut();
        if ndk_sys::ACameraDevice_createCaptureSession(device, container, &sess_cbs, &mut session) != ndk_sys::camera_status_t::ACAMERA_OK {
            ndk_sys::ACaptureSessionOutput_free(sess_out);
            ndk_sys::ACaptureSessionOutputContainer_free(container);
            ndk_sys::AImageReader_delete(reader);
            ndk_sys::ACameraDevice_close(device);
            ndk_sys::ACameraManager_delete(mgr);
            return Err("createCaptureSession failed".into());
        }

        let mut request: *mut ACaptureRequest = std::ptr::null_mut();
        ndk_sys::ACameraDevice_createCaptureRequest(device, TEMPLATE_PREVIEW, &mut request);
        let mut target: *mut ACameraOutputTarget = std::ptr::null_mut();
        ndk_sys::ACameraOutputTarget_create(window, &mut target);
        ndk_sys::ACaptureRequest_addTarget(request, target);

        let mut seq = 0i32;
        if ndk_sys::ACameraCaptureSession_setRepeatingRequest(
            session,
            std::ptr::null_mut(),
            1,
            &mut request,
            &mut seq,
        ) != ndk_sys::camera_status_t::ACAMERA_OK
        {
            ndk_sys::ACaptureRequest_free(request);
            ndk_sys::ACameraOutputTarget_free(target);
            ndk_sys::ACameraCaptureSession_close(session);
            ndk_sys::ACaptureSessionOutput_free(sess_out);
            ndk_sys::ACaptureSessionOutputContainer_free(container);
            ndk_sys::AImageReader_delete(reader);
            ndk_sys::ACameraDevice_close(device);
            ndk_sys::ACameraManager_delete(mgr);
            return Err("setRepeatingRequest failed".into());
        }

        let _ = &sess_cbs;

        tracing::info!("[camera] Streaming {}×{} NV12", CAM_WIDTH, CAM_HEIGHT);
        std::thread::park(); // keep objects alive

        // Unreachable cleanup
        ndk_sys::ACaptureRequest_free(request);
        ndk_sys::ACameraOutputTarget_free(target);
        ndk_sys::ACameraCaptureSession_close(session);
        ndk_sys::ACaptureSessionOutput_free(sess_out);
        ndk_sys::ACaptureSessionOutputContainer_free(container);
        ndk_sys::AImageReader_delete(reader);
        ndk_sys::ACameraDevice_close(device);
        ndk_sys::ACameraManager_delete(mgr);
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
