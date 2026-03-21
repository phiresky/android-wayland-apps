//! zwp_text_input_v3 handler for Android soft keyboard integration.
//!
//! When a Wayland client enables text_input_v3, IME input is forwarded via
//! the protocol (preedit_string, commit_string, delete_surrounding_text).
//! When text_input_v3 is not active, IME input falls back to synthetic
//! wl_keyboard key events.

use smithay::reexports::{
    wayland_protocols::wp::text_input::zv3::server::{
        zwp_text_input_manager_v3::{self, ZwpTextInputManagerV3},
        zwp_text_input_v3::{self, ContentHint, ContentPurpose, ZwpTextInputV3},
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
    pending_content_type: std::sync::Mutex<Option<(ContentHint, ContentPurpose)>>,
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

    /// Turn already-committed text back into preedit (for retroactive corrections).
    /// Sends delete_surrounding_text + preedit_string + done as one atomic group.
    pub fn send_recompose(&mut self, text: &str) {
        if let Some(active) = &mut self.active {
            active.instance.delete_surrounding_text(text.len() as u32, 0);
            let cursor = text.len() as i32;
            active.instance.preedit_string(Some(text.to_string()), cursor, cursor);
            active.done_serial += 1;
            active.instance.done(active.done_serial);
        }
    }
}

// --- Android InputType translation ---

// android.text.InputType constants
const TYPE_CLASS_TEXT: i32 = 0x1;
const TYPE_CLASS_NUMBER: i32 = 0x2;
const TYPE_CLASS_PHONE: i32 = 0x3;
const TYPE_CLASS_DATETIME: i32 = 0x4;

const TYPE_TEXT_FLAG_CAP_CHARACTERS: i32 = 0x1000;
const TYPE_TEXT_FLAG_CAP_WORDS: i32 = 0x2000;
const TYPE_TEXT_FLAG_CAP_SENTENCES: i32 = 0x4000;
const TYPE_TEXT_FLAG_AUTO_CORRECT: i32 = 0x8000;
const TYPE_TEXT_FLAG_AUTO_COMPLETE: i32 = 0x10000;
const TYPE_TEXT_FLAG_MULTI_LINE: i32 = 0x20000;
const TYPE_TEXT_FLAG_NO_SUGGESTIONS: i32 = 0x80000;

const TYPE_TEXT_VARIATION_URI: i32 = 0x10;
const TYPE_TEXT_VARIATION_EMAIL_ADDRESS: i32 = 0x20;
const TYPE_TEXT_VARIATION_PERSON_NAME: i32 = 0x60;
const TYPE_TEXT_VARIATION_PASSWORD: i32 = 0x80;

const TYPE_NUMBER_FLAG_SIGNED: i32 = 0x1000;
const TYPE_NUMBER_FLAG_DECIMAL: i32 = 0x2000;

const TYPE_DATETIME_VARIATION_DATE: i32 = 0x10;
const TYPE_DATETIME_VARIATION_TIME: i32 = 0x20;

/// Default Android InputType when no content type is set by the client.
pub const DEFAULT_ANDROID_INPUT_TYPE: i32 =
    TYPE_CLASS_TEXT | TYPE_TEXT_FLAG_AUTO_CORRECT | TYPE_TEXT_FLAG_MULTI_LINE;

/// Translate Wayland zwp_text_input_v3 content hint/purpose to Android InputType flags.
fn translate_content_type(hint: ContentHint, purpose: ContentPurpose) -> i32 {
    // Map purpose to base InputType class + variation
    let mut input_type = match purpose {
        ContentPurpose::Normal => TYPE_CLASS_TEXT,
        ContentPurpose::Alpha => TYPE_CLASS_TEXT,
        ContentPurpose::Digits => TYPE_CLASS_NUMBER,
        ContentPurpose::Number => TYPE_CLASS_NUMBER | TYPE_NUMBER_FLAG_SIGNED | TYPE_NUMBER_FLAG_DECIMAL,
        ContentPurpose::Phone => TYPE_CLASS_PHONE,
        ContentPurpose::Url => TYPE_CLASS_TEXT | TYPE_TEXT_VARIATION_URI,
        ContentPurpose::Email => TYPE_CLASS_TEXT | TYPE_TEXT_VARIATION_EMAIL_ADDRESS,
        ContentPurpose::Name => TYPE_CLASS_TEXT | TYPE_TEXT_VARIATION_PERSON_NAME,
        ContentPurpose::Password => TYPE_CLASS_TEXT | TYPE_TEXT_VARIATION_PASSWORD,
        ContentPurpose::Pin => TYPE_CLASS_NUMBER,
        ContentPurpose::Date => TYPE_CLASS_DATETIME | TYPE_DATETIME_VARIATION_DATE,
        ContentPurpose::Time => TYPE_CLASS_DATETIME | TYPE_DATETIME_VARIATION_TIME,
        ContentPurpose::Datetime => TYPE_CLASS_DATETIME,
        ContentPurpose::Terminal => TYPE_CLASS_TEXT | TYPE_TEXT_FLAG_NO_SUGGESTIONS,
        _ => TYPE_CLASS_TEXT,
    };

    // Apply hint flags (only meaningful for text class)
    if input_type & 0xF == TYPE_CLASS_TEXT {
        if hint.contains(ContentHint::Completion) {
            input_type |= TYPE_TEXT_FLAG_AUTO_COMPLETE;
        }
        if hint.contains(ContentHint::Spellcheck) {
            input_type |= TYPE_TEXT_FLAG_AUTO_CORRECT;
        }
        if hint.contains(ContentHint::AutoCapitalization) {
            input_type |= TYPE_TEXT_FLAG_CAP_SENTENCES;
        }
        if hint.contains(ContentHint::Uppercase) {
            input_type |= TYPE_TEXT_FLAG_CAP_CHARACTERS;
        }
        if hint.contains(ContentHint::Titlecase) {
            input_type |= TYPE_TEXT_FLAG_CAP_WORDS;
        }
        if hint.contains(ContentHint::Multiline) {
            input_type |= TYPE_TEXT_FLAG_MULTI_LINE;
        }
        if hint.contains(ContentHint::HiddenText) || hint.contains(ContentHint::SensitiveData) {
            input_type |= TYPE_TEXT_FLAG_NO_SUGGESTIONS;
        }
    }

    tracing::debug!(
        "text_input_v3: translated content type -> Android InputType 0x{:x}",
        input_type
    );
    input_type
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
                        pending_content_type: std::sync::Mutex::new(None),
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
                if let (Ok(h), Ok(p)) = (hint.into_result(), purpose.into_result()) {
                    if let Ok(mut guard) = data.pending_content_type.lock() {
                        *guard = Some((h, p));
                    }
                }
            }
            zwp_text_input_v3::Request::SetCursorRectangle { x, y, width, height } => {
                tracing::debug!(
                    "text_input_v3: cursor_rectangle {}x{} at ({},{})",
                    width, height, x, y
                );
            }
            zwp_text_input_v3::Request::Commit => {
                let content_type = data.pending_content_type.lock().ok().and_then(|mut g| g.take());
                if let Some(enable) = data.pending_enable.lock().ok().and_then(|mut g| g.take()) {
                    tracing::info!(
                        "text_input_v3: soft keyboard {}",
                        if enable { "show" } else { "hide" }
                    );
                    let android_input_type = content_type
                        .map(|(h, p)| translate_content_type(h, p))
                        .unwrap_or(DEFAULT_ANDROID_INPUT_TYPE);
                    if enable {
                        state.text_input_state.active = Some(ActiveTextInput {
                            instance: resource.clone(),
                            done_serial: 0,
                        });
                    } else {
                        state.text_input_state.active = None;
                        state.text_input_state.composing_text.clear();
                    }
                    state.soft_keyboard_request = Some((enable, android_input_type));
                } else if let Some((h, p)) = content_type {
                    // Content type changed while already active — trigger restart.
                    if state.text_input_state.is_active() {
                        let android_input_type = translate_content_type(h, p);
                        state.soft_keyboard_request = Some((true, android_input_type));
                    }
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
