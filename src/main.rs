//! VeilPilot — a heads-up AI copilot that does not appear in your screen share.
//!
//! Take a screenshot, transcribe what is being said, ask a model about either —
//! all from global hotkeys. The answer streams into a translucent panel that
//! the capture pipeline never sees.

// No console window on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod ai;
mod audio;
mod capture;
mod config;
mod dev;
mod devcat;
mod hotkeys;
mod icons;
mod instance;
mod live;
mod platform;
mod runtime;
mod secrets;
mod session;
mod transcribe;
mod tray;
mod ui;
mod whisper;

use futures::StreamExt as _;
use gpui::{
    App, AppContext as _, Application, Bounds, Size, WindowBackgroundAppearance, WindowBounds, WindowKind,
    WindowOptions, point, px,
};

use crate::ui::panel::{self, External, Panel};

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // `veilpilot --write-icons <dir>` renders the app icon for packaging.
    if let Some(pos) = args.iter().position(|a| a == "--write-icons") {
        let dir = args.get(pos + 1).map(String::as_str).unwrap_or("icons");
        if let Err(err) = icons::write_icons(std::path::Path::new(dir)) {
            eprintln!("could not write icons: {err:#}");
            std::process::exit(1);
        }
        return;
    }

    // `veilpilot --send <command>` talks to the running instance (dev builds
    // use it to drive the app from scripts).
    if let Some(pos) = args.iter().position(|a| a == "--send") {
        let line = args[pos + 1..].join(" ");
        match instance::send(&line) {
            Ok(reply) => {
                println!("{reply}");
                if reply.starts_with("error") {
                    std::process::exit(1);
                }
            }
            Err(err) => {
                eprintln!("no running instance: {err}");
                std::process::exit(2);
            }
        }
        return;
    }

    let commands = match instance::acquire() {
        Ok(instance::Launch::Primary { _lock, commands }) => {
            // The lock lives as long as the process.
            std::mem::forget(_lock);
            commands
        }
        Ok(instance::Launch::Secondary) => return,
        Err(err) => {
            eprintln!("[instance] could not take the single-instance lock: {err}");
            futures::channel::mpsc::unbounded().1
        }
    };

    Application::new().run(move |cx: &mut App| {
        platform::set_accessory_app();
        ui::bind_keys(cx);

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(0.), px(0.)),
                Size { width: px(panel::DEFAULT_SIZE.0 as f32), height: px(panel::DEFAULT_SIZE.1 as f32) },
            ))),
            titlebar: None,
            focus: false,
            show: false,
            kind: WindowKind::PopUp,
            is_movable: true,
            is_resizable: true,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            window_min_size: Some(Size { width: px(panel::MIN_SIZE.0 as f32), height: px(panel::MIN_SIZE.1 as f32) }),
            app_id: Some("co.interopera.veilpilot".into()),
            ..Default::default()
        };

        let window = cx
            .open_window(options, |window, cx| cx.new(|cx| Panel::new(window, cx)))
            .expect("opening the panel window");
        platform::guard_key_window_deadlock();

        let (external_tx, external_rx) = futures::channel::mpsc::unbounded::<External>();
        let panel_entity = window.update(cx, |_, _, cx| cx.entity()).expect("panel entity");

        window
            .update(cx, |panel, window, cx| {
                panel.start(external_tx.clone(), window, cx);

                // Hotkeys, tray clicks and second launches.
                let weak = cx.entity().downgrade();
                cx.spawn_in(window, async move |_, cx| panel::pump_external(weak, external_rx, cx).await).detach();

                // The control channel: `show` always, the rest in dev mode.
                let weak = cx.entity().downgrade();
                let mut commands = commands;
                cx.spawn_in(window, async move |_, cx| {
                    while let Some(command) = commands.next().await {
                        let weak = weak.clone();
                        let alive = cx.update(|window, cx| {
                            if let Some(command) = dev::handle_input(command, window, cx) {
                                if let Some(panel) = weak.upgrade() {
                                    panel.update(cx, |panel, cx| dev::handle(command, panel, window, cx));
                                }
                            }
                        });
                        if alive.is_err() {
                            break;
                        }
                    }
                })
                .detach();
            })
            .expect("starting the panel");

        // Tray app: closing the panel is hiding it, not quitting. A frameless
        // window has no chrome to close, but Cmd-W would still orphan the app.
        window
            .update(cx, |_, window, cx| {
                let weak = panel_entity.downgrade();
                window.on_window_should_close(cx, move |_, cx| {
                    let _ = weak.update(cx, |panel, cx| panel.hide(cx));
                    false
                });
            })
            .ok();
    });
}
