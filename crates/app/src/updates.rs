//! Update check: at startup (unless turned off) and from Settings or the palette, the
//! update feed on switchhead's website (`latest.json`, published with the site) is
//! compared with the running version, and a newer one is announced in the status bar with
//! a link to download it. switchhead does not download or install anything itself.

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use serde::{Deserialize, Serialize};
use switchhead_hosting::api::{self, Client, Release};

use crate::appearance::rpx;
use crate::theme::Palette;
use crate::ui;
use crate::workspace::Workspace;

/// The update feed. `SWITCHHEAD_UPDATE_URL` replaces it (manual checks against a stand-in).
const FEED: &str = "https://jeremielumandong.github.io/switchhead/latest.json";

/// Update and crash report settings, saved in the state file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateSettings {
    /// Check for a newer release at startup.
    pub check_at_startup: bool,
    /// File name of the newest crash report already offered.
    pub crash_seen: Option<String>,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self {
            check_at_startup: true,
            crash_seen: None,
        }
    }
}

/// What the status bar says about updates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateStatus {
    /// A check the user asked for is running.
    Checking,
    /// A newer release.
    Available(Release),
    /// The running version is the latest (after a check the user asked for).
    UpToDate,
    /// The check the user asked for failed.
    Failed(String),
}

/// Look up the latest release (blocking): a newer one, or `None` when this is the latest.
pub fn check() -> Result<Option<Release>, String> {
    let feed = std::env::var("SWITCHHEAD_UPDATE_URL").unwrap_or_else(|_| FEED.to_owned());
    let started = std::time::Instant::now();
    let latest = Client::new()
        .and_then(|c| c.latest_release(&feed))
        .map_err(|e| e.to_string())?;
    tracing::debug!(
        ms = started.elapsed().as_millis() as u64,
        latest = latest.as_ref().map(|r| r.version.as_str()),
        "update check"
    );
    Ok(latest.filter(|r| api::is_newer(&r.version, env!("CARGO_PKG_VERSION"))))
}

/// How long the result of a check the user asked for stays in the status bar.
const RESULT_SHOWN: std::time::Duration = std::time::Duration::from_secs(8);

impl Workspace {
    /// Check for a newer release in the background. `manual`: the user asked, so say
    /// that it is checking and how it went; a startup check only announces a release.
    pub(crate) fn check_for_updates(&mut self, manual: bool, cx: &mut Context<Self>) {
        if manual {
            self.update = Some(UpdateStatus::Checking);
            cx.notify();
        }
        let task = cx.background_executor().spawn(async { check() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let status = match result {
                Ok(Some(release)) => Some(UpdateStatus::Available(release)),
                Ok(None) => manual.then_some(UpdateStatus::UpToDate),
                Err(e) => {
                    tracing::info!(error = %e, "update check failed");
                    manual.then_some(UpdateStatus::Failed(e))
                }
            };
            let transient = matches!(
                status,
                Some(UpdateStatus::UpToDate | UpdateStatus::Failed(_))
            );
            let shown = status.clone();
            let _ = this.update(cx, |ws, cx| {
                // A release found earlier stays announced.
                if !matches!(ws.update, Some(UpdateStatus::Available(_))) || status.is_some() {
                    ws.update = status;
                }
                cx.notify();
            });
            if transient {
                cx.background_executor().timer(RESULT_SHOWN).await;
                let _ = this.update(cx, |ws, cx| {
                    if ws.update == shown {
                        ws.update = None;
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    /// Look for a crash report not offered yet (in the background).
    pub(crate) fn find_crash_report(&mut self, cx: &mut Context<Self>) {
        let dir = self.paths.crashes_dir();
        let seen = self.state.updates.crash_seen.clone();
        let task = cx
            .background_executor()
            .spawn(async move { crate::crash::unseen(&dir, seen.as_deref()) });
        cx.spawn(async move |this, cx| {
            if let Some(path) = task.await {
                let _ = this.update(cx, |ws, cx| {
                    ws.crash = Some(path);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Hide the crash notice for good (`show`: reveal the report first).
    fn close_crash_notice(&mut self, show: bool, cx: &mut Context<Self>) {
        let Some(path) = self.crash.take() else {
            return;
        };
        if show {
            cx.reveal_path(&path);
        }
        self.state.updates.crash_seen =
            path.file_name().and_then(|n| n.to_str()).map(str::to_owned);
        self.save(cx);
        cx.notify();
    }

    /// Open the crash reports folder.
    pub(crate) fn show_crash_reports(&mut self, cx: &mut Context<Self>) {
        let dir = self.paths.crashes_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.notice = Some(format!("Could not open {}: {e}", dir.display()).into());
            cx.notify();
            return;
        }
        cx.open_with_system(&dir);
    }

    /// Turn the startup check on or off.
    pub(crate) fn set_check_at_startup(&mut self, on: bool, cx: &mut Context<Self>) {
        self.state.updates.check_at_startup = on;
        self.save(cx);
        cx.notify();
    }

    /// The status bar's crash notice, if a report has not been offered yet.
    pub(crate) fn render_crash_notice(
        &self,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.crash.as_ref()?;
        let link = |id: &'static str, label: &'static str, color| {
            div()
                .id(id)
                .flex_none()
                .px(rpx(6.))
                .rounded(px(4.))
                .cursor_pointer()
                .text_color(color)
                .hover(|s| s.bg(p.hover).text_color(p.fg))
                .child(label)
        };
        Some(
            div()
                .flex()
                .items_center()
                .gap(rpx(5.))
                .min_w_0()
                .text_color(p.deleted)
                .child(ui::dot(p.deleted, 6.))
                .child(
                    div()
                        .truncate()
                        .child("switchhead ran into a problem last time"),
                )
                .child(
                    link("crash-show", "Show Crash Report", p.fg2)
                        .on_click(cx.listener(|ws, _, _, cx| ws.close_crash_notice(true, cx))),
                )
                .child(
                    link("crash-dismiss", "Dismiss", p.fg3)
                        .on_click(cx.listener(|ws, _, _, cx| ws.close_crash_notice(false, cx))),
                )
                .into_any_element(),
        )
    }

    /// The status bar's update item, if there is something to say.
    pub(crate) fn render_update(&self, p: &Palette) -> Option<AnyElement> {
        Some(match self.update.as_ref()? {
            UpdateStatus::Checking => div()
                .flex()
                .items_center()
                .gap(rpx(6.))
                .child(ui::pulse_dot("update-checking", p.acc, 6.))
                .child("Checking for updates…")
                .into_any_element(),
            UpdateStatus::UpToDate => div()
                .text_color(p.fg3)
                .child(format!(
                    "switchhead {} is the latest version",
                    env!("CARGO_PKG_VERSION")
                ))
                .into_any_element(),
            UpdateStatus::Failed(e) => div()
                .text_color(p.deleted)
                .truncate()
                .max_w(rpx(360.))
                .child(format!("Update check failed: {e}"))
                .into_any_element(),
            UpdateStatus::Available(r) => {
                let url = r.url.clone();
                div()
                    .id("update-available")
                    .flex()
                    .items_center()
                    .gap(rpx(5.))
                    .px(rpx(6.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .text_color(p.acc)
                    .hover(|s| s.bg(p.hover))
                    .child(ui::dot(p.acc, 6.))
                    .child(format!("switchhead {} is available", r.version))
                    .on_click(move |_, _, cx| cx.open_url(&url))
                    .tooltip(|w, cx| Tooltip::new("Open the download page").build(w, cx))
                    .into_any_element()
            }
        })
    }
}
