//! zwp_text_input_v3 handler for Android soft keyboard integration.
//!
//! When a Wayland client enables text_input_v3, IME input is forwarded via
//! the protocol (preedit_string, commit_string, delete_surrounding_text).
//! When text_input_v3 is not active, IME input falls back to synthetic
//! wl_keyboard key events.

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

/// Active text_input_v3 session for protocol-based IME forwarding.
pub struct ActiveTextInput {
    pub instance: ZwpTextInputV3,
    pub done_serial: u32,
}

/// Tracks text_input_v3 instances and focus for soft keyboard control.
#[derive(Default)]
pub struct TextInputState {
    instances: Vec<ZwpTextInputV3>,
    focus: Option<WlSurface>,
    /// Currently active (enabled) text_input instance.
    pub active: Option<ActiveTextInput>,
    /// Composing text tracked for key-event fallback path.
    pub composing_text: String,
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

    /// Whether a text_input_v3 session is active (client enabled it).
    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    /// Send preedit_string + done for composing text updates.
    pub fn send_preedit(&mut self, text: &str) {
        if let Some(active) = &mut self.active {
            if text.is_empty() {
                active.instance.preedit_string(None, 0, 0);
            } else {
                let cursor = text.len() as i32;
                active.instance.preedit_string(Some(text.to_string()), cursor, cursor);
            }
            active.done_serial += 1;
            active.instance.done(active.done_serial);
        }
    }

    /// Send commit_string + clear preedit + done for committed text.
    pub fn send_commit(&mut self, text: &str) {
        if let Some(active) = &mut self.active {
            active.instance.preedit_string(None, 0, 0);
            active.instance.commit_string(Some(text.to_string()));
            active.done_serial += 1;
            active.instance.done(active.done_serial);
        }
    }

    /// Send delete_surrounding_text + done.
    /// Counts are in bytes (callers should provide byte counts).
    pub fn send_delete(&mut self, before_bytes: u32, after_bytes: u32) {
        if let Some(active) = &mut self.active {
            active.instance.delete_surrounding_text(before_bytes, after_bytes);
            active.done_serial += 1;
            active.instance.done(active.done_serial);
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
        resource: &ZwpTextInputV3,
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
            zwp_text_input_v3::Request::SetSurroundingText { text, cursor, anchor } => {
                tracing::debug!(
                    "text_input_v3: surrounding_text len={} cursor={} anchor={}",
                    text.len(), cursor, anchor
                );
            }
            zwp_text_input_v3::Request::SetTextChangeCause { .. } => {}
            zwp_text_input_v3::Request::SetContentType { hint, purpose } => {
                tracing::debug!("text_input_v3: content_type hint={:?} purpose={:?}", hint, purpose);
            }
            zwp_text_input_v3::Request::SetCursorRectangle { x, y, width, height } => {
                tracing::debug!(
                    "text_input_v3: cursor_rectangle {}x{} at ({},{})",
                    width, height, x, y
                );
            }
            zwp_text_input_v3::Request::Commit => {
                if let Some(enable) = data.pending_enable.lock().ok().and_then(|mut g| g.take()) {
                    tracing::info!(
                        "text_input_v3: soft keyboard {}",
                        if enable { "show" } else { "hide" }
                    );
                    if enable {
                        state.text_input_state.active = Some(ActiveTextInput {
                            instance: resource.clone(),
                            done_serial: 0,
                        });
                    } else {
                        state.text_input_state.active = None;
                        state.text_input_state.composing_text.clear();
                    }
                    state.soft_keyboard_request = Some(enable);
                }
            }
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
        // Clear active if this instance was active
        if state.text_input_state.active.as_ref().is_some_and(|a| a.instance.id() == dead_id) {
            state.text_input_state.active = None;
            state.text_input_state.composing_text.clear();
        }
        state
            .text_input_state
            .instances
            .retain(|ti| ti.id() != dead_id);
    }
}
