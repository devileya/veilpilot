//! Global shortcuts, written in Electron's accelerator syntax
//! ("Command+Shift+1") so saved configs keep working.
//!
//! Global shortcuts are first-come-first-served per machine: if another app
//! already owns a combination, registering fails and the app reports it rather
//! than leaving a dead key the user keeps pressing.

use std::collections::HashMap;

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyManager, HotKeyState};

use crate::config::{Config, HotkeyId};

/// What a hotkey does when pressed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Builtin(HotkeyId),
    Prompt(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub accelerator: String,
    pub label: String,
    pub reason: String,
}

pub fn parse(accelerator: &str) -> Result<HotKey, String> {
    let mut mods = Modifiers::empty();
    let mut code = None;
    let parts: Vec<&str> = accelerator.split('+').collect();
    // "Command+Shift++" style accelerators are not something Electron
    // accepts either, so a plain split is enough.
    for (i, part) in parts.iter().enumerate() {
        let is_last = i == parts.len() - 1;
        let lower = part.trim().to_ascii_lowercase();
        let modifier = match lower.as_str() {
            "command" | "cmd" | "super" | "meta" => Some(Modifiers::SUPER),
            "control" | "ctrl" => Some(Modifiers::CONTROL),
            "commandorcontrol" | "cmdorctrl" => {
                Some(if cfg!(target_os = "macos") { Modifiers::SUPER } else { Modifiers::CONTROL })
            }
            "alt" | "option" | "altgr" => Some(Modifiers::ALT),
            "shift" => Some(Modifiers::SHIFT),
            _ => None,
        };
        match (modifier, is_last) {
            (Some(m), false) => mods |= m,
            (_, true) => code = Some(key_code(part.trim()).ok_or_else(|| format!("unknown key \"{part}\""))?),
            (None, false) => return Err(format!("unknown modifier \"{part}\"")),
        }
    }
    let code = code.ok_or("no key")?;
    if mods.is_empty() {
        return Err("at least one modifier is required".into());
    }
    Ok(HotKey::new(Some(mods), code))
}

fn key_code(key: &str) -> Option<Code> {
    let upper = key.to_ascii_uppercase();
    if upper.len() == 1 {
        let c = upper.chars().next().unwrap();
        return match c {
            'A'..='Z' => letter(c),
            '0'..='9' => digit(c),
            '-' => Some(Code::Minus),
            '=' => Some(Code::Equal),
            '\\' => Some(Code::Backslash),
            '[' => Some(Code::BracketLeft),
            ']' => Some(Code::BracketRight),
            ';' => Some(Code::Semicolon),
            '\'' => Some(Code::Quote),
            ',' => Some(Code::Comma),
            '.' => Some(Code::Period),
            '/' => Some(Code::Slash),
            '`' => Some(Code::Backquote),
            _ => None,
        };
    }
    if let Some(n) = upper.strip_prefix('F').and_then(|n| n.parse::<u8>().ok()) {
        return function_key(n);
    }
    Some(match upper.as_str() {
        "RETURN" | "ENTER" => Code::Enter,
        "ESCAPE" | "ESC" => Code::Escape,
        "SPACE" => Code::Space,
        "TAB" => Code::Tab,
        "BACKSPACE" => Code::Backspace,
        "DELETE" | "DEL" => Code::Delete,
        "UP" => Code::ArrowUp,
        "DOWN" => Code::ArrowDown,
        "LEFT" => Code::ArrowLeft,
        "RIGHT" => Code::ArrowRight,
        "HOME" => Code::Home,
        "END" => Code::End,
        "PAGEUP" => Code::PageUp,
        "PAGEDOWN" => Code::PageDown,
        "PLUS" => Code::Equal,
        _ => return None,
    })
}

fn letter(c: char) -> Option<Code> {
    use Code::*;
    const LETTERS: [Code; 26] = [
        KeyA, KeyB, KeyC, KeyD, KeyE, KeyF, KeyG, KeyH, KeyI, KeyJ, KeyK, KeyL, KeyM, KeyN, KeyO, KeyP, KeyQ, KeyR,
        KeyS, KeyT, KeyU, KeyV, KeyW, KeyX, KeyY, KeyZ,
    ];
    LETTERS.get((c as u8 - b'A') as usize).copied()
}

fn digit(c: char) -> Option<Code> {
    use Code::*;
    const DIGITS: [Code; 10] = [Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9];
    DIGITS.get((c as u8 - b'0') as usize).copied()
}

fn function_key(n: u8) -> Option<Code> {
    use Code::*;
    const KEYS: [Code; 24] =
        [F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12, F13, F14, F15, F16, F17, F18, F19, F20, F21, F22, F23, F24];
    KEYS.get((n as usize).checked_sub(1)?).copied()
}

/* ---------- display ---------- */

/// "Command+Shift+1" -> "⌘⇧1" on macOS, "Ctrl+Shift+1" elsewhere.
pub fn pretty(accelerator: &str) -> String {
    if accelerator.is_empty() {
        return "—".into();
    }
    let parts = accelerator.split('+');
    if cfg!(target_os = "macos") {
        parts
            .map(|p| match p {
                "Command" | "CommandOrControl" | "Cmd" => "⌘",
                "Control" | "Ctrl" => "⌃",
                "Alt" | "Option" => "⌥",
                "Shift" => "⇧",
                "Return" | "Enter" => "↩",
                "Escape" => "⎋",
                "Space" => "␣",
                "Up" => "↑",
                "Down" => "↓",
                "Left" => "←",
                "Right" => "→",
                "Backspace" => "⌫",
                "Delete" => "⌦",
                "Tab" => "⇥",
                other => other,
            })
            .collect()
    } else {
        parts
            .map(|p| match p {
                "CommandOrControl" | "Control" => "Ctrl",
                "Command" => "Win",
                "Return" => "Enter",
                "Escape" => "Esc",
                other => other,
            })
            .collect::<Vec<_>>()
            .join("+")
    }
}

/// Build an accelerator from a GPUI keystroke. Reads the physical key, so
/// Shift+2 is "Shift+2", not "@". `None` without a modifier or for keys an
/// accelerator cannot name.
pub fn from_keystroke(keystroke: &gpui::Keystroke) -> Option<String> {
    let m = &keystroke.modifiers;
    let mut parts: Vec<&str> = Vec::new();
    if m.platform {
        parts.push(if cfg!(target_os = "macos") { "Command" } else { "Super" });
    }
    if m.control {
        parts.push("Control");
    }
    if m.alt {
        parts.push("Alt");
    }
    if m.shift {
        parts.push("Shift");
    }
    if parts.is_empty() {
        return None;
    }
    let key = keystroke.key.as_str();
    let named = match key {
        "enter" => "Return".to_string(),
        "escape" => "Escape".into(),
        "space" => "Space".into(),
        "tab" => "Tab".into(),
        "backspace" => "Backspace".into(),
        "delete" => "Delete".into(),
        "up" => "Up".into(),
        "down" => "Down".into(),
        "left" => "Left".into(),
        "right" => "Right".into(),
        k if k.len() > 1 && k.starts_with('f') && k[1..].parse::<u8>().is_ok() => k.to_ascii_uppercase(),
        k if k.chars().count() == 1 => {
            let c = k.chars().next().unwrap();
            let c = shifted_base(c).unwrap_or(c);
            if c.is_ascii_alphanumeric() || "-=\\[];',./`".contains(c) {
                c.to_ascii_uppercase().to_string()
            } else {
                return None;
            }
        }
        _ => return None,
    };
    let accelerator = format!("{}+{}", parts.join("+"), named);
    parse(&accelerator).ok().map(|_| accelerator)
}

/// The unshifted key for a shifted US-layout character, in case a platform
/// reports the character rather than the key.
fn shifted_base(c: char) -> Option<char> {
    let pairs = [
        ('!', '1'),
        ('@', '2'),
        ('#', '3'),
        ('$', '4'),
        ('%', '5'),
        ('^', '6'),
        ('&', '7'),
        ('*', '8'),
        ('(', '9'),
        (')', '0'),
        ('_', '-'),
        ('+', '='),
        ('|', '\\'),
        ('{', '['),
        ('}', ']'),
        (':', ';'),
        ('"', '\''),
        ('<', ','),
        ('>', '.'),
        ('?', '/'),
        ('~', '`'),
    ];
    pairs.iter().find(|(s, _)| *s == c).map(|(_, b)| *b)
}

/* ---------- registration ---------- */

pub struct Hotkeys {
    manager: Option<GlobalHotKeyManager>,
    registered: Vec<HotKey>,
    actions: HashMap<u32, Action>,
    suspended: bool,
}

impl Hotkeys {
    pub fn new() -> Self {
        let manager = match GlobalHotKeyManager::new() {
            Ok(m) => Some(m),
            Err(err) => {
                eprintln!("[hotkeys] unavailable: {err}");
                None
            }
        };
        Self { manager, registered: Vec::new(), actions: HashMap::new(), suspended: false }
    }

    /// Forward presses to `on_press`. The callback runs on the platform's
    /// event thread and must be cheap.
    pub fn listen(on_press: impl Fn(u32) + Send + Sync + 'static) {
        global_hotkey::GlobalHotKeyEvent::set_event_handler(Some(move |event: global_hotkey::GlobalHotKeyEvent| {
            if event.state() == HotKeyState::Pressed {
                on_press(event.id());
            }
        }));
    }

    pub fn action(&self, id: u32) -> Option<Action> {
        if self.suspended {
            return None;
        }
        self.actions.get(&id).cloned()
    }

    pub fn release(&mut self) {
        if let Some(manager) = &self.manager {
            for hk in self.registered.drain(..) {
                let _ = manager.unregister(hk);
            }
        }
        self.actions.clear();
    }

    /// While Settings captures a new combination the global hotkeys must not
    /// swallow it, so they are released and re-applied afterwards.
    pub fn suspend(&mut self) {
        self.release();
        self.suspended = true;
    }

    pub fn apply(&mut self, cfg: &Config) -> Vec<Failure> {
        self.release();
        self.suspended = false;
        let Some(manager) = &self.manager else {
            return vec![Failure {
                accelerator: String::new(),
                label: "All hotkeys".into(),
                reason: "global shortcuts are not available".into(),
            }];
        };

        let mut bindings: Vec<(String, Action, String)> = HotkeyId::ALL
            .iter()
            .map(|id| (cfg.hotkeys.get(*id).to_string(), Action::Builtin(*id), id.failure_label().to_string()))
            .collect();
        for preset in &cfg.prompts {
            let label = if preset.label.is_empty() { preset.id.clone() } else { preset.label.clone() };
            bindings.push((preset.accelerator.clone(), Action::Prompt(preset.id.clone()), label));
        }

        let mut failed = Vec::new();
        for (accelerator, action, label) in bindings {
            if accelerator.is_empty() {
                continue;
            }
            let hotkey = match parse(&accelerator) {
                Ok(hk) => hk,
                Err(reason) => {
                    failed.push(Failure { accelerator, label, reason });
                    continue;
                }
            };
            if self.actions.contains_key(&hotkey.id()) {
                failed.push(Failure { accelerator, label, reason: "duplicate".into() });
                continue;
            }
            match manager.register(hotkey) {
                Ok(()) => {
                    self.registered.push(hotkey);
                    self.actions.insert(hotkey.id(), action);
                }
                Err(err) => {
                    failed.push(Failure { accelerator, label, reason: format!("in use by another app ({err})") })
                }
            }
        }
        failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_default() {
        let cfg = Config::default();
        for id in HotkeyId::ALL {
            parse(cfg.hotkeys.get(id)).unwrap_or_else(|e| panic!("{id:?}: {e}"));
        }
        for p in &cfg.prompts {
            parse(&p.accelerator).unwrap();
        }
    }

    #[test]
    fn parse_details() {
        let hk = parse("Command+Shift+\\").unwrap();
        assert_eq!(hk, HotKey::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::Backslash));
        assert_eq!(parse("Alt+Shift+Up").unwrap(), HotKey::new(Some(Modifiers::ALT | Modifiers::SHIFT), Code::ArrowUp));
        assert_eq!(parse("Control+F5").unwrap(), HotKey::new(Some(Modifiers::CONTROL), Code::F5));
        assert!(parse("Shift").is_err());
        assert!(parse("A").is_err(), "needs a modifier");
        assert!(parse("Hyper+A").is_err());
    }

    #[test]
    fn pretty_prints() {
        if cfg!(target_os = "macos") {
            assert_eq!(pretty("Command+Shift+Return"), "⌘⇧↩");
            assert_eq!(pretty("Alt+Shift+Up"), "⌥⇧↑");
        } else {
            assert_eq!(pretty("Control+Shift+Return"), "Ctrl+Shift+Enter");
        }
        assert_eq!(pretty(""), "—");
    }

    #[test]
    fn keystroke_to_accelerator() {
        let ks = |s: &str| gpui::Keystroke::parse(s).unwrap();
        let cmd = if cfg!(target_os = "macos") { "Command" } else { "Super" };
        assert_eq!(from_keystroke(&ks("cmd-shift-2")), Some(format!("{cmd}+Shift+2")));
        assert_eq!(from_keystroke(&ks("cmd-shift-@")), Some(format!("{cmd}+Shift+2")));
        assert_eq!(from_keystroke(&ks("ctrl-alt-enter")), Some("Control+Alt+Return".into()));
        assert_eq!(from_keystroke(&ks("alt-shift-up")), Some("Alt+Shift+Up".into()));
        assert_eq!(from_keystroke(&ks("cmd-f12")), Some(format!("{cmd}+F12")));
        assert_eq!(from_keystroke(&ks("a")), None);
    }
}
