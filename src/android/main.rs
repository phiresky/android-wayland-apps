use crate::android::{
    app::App,
    utils::application_context::ApplicationContext,
};
use winit::{
    event_loop::{ControlFlow, EventLoop},
    platform::android::{activity::AndroidApp, EventLoopBuilderExtAndroid},
};

#[unsafe(no_mangle)]
fn android_main(android_app: AndroidApp) {
    unsafe { std::env::set_var("RUST_BACKTRACE", "full") };
    unsafe { std::env::set_var("XKB_CONFIG_ROOT", "/data/data/io.github.phiresky.wayland_android/files/xkb") };

    // Initialize Android logger with trace-level output.
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Trace),
    );

    // Build the application context (resolves data_dir, native_library_dir, etc.)
    ApplicationContext::build(&android_app);

    let event_loop = EventLoop::builder()
        .with_android_app(android_app.clone())
        .build()
        .expect("Failed to create event loop");

    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = App::build(android_app);
    event_loop.run_app(&mut app).expect("Failed to run app");
}
