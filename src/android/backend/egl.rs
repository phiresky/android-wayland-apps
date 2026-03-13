//! EGL rendering backend for the Android Wayland compositor.
//!
//! Provides headless EGL initialization and per-window surface management
//! for rendering Wayland client content to Android Activity windows.

use khronos_egl::DynamicInstance;
use smithay::{
    backend::{
        egl::{
            context::{GlAttributes, PixelFormatRequirements},
            display::EGLDisplay,
            native::EGLNativeSurface,
            EGLContext, EGLError, EGLSurface,
        },
        renderer::{
            gles::{GlesRenderer, GlesTarget},
            Bind,
        },
        SwapBuffersError,
    },
};
use std::ffi::c_void;
use std::sync::Arc;
use raw_window_handle::AndroidNdkWindowHandle;

pub struct AndroidNativeSurface {
    pub handle: AndroidNdkWindowHandle,
}

unsafe impl Send for AndroidNativeSurface {}

unsafe impl EGLNativeSurface for AndroidNativeSurface {
    unsafe fn create(
        &self,
        display: &Arc<smithay::backend::egl::display::EGLDisplayHandle>,
        config_id: smithay::backend::egl::ffi::egl::types::EGLConfig,
    ) -> Result<*const std::os::raw::c_void, smithay::backend::egl::EGLError> {
        let surface = unsafe {
            smithay::backend::egl::ffi::egl::CreateWindowSurface(
                display.handle,
                config_id,
                self.handle.a_native_window.as_ptr(),
                std::ptr::null(),
            )
        };
        if surface.is_null() {
            return Err(smithay::backend::egl::EGLError::BadAlloc);
        }
        Ok(surface)
    }
}

/// Headless EGL renderer for the compositor thread.
/// Does not require a window — renders to WaylandWindowActivity EGL surfaces.
pub struct CompositorRenderer {
    pub renderer: GlesRenderer,
    pub display: EGLDisplay,
    pixel_format: smithay::backend::egl::display::PixelFormat,
    config_id: smithay::backend::egl::ffi::egl::types::EGLConfig,
}

impl CompositorRenderer {
    pub fn create_surface_for_native_window(
        &self,
        handle: AndroidNdkWindowHandle,
    ) -> Result<EGLSurface, EGLError> {
        unsafe {
            EGLSurface::new(
                &self.display,
                self.pixel_format,
                self.config_id,
                AndroidNativeSurface { handle },
            )
        }
    }

    pub fn bind_surface<'a>(
        &'a mut self,
        surface: &'a mut EGLSurface,
    ) -> Result<(&'a mut GlesRenderer, GlesTarget<'a>), SwapBuffersError> {
        let fb = self.renderer.bind(surface)?;
        Ok((&mut self.renderer, fb))
    }

    pub fn submit_surface(&self, surface: &EGLSurface) -> Result<(), SwapBuffersError> {
        surface.swap_buffers(None)?;
        Ok(())
    }
}

/// Initialize a headless EGL context (no window surface needed).
/// The compositor thread uses this to render to WaylandWindowActivity surfaces.
pub fn init_egl_headless() -> Result<CompositorRenderer, Box<dyn std::error::Error>> {
    let display = create_egl_display_headless()?;

    let gl_attributes = GlAttributes {
        version: (3, 0),
        profile: None,
        debug: cfg!(debug_assertions),
        vsync: false,
    };
    let context = EGLContext::new_with_config(
        &display,
        gl_attributes,
        PixelFormatRequirements::_10_bit(),
    )
    .or_else(|_| {
        EGLContext::new_with_config(
            &display,
            gl_attributes,
            PixelFormatRequirements::_8_bit(),
        )
    })
    .map_err(|e| format!("Failed to create EGLContext: {e}"))?;

    let pixel_format = context
        .pixel_format()
        .ok_or("EGLContext has no pixel format")?;
    let config_id = context.config_id();

    let renderer = unsafe { GlesRenderer::new(context) }
        .map_err(|e| format!("Failed to create GLES Renderer: {e}"))?;

    log::info!("Headless EGL initialized successfully");
    Ok(CompositorRenderer { renderer, display, pixel_format, config_id })
}

fn create_egl_display_headless() -> Result<EGLDisplay, Box<dyn std::error::Error>> {
    let lib = unsafe { libloading::Library::new("libEGL.so") }?;
    let egl = unsafe { DynamicInstance::<khronos_egl::EGL1_4>::load_required_from(lib) }?;
    let display = unsafe { egl.get_display(khronos_egl::DEFAULT_DISPLAY) }
        .ok_or("Failed to get EGL display")?;
    let (_major, _minor) = egl.initialize(display)?;
    let config_attribs = [khronos_egl::NONE];
    let config = egl
        .choose_first_config(display, &config_attribs)
        .map_err(|e| format!("Failed to choose EGL config: {e}"))?
        .ok_or("No suitable EGL config found")?;
    let egl_display = unsafe {
        EGLDisplay::from_raw(
            display.as_ptr() as *mut c_void,
            config.as_ptr() as *mut c_void,
        )
    }
    .map_err(|e| format!("Failed to create EGL display: {e}"))?;
    Ok(egl_display)
}

