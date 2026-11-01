//! Checking for a newer release and installing it.

use std::time::Duration;

use gpui::Context;

use super::{Panel, ToastKind};
use crate::update::{self, Release};

#[derive(Default)]
pub struct UpdateState {
    pub checking: bool,
    pub installing: bool,
    /// A newer release than this build.
    pub available: Option<Release>,
    /// The outcome of the last check, for Settings › About.
    pub status: Option<String>,
}

/// How long a check stays fresh before the next launch checks again.
const CHECK_EVERY: chrono::TimeDelta = chrono::TimeDelta::hours(20);

impl Panel {
    /// At launch: check once a day, a few seconds in (never in dev mode,
    /// unless a test points the check at its own server).
    pub(super) fn schedule_update_check(&mut self, cx: &mut Context<Self>) {
        if !self.cfg.updates.check {
            return;
        }
        if std::env::var_os("VEILPILOT_DEV").is_some() && std::env::var_os("VEILPILOT_UPDATE_URL").is_none() {
            return;
        }
        let fresh = chrono::DateTime::parse_from_rfc3339(&self.cfg.updates.last_check)
            .is_ok_and(|t| chrono::Utc::now().signed_duration_since(t) < CHECK_EVERY);
        if fresh {
            return;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(4)).await;
            let _ = this.update(cx, |this, cx| this.check_for_updates(false, cx));
        })
        .detach();
    }

    /// Ask GitHub for the latest release. `manual`: the user asked, so say
    /// how it went even when there is nothing new.
    pub fn check_for_updates(&mut self, manual: bool, cx: &mut Context<Self>) {
        if self.update.checking || self.update.installing {
            return;
        }
        self.update.checking = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = crate::runtime::spawn(update::check()).await.unwrap_or_else(|e| Err(anyhow::anyhow!("{e}")));
            let _ = this.update(cx, |this, cx| {
                this.update.checking = false;
                this.cfg.updates.last_check = chrono::Utc::now().to_rfc3339();
                this.save_config();
                match result {
                    Ok(Some(release)) => {
                        let text = format!(
                            "VeilPilot {} is available (you have {}).",
                            release.version,
                            update::current_version()
                        );
                        this.update.status = Some(text.clone());
                        this.update.available = Some(release);
                        this.toast(ToastKind::Info, format!("{text} Settings › About to update."), cx);
                    }
                    Ok(None) => {
                        let text = format!("VeilPilot {} is the latest version.", update::current_version());
                        this.update.status = Some(text.clone());
                        this.update.available = None;
                        if manual {
                            this.toast(ToastKind::Info, text, cx);
                        }
                    }
                    Err(err) => {
                        let text = format!("Could not check for updates: {err:#}");
                        this.update.status = Some(text.clone());
                        if manual {
                            this.toast(ToastKind::Error, text, cx);
                        } else {
                            eprintln!("[update] {text}");
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Install the available release and restart into it — or, where this
    /// build cannot replace itself, open its download.
    pub fn install_update(&mut self, cx: &mut Context<Self>) {
        let Some(release) = self.update.available.clone() else { return };
        if self.update.installing {
            return;
        }
        if !update::can_install() {
            let target = release.asset.as_ref().map(|a| a.url.clone()).unwrap_or_else(|| release.page.clone());
            crate::platform::open_external(&target);
            self.toast(ToastKind::Info, format!("Downloading VeilPilot {} in your browser.", release.version), cx);
            return;
        }
        self.update.installing = true;
        self.update.status = Some(format!("Downloading VeilPilot {}…", release.version));
        self.toast(ToastKind::Info, format!("Downloading VeilPilot {}…", release.version), cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let wanted = release.clone();
            let result = crate::runtime::spawn(async move { update::install(&wanted).await })
                .await
                .unwrap_or_else(|e| Err(anyhow::anyhow!("{e}")));
            let _ = this.update(cx, |this, cx| match result.and_then(|bundle| update::relaunch_after_exit(&bundle)) {
                Ok(()) => {
                    this.toast(ToastKind::Info, format!("Restarting into VeilPilot {}…", release.version), cx);
                    cx.spawn(async move |_, cx| {
                        cx.background_executor().timer(Duration::from_millis(600)).await;
                        let _ = cx.update(|cx| cx.quit());
                    })
                    .detach();
                }
                Err(err) => {
                    this.update.installing = false;
                    let text = format!("Could not install the update: {err:#}");
                    this.update.status = Some(text.clone());
                    this.toast(ToastKind::Error, text, cx);
                }
            });
        })
        .detach();
    }
}
