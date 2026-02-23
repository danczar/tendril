slint::include_modules!();

mod bridge;
mod models;
mod state;

use anyhow::Result;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "tendril=info".into()),
        )
        .init();

    tracing::info!("Starting Tendril");

    #[cfg(target_os = "macos")]
    set_macos_dock_icon();

    // Initialize core
    let dirs = tendril_core::dirs::AppDirs::resolve()?;
    let config = tendril_core::config::Config::load(&dirs.config_dir)?;

    // Build shared state
    let shared = state::create_shared_state(config, dirs);

    // Tokio runtime for async work alongside Slint's event loop
    let rt = tokio::runtime::Runtime::new()?;

    // Create UI
    let window = MainWindow::new()?;

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
    }

    // Wire callbacks
    bridge::connect_callbacks(&window, shared.clone(), rt.handle().clone());

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

            let demucs_ready = mgr.is_demucs_ready();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = weak.upgrade() {
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

#[cfg(target_os = "macos")]
fn set_macos_dock_icon() {
    use objc2::MainThreadMarker;
    use objc2::AllocAnyThread;
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    static ICON_PNG: &[u8] = include_bytes!("../ui/tendril-icon.png");

    let mtm = MainThreadMarker::new().expect("must be called on main thread");
    let app = NSApplication::sharedApplication(mtm);
    let data = NSData::with_bytes(ICON_PNG);
    if let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) {
        unsafe { app.setApplicationIconImage(Some(&image)) };
    }
}
