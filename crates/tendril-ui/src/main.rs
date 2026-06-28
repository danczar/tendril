#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

slint::include_modules!();

mod bridge;
mod models;
mod state;

use anyhow::Result;

fn main() -> Result<()> {
    // Resolve directories first so logging can write to a file under data_dir.
    let dirs = tendril_core::dirs::AppDirs::resolve()?;

    init_logging(&dirs);
    tracing::info!("Starting Tendril v{}", env!("CARGO_PKG_VERSION"));

    let config = tendril_core::config::Config::load(&dirs.config_dir)?;

    // Build shared state
    let shared = state::create_shared_state(config, dirs);

    // Tokio runtime for async work alongside Slint's event loop
    let rt = tokio::runtime::Runtime::new()?;

    // Create UI
    let window = MainWindow::new()?;
    window.set_app_version(env!("CARGO_PKG_VERSION").into());

    // Set initial state from config
    {
        let state = shared.lock().unwrap();
        window.set_output_dir(state.config.output_dir.display().to_string().into());
        window.set_format_index(match state.config.output_format {
            tendril_core::config::OutputFormat::Wav => 0,
            tendril_core::config::OutputFormat::Flac => 1,
            tendril_core::config::OutputFormat::Mp3 => 2,
            tendril_core::config::OutputFormat::Aac => 3,
        });
        window.set_gpu_index(match state.config.gpu_backend {
            tendril_core::config::GpuBackend::Cpu => 1,
            _ => 0,
        });
        window.set_model_index(match state.config.model_variant {
            tendril_core::config::ModelVariant::Htdemucs => 0,
            tendril_core::config::ModelVariant::HtdemucsFineTuned => 1,
        });
        window.set_preserve_full_mix(state.config.preserve_full_mix);
        window.set_create_instrumental(state.config.create_instrumental);
        window.set_target_lufs(state.config.target_lufs.round() as i32);
    }

    // Wire callbacks
    bridge::connect_callbacks(&window, shared.clone(), rt.handle().clone());

    // Hook winit events for file drag-and-drop
    {
        use slint::winit_030::WinitWindowAccessor;
        let weak = window.as_weak();
        window.window().on_winit_window_event(move |_win, event| {
            match event {
                slint::winit_030::winit::event::WindowEvent::HoveredFile(_) => {
                    if let Some(w) = weak.upgrade() {
                        w.set_drop_hovering(true);
                    }
                }
                slint::winit_030::winit::event::WindowEvent::HoveredFileCancelled => {
                    if let Some(w) = weak.upgrade() {
                        w.set_drop_hovering(false);
                    }
                }
                slint::winit_030::winit::event::WindowEvent::DroppedFile(path) => {
                    if let Some(w) = weak.upgrade() {
                        w.set_drop_hovering(false);
                        w.invoke_file_dropped(path.to_string_lossy().to_string().into());
                    }
                }
                _ => return slint::winit_030::EventResult::Propagate,
            }
            slint::winit_030::EventResult::Propagate
        });
    }

    // Populate initial dependency status
    bridge::init_dep_status(&window, &shared);

    // Start pipeline runner (processes queued jobs in background)
    bridge::start_pipeline_runner(shared.clone(), rt.handle().clone(), window.as_weak());

    // Start progress timer (refreshes queue UI every 250ms)
    bridge::start_progress_timer(&window, shared.clone());

    // Ensure lightweight dependencies (ffmpeg + yt-dlp) and check demucs status
    {
        let weak = window.as_weak();
        let dirs = shared.lock().unwrap().dirs.clone();
        rt.spawn(async move {
            let weak_inner = weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = weak_inner.upgrade() {
                    w.set_status_message("Checking dependencies...".into());
                }
            });

            let mgr = tendril_core::deps::DependencyManager::new(&dirs);

            // Download lightweight deps silently
            if let Err(e) = mgr.ensure_lightweight().await {
                tracing::error!("Lightweight dependency setup failed: {e}");
            }

            // Repopulate the dep list after the silent install — the
            // synchronous `init_dep_status` call before this task ran
            // off any stale yt-dlp version cached in versions.json.
            let statuses = mgr.check_status();
            let demucs_ready = mgr.is_demucs_ready();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = weak.upgrade() {
                    w.set_dep_items(bridge::dep_status_model(&statuses));
                    w.set_deps_all_installed(
                        statuses
                            .iter()
                            .all(|s| s.state != tendril_core::deps::DepState::Missing),
                    );
                    if demucs_ready {
                        w.set_status_message("Ready".into());
                    } else {
                        w.set_status_message(
                            "Dependencies missing — click \u{2B07} to set up".into(),
                        );
                    }
                }
            });
        });
    }

    // Run the Slint event loop (blocks until window is closed)
    window.run()?;

    tracing::info!("Shutting down");
    Ok(())
}

/// Set up logging: human-readable lines to stderr (for terminal runs) plus a
/// persistent debug log file under the app data dir (for bundled `.app` runs
/// that have no console).
///
/// Levels:
/// - stderr defaults to `tendril=info`. Passing `debug` / `-v` / `--verbose`
///   on the command line, or setting `RUST_LOG`, raises it to debug across the
///   tendril crates (including yt-dlp and pip output).
/// - the file always captures debug-level detail regardless, so a failed
///   download leaves a complete record to inspect afterwards.
fn init_logging(dirs: &tendril_core::dirs::AppDirs) {
    use tracing_subscriber::{EnvFilter, fmt, prelude::*};

    const DEBUG_DIRECTIVES: &str = "tendril_core=debug,tendril_ui=debug,deps=debug";

    let debug_flag = std::env::args()
        .skip(1)
        .any(|a| matches!(a.as_str(), "debug" | "-d" | "--debug" | "-v" | "--verbose"));
    let stderr_default = if debug_flag {
        DEBUG_DIRECTIVES
    } else {
        "tendril=info"
    };

    let stderr_layer = fmt::layer().with_writer(std::io::stderr).with_filter(
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(stderr_default)),
    );

    let log_path = dirs.log_file();
    let file_layer = std::fs::File::create(&log_path).ok().map(|file| {
        let filter = EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| DEBUG_DIRECTIVES.to_string()),
        );
        fmt::layer()
            .with_ansi(false)
            .with_writer(move || file.try_clone().expect("clone log file handle"))
            .with_filter(filter)
    });
    let logging_to_file = file_layer.is_some();

    tracing_subscriber::registry()
        .with(stderr_layer)
        .with(file_layer)
        .init();

    if logging_to_file {
        tracing::info!("Debug log: {}", log_path.display());
    } else {
        tracing::warn!("Could not open log file at {}", log_path.display());
    }
}
