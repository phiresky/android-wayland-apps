//! Minimal zwp_text_input_v3 handler for Android soft keyboard integration.
//!
//! Tracks enable/disable signals from Wayland clients to show/hide the
//! Android soft keyboard. Full text composition (surrounding text, content
//! hints, pre-edit) is not implemented — Android's InputMethodManager handles
//! those natively, and key events arrive via the existing onKeyDown/onKeyUp path.

use smithay::reexports::{
    wayland_protocols::wp::text_input::zv3::server::{
        zwp_text_input_manager_v3::{self, ZwpTextInputManagerV3},
        zwp_text_input_v3::{self, ZwpTextInputV3},
    },
    wayland_server::{
        backend::{ClientId, GlobalId},
        protocol::wl_surface::WlSurface,
        Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
    },
};

use super::State;

/// Per-resource data for each ZwpTextInputV3 instance.
pub struct TextInputData {
    pending_enable: std::sync::Mutex<Option<bool>>,
}

/// Tracks text_input_v3 instances and focus for soft keyboard control.
#[derive(Default)]
pub struct TextInputState {
    instances: Vec<ZwpTextInputV3>,
    focus: Option<WlSurface>,
}

impl TextInputState {
    /// Register the zwp_text_input_manager_v3 global.
    pub fn init(dh: &DisplayHandle) -> GlobalId {
        dh.create_global::<State, ZwpTextInputManagerV3, _>(1, ())
    }

    /// Update text_input focus when keyboard focus changes.
    /// Sends leave/enter events to matching text_input instances.
    pub fn focus_changed(&mut self, surface: Option<WlSurface>) {
        // Leave old focus
        if let Some(old) = &self.focus {
            if old.is_alive() {
                let old_id = old.id();
                for ti in &self.instances {
                    if ti.is_alive() && ti.id().same_client_as(&old_id) {
                        ti.leave(old);
                    }
                }
            }
        }

        self.focus = surface;

        // Enter new focus
        if let Some(new) = &self.focus {
            if new.is_alive() {
                let new_id = new.id();
                for ti in &self.instances {
                    if ti.is_alive() && ti.id().same_client_as(&new_id) {
                        ti.enter(new);
                    }
                }
            }
        }
    }
}

// --- Protocol dispatch implementations ---

impl GlobalDispatch<ZwpTextInputManagerV3, ()> for State {
    fn bind(
        _state: &mut State,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ZwpTextInputManagerV3>,
        _data: &(),
        data_init: &mut DataInit<'_, State>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwpTextInputManagerV3, ()> for State {
    fn request(
        state: &mut State,
        _client: &Client,
        _resource: &ZwpTextInputManagerV3,
        request: zwp_text_input_manager_v3::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, State>,
    ) {
        match request {
            zwp_text_input_manager_v3::Request::GetTextInput { id, seat: _ } => {
                let instance = data_init.init(
                    id,
                    TextInputData {
                        pending_enable: std::sync::Mutex::new(None),
                    },
                );
                // Send enter if this client already has keyboard focus
                if let Some(focus) = &state.text_input_state.focus {
                    if focus.is_alive() && instance.id().same_client_as(&focus.id()) {
                        instance.enter(focus);
                    }
                }
                state.text_input_state.instances.push(instance);
            }
            zwp_text_input_manager_v3::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl Dispatch<ZwpTextInputV3, TextInputData> for State {
    fn request(
        state: &mut State,
        _client: &Client,
        _resource: &ZwpTextInputV3,
        request: zwp_text_input_v3::Request,
        data: &TextInputData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, State>,
    ) {
        match request {
            zwp_text_input_v3::Request::Enable => {
                if let Ok(mut guard) = data.pending_enable.lock() {
                    *guard = Some(true);
                }
            }
            zwp_text_input_v3::Request::Disable => {
                if let Ok(mut guard) = data.pending_enable.lock() {
                    *guard = Some(false);
                }
            }
            zwp_text_input_v3::Request::Commit => {
                if let Some(enable) = data.pending_enable.lock().ok().and_then(|mut g| g.take()) {
                    log::info!(
                        "text_input_v3: soft keyboard {}",
                        if enable { "show" } else { "hide" }
                    );
                    state.soft_keyboard_request = Some(enable);
                }
            }
            // Ignore surrounding text, content type, cursor rect — Android handles these
            _ => {}
        }
    }

    fn destroyed(
        state: &mut State,
        _client: ClientId,
        resource: &ZwpTextInputV3,
        _data: &TextInputData,
    ) {
        let dead_id = resource.id();
        state
            .text_input_state
            .instances
            .retain(|ti| ti.id() != dead_id);
    }
}
