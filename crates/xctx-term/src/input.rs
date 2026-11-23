//! Turning key presses, pastes, mouse events and focus changes into the bytes a terminal
//! program expects, given the modes it has set.
//!
//! Key encoding follows xterm for legacy mode and the kitty keyboard protocol once a program
//! pushes kitty flags (`CSI > flags u`). `pi` pushes flags 7 at startup, so Shift+Enter reaches
//! it as `CSI 13;2u`; programs that never ask get xterm bytes (Shift+Enter is then plain CR, as
//! in xterm, because there is no legacy encoding for it).

use crate::snapshot::{
    KittyKeyboardFlags, MouseEncoding, MouseReporting, TerminalModes, ViewportPoint,
};

/// Modifier keys held during an input event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Modifiers {
    /// Shift.
    pub shift: bool,
    /// Control.
    pub ctrl: bool,
    /// Alt / Option, treated as Meta (sends an ESC prefix in legacy mode).
    pub alt: bool,
    /// Super / Command. Legacy mode has no encoding for it, so such keys produce no bytes.
    pub super_key: bool,
}

impl Modifiers {
    /// No modifiers.
    pub const NONE: Self = Self {
        shift: false,
        ctrl: false,
        alt: false,
        super_key: false,
    };
    /// Shift only.
    pub const SHIFT: Self = Self {
        shift: true,
        ..Self::NONE
    };
    /// Control only.
    pub const CTRL: Self = Self {
        ctrl: true,
        ..Self::NONE
    };
    /// Alt only.
    pub const ALT: Self = Self {
        alt: true,
        ..Self::NONE
    };
    /// Super only.
    pub const SUPER: Self = Self {
        super_key: true,
        ..Self::NONE
    };

    /// Whether no modifier is held.
    pub fn is_empty(self) -> bool {
        self == Self::NONE
    }

    /// The xterm modifier parameter (`1 + bits`) for shift, alt and ctrl, or `None` when none of
    /// them is held.
    fn xterm_parameter(self) -> Option<u8> {
        let bits = u8::from(self.shift) | u8::from(self.alt) << 1 | u8::from(self.ctrl) << 2;
        (bits != 0).then_some(1 + bits)
    }

    /// The kitty modifier bits (shift 1, alt 2, ctrl 4, super 8).
    fn kitty_bits(self) -> u8 {
        u8::from(self.shift)
            | u8::from(self.alt) << 1
            | u8::from(self.ctrl) << 2
            | u8::from(self.super_key) << 3
    }
}

/// A key on the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    /// A key that types a character, given as the character it types *without* Shift
    /// (`'a'`, `'1'`, `'/'`, `' '` for the space bar).
    Char(char),
    /// Return / Enter.
    Enter,
    /// Tab. Shift+Tab is Backtab.
    Tab,
    /// Backspace (delete backwards).
    Backspace,
    /// Escape.
    Escape,
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
    /// Left arrow.
    Left,
    /// Right arrow.
    Right,
    /// Home.
    Home,
    /// End.
    End,
    /// Page Up.
    PageUp,
    /// Page Down.
    PageDown,
    /// Insert / Help.
    Insert,
    /// Forward delete.
    Delete,
    /// Function key F1 to F35. Legacy mode encodes F1 to F12 only.
    F(u8),
}

/// Whether a key went down, auto-repeated, or came up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum KeyEventKind {
    /// The key went down.
    #[default]
    Press,
    /// The key is held and auto-repeated.
    Repeat,
    /// The key came up. Only sent when the program asked for event types (kitty flag 2).
    Release,
}

/// A key event from the view.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyEvent {
    /// The key.
    pub key: Key,
    /// The text the key types with Shift applied but without Ctrl, Alt or Option composition
    /// (`"A"` for Shift+a, `"!"` for Shift+1). `None` derives it from the key, upper-casing
    /// letters when Shift is held. A view that lets Option compose characters (Option+e for
    /// `é`) should send the composed text here with `alt` cleared.
    pub text: Option<String>,
    /// Held modifiers.
    pub modifiers: Modifiers,
    /// Press, repeat or release.
    pub kind: KeyEventKind,
}

impl KeyEvent {
    /// A key press with no explicit text.
    pub fn new(key: Key, modifiers: Modifiers) -> Self {
        Self {
            key,
            text: None,
            modifiers,
            kind: KeyEventKind::Press,
        }
    }

    /// Sets the typed text.
    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    /// Sets press, repeat or release.
    pub fn with_kind(mut self, kind: KeyEventKind) -> Self {
        self.kind = kind;
        self
    }

    /// The text this key types with Shift applied.
    fn typed_text(&self, c: char) -> String {
        match &self.text {
            Some(text) if !text.is_empty() => text.clone(),
            _ if self.modifiers.shift => c.to_uppercase().collect(),
            _ => c.to_string(),
        }
    }
}

/// The bytes to send for a key event, or `None` when the key sends nothing in the current
/// modes (Command chords in legacy mode, releases the program did not ask for).
pub fn encode_key(event: &KeyEvent, modes: &TerminalModes) -> Option<Vec<u8>> {
    if modes.kitty_keyboard.is_empty() {
        encode_legacy_key(event, modes)
    } else {
        encode_kitty_key(event, modes)
    }
}

fn with_alt_prefix(alt: bool, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 1);
    if alt {
        out.push(0x1b);
    }
    out.extend_from_slice(bytes);
    out
}

/// The C0 control byte for Ctrl plus a character key, as xterm sends it.
fn ctrl_byte(c: char) -> Option<u8> {
    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        'A'..='Z' => Some(c as u8 - b'A' + 1),
        ' ' | '@' | '2' | '`' => Some(0x00),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' | '~' => Some(0x1e),
        '_' | '/' | '-' | '7' => Some(0x1f),
        '8' | '?' => Some(0x7f),
        _ => None,
    }
}

/// Final byte for keys encoded as `CSI 1;m X` / `SS3 X`.
fn letter_key(key: Key) -> Option<u8> {
    match key {
        Key::Up => Some(b'A'),
        Key::Down => Some(b'B'),
        Key::Right => Some(b'C'),
        Key::Left => Some(b'D'),
        Key::Home => Some(b'H'),
        Key::End => Some(b'F'),
        Key::F(1) => Some(b'P'),
        Key::F(2) => Some(b'Q'),
        Key::F(3) => Some(b'R'),
        Key::F(4) => Some(b'S'),
        _ => None,
    }
}

/// Number for keys encoded as `CSI n ~` / `CSI n;m ~`.
fn tilde_key(key: Key) -> Option<u8> {
    match key {
        Key::Insert => Some(2),
        Key::Delete => Some(3),
        Key::PageUp => Some(5),
        Key::PageDown => Some(6),
        Key::F(5) => Some(15),
        Key::F(6) => Some(17),
        Key::F(7) => Some(18),
        Key::F(8) => Some(19),
        Key::F(9) => Some(20),
        Key::F(10) => Some(21),
        Key::F(11) => Some(23),
        Key::F(12) => Some(24),
        _ => None,
    }
}

fn encode_legacy_key(event: &KeyEvent, modes: &TerminalModes) -> Option<Vec<u8>> {
    let mods = event.modifiers;
    if event.kind == KeyEventKind::Release || mods.super_key {
        return None;
    }

    match event.key {
        Key::Char(c) => {
            if mods.ctrl
                && let Some(byte) = ctrl_byte(c)
            {
                return Some(with_alt_prefix(mods.alt, &[byte]));
            }
            Some(with_alt_prefix(mods.alt, event.typed_text(c).as_bytes()))
        }
        Key::Enter => {
            let enter: &[u8] = if modes.line_feed_new_line {
                b"\r\n"
            } else {
                b"\r"
            };
            Some(with_alt_prefix(mods.alt, enter))
        }
        Key::Tab if mods.shift => Some(with_alt_prefix(mods.alt, b"\x1b[Z")),
        Key::Tab => Some(with_alt_prefix(mods.alt, b"\t")),
        Key::Backspace => {
            let byte = if mods.ctrl { 0x08 } else { 0x7f };
            Some(with_alt_prefix(mods.alt, &[byte]))
        }
        Key::Escape => Some(with_alt_prefix(mods.alt, b"\x1b")),
        key => {
            if let Some(letter) = letter_key(key) {
                let bytes = match mods.xterm_parameter() {
                    Some(parameter) => format!("\x1b[1;{parameter}{}", letter as char),
                    None => {
                        let ss3 = matches!(key, Key::F(_))
                            || (modes.app_cursor
                                && matches!(
                                    key,
                                    Key::Up
                                        | Key::Down
                                        | Key::Left
                                        | Key::Right
                                        | Key::Home
                                        | Key::End
                                ));
                        let intro = if ss3 { "\x1bO" } else { "\x1b[" };
                        format!("{intro}{}", letter as char)
                    }
                };
                return Some(bytes.into_bytes());
            }
            let number = tilde_key(key)?;
            let bytes = match mods.xterm_parameter() {
                Some(parameter) => format!("\x1b[{number};{parameter}~"),
                None => format!("\x1b[{number}~"),
            };
            Some(bytes.into_bytes())
        }
    }
}

fn encode_kitty_key(event: &KeyEvent, modes: &TerminalModes) -> Option<Vec<u8>> {
    let flags = modes.kitty_keyboard;
    let all_keys = flags.contains(KittyKeyboardFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES);
    let event_types = flags.contains(KittyKeyboardFlags::REPORT_EVENT_TYPES);
    let mods = event.modifiers;
    if event.kind == KeyEventKind::Release && !event_types {
        return None;
    }
    let is_release = event.kind == KeyEventKind::Release;

    // Modifier field: `1 + bits`, then `:2` for repeat or `:3` for release when asked for.
    let event_suffix = match event.kind {
        KeyEventKind::Press => None,
        KeyEventKind::Repeat if event_types => Some(2),
        KeyEventKind::Repeat => None,
        KeyEventKind::Release => Some(3),
    };
    let modifier_value = 1 + mods.kitty_bits();
    let modifier_field = match event_suffix {
        Some(suffix) => Some(format!("{modifier_value}:{suffix}")),
        None if modifier_value > 1 => Some(modifier_value.to_string()),
        None => None,
    };

    let csi_u = |key_field: String, text_field: Option<String>| -> Vec<u8> {
        let mut out = format!("\x1b[{key_field}");
        match (&modifier_field, text_field) {
            (Some(modifier), Some(text)) => out.push_str(&format!(";{modifier};{text}")),
            (Some(modifier), None) => out.push_str(&format!(";{modifier}")),
            (None, Some(text)) => out.push_str(&format!(";1;{text}")),
            (None, None) => {}
        }
        out.push('u');
        out.into_bytes()
    };

    // Enter, Tab and Backspace keep their legacy bytes unless modified or all keys are
    // reported; their releases are only reported with all keys.
    let legacy_control = |code: u32, legacy: &[u8]| -> Option<Vec<u8>> {
        if !all_keys && mods.is_empty() {
            return (!is_release).then(|| legacy.to_vec());
        }
        if is_release && !all_keys {
            return None;
        }
        Some(csi_u(code.to_string(), None))
    };

    match event.key {
        Key::Char(c) => {
            let composes_text = !(mods.ctrl || mods.alt || mods.super_key);
            if composes_text && !all_keys {
                return (!is_release).then(|| event.typed_text(c).into_bytes());
            }
            let base: char = c.to_lowercase().next().unwrap_or(c);
            let mut key_field = (base as u32).to_string();
            if flags.contains(KittyKeyboardFlags::REPORT_ALTERNATE_KEYS) && mods.shift {
                let shifted = event.typed_text(c);
                let mut chars = shifted.chars();
                if let (Some(shifted), None) = (chars.next(), chars.next())
                    && shifted != base
                {
                    key_field.push_str(&format!(":{}", shifted as u32));
                }
            }
            let text_field = (flags.contains(KittyKeyboardFlags::REPORT_ASSOCIATED_TEXT)
                && composes_text
                && !is_release)
                .then(|| {
                    event
                        .typed_text(c)
                        .chars()
                        .map(|ch| (ch as u32).to_string())
                        .collect::<Vec<_>>()
                        .join(":")
                });
            Some(csi_u(key_field, text_field))
        }
        Key::Enter => legacy_control(13, b"\r"),
        Key::Tab => legacy_control(9, b"\t"),
        Key::Backspace => legacy_control(127, b"\x7f"),
        Key::Escape => Some(csi_u("27".to_owned(), None)),
        Key::F(n @ 13..=35) => Some(csi_u((57376 + u32::from(n) - 13).to_string(), None)),
        Key::F(3) => Some(match &modifier_field {
            Some(modifier) => format!("\x1b[13;{modifier}~").into_bytes(),
            None => b"\x1b[13~".to_vec(),
        }),
        key => {
            if let Some(letter) = letter_key(key) {
                let bytes = match &modifier_field {
                    Some(modifier) => format!("\x1b[1;{modifier}{}", letter as char),
                    None if all_keys => format!("\x1b[{}", letter as char),
                    // Unmodified presses keep their legacy bytes.
                    None => {
                        return encode_legacy_key(&KeyEvent::new(key, Modifiers::NONE), modes);
                    }
                };
                return Some(bytes.into_bytes());
            }
            let number = tilde_key(key)?;
            let bytes = match &modifier_field {
                Some(modifier) => format!("\x1b[{number};{modifier}~"),
                None => format!("\x1b[{number}~"),
            };
            Some(bytes.into_bytes())
        }
    }
}

/// The bytes for pasting `text`.
///
/// With bracketed paste the text is wrapped in `CSI 200~` / `CSI 201~` and ESC characters are
/// removed so the paste cannot end the bracket early. Without it, line endings become CR, as a
/// typed Enter would send.
pub fn encode_paste(text: &str, bracketed_paste: bool) -> Vec<u8> {
    if bracketed_paste {
        let mut out = b"\x1b[200~".to_vec();
        out.extend(text.bytes().filter(|&byte| byte != 0x1b));
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

/// A mouse button or wheel direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    /// Primary button.
    Left,
    /// Middle button.
    Middle,
    /// Secondary button.
    Right,
    /// Wheel toward the top of the content (scroll up / back).
    WheelUp,
    /// Wheel toward the bottom of the content.
    WheelDown,
    /// Horizontal wheel left.
    WheelLeft,
    /// Horizontal wheel right.
    WheelRight,
}

impl MouseButton {
    fn code(self) -> u8 {
        match self {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            MouseButton::WheelUp => 64,
            MouseButton::WheelDown => 65,
            MouseButton::WheelLeft => 66,
            MouseButton::WheelRight => 67,
        }
    }

    fn is_wheel(self) -> bool {
        matches!(
            self,
            MouseButton::WheelUp
                | MouseButton::WheelDown
                | MouseButton::WheelLeft
                | MouseButton::WheelRight
        )
    }
}

/// What happened to the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseEventKind {
    /// A button went down, or one wheel step.
    Press,
    /// A button came up.
    Release,
    /// The pointer moved to a new cell.
    Motion,
}

/// A mouse event in cell coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MouseEvent {
    /// Press, release or motion.
    pub kind: MouseEventKind,
    /// The button pressed or released, or held during motion; `None` for motion with no
    /// button held.
    pub button: Option<MouseButton>,
    /// The cell under the pointer, in viewport coordinates.
    pub point: ViewportPoint,
    /// Held modifiers (Shift, Alt and Ctrl are reported; Super is not).
    pub modifiers: Modifiers,
}

/// The bytes reporting a mouse event, or `None` when the program has not asked for this kind
/// of event or the position cannot be encoded.
pub fn encode_mouse(event: &MouseEvent, modes: &TerminalModes) -> Option<Vec<u8>> {
    let reporting = modes.mouse_reporting;
    match (event.kind, event.button) {
        (_, _) if reporting == MouseReporting::Off => return None,
        (MouseEventKind::Motion, Some(button)) if button.is_wheel() => return None,
        (MouseEventKind::Motion, Some(_))
            if !matches!(reporting, MouseReporting::Drag | MouseReporting::Motion) =>
        {
            return None;
        }
        (MouseEventKind::Motion, None) if reporting != MouseReporting::Motion => return None,
        (MouseEventKind::Press | MouseEventKind::Release, None) => return None,
        (MouseEventKind::Release, Some(button)) if button.is_wheel() => return None,
        _ => {}
    }

    let mods = event.modifiers;
    let modifier_bits =
        u8::from(mods.shift) << 2 | u8::from(mods.alt) << 3 | u8::from(mods.ctrl) << 4;
    let motion_bit = if event.kind == MouseEventKind::Motion {
        32
    } else {
        0
    };
    let sgr = modes.mouse_encoding == MouseEncoding::Sgr;
    let button_code = match (event.kind, event.button) {
        (MouseEventKind::Release, _) if !sgr => 3,
        (_, Some(button)) => button.code(),
        (_, None) => 3,
    };
    let code = button_code + motion_bit + modifier_bits;
    let col = event.point.col;
    let row = event.point.row;

    match modes.mouse_encoding {
        MouseEncoding::Sgr => {
            let final_byte = if event.kind == MouseEventKind::Release {
                'm'
            } else {
                'M'
            };
            Some(format!("\x1b[<{code};{};{}{final_byte}", col + 1, row + 1).into_bytes())
        }
        MouseEncoding::Default => {
            let encode = |value: usize| u8::try_from(32 + value + 1).ok();
            Some(vec![
                0x1b,
                b'[',
                b'M',
                32 + code,
                encode(col)?,
                encode(row)?,
            ])
        }
        MouseEncoding::Utf8 => {
            let mut out = b"\x1b[M".to_vec();
            for value in [usize::from(code), col + 1, row + 1] {
                let value = u32::try_from(32 + value)
                    .ok()
                    .filter(|&value| value < 0x800)?;
                let mut buf = [0u8; 4];
                out.extend_from_slice(char::from_u32(value)?.encode_utf8(&mut buf).as_bytes());
            }
            Some(out)
        }
    }
}

/// Arrow keys standing in for wheel scrolling on the alternate screen (mode 1007), so the
/// wheel scrolls `less` or `man`. `lines` > 0 scrolls up. Returns `None` when the view should
/// scroll its own scrollback instead or report the wheel as a mouse event.
pub fn encode_scroll_as_arrows(lines: i32, modes: &TerminalModes) -> Option<Vec<u8>> {
    if lines == 0
        || !modes.alt_screen
        || !modes.alternate_scroll
        || modes.mouse_reporting != MouseReporting::Off
    {
        return None;
    }
    let intro = if modes.app_cursor { "\x1bO" } else { "\x1b[" };
    let letter = if lines > 0 { 'A' } else { 'B' };
    Some(
        format!("{intro}{letter}")
            .repeat(lines.unsigned_abs() as usize)
            .into_bytes(),
    )
}

/// The focus report for a focus change, or `None` when the program did not ask for them
/// (mode 1004).
pub fn encode_focus(focused: bool, modes: &TerminalModes) -> Option<Vec<u8>> {
    modes.focus_reporting.then(|| {
        if focused {
            b"\x1b[I".to_vec()
        } else {
            b"\x1b[O".to_vec()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy() -> TerminalModes {
        TerminalModes {
            show_cursor: true,
            ..TerminalModes::default()
        }
    }

    fn kitty(flags: KittyKeyboardFlags) -> TerminalModes {
        TerminalModes {
            kitty_keyboard: flags,
            ..legacy()
        }
    }

    /// The flags `pi` pushes: disambiguate, event types, alternate keys.
    fn pi_modes() -> TerminalModes {
        kitty(
            KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES
                | KittyKeyboardFlags::REPORT_EVENT_TYPES
                | KittyKeyboardFlags::REPORT_ALTERNATE_KEYS,
        )
    }

    fn key(key: Key, modifiers: Modifiers, modes: &TerminalModes) -> Option<Vec<u8>> {
        encode_key(&KeyEvent::new(key, modifiers), modes)
    }

    fn bytes(s: &str) -> Option<Vec<u8>> {
        Some(s.as_bytes().to_vec())
    }

    fn mods(shift: bool, ctrl: bool, alt: bool) -> Modifiers {
        Modifiers {
            shift,
            ctrl,
            alt,
            super_key: false,
        }
    }

    #[test]
    fn plain_text_and_shifted_text() {
        let modes = legacy();
        assert_eq!(key(Key::Char('a'), Modifiers::NONE, &modes), bytes("a"));
        assert_eq!(key(Key::Char('a'), Modifiers::SHIFT, &modes), bytes("A"));
        let bang = KeyEvent::new(Key::Char('1'), Modifiers::SHIFT).with_text("!");
        assert_eq!(encode_key(&bang, &modes), bytes("!"));
        let accent = KeyEvent::new(Key::Char('e'), Modifiers::NONE).with_text("é");
        assert_eq!(encode_key(&accent, &modes), bytes("é"));
    }

    #[test]
    fn shift_enter_in_kitty_mode_is_csi_u() {
        assert_eq!(
            key(Key::Enter, Modifiers::SHIFT, &pi_modes()),
            bytes("\x1b[13;2u")
        );
        let disambiguate = kitty(KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES);
        assert_eq!(
            key(Key::Enter, Modifiers::SHIFT, &disambiguate),
            bytes("\x1b[13;2u")
        );
        assert_eq!(key(Key::Enter, Modifiers::NONE, &pi_modes()), bytes("\r"));
        assert_eq!(
            key(Key::Enter, Modifiers::ALT, &pi_modes()),
            bytes("\x1b[13;3u")
        );
        assert_eq!(
            key(Key::Enter, Modifiers::CTRL, &pi_modes()),
            bytes("\x1b[13;5u")
        );
    }

    #[test]
    fn shift_enter_in_legacy_mode_is_cr() {
        assert_eq!(key(Key::Enter, Modifiers::SHIFT, &legacy()), bytes("\r"));
        assert_eq!(key(Key::Enter, Modifiers::NONE, &legacy()), bytes("\r"));
        assert_eq!(key(Key::Enter, Modifiers::ALT, &legacy()), bytes("\x1b\r"));
        let lnm = TerminalModes {
            line_feed_new_line: true,
            ..legacy()
        };
        assert_eq!(key(Key::Enter, Modifiers::NONE, &lnm), bytes("\r\n"));
    }

    #[test]
    fn ctrl_chords_in_legacy_mode() {
        let modes = legacy();
        for (c, expected) in [('a', 0x01u8), ('c', 0x03), ('z', 0x1a), ('A', 0x01)] {
            assert_eq!(
                key(Key::Char(c), Modifiers::CTRL, &modes),
                Some(vec![expected])
            );
        }
        assert_eq!(
            key(Key::Char('c'), mods(true, true, false), &modes),
            Some(vec![0x03])
        );
        assert_eq!(
            key(Key::Char(' '), Modifiers::CTRL, &modes),
            Some(vec![0x00])
        );
        assert_eq!(
            key(Key::Char('['), Modifiers::CTRL, &modes),
            Some(vec![0x1b])
        );
        assert_eq!(
            key(Key::Char(']'), Modifiers::CTRL, &modes),
            Some(vec![0x1d])
        );
        assert_eq!(
            key(Key::Char('\\'), Modifiers::CTRL, &modes),
            Some(vec![0x1c])
        );
        assert_eq!(
            key(Key::Char('/'), Modifiers::CTRL, &modes),
            Some(vec![0x1f])
        );
        assert_eq!(
            key(Key::Backspace, Modifiers::CTRL, &modes),
            Some(vec![0x08])
        );
        assert_eq!(
            key(Key::Backspace, Modifiers::NONE, &modes),
            Some(vec![0x7f])
        );
        // No control code for Ctrl+1: the digit is sent.
        assert_eq!(key(Key::Char('1'), Modifiers::CTRL, &modes), bytes("1"));
    }

    #[test]
    fn alt_chords_in_legacy_mode() {
        let modes = legacy();
        assert_eq!(key(Key::Char('b'), Modifiers::ALT, &modes), bytes("\x1bb"));
        assert_eq!(
            key(Key::Char('b'), mods(true, false, true), &modes),
            bytes("\x1bB")
        );
        assert_eq!(
            key(Key::Char('x'), mods(false, true, true), &modes),
            Some(vec![0x1b, 0x18])
        );
        assert_eq!(
            key(Key::Backspace, Modifiers::ALT, &modes),
            bytes("\x1b\x7f")
        );
        assert_eq!(key(Key::Char(' '), Modifiers::ALT, &modes), bytes("\x1b "));
        assert_eq!(key(Key::Escape, Modifiers::ALT, &modes), bytes("\x1b\x1b"));
    }

    #[test]
    fn super_chords_send_nothing_in_legacy_mode() {
        assert_eq!(key(Key::Char('c'), Modifiers::SUPER, &legacy()), None);
    }

    #[test]
    fn arrows_with_modifiers_and_app_cursor() {
        let modes = legacy();
        assert_eq!(key(Key::Up, Modifiers::NONE, &modes), bytes("\x1b[A"));
        assert_eq!(key(Key::Left, Modifiers::ALT, &modes), bytes("\x1b[1;3D"));
        assert_eq!(key(Key::Right, Modifiers::CTRL, &modes), bytes("\x1b[1;5C"));
        assert_eq!(key(Key::Down, Modifiers::SHIFT, &modes), bytes("\x1b[1;2B"));
        assert_eq!(
            key(Key::Up, mods(true, true, true), &modes),
            bytes("\x1b[1;8A")
        );
        let app = TerminalModes {
            app_cursor: true,
            ..legacy()
        };
        assert_eq!(key(Key::Up, Modifiers::NONE, &app), bytes("\x1bOA"));
        assert_eq!(key(Key::Home, Modifiers::NONE, &app), bytes("\x1bOH"));
        assert_eq!(key(Key::Up, Modifiers::CTRL, &app), bytes("\x1b[1;5A"));
    }

    #[test]
    fn navigation_and_function_keys() {
        let modes = legacy();
        assert_eq!(key(Key::Home, Modifiers::NONE, &modes), bytes("\x1b[H"));
        assert_eq!(key(Key::End, Modifiers::SHIFT, &modes), bytes("\x1b[1;2F"));
        assert_eq!(key(Key::PageUp, Modifiers::NONE, &modes), bytes("\x1b[5~"));
        assert_eq!(
            key(Key::PageDown, Modifiers::CTRL, &modes),
            bytes("\x1b[6;5~")
        );
        assert_eq!(key(Key::Insert, Modifiers::NONE, &modes), bytes("\x1b[2~"));
        assert_eq!(key(Key::Delete, Modifiers::NONE, &modes), bytes("\x1b[3~"));
        assert_eq!(key(Key::Delete, Modifiers::ALT, &modes), bytes("\x1b[3;3~"));
        assert_eq!(key(Key::Tab, Modifiers::NONE, &modes), bytes("\t"));
        assert_eq!(key(Key::Tab, Modifiers::SHIFT, &modes), bytes("\x1b[Z"));
        assert_eq!(key(Key::Escape, Modifiers::NONE, &modes), bytes("\x1b"));
        assert_eq!(key(Key::F(1), Modifiers::NONE, &modes), bytes("\x1bOP"));
        assert_eq!(key(Key::F(4), Modifiers::NONE, &modes), bytes("\x1bOS"));
        assert_eq!(key(Key::F(1), Modifiers::SHIFT, &modes), bytes("\x1b[1;2P"));
        assert_eq!(key(Key::F(5), Modifiers::NONE, &modes), bytes("\x1b[15~"));
        assert_eq!(
            key(Key::F(12), Modifiers::CTRL, &modes),
            bytes("\x1b[24;5~")
        );
        assert_eq!(key(Key::F(13), Modifiers::NONE, &modes), None);
    }

    #[test]
    fn kitty_disambiguates_modified_text_keys() {
        let modes = pi_modes();
        assert_eq!(key(Key::Char('a'), Modifiers::NONE, &modes), bytes("a"));
        assert_eq!(key(Key::Char('a'), Modifiers::SHIFT, &modes), bytes("A"));
        assert_eq!(
            key(Key::Char('c'), Modifiers::CTRL, &modes),
            bytes("\x1b[99;5u")
        );
        assert_eq!(
            key(Key::Char('f'), Modifiers::ALT, &modes),
            bytes("\x1b[102;3u")
        );
        assert_eq!(
            key(Key::Char(' '), Modifiers::CTRL, &modes),
            bytes("\x1b[32;5u")
        );
        assert_eq!(
            key(Key::Char('['), Modifiers::CTRL, &modes),
            bytes("\x1b[91;5u")
        );
        // Alternate key reported for Ctrl+Shift+a.
        assert_eq!(
            key(Key::Char('a'), mods(true, true, false), &modes),
            bytes("\x1b[97:65;6u")
        );
        assert_eq!(key(Key::Escape, Modifiers::NONE, &modes), bytes("\x1b[27u"));
        assert_eq!(key(Key::Tab, Modifiers::SHIFT, &modes), bytes("\x1b[9;2u"));
        assert_eq!(
            key(Key::Backspace, Modifiers::CTRL, &modes),
            bytes("\x1b[127;5u")
        );
        assert_eq!(key(Key::Backspace, Modifiers::NONE, &modes), bytes("\x7f"));
        assert_eq!(
            key(Key::Char('c'), Modifiers::SUPER, &modes),
            bytes("\x1b[99;9u")
        );
    }

    #[test]
    fn kitty_functional_keys() {
        let modes = pi_modes();
        assert_eq!(key(Key::Up, Modifiers::NONE, &modes), bytes("\x1b[A"));
        assert_eq!(key(Key::Left, Modifiers::ALT, &modes), bytes("\x1b[1;3D"));
        assert_eq!(key(Key::Delete, Modifiers::NONE, &modes), bytes("\x1b[3~"));
        assert_eq!(key(Key::F(3), Modifiers::NONE, &modes), bytes("\x1b[13~"));
        assert_eq!(
            key(Key::F(3), Modifiers::SHIFT, &modes),
            bytes("\x1b[13;2~")
        );
        assert_eq!(
            key(Key::F(13), Modifiers::NONE, &modes),
            bytes("\x1b[57376u")
        );
    }

    #[test]
    fn kitty_event_types() {
        let modes = pi_modes();
        let repeat = KeyEvent::new(Key::Up, Modifiers::NONE).with_kind(KeyEventKind::Repeat);
        assert_eq!(encode_key(&repeat, &modes), bytes("\x1b[1;1:2A"));
        let release =
            KeyEvent::new(Key::Char('c'), Modifiers::CTRL).with_kind(KeyEventKind::Release);
        assert_eq!(encode_key(&release, &modes), bytes("\x1b[99;5:3u"));
        // Text keys and Enter do not report releases unless all keys are escape codes.
        let text_release =
            KeyEvent::new(Key::Char('a'), Modifiers::NONE).with_kind(KeyEventKind::Release);
        assert_eq!(encode_key(&text_release, &modes), None);
        let enter_release =
            KeyEvent::new(Key::Enter, Modifiers::NONE).with_kind(KeyEventKind::Release);
        assert_eq!(encode_key(&enter_release, &modes), None);
        // Without the flag, releases are dropped and repeats look like presses.
        let disambiguate = kitty(KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES);
        assert_eq!(encode_key(&release, &disambiguate), None);
        let repeat_c =
            KeyEvent::new(Key::Char('c'), Modifiers::CTRL).with_kind(KeyEventKind::Repeat);
        assert_eq!(encode_key(&repeat_c, &disambiguate), bytes("\x1b[99;5u"));
        // Legacy mode never sends releases.
        assert_eq!(encode_key(&release, &legacy()), None);
    }

    #[test]
    fn kitty_report_all_keys_and_text() {
        let modes = kitty(
            KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES
                | KittyKeyboardFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
                | KittyKeyboardFlags::REPORT_ASSOCIATED_TEXT,
        );
        assert_eq!(
            key(Key::Char('a'), Modifiers::NONE, &modes),
            bytes("\x1b[97;1;97u")
        );
        assert_eq!(
            key(Key::Char('a'), Modifiers::SHIFT, &modes),
            bytes("\x1b[97;2;65u")
        );
        assert_eq!(key(Key::Enter, Modifiers::NONE, &modes), bytes("\x1b[13u"));
        assert_eq!(key(Key::Up, Modifiers::NONE, &modes), bytes("\x1b[A"));
        assert_eq!(
            key(Key::Char('c'), Modifiers::CTRL, &modes),
            bytes("\x1b[99;5u")
        );
    }

    #[test]
    fn bracketed_paste_wraps_and_strips_escape() {
        assert_eq!(
            encode_paste("line1\nline2", true),
            b"\x1b[200~line1\nline2\x1b[201~".to_vec()
        );
        assert_eq!(
            encode_paste("evil\x1b[201~rm -rf", true),
            b"\x1b[200~evil[201~rm -rf\x1b[201~".to_vec()
        );
    }

    #[test]
    fn plain_paste_normalizes_newlines() {
        assert_eq!(encode_paste("a\r\nb\nc", false), b"a\rb\rc".to_vec());
    }

    fn mouse(
        kind: MouseEventKind,
        button: Option<MouseButton>,
        col: usize,
        row: usize,
    ) -> MouseEvent {
        MouseEvent {
            kind,
            button,
            point: ViewportPoint::new(row, col),
            modifiers: Modifiers::NONE,
        }
    }

    fn mouse_modes(reporting: MouseReporting, encoding: MouseEncoding) -> TerminalModes {
        TerminalModes {
            mouse_reporting: reporting,
            mouse_encoding: encoding,
            ..legacy()
        }
    }

    #[test]
    fn sgr_mouse_press_release_motion_wheel() {
        let modes = mouse_modes(MouseReporting::Drag, MouseEncoding::Sgr);
        let press = mouse(MouseEventKind::Press, Some(MouseButton::Left), 4, 9);
        assert_eq!(encode_mouse(&press, &modes), bytes("\x1b[<0;5;10M"));
        let release = mouse(MouseEventKind::Release, Some(MouseButton::Left), 4, 9);
        assert_eq!(encode_mouse(&release, &modes), bytes("\x1b[<0;5;10m"));
        let right = mouse(MouseEventKind::Press, Some(MouseButton::Right), 0, 0);
        assert_eq!(encode_mouse(&right, &modes), bytes("\x1b[<2;1;1M"));
        let drag = mouse(MouseEventKind::Motion, Some(MouseButton::Left), 5, 9);
        assert_eq!(encode_mouse(&drag, &modes), bytes("\x1b[<32;6;10M"));
        let hover = mouse(MouseEventKind::Motion, None, 5, 9);
        assert_eq!(encode_mouse(&hover, &modes), None);
        let wheel_up = mouse(MouseEventKind::Press, Some(MouseButton::WheelUp), 1, 1);
        assert_eq!(encode_mouse(&wheel_up, &modes), bytes("\x1b[<64;2;2M"));
        let wheel_down = mouse(MouseEventKind::Press, Some(MouseButton::WheelDown), 1, 1);
        assert_eq!(encode_mouse(&wheel_down, &modes), bytes("\x1b[<65;2;2M"));
        let mut ctrl_click = press;
        ctrl_click.modifiers = mods(true, true, true);
        assert_eq!(encode_mouse(&ctrl_click, &modes), bytes("\x1b[<28;5;10M"));
        // Large coordinates are fine in SGR.
        let far = mouse(MouseEventKind::Press, Some(MouseButton::Left), 300, 0);
        assert_eq!(encode_mouse(&far, &modes), bytes("\x1b[<0;301;1M"));
    }

    #[test]
    fn mouse_reporting_modes_filter_events() {
        let off = mouse_modes(MouseReporting::Off, MouseEncoding::Sgr);
        let press = mouse(MouseEventKind::Press, Some(MouseButton::Left), 0, 0);
        assert_eq!(encode_mouse(&press, &off), None);
        let click = mouse_modes(MouseReporting::Click, MouseEncoding::Sgr);
        let drag = mouse(MouseEventKind::Motion, Some(MouseButton::Left), 1, 0);
        assert_eq!(encode_mouse(&drag, &click), None);
        assert!(encode_mouse(&press, &click).is_some());
        let motion = mouse_modes(MouseReporting::Motion, MouseEncoding::Sgr);
        let hover = mouse(MouseEventKind::Motion, None, 1, 0);
        assert_eq!(encode_mouse(&hover, &motion), bytes("\x1b[<35;2;1M"));
    }

    #[test]
    fn normal_mouse_encoding() {
        let modes = mouse_modes(MouseReporting::Click, MouseEncoding::Default);
        let press = mouse(MouseEventKind::Press, Some(MouseButton::Left), 0, 0);
        assert_eq!(
            encode_mouse(&press, &modes),
            Some(vec![0x1b, b'[', b'M', 32, 33, 33])
        );
        let release = mouse(MouseEventKind::Release, Some(MouseButton::Left), 2, 3);
        assert_eq!(
            encode_mouse(&release, &modes),
            Some(vec![0x1b, b'[', b'M', 35, 35, 36])
        );
        let edge = mouse(MouseEventKind::Press, Some(MouseButton::Left), 222, 0);
        assert_eq!(
            encode_mouse(&edge, &modes),
            Some(vec![0x1b, b'[', b'M', 32, 255, 33])
        );
        let far = mouse(MouseEventKind::Press, Some(MouseButton::Left), 223, 0);
        assert_eq!(encode_mouse(&far, &modes), None);
    }

    #[test]
    fn utf8_mouse_encoding() {
        let modes = mouse_modes(MouseReporting::Click, MouseEncoding::Utf8);
        let far = mouse(MouseEventKind::Press, Some(MouseButton::Left), 300, 0);
        let mut expected = b"\x1b[M ".to_vec();
        expected.extend("\u{14d}".as_bytes());
        expected.push(b'!');
        assert_eq!(encode_mouse(&far, &modes), Some(expected));
    }

    #[test]
    fn alternate_scroll_sends_arrows() {
        let modes = TerminalModes {
            alt_screen: true,
            alternate_scroll: true,
            ..legacy()
        };
        assert_eq!(encode_scroll_as_arrows(2, &modes), bytes("\x1b[A\x1b[A"));
        assert_eq!(encode_scroll_as_arrows(-1, &modes), bytes("\x1b[B"));
        let app = TerminalModes {
            app_cursor: true,
            ..modes
        };
        assert_eq!(encode_scroll_as_arrows(1, &app), bytes("\x1bOA"));
        assert_eq!(encode_scroll_as_arrows(1, &legacy()), None);
        let reporting = TerminalModes {
            mouse_reporting: MouseReporting::Click,
            ..modes
        };
        assert_eq!(encode_scroll_as_arrows(1, &reporting), None);
    }

    #[test]
    fn focus_reports_only_when_enabled() {
        assert_eq!(encode_focus(true, &legacy()), None);
        let modes = TerminalModes {
            focus_reporting: true,
            ..legacy()
        };
        assert_eq!(encode_focus(true, &modes), bytes("\x1b[I"));
        assert_eq!(encode_focus(false, &modes), bytes("\x1b[O"));
    }
}
