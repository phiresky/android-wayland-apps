use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::platform::android::activity::AndroidApp;
use winit::window::WindowId;

use crate::android::{
    backend::{bind_egl, centralize, handle, WaylandBackend},
    compositor::{Compositor, State},
    main::show_setup_overlay,
    proot::launch::launch,
    window_manager::WindowManager,
};
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::utils::{Clock, Monotonic, Transform};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct App {
    pub android_app: AndroidApp,
    pub backend: WaylandBackend,
    pub setup_done: Arc<AtomicBool>,
    pub launched: bool,
}

impl App {
    pub fn build(android_app: AndroidApp, setup_done: Arc<AtomicBool>) -> Result<Self, Box<dyn std::error::Error>> {
        let compositor = Compositor::build()?;
        Ok(Self {
            backend: WaylandBackend {
                compositor,
                graphic_renderer: None,
                window_manager: None,
                android_app: android_app.clone(),
                clock: Clock::<Monotonic>::new(),
                key_counter: 0,
                scale_factor: 1.0,
            },
            android_app: android_app.clone(),
            setup_done,
            launched: false,
        })
    }

    fn try_launch(&mut self) {
        if !self.launched && self.setup_done.load(Ordering::Acquire) {
            self.backend.compositor.init_keyboard();
            launch();
            self.launched = true;
        }
    }
}

impl ApplicationHandler for App {
    fn can_create_surfaces(&mut self, event_loop: &dyn ActiveEventLoop) {
        // Initialize the Wayland backend by binding EGL to the winit window.
        // In winit 0.31, the native window is only available in can_create_surfaces.
        if self.backend.graphic_renderer.is_some() {
            return; // Already initialized (e.g. after suspend/resume cycle)
        }
        let winit = match bind_egl(event_loop) {
            Ok(w) => w,
            Err(e) => {
                log::error!("Failed to bind EGL: {:?}", e);
                return;
            }
        };
        let window_size = winit.window_size();
        let scale_factor = winit.scale_factor();
        let size = (window_size.w, window_size.h);
        self.backend.graphic_renderer = Some(winit);
        self.backend.compositor.state.size = size.into();

        // Create the Wayland output representing the Android display.
        let output = Output::new(
            "Android Wayland Launcher".into(),
            PhysicalProperties {
                size: size.into(),
                subpixel: Subpixel::HorizontalRgb,
                make: "Android".into(),
                model: "Wayland Launcher".into(),
                serial_number: String::new(),
            },
        );

        let dh = self.backend.compositor.display.handle();
        let _global = output.create_global::<State>(&dh);
        output.change_current_state(
            Some(Mode {
                size: size.into(),
                refresh: 60000,
            }),
            Some(Transform::Normal),
            Some(Scale::Fractional(scale_factor)),
            Some((0, 0).into()),
        );

        self.backend.compositor.output.replace(output);

        // Create the window manager for multi-window support.
        self.backend.window_manager = Some(WindowManager::new(self.android_app.clone()));

        // Show setup overlay now that the window is ready.
        if !self.setup_done.load(Ordering::Acquire) {
            let _ = show_setup_overlay(&self.android_app);
        }

        // Launch the proot environment once setup is complete.
        self.try_launch();
    }

    fn window_event(&mut self, event_loop: &dyn ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        // Check if background setup has completed and we can launch proot.
        self.try_launch();

        let event = centralize(event, &mut self.backend);
        handle(event, &mut self.backend, event_loop);
    }

    fn destroy_surfaces(&mut self, _event_loop: &dyn ActiveEventLoop) {
        log::info!("destroy_surfaces called");
    }

    fn suspended(&mut self, _event_loop: &dyn ActiveEventLoop) {
        log::info!("App suspended");
    }

    fn memory_warning(&mut self, _event_loop: &dyn ActiveEventLoop) {
        log::warn!("Memory warning received");
    }
}
