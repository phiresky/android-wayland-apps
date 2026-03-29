/// Convert an Android KeyEvent keycode (raw i32 from JNI) to a keycode for smithay.
/// Smithay expects xkb keycodes (evdev + 8), not raw evdev scancodes,
/// because it subtracts 8 when sending to Wayland clients.
pub fn android_keycode_to_smithay(keycode: i32) -> Option<u32> {
    android_keycode_to_evdev(keycode).map(|evdev| evdev + 8)
}

/// Map Android KeyEvent keycode (AKEYCODE_*) directly to Linux evdev scancode.
/// Values from android/keycodes.h. Sparse lookup table indexed by Android keycode.
fn android_keycode_to_evdev(keycode: i32) -> Option<u32> {
    let idx = keycode as usize;
    if idx < ANDROID_TO_EVDEV.len() {
        let val = ANDROID_TO_EVDEV[idx];
        if val != 0 { Some(val as u32) } else { None }
    } else {
        None
    }
}

/// Sparse lookup table: ANDROID_TO_EVDEV[android_keycode] = evdev_scancode.
/// 0 means unmapped. Max Android keycode we handle is 164 (VOLUME_MUTE).
#[rustfmt::skip]
const ANDROID_TO_EVDEV: [u16; 165] = {
    let mut t = [0u16; 165];

    // Digits (AKEYCODE_0=7 .. AKEYCODE_9=16)
    t[7] = 11; t[8] = 2; t[9] = 3; t[10] = 4; t[11] = 5;
    t[12] = 6; t[13] = 7; t[14] = 8; t[15] = 9; t[16] = 10;

    // Arrow keys
    t[19] = 103; t[20] = 108; t[21] = 105; t[22] = 106;

    // Volume
    t[24] = 115; t[25] = 114;

    // Letters (AKEYCODE_A=29 .. AKEYCODE_Z=54)
    t[29] = 30; t[30] = 48; t[31] = 46; t[32] = 32; t[33] = 18;
    t[34] = 33; t[35] = 34; t[36] = 35; t[37] = 23; t[38] = 36;
    t[39] = 37; t[40] = 38; t[41] = 50; t[42] = 49; t[43] = 24;
    t[44] = 25; t[45] = 16; t[46] = 19; t[47] = 31; t[48] = 20;
    t[49] = 22; t[50] = 47; t[51] = 17; t[52] = 45; t[53] = 21;
    t[54] = 44;

    // Punctuation
    t[55] = 51; t[56] = 52; // COMMA, PERIOD

    // Modifiers
    t[57] = 56; t[58] = 100; // ALT_LEFT, ALT_RIGHT
    t[59] = 42; t[60] = 54;  // SHIFT_LEFT, SHIFT_RIGHT

    // Tab, Space
    t[61] = 15; t[62] = 57;

    // Enter, DEL(Backspace), GRAVE
    t[66] = 28; t[67] = 14; t[68] = 41;

    // Punctuation continued
    t[69] = 12; t[70] = 13; // MINUS, EQUALS
    t[71] = 26; t[72] = 27; // LEFT_BRACKET, RIGHT_BRACKET
    t[73] = 43; t[74] = 39; // BACKSLASH, SEMICOLON
    t[75] = 40; t[76] = 53; // APOSTROPHE, SLASH

    // Media
    t[85] = 164; t[86] = 166; t[87] = 163; t[88] = 165;

    // Navigation
    t[92] = 104; t[93] = 109; // PAGE_UP, PAGE_DOWN

    // Escape, FORWARD_DEL
    t[111] = 1; t[112] = 111;

    // CTRL_LEFT, CTRL_RIGHT, CAPS_LOCK, SCROLL_LOCK
    t[113] = 29; t[114] = 97; t[115] = 58; t[116] = 70;

    // META_LEFT, META_RIGHT
    t[117] = 125; t[118] = 126;

    // SYSRQ, BREAK
    t[120] = 99; t[121] = 119;

    // MOVE_HOME, MOVE_END, INSERT
    t[122] = 102; t[123] = 107; t[124] = 110;

    // Function keys (AKEYCODE_F1=131 .. AKEYCODE_F12=142)
    t[131] = 59; t[132] = 60; t[133] = 61; t[134] = 62;
    t[135] = 63; t[136] = 64; t[137] = 65; t[138] = 66;
    t[139] = 67; t[140] = 68; t[141] = 87; t[142] = 88;

    // NUM_LOCK
    t[143] = 69;

    // Numpad (AKEYCODE_NUMPAD_0=144 .. AKEYCODE_NUMPAD_9=153)
    t[144] = 82; t[145] = 79; t[146] = 80; t[147] = 81;
    t[148] = 75; t[149] = 76; t[150] = 77; t[151] = 71;
    t[152] = 72; t[153] = 73;

    // Numpad operators
    t[154] = 98; t[155] = 55; t[156] = 74; t[157] = 78; // DIV, MUL, SUB, ADD
    t[158] = 83; t[159] = 121; t[160] = 96; t[161] = 117; // DOT, COMMA, ENTER, EQUALS

    // VOLUME_MUTE
    t[164] = 113;

    t
};

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
