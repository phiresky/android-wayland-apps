use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::platform::android::activity::AndroidApp;
use winit::window::WindowId;

use crate::android::{
    backend::{bind_egl, centralize, handle, WaylandBackend},
    compositor::{Compositor, State},
    proot::launch::launch,
};
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::utils::{Clock, Monotonic, Transform};

pub struct App {
    pub android_app: AndroidApp,
    pub backend: WaylandBackend,
}

impl App {
    pub fn build(android_app: AndroidApp) -> Self {
        let compositor = Compositor::build().expect("Failed to build compositor");
        Self {
            backend: WaylandBackend {
                compositor,
                graphic_renderer: None,
                clock: Clock::<Monotonic>::new(),
                key_counter: 0,
                scale_factor: 1.0,
            },
            android_app,
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Initialize the Wayland backend by binding EGL to the winit window.
        let winit = bind_egl(event_loop);
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

        // Launch the proot environment so clients can connect.
        launch();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let event = centralize(event, &mut self.backend);
        handle(event, &mut self.backend, event_loop);
    }
}
