//! switchhead: a fast, native Git client.

// Release builds on Windows are GUI-subsystem binaries: no console window behind the app.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

extern crate gpui_kit as gpui;

mod actions;
mod activity;
mod appearance;
mod assets;
mod changes_model;
mod changes_view;
mod commit_box;
mod commit_view;
mod diff_model;
mod diff_view;
mod file_list;
mod file_view;
mod graph_paint;
mod image_diff;
mod log_file;
mod manager;
mod merge_view;
mod minimap;
mod omarchy;
mod palette;
mod rebase_editor;
mod rebase_model;
mod reflog_view;
mod repo_view;
mod sidebar_model;
mod split;
mod store;
mod theme;
mod tree_view;
mod ui;
mod workspace;

use std::borrow::Cow;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use gpui_kit::component::TitleBar;
use gpui_kit::{App, AppContext as _, Bounds, WindowBounds, WindowOptions, px, size};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

use crate::store::{Paths, State};

const FONTS: &[&[u8]] = &[
    include_bytes!("../assets/fonts/Geist-Regular.ttf"),
    include_bytes!("../assets/fonts/Geist-Medium.ttf"),
    include_bytes!("../assets/fonts/Geist-SemiBold.ttf"),
    include_bytes!("../assets/fonts/Geist-Italic.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Regular.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Medium.ttf"),
    include_bytes!("../assets/fonts/GeistMono-SemiBold.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Italic.ttf"),
];

fn load_fonts(cx: &mut App) {
    let fonts: Vec<Cow<'static, [u8]>> = FONTS.iter().map(|f| Cow::Borrowed(*f)).collect();
    if let Err(e) = cx.text_system().add_fonts(fonts) {
        tracing::warn!(error = %e, "could not load bundled fonts");
    }
}

/// Log to stdout and to `<data>/logs/switchhead.log` (rotated; see [`log_file`]). Panics
/// are logged too, then reported as usual.
fn init_logging(paths: &Paths) {
    let filter =
        EnvFilter::try_from_env("SWITCHHEAD_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let file = match log_file::LogFile::start(&paths.logs_dir()) {
        Ok(f) => Some(f),
        Err(e) => {
            eprintln!(
                "switchhead: no log file in {}: {e}",
                paths.logs_dir().display()
            );
            None
        }
    };
    let file_layer = file.map(|f| {
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(f)
    });
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stdout))
        .with(file_layer)
        .init();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(panic = %info, "the app panicked");
        default_hook(info);
    }));
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        "switchhead starting"
    );
}

fn main() -> Result<()> {
    let started = std::time::Instant::now();
    let paths = Paths::resolve().context("could not determine a home directory")?;
    init_logging(&paths);
    let state = State::load(&paths.state_file());
    // `switchhead <path>` opens a repository (or scans a folder).
    let open = std::env::args_os().nth(1).map(PathBuf::from);

    gpui_kit::application()
        .with_assets(assets::AppAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            load_fonts(cx);
            actions::init(cx);
            // SWITCHHEAD_ZOOM and SWITCHHEAD_THEME (tests, screenshots) win over the saved ones.
            let mut look = state.appearance.clone();
            if let Some(z) = appearance::env_zoom() {
                look.zoom = z;
            }
            appearance::set(look, cx);
            cx.set_global(state.diff);
            cx.set_global(state.messages.clone());
            cx.set_global(state.identities.clone());
            cx.set_global(image_diff::ImageView::default());
            let theme_key = std::env::var("SWITCHHEAD_THEME")
                .ok()
                .or_else(|| state.theme.clone());
            let start = theme_key
                .map(|k| theme::ThemeId::from_key(&k))
                .unwrap_or(theme::ThemeId::Dark);
            theme::apply(start.palette(), None, cx);
            let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(800.), px(500.))),
                app_id: Some("switchhead".into()),
                ..TitleBar::window_options()
            };
            let opened = gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| workspace::Workspace::new(paths, state, open, window, cx))
            });
            if let Err(e) = opened {
                tracing::error!(error = %e, "failed to open the main window");
                cx.quit();
                return;
            }
            tracing::debug!(ms = started.elapsed().as_millis() as u64, "window opened");
            cx.activate(true);
        });
    Ok(())
}
