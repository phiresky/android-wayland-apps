/// Convert an Android KeyEvent keycode (raw i32 from JNI) to a keycode for smithay.
/// Smithay expects xkb keycodes (evdev + 8), not raw evdev scancodes,
/// because it subtracts 8 when sending to Wayland clients.
pub fn android_keycode_to_smithay(keycode: i32) -> Option<u32> {
    android_keycode_to_evdev(keycode).map(|evdev| evdev + 8)
}

/// Map Android KeyEvent keycode (AKEYCODE_*) directly to Linux evdev scancode.
/// Values from android/keycodes.h.
fn android_keycode_to_evdev(keycode: i32) -> Option<u32> {
    match keycode {
        // Letters (AKEYCODE_A=29 .. AKEYCODE_Z=54)
        29 => Some(30), 30 => Some(48), 31 => Some(46), // A, B, C
        32 => Some(32), 33 => Some(18), 34 => Some(33), // D, E, F
        35 => Some(34), 36 => Some(35), 37 => Some(23), // G, H, I
        38 => Some(36), 39 => Some(37), 40 => Some(38), // J, K, L
        41 => Some(50), 42 => Some(49), 43 => Some(24), // M, N, O
        44 => Some(25), 45 => Some(16), 46 => Some(19), // P, Q, R
        47 => Some(31), 48 => Some(20), 49 => Some(22), // S, T, U
        50 => Some(47), 51 => Some(17), 52 => Some(45), // V, W, X
        53 => Some(21), 54 => Some(44),                  // Y, Z

        // Digits (AKEYCODE_0=7 .. AKEYCODE_9=16)
        7 => Some(11),  8 => Some(2),  9 => Some(3),
        10 => Some(4),  11 => Some(5), 12 => Some(6),
        13 => Some(7),  14 => Some(8), 15 => Some(9),
        16 => Some(10),

        // Numpad (AKEYCODE_NUMPAD_0=144 .. AKEYCODE_NUMPAD_9=153)
        144 => Some(82), 145 => Some(79), 146 => Some(80), 147 => Some(81),
        148 => Some(75), 149 => Some(76), 150 => Some(77), 151 => Some(71),
        152 => Some(72), 153 => Some(73),
        157 => Some(78),  // NUMPAD_ADD
        156 => Some(74),  // NUMPAD_SUBTRACT
        155 => Some(55),  // NUMPAD_MULTIPLY
        154 => Some(98),  // NUMPAD_DIVIDE
        160 => Some(96),  // NUMPAD_ENTER
        161 => Some(117), // NUMPAD_EQUALS
        159 => Some(121), // NUMPAD_COMMA
        158 => Some(83),  // NUMPAD_DOT
        143 => Some(69),  // NUM_LOCK

        // Arrow keys
        19 => Some(103), // DPAD_UP
        20 => Some(108), // DPAD_DOWN
        21 => Some(105), // DPAD_LEFT
        22 => Some(106), // DPAD_RIGHT

        // Function keys (AKEYCODE_F1=131 .. AKEYCODE_F12=142)
        131 => Some(59), 132 => Some(60), 133 => Some(61),
        134 => Some(62), 135 => Some(63), 136 => Some(64),
        137 => Some(65), 138 => Some(66), 139 => Some(67),
        140 => Some(68), 141 => Some(87), 142 => Some(88),

        // Common keys
        62 => Some(57),   // SPACE
        111 => Some(1),   // ESCAPE
        66 => Some(28),   // ENTER
        61 => Some(15),   // TAB
        67 => Some(14),   // DEL (Backspace)
        112 => Some(111), // FORWARD_DEL (Delete)

        // Navigation
        92 => Some(104),  // PAGE_UP
        93 => Some(109),  // PAGE_DOWN
        122 => Some(102), // MOVE_HOME
        123 => Some(107), // MOVE_END
        124 => Some(110), // INSERT

        // Punctuation
        55 => Some(51),  // COMMA
        56 => Some(52),  // PERIOD
        69 => Some(12),  // MINUS
        70 => Some(13),  // EQUALS
        71 => Some(26),  // LEFT_BRACKET
        72 => Some(27),  // RIGHT_BRACKET
        73 => Some(43),  // BACKSLASH
        74 => Some(39),  // SEMICOLON
        75 => Some(40),  // APOSTROPHE
        68 => Some(41),  // GRAVE
        76 => Some(53),  // SLASH

        // Modifiers
        57 => Some(56),   // ALT_LEFT
        58 => Some(100),  // ALT_RIGHT
        59 => Some(42),   // SHIFT_LEFT
        60 => Some(54),   // SHIFT_RIGHT
        113 => Some(29),  // CTRL_LEFT
        114 => Some(97),  // CTRL_RIGHT
        115 => Some(58),  // CAPS_LOCK
        116 => Some(70),  // SCROLL_LOCK
        117 => Some(125), // META_LEFT
        118 => Some(126), // META_RIGHT

        // Media
        87 => Some(163),  // MEDIA_NEXT
        88 => Some(165),  // MEDIA_PREVIOUS
        85 => Some(164),  // MEDIA_PLAY_PAUSE
        86 => Some(166),  // MEDIA_STOP
        24 => Some(115),  // VOLUME_UP
        25 => Some(114),  // VOLUME_DOWN
        164 => Some(113), // VOLUME_MUTE

        // Misc
        120 => Some(99),  // SYSRQ (PrintScreen)
        121 => Some(119), // BREAK (Pause)

        _ => None,
    }
}

/// Map a Unicode character to `(evdev_keycode, shift_needed)` for US QWERTY layout.
/// Used to convert IME committed text into synthetic wl_keyboard key events.
pub fn char_to_evdev_key(ch: char) -> Option<(u32, bool)> {
    match ch {
        'a'..='z' => {
            const KEYS: [u32; 26] = [
                30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50,
                49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45, 21, 44,
            ];
            Some((KEYS[(ch as u32 - 'a' as u32) as usize], false))
        }
        'A'..='Z' => char_to_evdev_key(ch.to_ascii_lowercase()).map(|(k, _)| (k, true)),

        '0' => Some((11, false)),
        '1'..='9' => Some((ch as u32 - '0' as u32 + 1, false)),

        ' ' => Some((57, false)),
        '\n' => Some((28, false)),
        '\t' => Some((15, false)),

        '-' => Some((12, false)),
        '=' => Some((13, false)),
        '[' => Some((26, false)),
        ']' => Some((27, false)),
        '\\' => Some((43, false)),
        ';' => Some((39, false)),
        '\'' => Some((40, false)),
        '`' => Some((41, false)),
        ',' => Some((51, false)),
        '.' => Some((52, false)),
        '/' => Some((53, false)),

        '!' => Some((2, true)),
        '@' => Some((3, true)),
        '#' => Some((4, true)),
        '$' => Some((5, true)),
        '%' => Some((6, true)),
        '^' => Some((7, true)),
        '&' => Some((8, true)),
        '*' => Some((9, true)),
        '(' => Some((10, true)),
        ')' => Some((11, true)),
        '_' => Some((12, true)),
        '+' => Some((13, true)),
        '{' => Some((26, true)),
        '}' => Some((27, true)),
        '|' => Some((43, true)),
        ':' => Some((39, true)),
        '"' => Some((40, true)),
        '~' => Some((41, true)),
        '<' => Some((51, true)),
        '>' => Some((52, true)),
        '?' => Some((53, true)),

        _ => None,
    }
}
