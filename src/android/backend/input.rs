use crate::android::backend::WaylandBackend;
use crate::android::window_manager::SurfaceKind;
use smithay::backend::input::ButtonState;
use smithay::desktop::{utils::under_from_surface_tree, PopupManager, WindowSurfaceType};
use smithay::input::keyboard::FilterResult;
use smithay::input::pointer;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Point, SERIAL_COUNTER};
use smithay::wayland::compositor as wl_compositor;
use smithay::wayland::shell::xdg::SurfaceCachedState;

// Linux input event button codes.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

// Android MotionEvent action constants.
const ACTION_DOWN: i32 = 0;
const ACTION_UP: i32 = 1;
const ACTION_MOVE: i32 = 2;
const ACTION_HOVER_MOVE: i32 = 7;
const ACTION_HOVER_ENTER: i32 = 9;

/// Look up the Wayland surface under a touch point, converting physical Android
/// coordinates to logical Wayland coordinates. Checks popups first, then the
/// main surface tree, so menu clicks are routed to the popup surface.
pub(crate) fn resolve_surface_and_coords(
    backend: &WaylandBackend,
    window_id: u32,
    x: f32,
    y: f32,
) -> Option<(WlSurface, f64, f64)> {
    let wm = backend.window_manager.as_ref()?;
    let window = wm.windows.get(&window_id)?;
    let root_surface = window.surface_kind.wl_surface().clone();
    let geo_offset: Point<i32, _> = match &window.surface_kind {
        SurfaceKind::Toplevel(_) => {
            wl_compositor::with_states(&root_surface, |states| {
                states.cached_state.get::<SurfaceCachedState>()
                    .current()
                    .geometry
                    .map(|g| g.loc)
                    .unwrap_or_default()
            })
        }
        SurfaceKind::Layer(_) => Default::default(),
    };
    // Convert from physical (Android) to logical (Wayland surface) coordinates.
    // Touch position 0,0 in the Activity corresponds to geo_offset in the surface.
    // If content is centered vertically (due to DeX minimum height), subtract that offset.
    let scale = backend.scale_factor;
    let center_y_logical = window.preferred_size.map(|p| {
        let preferred_h = (p.h as f64 * scale).round() as i32;
        ((window.size.h - preferred_h) / 2).max(0) as f64 / scale
    }).unwrap_or(0.0);
    let lx = x as f64 / scale + geo_offset.x as f64;
    let ly = y as f64 / scale - center_y_logical + geo_offset.y as f64;
    let point: Point<f64, _> = (lx, ly).into();

    // Check popups first — menus/dropdowns take priority over the main surface.
    for (popup, popup_offset) in PopupManager::popups_for_surface(&root_surface) {
        let offset = geo_offset + popup_offset - popup.geometry().loc;
        if let Some((surface, surface_offset)) = under_from_surface_tree(
            popup.wl_surface(),
            point,
            offset,
            WindowSurfaceType::ALL,
        ) {
            let sx = point.x - surface_offset.x as f64;
            let sy = point.y - surface_offset.y as f64;
            return Some((surface, sx, sy));
        }
    }

    // Fall back to the main surface tree (toplevel + subsurfaces).
    if let Some((surface, surface_offset)) = under_from_surface_tree(
        &root_surface,
        point,
        (0, 0),
        WindowSurfaceType::ALL,
    ) {
        let sx = point.x - surface_offset.x as f64;
        let sy = point.y - surface_offset.y as f64;
        return Some((surface, sx, sy));
    }

    // Nothing under the point — fall back to root surface at logical coords.
    Some((root_surface, lx, ly))
}

/// Handle touch events from a WaylandWindowActivity.
pub(crate) fn handle_activity_touch(
    backend: &mut WaylandBackend,
    window_id: u32,
    action: i32,
    x: f32,
    y: f32,
) {
    let Some((wl_surface, x, y)) = resolve_surface_and_coords(backend, window_id, x, y) else {
        return;
    };

    let compositor = &mut backend.compositor;
    let serial = SERIAL_COUNTER.next_serial();
    let time = compositor.start_time.elapsed().as_millis() as u32;
    let pointer = compositor.pointer.clone();

    match action {
        ACTION_DOWN => {
            if let Some(kb) = &compositor.keyboard {
                kb.set_focus(
                    &mut compositor.state,
                    Some(wl_surface.clone()),
                    serial,
                );
            }
            pointer.motion(
                &mut compositor.state,
                Some((wl_surface.clone(), (0f64, 0f64).into())),
                &pointer::MotionEvent {
                    location: (x, y).into(),
                    serial,
                    time,
                },
            );
            pointer.button(
                &mut compositor.state,
                &pointer::ButtonEvent {
                    button: BTN_LEFT,
                    state: ButtonState::Pressed,
                    serial,
                    time,
                },
            );
            pointer.frame(&mut compositor.state);
        }
        ACTION_UP => {
            pointer.button(
                &mut compositor.state,
                &pointer::ButtonEvent {
                    button: BTN_LEFT,
                    state: ButtonState::Released,
                    serial,
                    time,
                },
            );
            pointer.frame(&mut compositor.state);
        }
        ACTION_MOVE | ACTION_HOVER_MOVE | ACTION_HOVER_ENTER => {
            pointer.motion(
                &mut compositor.state,
                Some((wl_surface.clone(), (0f64, 0f64).into())),
                &pointer::MotionEvent {
                    location: (x, y).into(),
                    serial,
                    time,
                },
            );
            pointer.frame(&mut compositor.state);
        }
        _ => {}
    }
}

/// Handle a right-click from the long-press context menu.
/// Sends pointer motion + BTN_RIGHT press + release to the Wayland client.
pub(crate) fn handle_activity_right_click(
    backend: &mut WaylandBackend,
    window_id: u32,
    x: f32,
    y: f32,
) {
    let Some((wl_surface, x, y)) = resolve_surface_and_coords(backend, window_id, x, y) else {
        return;
    };

    let compositor = &mut backend.compositor;
    let serial = SERIAL_COUNTER.next_serial();
    let time = compositor.start_time.elapsed().as_millis() as u32;
    let pointer = compositor.pointer.clone();

    pointer.motion(
        &mut compositor.state,
        Some((wl_surface.clone(), (0f64, 0f64).into())),
        &pointer::MotionEvent {
            location: (x, y).into(),
            serial,
            time,
        },
    );
    pointer.button(
        &mut compositor.state,
        &pointer::ButtonEvent {
            button: BTN_RIGHT,
            state: ButtonState::Pressed,
            serial,
            time,
        },
    );
    let serial = SERIAL_COUNTER.next_serial();
    pointer.button(
        &mut compositor.state,
        &pointer::ButtonEvent {
            button: BTN_RIGHT,
            state: ButtonState::Released,
            serial,
            time,
        },
    );
    pointer.frame(&mut compositor.state);
}

/// Handle key events from a WaylandWindowActivity.
pub(crate) fn handle_activity_key(
    backend: &mut WaylandBackend,
    window_id: u32,
    key_code: i32,
    action: i32,
    meta_state: i32,
) {
    let Some(wm) = backend.window_manager.as_ref() else { return };
    let Some(window) = wm.windows.get(&window_id) else { return };
    let wl_surface = window.surface_kind.wl_surface().clone();

    let compositor = &mut backend.compositor;
    let serial = SERIAL_COUNTER.next_serial();
    let time = compositor.start_time.elapsed().as_millis() as u32;

    // Set keyboard focus only if this surface doesn't already have it.
    // Calling set_focus on every key event floods the client with keymap data and causes ANR.
    if let Some(kb) = &compositor.keyboard {
        let needs_focus = kb.current_focus().as_ref() != Some(&wl_surface);
        if needs_focus {
            kb.set_focus(&mut compositor.state, Some(wl_surface.clone()), serial);
        }
    }

    let Some(linux_keycode) = super::keymap::android_keycode_to_smithay(key_code) else {
        tracing::debug!("Unmapped Android keycode: {}", key_code);
        return;
    };
    let key_state = if action == ACTION_DOWN {
        smithay::backend::input::KeyState::Pressed
    } else {
        smithay::backend::input::KeyState::Released
    };

    if let Some(kb) = &compositor.keyboard {
        // Sync modifier state from Android's meta_state to prevent stuck modifiers.
        // If Android says a modifier isn't held but xkb thinks it is, release it.
        // This handles cases where modifier UP events are lost (app restart, focus change).
        const META_CTRL_ON: i32 = 0x1000;
        const META_SHIFT_ON: i32 = 0x1;
        const META_ALT_ON: i32 = 0x2;
        const META_META_ON: i32 = 0x10000;
        let xkb_mods = kb.modifier_state();
        let checks: [(i32, bool, u32); 4] = [
            (META_CTRL_ON, xkb_mods.ctrl, 29 + 8),   // KEY_LEFTCTRL
            (META_SHIFT_ON, xkb_mods.shift, 42 + 8),  // KEY_LEFTSHIFT
            (META_ALT_ON, xkb_mods.alt, 56 + 8),      // KEY_LEFTALT
            (META_META_ON, xkb_mods.logo, 125 + 8),   // KEY_LEFTMETA
        ];
        for (android_flag, xkb_active, mod_keycode) in checks {
            if xkb_active && (meta_state & android_flag == 0) {
                let serial = SERIAL_COUNTER.next_serial();
                kb.input::<(), _>(
                    &mut compositor.state,
                    mod_keycode.into(),
                    smithay::backend::input::KeyState::Released,
                    serial,
                    time,
                    |_, _, _| FilterResult::Forward,
                );
            }
        }

        kb.input::<(), _>(
            &mut compositor.state,
            linux_keycode.into(),
            key_state,
            serial,
            time,
            |_, _, _| FilterResult::Forward,
        );
    }
}
