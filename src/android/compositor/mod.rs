pub mod bind;
mod text_input;

use bind::bind_socket;
pub use text_input::TextInputState;
use smithay::{
    backend::{
        allocator::dmabuf::Dmabuf,
        renderer::utils::on_commit_buffer_handler,
    },
    delegate_compositor, delegate_data_device, delegate_dmabuf, delegate_fractional_scale,
    delegate_layer_shell, delegate_output, delegate_seat, delegate_shm, delegate_xdg_decoration,
    delegate_xdg_shell,
    desktop::{PopupKind, PopupManager},
    input::{self, keyboard::KeyboardHandle, touch::TouchHandle, Seat, SeatHandler, SeatState},
    output::Output,
    reexports::{
        wayland_protocols::xdg::{
            decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode,
            shell::server::xdg_toplevel,
        },
        wayland_server::{protocol::{wl_output::WlOutput, wl_seat}, Display},
    },
    utils::{Logical, Serial, Size},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            with_surface_tree_downward, CompositorClientState, CompositorHandler, CompositorState,
            SurfaceAttributes, TraversalAction,
        },
        dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
        output::OutputHandler,
        selection::{
            data_device::{
                DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler,
            },
            SelectionHandler,
        },
        shell::wlr_layer::{
            Layer, LayerSurface, WlrLayerShellHandler, WlrLayerShellState,
        },
        shell::xdg::{
            decoration::{XdgDecorationHandler, XdgDecorationState},
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
        },
        shm::{ShmHandler, ShmState},
        fractional_scale::{FractionalScaleHandler, FractionalScaleManagerState},
        viewporter::ViewporterState,
    },
};
use smithay::{
    input::pointer::PointerHandle,
    reexports::wayland_server::{
        backend::{ClientData, ClientId, DisconnectReason},
        protocol::{wl_buffer, wl_surface::WlSurface},
        Client, ListeningSocket,
    },
};
use smithay::backend::allocator::format::FormatSet;
use std::{error::Error, time::Instant};
use std::os::unix::io::RawFd;
use crate::android::backend::signal_wake;

pub struct Compositor {
    pub state: State,
    pub display: Display<State>,
    pub listener: ListeningSocket,
    pub clients: Vec<Client>,
    pub start_time: Instant,
    pub seat: Seat<State>,
    pub keyboard: Option<KeyboardHandle<State>>,
    pub touch: TouchHandle<State>,
    pub pointer: PointerHandle<State>,
    pub output: Option<Output>,
}

pub struct State {
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub xdg_decoration_state: XdgDecorationState,
    pub layer_shell_state: WlrLayerShellState,
    pub fractional_scale_state: FractionalScaleManagerState,
    pub viewporter_state: ViewporterState,
    pub shm_state: ShmState,
    pub dmabuf_state: DmabufState,
    #[allow(dead_code)]
    pub dmabuf_global: DmabufGlobal,
    pub data_device_state: DataDeviceState,
    pub seat_state: SeatState<Self>,
    pub size: Size<i32, Logical>,
    /// New toplevels queued by XdgShellHandler, drained by the main loop.
    pub pending_toplevels: Vec<ToplevelSurface>,
    /// New layer surfaces queued by WlrLayerShellHandler, drained by the main loop.
    pub pending_layer_surfaces: Vec<LayerSurface>,
    /// Eventfd to wake the compositor thread when clients commit buffers.
    pub wake_fd: Option<RawFd>,
    /// Surfaces that had a commit since last render (for needs_redraw).
    pub committed_surfaces: Vec<WlSurface>,
    /// Text input state for soft keyboard integration.
    pub text_input_state: TextInputState,
    /// Pending soft keyboard show/hide request from text_input_v3.
    pub soft_keyboard_request: Option<bool>,
    /// Toplevels destroyed by the client, queued for Activity cleanup.
    pub destroyed_toplevels: Vec<ToplevelSurface>,
    /// Layer surfaces destroyed by the client, queued for Activity cleanup.
    pub destroyed_layer_surfaces: Vec<LayerSurface>,
    /// Tracks popup surfaces for rendering and hit-testing.
    pub popup_manager: PopupManager,
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // Don't configure size yet — the Activity will tell us its dimensions.
        surface.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Activated);
        });
        surface.send_configure();
        self.pending_toplevels.push(surface);
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        // Send initial configure so the client doesn't block waiting for it.
        if let Err(e) = surface.send_configure() {
            log::warn!("Failed to send popup configure: {:?}", e);
        }
        // Register with PopupManager so it's included in rendering and hit-testing.
        if let Err(e) = self.popup_manager.track_popup(PopupKind::Xdg(surface)) {
            log::warn!("Failed to track popup: {:?}", e);
        }
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        self.destroyed_toplevels.push(surface);
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {
        // No-op: popup grabs not needed — Android handles window focus.
    }

    fn reposition_request(
        &mut self,
        _surface: PopupSurface,
        _positioner: PositionerState,
        _token: u32,
    ) {
        // No-op: popups render as subsurfaces, no repositioning needed.
    }
}

impl WlrLayerShellHandler for State {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: LayerSurface,
        _output: Option<WlOutput>,
        layer: Layer,
        namespace: String,
    ) {
        log::info!("New layer surface: namespace={namespace}, layer={layer:?}");
        // Send initial configure with (0,0) — client picks its own size.
        surface.send_configure();
        self.pending_layer_surfaces.push(surface);
    }

    fn layer_destroyed(&mut self, surface: LayerSurface) {
        self.destroyed_layer_surfaces.push(surface);
    }
}

impl SelectionHandler for State {
    type SelectionUserData = ();
}

impl DataDeviceHandler for State {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

// No-op: drag-and-drop not supported between Android and Wayland clients.
impl WaylandDndGrabHandler for State {}

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        match client.get_data::<ClientState>() {
            Some(state) => &state.compositor_state,
            None => {
                panic!("Client has no ClientState attached");
            }
        }
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        // Update PopupManager so newly-mapped popups are tracked on their parent.
        self.popup_manager.commit(surface);
        // Track committed surface so the render loop can set needs_redraw.
        self.committed_surfaces.push(surface.clone());
        // Wake the compositor thread to render the new content.
        if let Some(&fd) = self.wake_fd.as_ref() {
            signal_wake(fd);
        }
    }
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(&mut self, _global: &DmabufGlobal, _dmabuf: Dmabuf, notifier: ImportNotifier) {
        // Accept optimistically — actual import happens at render time via GlesRenderer.
        let _ = notifier.successful::<State>();
    }
}

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, _seat: &Seat<Self>, focused: Option<&WlSurface>) {
        self.text_input_state.focus_changed(focused.cloned());
    }
    // No-op: Android renders its own cursor; compositor doesn't need to track it.
    fn cursor_image(&mut self, _seat: &Seat<Self>, _image: input::pointer::CursorImageStatus) {}
}

pub fn send_frames_surface_tree(surface: &WlSurface, time: u32) {
    with_surface_tree_downward(
        surface,
        (),
        |_, _, &()| TraversalAction::DoChildren(()),
        |_surf, states, &()| {
            // the surface may not have any user_data if it is a subsurface and has not
            // yet been commited
            for callback in states
                .cached_state
                .get::<SurfaceAttributes>()
                .current()
                .frame_callbacks
                .drain(..)
            {
                callback.done(time);
            }
        },
        |_, _, &()| true,
    );
}

pub struct ClientState {
    compositor_state: CompositorClientState,
    alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Default for ClientState {
    fn default() -> Self {
        Self {
            compositor_state: CompositorClientState::default(),
            alive: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }
}

impl ClientState {
    pub fn is_alive(&self) -> bool {
        self.alive.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}

    fn disconnected(&self, client_id: ClientId, reason: DisconnectReason) {
        log::info!("Client disconnected: {client_id:?} reason={reason:?}");
        self.alive.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

impl OutputHandler for State {}

impl FractionalScaleHandler for State {
    fn new_fractional_scale(&mut self, _surface: WlSurface) {
        // Preferred scale is set per-surface when we know the output.
    }
}

impl XdgDecorationHandler for State {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        log::info!("new_decoration: telling client to use server-side decorations");
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(DecorationMode::ServerSide);
        });
        toplevel.send_pending_configure();
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: DecorationMode) {
        log::info!("request_mode: client requested {:?}, forcing ServerSide", mode);
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(DecorationMode::ServerSide);
        });
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        log::info!("unset_mode: forcing ServerSide");
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(DecorationMode::ServerSide);
        });
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }
}

// Macros used to delegate protocol handling to types in the app state.
delegate_xdg_shell!(State);
delegate_xdg_decoration!(State);
delegate_layer_shell!(State);
delegate_compositor!(State);
delegate_shm!(State);
delegate_dmabuf!(State);
delegate_seat!(State);
delegate_data_device!(State);
delegate_output!(State);
delegate_fractional_scale!(State);
smithay::delegate_viewporter!(State);

impl Compositor {
    pub fn build(dmabuf_formats: FormatSet) -> Result<Compositor, Box<dyn Error>> {
        let display = Display::new()?;
        let dh = display.handle();

        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&dh, "Android Wayland");

        let listener = bind_socket()?;
        let clients = Vec::new();

        let start_time = Instant::now();

        let touch = seat.add_touch();
        let pointer = seat.add_pointer();

        let _text_input_global = TextInputState::init(&dh);

        let mut dmabuf_state = DmabufState::new();
        let dmabuf_global = dmabuf_state.create_global::<State>(&dh, dmabuf_formats);
        log::info!("Dmabuf global created");

        let state = State {
            compositor_state: CompositorState::new::<State>(&dh),
            xdg_shell_state: XdgShellState::new::<State>(&dh),
            xdg_decoration_state: XdgDecorationState::new::<State>(&dh),
            layer_shell_state: WlrLayerShellState::new::<State>(&dh),
            fractional_scale_state: FractionalScaleManagerState::new::<State>(&dh),
            viewporter_state: ViewporterState::new::<State>(&dh),
            shm_state: ShmState::new::<State>(&dh, vec![]),
            dmabuf_state,
            dmabuf_global,
            data_device_state: DataDeviceState::new::<State>(&dh),
            seat_state,
            size: (1920, 1080).into(),
            pending_toplevels: Vec::new(),
            pending_layer_surfaces: Vec::new(),
            wake_fd: None,
            committed_surfaces: Vec::new(),
            text_input_state: TextInputState::default(),
            soft_keyboard_request: None,
            destroyed_toplevels: Vec::new(),
            destroyed_layer_surfaces: Vec::new(),
            popup_manager: PopupManager::default(),
        };

        Ok(Compositor {
            state,
            listener,
            clients,
            start_time,
            display,
            seat,
            keyboard: None,
            touch,
            pointer,
            output: None,
        })
    }

    /// Initialize the keyboard. Must be called after xkb data is available.
    pub fn init_keyboard(&mut self) {
        if self.keyboard.is_some() {
            return;
        }
        let xkb_path = std::env::var("XKB_CONFIG_ROOT").unwrap_or_default();
        if !std::path::Path::new(&xkb_path).join("rules").exists() {
            log::warn!("XKB data not found at {xkb_path}, deferring keyboard init");
            return;
        }
        match self.seat.add_keyboard(Default::default(), 1000, 200) {
            Ok(kb) => self.keyboard = Some(kb),
            Err(e) => log::error!("Failed to add keyboard: {e}"),
        }
    }
}
