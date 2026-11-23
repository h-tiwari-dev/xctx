//! Translating GPUI keystrokes into `xctx-term` key events.

use gpui::Keystroke;
use xctx_term::input::{Key, KeyEvent, KeyEventKind, Modifiers};

/// The terminal key event for a GPUI keystroke, or `None` for keys a terminal has no use for
/// (lone modifiers, Caps Lock, media keys).
///
/// Option acts as Meta: Option chords send the unmodified key with Alt held (ESC prefix in
/// legacy mode) instead of the character macOS would compose, because TUIs bind Alt chords.
/// Without Ctrl, Option or Command, the typed text (`key_char`) is passed on, so shifted
/// punctuation and non-ASCII layouts type what the key shows. `is_held` makes the event an
/// auto-repeat.
pub fn key_event_for_keystroke(keystroke: &Keystroke, is_held: bool) -> Option<KeyEvent> {
    let gpui_modifiers = keystroke.modifiers;
    let modifiers = Modifiers {
        shift: gpui_modifiers.shift,
        ctrl: gpui_modifiers.control,
        alt: gpui_modifiers.alt,
        super_key: gpui_modifiers.platform,
    };
    let key = key_for_name(&keystroke.key)?;

    let composes_text = !(modifiers.ctrl || modifiers.alt || modifiers.super_key);
    let text = match key {
        Key::Char(_) if composes_text => keystroke
            .key_char
            .clone()
            .filter(|text| !text.is_empty() && !text.chars().any(char::is_control)),
        _ => None,
    };

    let kind = if is_held {
        KeyEventKind::Repeat
    } else {
        KeyEventKind::Press
    };
    Some(KeyEvent {
        key,
        text,
        modifiers,
        kind,
    })
}

/// The terminal key for a GPUI key name (`"enter"`, `"pageup"`, `"f5"`, `"a"`, `"!"`).
fn key_for_name(name: &str) -> Option<Key> {
    let key = match name {
        "enter" => Key::Enter,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "escape" => Key::Escape,
        "space" => Key::Char(' '),
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "insert" => Key::Insert,
        "delete" => Key::Delete,
        _ => {
            if let Some(number) = name.strip_prefix('f')
                && let Ok(number) = number.parse::<u8>()
                && (1..=35).contains(&number)
            {
                return Some(Key::F(number));
            }
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if !c.is_control() => Key::Char(c),
                _ => return None,
            }
        }
    };
    Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use xctx_term::TerminalModes;
    use xctx_term::input::encode_key;

    fn keystroke(key: &str, key_char: Option<&str>, mods: gpui::Modifiers) -> Keystroke {
        Keystroke {
            modifiers: mods,
            key: key.to_owned(),
            key_char: key_char.map(str::to_owned),
        }
    }

    fn mods(shift: bool, control: bool, alt: bool) -> gpui::Modifiers {
        gpui::Modifiers {
            control,
            alt,
            shift,
            platform: false,
            function: false,
        }
    }

    fn legacy() -> TerminalModes {
        TerminalModes::default()
    }

    fn pi_modes() -> TerminalModes {
        use xctx_term::KittyKeyboardFlags as F;
        TerminalModes {
            kitty_keyboard: F::DISAMBIGUATE_ESCAPE_CODES
                | F::REPORT_EVENT_TYPES
                | F::REPORT_ALTERNATE_KEYS,
            ..TerminalModes::default()
        }
    }

    fn bytes(stroke: &Keystroke, modes: &TerminalModes) -> Option<Vec<u8>> {
        key_event_for_keystroke(stroke, false).and_then(|event| encode_key(&event, modes))
    }

    #[test]
    fn printable_text_uses_key_char() {
        let a = keystroke("a", Some("a"), mods(false, false, false));
        assert_eq!(bytes(&a, &legacy()), Some(b"a".to_vec()));
        let shift_a = keystroke("a", Some("A"), mods(true, false, false));
        assert_eq!(bytes(&shift_a, &legacy()), Some(b"A".to_vec()));
        // macOS reports Shift+1 as key "!" with Shift cleared.
        let bang = keystroke("!", Some("!"), mods(false, false, false));
        assert_eq!(bytes(&bang, &legacy()), Some(b"!".to_vec()));
        let space = keystroke("space", Some(" "), mods(false, false, false));
        assert_eq!(bytes(&space, &legacy()), Some(b" ".to_vec()));
    }

    #[test]
    fn shift_enter_reaches_kitty_programs() {
        let shift_enter = keystroke("enter", Some("\n"), mods(true, false, false));
        let event = key_event_for_keystroke(&shift_enter, false);
        assert_eq!(event, Some(KeyEvent::new(Key::Enter, Modifiers::SHIFT)));
        assert_eq!(
            bytes(&shift_enter, &pi_modes()),
            Some(b"\x1b[13;2u".to_vec())
        );
        let enter = keystroke("enter", Some("\n"), mods(false, false, false));
        assert_eq!(bytes(&enter, &legacy()), Some(b"\r".to_vec()));
    }

    #[test]
    fn ctrl_chords() {
        let ctrl_c = keystroke("c", None, mods(false, true, false));
        assert_eq!(bytes(&ctrl_c, &legacy()), Some(vec![0x03]));
        assert_eq!(bytes(&ctrl_c, &pi_modes()), Some(b"\x1b[99;5u".to_vec()));
        let ctrl_bracket = keystroke("[", None, mods(false, true, false));
        assert_eq!(bytes(&ctrl_bracket, &legacy()), Some(vec![0x1b]));
        let ctrl_space = keystroke("space", Some(" "), mods(false, true, false));
        assert_eq!(bytes(&ctrl_space, &legacy()), Some(vec![0x00]));
    }

    #[test]
    fn alt_chords_send_meta_not_composed_text() {
        // Option+b composes "∫" on a US layout; the terminal wants ESC b.
        let alt_b = keystroke("b", Some("∫"), mods(false, false, true));
        let event = key_event_for_keystroke(&alt_b, false);
        assert_eq!(event.as_ref().and_then(|e| e.text.clone()), None);
        assert_eq!(bytes(&alt_b, &legacy()), Some(b"\x1bb".to_vec()));
        assert_eq!(bytes(&alt_b, &pi_modes()), Some(b"\x1b[98;3u".to_vec()));
        let alt_shift_b = keystroke("b", Some("ı"), mods(true, false, true));
        assert_eq!(bytes(&alt_shift_b, &legacy()), Some(b"\x1bB".to_vec()));
        let alt_backspace = keystroke("backspace", None, mods(false, false, true));
        assert_eq!(bytes(&alt_backspace, &legacy()), Some(b"\x1b\x7f".to_vec()));
    }

    #[test]
    fn arrows_and_navigation() {
        let up = keystroke("up", None, mods(false, false, false));
        assert_eq!(bytes(&up, &legacy()), Some(b"\x1b[A".to_vec()));
        let ctrl_left = keystroke("left", None, mods(false, true, false));
        assert_eq!(bytes(&ctrl_left, &legacy()), Some(b"\x1b[1;5D".to_vec()));
        let alt_right = keystroke("right", None, mods(false, false, true));
        assert_eq!(bytes(&alt_right, &legacy()), Some(b"\x1b[1;3C".to_vec()));
        let shift_down = keystroke("down", None, mods(true, false, false));
        assert_eq!(bytes(&shift_down, &legacy()), Some(b"\x1b[1;2B".to_vec()));
        let page_up = keystroke("pageup", None, mods(false, false, false));
        assert_eq!(bytes(&page_up, &legacy()), Some(b"\x1b[5~".to_vec()));
        let delete = keystroke("delete", None, mods(false, false, false));
        assert_eq!(bytes(&delete, &legacy()), Some(b"\x1b[3~".to_vec()));
        let home = keystroke("home", None, mods(false, false, false));
        assert_eq!(bytes(&home, &legacy()), Some(b"\x1b[H".to_vec()));
    }

    #[test]
    fn tab_backtab_escape_backspace_function() {
        let tab = keystroke("tab", Some("\t"), mods(false, false, false));
        assert_eq!(bytes(&tab, &legacy()), Some(b"\t".to_vec()));
        let backtab = keystroke("tab", None, mods(true, false, false));
        assert_eq!(bytes(&backtab, &legacy()), Some(b"\x1b[Z".to_vec()));
        let escape = keystroke("escape", None, mods(false, false, false));
        assert_eq!(bytes(&escape, &legacy()), Some(b"\x1b".to_vec()));
        let backspace = keystroke("backspace", None, mods(false, false, false));
        assert_eq!(bytes(&backspace, &legacy()), Some(vec![0x7f]));
        let f5 = keystroke("f5", None, mods(false, false, false));
        assert_eq!(bytes(&f5, &legacy()), Some(b"\x1b[15~".to_vec()));
        let f1 = keystroke("f1", None, mods(false, false, false));
        assert_eq!(bytes(&f1, &legacy()), Some(b"\x1bOP".to_vec()));
    }

    #[test]
    fn held_keys_repeat_and_unknown_keys_are_dropped() {
        let held = keystroke("up", None, mods(false, false, false));
        assert_eq!(
            key_event_for_keystroke(&held, true).map(|event| event.kind),
            Some(KeyEventKind::Repeat)
        );
        let caps = keystroke("capslock", None, mods(false, false, false));
        assert_eq!(key_event_for_keystroke(&caps, false), None);
        let f0 = keystroke("f0", None, mods(false, false, false));
        assert_eq!(key_event_for_keystroke(&f0, false), None);
    }

    #[test]
    fn command_chords_map_to_super() {
        let cmd_k = keystroke(
            "k",
            None,
            gpui::Modifiers {
                platform: true,
                ..gpui::Modifiers::default()
            },
        );
        let event = key_event_for_keystroke(&cmd_k, false);
        assert_eq!(event, Some(KeyEvent::new(Key::Char('k'), Modifiers::SUPER)));
        assert_eq!(bytes(&cmd_k, &legacy()), None);
    }
}
