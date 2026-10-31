//! Commands for the dev control channel (`VEILPILOT_DEV=1`). They drive the
//! real app the way a user would — hotkey actions, keystrokes, clicks — and
//! report state back as JSON, so end-to-end tests need no screen access.

use std::time::Duration;

use gpui::{Context, Keystroke, Modifiers, Window};
use serde_json::{Value, json};

use crate::config::{Config, HotkeyId};
use crate::hotkeys::Action;
use crate::instance::Command;
use crate::ui::panel::Panel;
use crate::ui::region;
use crate::ui::settings::SettingsTab;

fn hotkey_by_name(name: &str) -> Option<HotkeyId> {
    HotkeyId::ALL.iter().copied().find(|id| format!("{id:?}").eq_ignore_ascii_case(name))
}

fn merge(base: &mut Value, patch: Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                match b.get_mut(&k) {
                    Some(slot) if slot.is_object() && v.is_object() => merge(slot, v),
                    _ => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (b, p) => *b = p,
    }
}

/// Deliver a "hotkey pressed" Carbon event for `accelerator` to this app,
/// through the same handler the OS calls when the real keys are pressed.
#[cfg(target_os = "macos")]
fn press_hotkey(accelerator: &str) -> Result<(), String> {
    use std::ffi::c_void;
    #[repr(C)]
    struct EventHotKeyId {
        signature: u32,
        id: u32,
    }
    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn CreateEvent(
            alloc: *const c_void,
            class: u32,
            kind: u32,
            when: f64,
            flags: u32,
            out: *mut *mut c_void,
        ) -> i32;
        fn SetEventParameter(event: *mut c_void, name: u32, ty: u32, size: usize, data: *const c_void) -> i32;
        fn GetApplicationEventTarget() -> *mut c_void;
        fn SendEventToEventTarget(event: *mut c_void, target: *mut c_void) -> i32;
        fn ReleaseEvent(event: *mut c_void);
    }
    const fn four(c: &[u8; 4]) -> u32 {
        ((c[0] as u32) << 24) | ((c[1] as u32) << 16) | ((c[2] as u32) << 8) | c[3] as u32
    }
    let hotkey = crate::hotkeys::parse(accelerator)?;
    let id = EventHotKeyId { signature: four(b"htrs"), id: hotkey.id() };
    unsafe {
        let mut event = std::ptr::null_mut();
        if CreateEvent(std::ptr::null(), four(b"keyb"), 5, 0.0, 0, &mut event) != 0 {
            return Err("CreateEvent failed".into());
        }
        SetEventParameter(
            event,
            four(b"----"),
            four(b"hkid"),
            std::mem::size_of::<EventHotKeyId>(),
            &id as *const _ as *const c_void,
        );
        let status = SendEventToEventTarget(event, GetApplicationEventTarget());
        ReleaseEvent(event);
        if status != 0 {
            return Err(format!("SendEventToEventTarget returned {status}"));
        }
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn press_hotkey(_accelerator: &str) -> Result<(), String> {
    Err("hotkey simulation is macOS-only".into())
}

pub fn state(panel: &Panel, window: &Window, cx: &gpui::App) -> Value {
    let frame = panel.native.map(|n| n.frame());
    json!({
        "visible": panel.is_visible(),
        "focused": panel.native.map(|n| n.is_key()).unwrap_or(false),
        "protection": panel.cfg.window.protection,
        "clickThrough": panel.cfg.window.click_through,
        "opacity": panel.cfg.window.opacity,
        "frame": frame.map(|f| json!({"x": f.x, "y": f.y, "width": f.width, "height": f.height})),
        "streaming": panel.is_streaming(),
        "recording": panel.is_recording(),
        "uploading": panel.uploading,
        "capturing": panel.capturing,
        "input": panel.input.read(cx).text(),
        "inputFocused": panel.input.read(cx).is_focused(window),
        "attachments": panel.attachments.iter().map(|s| json!({"id": s.id, "width": s.width, "height": s.height})).collect::<Vec<_>>(),
        "cards": panel.cards.iter().map(|c| json!({
            "label": c.label, "accelerator": c.accelerator, "meta": c.meta, "text": c.text,
            "thinking": c.thinking.chars().count(), "notes": c.notes, "error": c.error, "done": c.done,
            "shots": c.shots.len(),
        })).collect::<Vec<_>>(),
        "transcript": panel.session.transcript_lines().iter().map(|l| l.text.clone()).collect::<Vec<_>>(),
        "drawerOpen": panel.drawer_open,
        "toasts": panel.toasts.iter().map(|t| format!("{:?}: {}", t.kind, t.text)).collect::<Vec<_>>(),
        "settings": panel.settings.as_ref().map(|s| format!("{:?}", s.read(cx).tab())),
        "hotkeyErrors": panel.hotkey_errors.iter().map(|f| format!("{} {} ({})", f.accelerator, f.label, f.reason)).collect::<Vec<_>>(),
        "regionOpen": region::is_open(),
        "tray": panel.tray.as_ref().and_then(|t| t.rect()),
        "micPermission": crate::platform::microphone_permission().as_str(),
        "screenPermission": crate::capture::screen_permission().as_str(),
        "inputDevices": crate::audio::input_devices(),
        "regionFrame": region::native_frame(),
    })
}

/// How much of an image is actually painted, to tell a real capture from a
/// blank one.
fn coverage(image: &image::RgbaImage) -> f64 {
    let opaque = image.pixels().filter(|p| p.0[3] > 0).count();
    opaque as f64 / (image.width() * image.height()).max(1) as f64
}

/// Capture a window of ours (`own`) or the whole display under it.
fn snapshot(native: crate::platform::NativeWindow, path: &str, own: bool) -> anyhow::Result<String> {
    let number = native.window_number() as u32;
    let image = if own {
        let window = xcap::Window::all()?
            .into_iter()
            .find(|w| w.id().ok() == Some(number))
            .ok_or_else(|| anyhow::anyhow!("own window not in the window list"))?;
        window.capture_image()?
    } else {
        let f = native.frame();
        let display = crate::platform::display_at(f.x + 1.0, f.y + 1.0).ok_or_else(|| anyhow::anyhow!("no display"))?;
        crate::capture::grab(&display)?.image
    };
    image.save(path)?;
    Ok(format!("{}x{} coverage={:.3}", image.width(), image.height(), coverage(&image)))
}

/// Commands that feed input to the window. They must run with the panel not
/// leased, because dispatching input redraws the window, which renders the
/// panel. Returns the command back when it is not one of these.
pub fn handle_input(command: Command, window: &mut Window, cx: &mut gpui::App) -> Option<Command> {
    let (verb, rest) = command.line.split_once(' ').unwrap_or((command.line.as_str(), ""));
    let rest = rest.trim().to_string();
    let reply = match verb {
        "key" => match Keystroke::parse(&rest) {
            // The picker is the key window while it is open.
            Ok(ks) if region::is_open() => format!("ok picker handled={:?}", region::dispatch_keystroke(ks, cx)),
            Ok(ks) => format!("ok handled={}", window.dispatch_keystroke(ks, cx)),
            Err(err) => format!("error: {err}"),
        },
        "type" => {
            for ch in rest.chars() {
                let s = ch.to_string();
                let ks = Keystroke { modifiers: Modifiers::default(), key: s.clone(), key_char: Some(s) };
                window.dispatch_keystroke(ks, cx);
            }
            "ok".into()
        }
        "click" | "rclick" => {
            let nums: Vec<f64> = rest.split_whitespace().filter_map(|n| n.parse().ok()).collect();
            match (nums.len(), crate::platform::NativeWindow::from_window(window)) {
                (2, Some(native)) => {
                    // Delivered once the window is free: AppKit hands the
                    // event straight back to GPUI, which drops input for a
                    // window that is mid-update.
                    let (x, y, right) = (nums[0], nums[1], verb == "rclick");
                    cx.spawn(async move |cx| {
                        cx.background_executor().timer(Duration::from_millis(10)).await;
                        native.send_click(x, y, right);
                    })
                    .detach();
                    "ok".into()
                }
                (2, None) => "error: no native window".into(),
                _ => "error: click x y".into(),
            }
        }
        "scroll" => {
            let nums: Vec<f64> = rest.split_whitespace().filter_map(|n| n.parse().ok()).collect();
            match (nums.len(), crate::platform::NativeWindow::from_window(window)) {
                (3, Some(native)) => {
                    let (x, y, lines) = (nums[0], nums[1], nums[2] as i32);
                    cx.spawn(async move |cx| {
                        cx.background_executor().timer(Duration::from_millis(10)).await;
                        native.send_scroll(x, y, lines);
                    })
                    .detach();
                    "ok".into()
                }
                _ => "error: scroll x y lines".into(),
            }
        }
        _ => return Some(command),
    };
    let _ = command.reply.send(reply);
    None
}

pub fn handle(command: Command, panel: &mut Panel, window: &mut Window, cx: &mut Context<Panel>) {
    let line = command.line.clone();
    let (verb, rest) = line.split_once(' ').unwrap_or((line.as_str(), ""));
    let rest = rest.trim().to_string();
    let tx = command.reply;
    // Each branch replies exactly once; a macro lets each one move `tx`.
    macro_rules! reply {
        ($text:expr) => {{
            let _ = tx.send($text);
        }};
    }
    let ok = || "ok".to_string();

    match verb {
        "show" => {
            panel.show(true, window, cx);
            reply!(ok());
        }
        "state" => reply!(state(panel, window, cx).to_string()),
        "config" => reply!(serde_json::to_string(&panel.cfg).unwrap_or_default()),
        "set-config" => match serde_json::from_str::<Value>(&rest) {
            Ok(patch) => {
                let mut value = serde_json::to_value(&panel.cfg).unwrap_or(Value::Null);
                merge(&mut value, patch);
                match serde_json::from_value::<Config>(value) {
                    Ok(next) => {
                        panel.update_config(window, cx, |c| *c = next);
                        reply!(ok());
                    }
                    Err(err) => reply!(format!("error: {err}")),
                }
            }
            Err(err) => reply!(format!("error: {err}")),
        },
        "set-key" => {
            let (provider, key) = rest.split_once(' ').unwrap_or((rest.as_str(), ""));
            panel.set_key(provider, key, cx);
            reply!(ok());
        }
        "action" => match hotkey_by_name(&rest) {
            Some(id) => {
                panel.run_action(Action::Builtin(id), window, cx);
                reply!(ok());
            }
            None => reply!(format!("error: unknown action {rest}")),
        },
        "press" => match press_hotkey(&rest) {
            Ok(()) => reply!(ok()),
            Err(err) => reply!(format!("error: {err}")),
        },
        "prompt" => {
            panel.run_action(Action::Prompt(rest), window, cx);
            reply!(ok());
        }
        "screenshot-raw" => {
            // The display as any screen recorder sees it, protection untouched.
            let target = panel.native;
            let path = rest;
            cx.spawn_in(window, async move |_, cx| {
                cx.background_executor().timer(Duration::from_millis(300)).await;
                let text = match target.map(|t| snapshot(t, &path, false)) {
                    Some(Ok(size)) => format!("ok {size}"),
                    Some(Err(err)) => format!("error: {err:#}"),
                    None => "error: no window".into(),
                };
                let _ = tx.send(text);
            })
            .detach();
        }
        "snapshot" | "screenshot" | "snapshot-region" => {
            // A protected window is invisible to every capture, ours included,
            // so protection is lifted for the capture and restored after.
            let own = verb != "screenshot";
            let target = if verb == "snapshot-region" { region::native(cx) } else { panel.native };
            let Some(target) = target else { return reply!("error: no such window".into()) };
            // AppKit's sharingType getter is unreliable on recent macOS, so the
            // state comes from what was set: the picker is always protected.
            let was = verb == "snapshot-region" || panel.cfg.window.protection;
            if was {
                target.set_content_protection(false);
            }
            // Paint the latest state before capturing it.
            window.refresh();
            let path = rest;
            let panel_entity = cx.entity().downgrade();
            cx.spawn_in(window, async move |_, cx| {
                cx.background_executor().timer(Duration::from_millis(300)).await;
                let result = panel_entity.update(cx, |_, _| {
                    let r = snapshot(target, &path, own);
                    if was {
                        target.set_content_protection(true);
                    }
                    r
                });
                let text = match result {
                    Ok(Ok(size)) => format!("ok {size}"),
                    Ok(Err(err)) => format!("error: {err:#}"),
                    Err(err) => format!("error: {err:#}"),
                };
                let _ = tx.send(text);
            })
            .detach();
        }
        "settings" => {
            if rest == "close" {
                panel.close_settings(window, cx);
            } else {
                panel.open_settings(SettingsTab::from_name(&rest), window, cx);
            }
            reply!(ok());
        }
        "settings-scroll" => match (rest.parse::<f32>(), panel.settings.clone()) {
            (Ok(y), Some(view)) => {
                view.update(cx, |v, cx| v.scroll_to(y, cx));
                reply!(ok());
            }
            _ => reply!("error: settings-scroll <y> with settings open".into()),
        },
        "windows" => {
            // Our own windows as the window server lists them (the tray icon
            // is a status-bar window).
            let pid = std::process::id();
            let list: Vec<Value> = xcap::Window::all()
                .unwrap_or_default()
                .into_iter()
                .filter(|w| w.pid().ok() == Some(pid) || w.app_name().unwrap_or_default().contains("veilpilot"))
                .map(|w| {
                    json!({
                        "pid": w.pid().ok(), "app": w.app_name().unwrap_or_default(),
                        "title": w.title().unwrap_or_default(),
                        "x": w.x().unwrap_or(0), "y": w.y().unwrap_or(0),
                        "width": w.width().unwrap_or(0), "height": w.height().unwrap_or(0),
                        "z": w.z().unwrap_or(0),
                    })
                })
                .collect();
            reply!(Value::Array(list).to_string());
        }
        "transcript" => {
            panel.append_transcript(&rest, cx);
            reply!(ok());
        }
        "feed" => {
            // Record from a WAV file as if it were the microphone, in real
            // time (or `speed` times faster). Stop with the record action.
            let mut words = rest.split_whitespace();
            let path = words.next().unwrap_or_default().to_string();
            let speed: f64 = words.next().and_then(|w| w.parse().ok()).unwrap_or(1.0);
            let decoded = std::fs::read(&path).ok().and_then(|b| crate::audio::decode_wav(&b));
            match decoded {
                Some((samples, rate, _)) => {
                    let seconds = samples.len() as f64 / rate as f64;
                    panel.begin_recording(crate::audio::Recorder::simulated(samples, rate, speed), cx);
                    reply!(format!("ok {seconds:.1}s"));
                }
                None => reply!(format!("error: {path} is not a 16-bit PCM WAV")),
            }
        }
        "transcribe" => {
            let job = match crate::transcribe::job(&panel.cfg, &panel.secrets) {
                Ok(job) => job,
                Err(err) => return reply!(format!("error: {err:#}")),
            };
            let path = rest;
            cx.spawn(async move |this, cx| {
                let result = async {
                    let bytes = std::fs::read(&path)?;
                    let (samples, rate, _) =
                        crate::audio::decode_wav(&bytes).ok_or_else(|| anyhow::anyhow!("not a 16-bit PCM WAV"))?;
                    let wav = crate::audio::encode_segment(vec![(samples, rate)])
                        .ok_or_else(|| anyhow::anyhow!("too short"))?;
                    crate::runtime::spawn(crate::transcribe::run(job, wav)).await?
                }
                .await;
                let text = match result {
                    Ok(text) => {
                        let _ = this.update(cx, |panel, cx| panel.append_transcript(&text, cx));
                        format!("ok {text}")
                    }
                    Err(err) => format!("error: {err:#}"),
                };
                let _ = tx.send(text);
            })
            .detach();
        }
        "region" => {
            let result = if rest == "cancel" {
                region::RegionResult::Cancelled
            } else {
                let n: Vec<f64> = rest.split_whitespace().filter_map(|n| n.parse().ok()).collect();
                if n.len() != 4 {
                    return reply!("error: region x y w h | cancel".into());
                }
                region::RegionResult::Selected(crate::platform::Rect { x: n[0], y: n[1], width: n[2], height: n[3] })
            };
            reply!(if region::complete(result, cx) { ok() } else { "error: no picker open".into() });
        }
        "wait-idle" => {
            let panel_entity = cx.entity().downgrade();
            cx.spawn(async move |_, cx| {
                for _ in 0..1200 {
                    let busy = panel_entity
                        .update(cx, |p, _| p.is_streaming() || p.uploading > 0 || p.capturing > 0)
                        .unwrap_or(false);
                    if !busy {
                        break;
                    }
                    cx.background_executor().timer(Duration::from_millis(100)).await;
                }
                let _ = tx.send("ok".into());
            })
            .detach();
        }
        "quit" => {
            reply!(ok());
            cx.quit();
        }
        _ => reply!(format!("error: unknown command {verb}")),
    }
}
