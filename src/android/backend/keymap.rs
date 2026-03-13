use android_activity::input::Keycode;

/// Convert an Android KeyEvent keycode (raw i32 from JNI) to a keycode for smithay.
/// Smithay expects xkb keycodes (evdev + 8), not raw evdev scancodes,
/// because it subtracts 8 when sending to Wayland clients.
pub fn android_keycode_to_smithay(keycode: i32) -> Option<u32> {
    let android = Keycode::from(keycode as u32);
    android_keycode_to_evdev(android).map(|evdev| evdev + 8)
}

/// Map Android Keycode directly to Linux evdev scancode.
fn android_keycode_to_evdev(keycode: Keycode) -> Option<u32> {
    match keycode {
        // Letters
        Keycode::A => Some(30), Keycode::B => Some(48), Keycode::C => Some(46),
        Keycode::D => Some(32), Keycode::E => Some(18), Keycode::F => Some(33),
        Keycode::G => Some(34), Keycode::H => Some(35), Keycode::I => Some(23),
        Keycode::J => Some(36), Keycode::K => Some(37), Keycode::L => Some(38),
        Keycode::M => Some(50), Keycode::N => Some(49), Keycode::O => Some(24),
        Keycode::P => Some(25), Keycode::Q => Some(16), Keycode::R => Some(19),
        Keycode::S => Some(31), Keycode::T => Some(20), Keycode::U => Some(22),
        Keycode::V => Some(47), Keycode::W => Some(17), Keycode::X => Some(45),
        Keycode::Y => Some(21), Keycode::Z => Some(44),

        // Digits
        Keycode::Keycode0 => Some(11), Keycode::Keycode1 => Some(2),
        Keycode::Keycode2 => Some(3),  Keycode::Keycode3 => Some(4),
        Keycode::Keycode4 => Some(5),  Keycode::Keycode5 => Some(6),
        Keycode::Keycode6 => Some(7),  Keycode::Keycode7 => Some(8),
        Keycode::Keycode8 => Some(9),  Keycode::Keycode9 => Some(10),

        // Numpad
        Keycode::Numpad0 => Some(82), Keycode::Numpad1 => Some(79),
        Keycode::Numpad2 => Some(80), Keycode::Numpad3 => Some(81),
        Keycode::Numpad4 => Some(75), Keycode::Numpad5 => Some(76),
        Keycode::Numpad6 => Some(77), Keycode::Numpad7 => Some(71),
        Keycode::Numpad8 => Some(72), Keycode::Numpad9 => Some(73),
        Keycode::NumpadAdd => Some(78), Keycode::NumpadSubtract => Some(74),
        Keycode::NumpadMultiply => Some(55), Keycode::NumpadDivide => Some(98),
        Keycode::NumpadEnter => Some(96), Keycode::NumpadEquals => Some(117),
        Keycode::NumpadComma => Some(121), Keycode::NumpadDot => Some(83),
        Keycode::NumLock => Some(69),

        // Arrow keys
        Keycode::DpadUp => Some(103), Keycode::DpadDown => Some(108),
        Keycode::DpadLeft => Some(105), Keycode::DpadRight => Some(106),

        // Function keys
        Keycode::F1 => Some(59), Keycode::F2 => Some(60), Keycode::F3 => Some(61),
        Keycode::F4 => Some(62), Keycode::F5 => Some(63), Keycode::F6 => Some(64),
        Keycode::F7 => Some(65), Keycode::F8 => Some(66), Keycode::F9 => Some(67),
        Keycode::F10 => Some(68), Keycode::F11 => Some(87), Keycode::F12 => Some(88),

        // Common keys
        Keycode::Space => Some(57), Keycode::Escape => Some(1),
        Keycode::Enter => Some(28), Keycode::Tab => Some(15),
        Keycode::Del => Some(14), // Backspace
        Keycode::ForwardDel => Some(111), // Delete

        // Navigation
        Keycode::PageUp => Some(104), Keycode::PageDown => Some(109),
        Keycode::MoveHome => Some(102), Keycode::MoveEnd => Some(107),
        Keycode::Insert => Some(110),

        // Punctuation
        Keycode::Comma => Some(51), Keycode::Period => Some(52),
        Keycode::Minus => Some(12), Keycode::Equals => Some(13),
        Keycode::LeftBracket => Some(26), Keycode::RightBracket => Some(27),
        Keycode::Backslash => Some(43), Keycode::Semicolon => Some(39),
        Keycode::Apostrophe => Some(40), Keycode::Grave => Some(41),
        Keycode::Slash => Some(53),

        // Modifiers
        Keycode::AltLeft => Some(56), Keycode::AltRight => Some(100),
        Keycode::ShiftLeft => Some(42), Keycode::ShiftRight => Some(54),
        Keycode::CtrlLeft => Some(29), Keycode::CtrlRight => Some(97),
        Keycode::CapsLock => Some(58), Keycode::ScrollLock => Some(70),
        Keycode::MetaLeft => Some(125), Keycode::MetaRight => Some(126),

        // Media
        Keycode::MediaNext => Some(163), Keycode::MediaPrevious => Some(165),
        Keycode::MediaPlayPause => Some(164), Keycode::MediaStop => Some(166),
        Keycode::VolumeUp => Some(115), Keycode::VolumeDown => Some(114),
        Keycode::VolumeMute => Some(113),

        // Misc
        Keycode::Sysrq => Some(99), // PrintScreen
        Keycode::Break => Some(119), // Pause

        _ => None,
    }
}
