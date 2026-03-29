use crate::android::backend::WaylandBackend;
use smithay::input::keyboard::FilterResult;
use smithay::utils::SERIAL_COUNTER;

// ============================================================
// IME text input handlers (composing / commit / delete)
// ============================================================

/// Ensure the keyboard is focused on the given window's surface.
pub(crate) fn ensure_ime_focus(backend: &mut WaylandBackend, window_id: u32) {
    let Some(wm) = backend.window_manager.as_ref() else { return };
    let Some(window) = wm.windows.get(&window_id) else { return };
    let wl_surface = window.surface_kind.wl_surface().clone();
    let compositor = &mut backend.compositor;
    if let Some(kb) = &compositor.keyboard {
        if kb.current_focus().as_ref() != Some(&wl_surface) {
            let serial = SERIAL_COUNTER.next_serial();
            kb.set_focus(&mut compositor.state, Some(wl_surface), serial);
        }
    }
}

/// Handle composing (preedit) text from Android IME.
pub(crate) fn handle_ime_composing(backend: &mut WaylandBackend, window_id: u32, text: String) {
    ensure_ime_focus(backend, window_id);

    if backend.compositor.state.text_input_state.is_active() {
        // text_input_v3 path: send preedit_string + done
        backend.compositor.state.text_input_state.send_preedit(&text);
        backend.compositor.state.text_input_state.composing_text = text;
    } else {
        // Key-event fallback: diff against previous composing text
        let old = std::mem::replace(
            &mut backend.compositor.state.text_input_state.composing_text,
            text.clone(),
        );
        let common = common_prefix_byte_len(&old, &text);
        let del = old[common..].chars().count();
        let ins = &text[common..];
        send_ime_key_events(backend, del, 0, ins);
    }
}

/// Handle committed text from Android IME.
pub(crate) fn handle_ime_commit(backend: &mut WaylandBackend, window_id: u32, text: String) {
    ensure_ime_focus(backend, window_id);

    if backend.compositor.state.text_input_state.is_active() {
        // text_input_v3 path: send commit_string + done
        backend.compositor.state.text_input_state.send_commit(&text);
        backend.compositor.state.text_input_state.composing_text.clear();
    } else {
        // Key-event fallback: diff against composing text, then clear
        let old = std::mem::take(&mut backend.compositor.state.text_input_state.composing_text);
        let common = common_prefix_byte_len(&old, &text);
        let del = old[common..].chars().count();
        let ins = &text[common..];
        send_ime_key_events(backend, del, 0, ins);
    }
}

/// Handle delete surrounding text from Android IME (e.g. backspace).
/// `deleted_text` is the actual text being deleted (from the Editable), used for
/// correct UTF-8 byte counts in text_input_v3 (emoji = 4 bytes, not 1).
pub(crate) fn handle_ime_delete(backend: &mut WaylandBackend, window_id: u32, before: i32, after: i32, deleted_text: &str) {
    ensure_ime_focus(backend, window_id);

    if backend.compositor.state.text_input_state.is_active() {
        // text_input_v3 path: use actual UTF-8 byte length for correct emoji handling
        let before_bytes = if deleted_text.is_empty() { before as u32 } else { deleted_text.len() as u32 };
        backend.compositor.state.text_input_state.send_delete(before_bytes, after as u32);
    } else {
        // Key-event fallback: use character counts (one backspace per char)
        send_ime_key_events(backend, before as usize, after as usize, "");
    }
}

/// Handle setComposingRegion: already-committed text is being turned back into composing.
/// For text_input_v3: delete the committed text and re-show as preedit.
/// For key-event fallback: just update tracking (text is already on screen).
pub(crate) fn handle_ime_recompose(backend: &mut WaylandBackend, window_id: u32, text: String) {
    ensure_ime_focus(backend, window_id);

    if backend.compositor.state.text_input_state.is_active() {
        // text_input_v3: atomically delete committed text and show as preedit
        backend.compositor.state.text_input_state.send_recompose(&text);
    }
    // Both paths: update composing text tracking (no key events needed)
    backend.compositor.state.text_input_state.composing_text = text;
}

/// Find the byte length of the common character prefix between two strings.
pub(crate) fn common_prefix_byte_len(a: &str, b: &str) -> usize {
    a.chars()
        .zip(b.chars())
        .take_while(|(ca, cb)| ca == cb)
        .map(|(c, _)| c.len_utf8())
        .sum()
}

/// Send synthetic wl_keyboard events: backspaces, forward deletes, then characters.
/// Used as fallback when text_input_v3 is not active.
pub(crate) fn send_ime_key_events(
    backend: &mut WaylandBackend,
    delete_before: usize,
    delete_after: usize,
    text: &str,
) {
    let compositor = &mut backend.compositor;
    let time = compositor.start_time.elapsed().as_millis() as u32;

    let Some(kb) = &compositor.keyboard else { return };

    const BACKSPACE: u32 = 14 + 8;
    for _ in 0..delete_before {
        let serial = SERIAL_COUNTER.next_serial();
        kb.input::<(), _>(&mut compositor.state, BACKSPACE.into(),
            smithay::backend::input::KeyState::Pressed, serial, time,
            |_, _, _| FilterResult::Forward);
        let serial = SERIAL_COUNTER.next_serial();
        kb.input::<(), _>(&mut compositor.state, BACKSPACE.into(),
            smithay::backend::input::KeyState::Released, serial, time,
            |_, _, _| FilterResult::Forward);
    }

    const DELETE: u32 = 111 + 8;
    for _ in 0..delete_after {
        let serial = SERIAL_COUNTER.next_serial();
        kb.input::<(), _>(&mut compositor.state, DELETE.into(),
            smithay::backend::input::KeyState::Pressed, serial, time,
            |_, _, _| FilterResult::Forward);
        let serial = SERIAL_COUNTER.next_serial();
        kb.input::<(), _>(&mut compositor.state, DELETE.into(),
            smithay::backend::input::KeyState::Released, serial, time,
            |_, _, _| FilterResult::Forward);
    }

    const SHIFT: u32 = 42 + 8;
    for ch in text.chars() {
        if let Some((evdev, shift)) = super::keymap::char_to_evdev_key(ch) {
            let code = evdev + 8;
            if shift {
                let serial = SERIAL_COUNTER.next_serial();
                kb.input::<(), _>(&mut compositor.state, SHIFT.into(),
                    smithay::backend::input::KeyState::Pressed, serial, time,
                    |_, _, _| FilterResult::Forward);
            }
            let serial = SERIAL_COUNTER.next_serial();
            kb.input::<(), _>(&mut compositor.state, code.into(),
                smithay::backend::input::KeyState::Pressed, serial, time,
                |_, _, _| FilterResult::Forward);
            let serial = SERIAL_COUNTER.next_serial();
            kb.input::<(), _>(&mut compositor.state, code.into(),
                smithay::backend::input::KeyState::Released, serial, time,
                |_, _, _| FilterResult::Forward);
            if shift {
                let serial = SERIAL_COUNTER.next_serial();
                kb.input::<(), _>(&mut compositor.state, SHIFT.into(),
                    smithay::backend::input::KeyState::Released, serial, time,
                    |_, _, _| FilterResult::Forward);
            }
        } else {
            tracing::debug!("IME: unmapped char {:?} (U+{:04X})", ch, ch as u32);
        }
    }
}
