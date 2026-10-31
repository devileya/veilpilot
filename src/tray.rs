//! The menu-bar (macOS) / notification-area (Windows) icon and its menu.

use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::config::Config;
use crate::hotkeys::pretty;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayCommand {
    ToggleOverlay,
    RecordToggle,
    NewSession,
    ToggleProtection,
    ClickThrough,
    Quit,
}

const IDS: [(&str, TrayCommand); 6] = [
    ("toggle", TrayCommand::ToggleOverlay),
    ("record", TrayCommand::RecordToggle),
    ("new", TrayCommand::NewSession),
    ("protect", TrayCommand::ToggleProtection),
    ("click", TrayCommand::ClickThrough),
    ("quit", TrayCommand::Quit),
];

pub struct Tray {
    icon: TrayIcon,
    toggle: MenuItem,
    record: MenuItem,
    new_session: MenuItem,
    protection: CheckMenuItem,
    click_through: CheckMenuItem,
}

impl Tray {
    /// Must run on the main thread after the app has finished launching.
    pub fn new(cfg: &Config, on_command: impl Fn(TrayCommand) + Send + Sync + Clone + 'static) -> anyhow::Result<Self> {
        if cfg!(target_os = "linux") {
            // tray-icon needs a running GTK main loop on Linux; GPUI has its own.
            anyhow::bail!("no tray icon on Linux; use the hotkeys");
        }
        let toggle = MenuItem::with_id("toggle", "", true, None);
        let record = MenuItem::with_id("record", "", true, None);
        let new_session = MenuItem::with_id("new", "", true, None);
        let protection = CheckMenuItem::with_id("protect", "Hidden from screen share", true, true, None);
        let click_through = CheckMenuItem::with_id("click", "Click-through", true, false, None);
        let quit = MenuItem::with_id("quit", "Quit VeilPilot", true, None);

        let menu = Menu::new();
        menu.append_items(&[
            &toggle,
            &record,
            &new_session,
            &PredefinedMenuItem::separator(),
            &protection,
            &click_through,
            &PredefinedMenuItem::separator(),
            &quit,
        ])?;

        let size = 32;
        let icon = tray_icon::Icon::from_rgba(crate::icons::tray(size), size, size)?;
        let icon = TrayIconBuilder::new()
            .with_icon(icon)
            .with_icon_as_template(true)
            .with_menu(Box::new(menu))
            // Left click toggles the panel; the menu is on right click.
            .with_menu_on_left_click(false)
            .build()?;

        let menu_handler = on_command.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some((_, cmd)) = IDS.iter().find(|(id, _)| event.id.0 == *id) {
                menu_handler(*cmd);
            }
        }));
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                on_command(TrayCommand::ToggleOverlay);
            }
        }));

        let tray = Self { icon, toggle, record, new_session, protection, click_through };
        tray.refresh(cfg);
        Ok(tray)
    }

    /// Where the icon sits in the menu bar, if it is showing.
    pub fn rect(&self) -> Option<(f64, f64, u32, u32)> {
        self.icon.rect().map(|r| (r.position.x, r.position.y, r.size.width, r.size.height))
    }

    pub fn refresh(&self, cfg: &Config) {
        let protected = cfg.window.protection;
        let _ = self.icon.set_tooltip(Some(if protected {
            "VeilPilot — hidden from screen share"
        } else {
            "VeilPilot — VISIBLE in screen share"
        }));
        self.toggle.set_text(format!("Show / hide  ({})", pretty(&cfg.hotkeys.toggle_overlay)));
        self.record.set_text(format!("Start / stop transcribe  ({})", pretty(&cfg.hotkeys.record_toggle)));
        self.new_session.set_text(format!("New session  ({})", pretty(&cfg.hotkeys.new_session)));
        self.protection.set_checked(protected);
        self.click_through.set_checked(cfg.window.click_through);
    }
}
