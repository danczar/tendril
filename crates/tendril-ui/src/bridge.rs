use std::rc::Rc;

use slint::{ComponentHandle, Model};
use tokio::runtime::Handle;

use tendril_core::pipeline::orchestrator::PipelineContext;
use tendril_core::progress::PipelineStage;

use crate::state::SharedState;
use crate::MainWindow;

/// Wire all Slint callbacks to their Rust implementations.
pub fn connect_callbacks(window: &MainWindow, state: SharedState, rt: Handle) {
    connect_search(window, state.clone(), rt.clone());
    connect_enqueue(window, state.clone());
    connect_remove(window, state.clone());
    connect_open_folder(window, state.clone());
    connect_settings(window, state.clone());
    connect_deps(window, state.clone(), rt.clone());
    connect_browse_output(window, state);
}

/// Populate initial dependency status on the UI.
pub fn init_dep_status(window: &MainWindow, state: &SharedState) {
    let s = state.lock().unwrap();
    let mgr = tendril_core::deps::DependencyManager::new(&s.dirs);
    let statuses = mgr.check_status();
    let model = dep_status_model(&statuses);
    window.set_dep_items(model);
}

/// Start the background pipeline runner that processes queued jobs.
pub fn start_pipeline_runner(state: SharedState, rt: Handle, weak: slint::Weak<MainWindow>) {
    rt.spawn(async move {
        loop {
            // Find the first Queued job
            let job_info = {
                let s = state.lock().unwrap();
                let found = s.queue.iter().find(|job| {
                    job.progress_rx.borrow().stage == PipelineStage::Queued
                });
                found.map(|job| {
                    (
                        job.id,
                        job.source.clone(),
                        job.progress_tx.clone(),
                        job.cancel_rx.clone(),
                    )
                })
            };

            if let Some((job_id, source, progress_tx, cancel_rx)) = job_info {
                tracing::info!("Processing job {job_id}: {}", source.display_name());

                // Build pipeline context from current state
                let ctx = {
                    let s = state.lock().unwrap();
                    PipelineContext {
                        ytdlp_bin: s.dirs.bin_dir().join(ytdlp_binary_name()),
                        ffmpeg_bin: resolve_ffmpeg(&s.dirs),
                        python_bin: s.dirs.python_bin(),
                        models_dir: s.dirs.models_dir(),
                        cache_dir: s.dirs.cache_dir.clone(),
                        output_dir: s.config.output_dir.clone(),
                        output_format: s.config.output_format,
                        gpu_backend: s.config.gpu_backend,
                        model_name: s.config.model_variant.model_name().to_string(),
                        preserve_full_mix: s.config.preserve_full_mix,
                    }
                };

                let result = tendril_core::pipeline::orchestrator::run(
                    &ctx,
                    source,
                    progress_tx.clone(),
                    cancel_rx,
                )
                .await;

                match &result {
                    Ok(()) => {
                        // Brief delay so user can see Done status
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                    Err(e) => {
                        let is_cancelled = matches!(e, tendril_core::error::PipelineError::Cancelled);
                        if is_cancelled {
                            tracing::info!("Job {job_id} cancelled");
                        } else {
                            tracing::error!("Pipeline failed for job {job_id}: {e}");
                            let _ = progress_tx.send(tendril_core::progress::ProgressEvent {
                                stage: PipelineStage::Failed,
                                fraction: 0.0,
                                message: e.to_string(),
                            });
                            // Brief delay so user can see Failed status
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        }
                    }
                }

                // Remove completed/failed/cancelled job from queue
                {
                    let mut s = state.lock().unwrap();
                    s.queue.remove(job_id);
                }

                // Refresh UI
                let state_c = state.clone();
                let weak_c = weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(window) = weak_c.upgrade() {
                        let s = state_c.lock().unwrap();
                        let model = crate::models::queue_items_model(&s.queue, &s.thumbnail_cache);
                        window.set_queue_items(model);
                    }
                });
            }

            // Poll interval
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    });
}

/// Start a Slint timer that periodically refreshes queue progress in the UI.
pub fn start_progress_timer(window: &MainWindow, state: SharedState) {
    let weak = window.as_weak();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(250),
        move || {
            if let Some(window) = weak.upgrade() {
                let s = state.lock().unwrap();
                if !s.queue.is_empty() {
                    let model = crate::models::queue_items_model(&s.queue, &s.thumbnail_cache);
                    window.set_queue_items(model);
                }
            }
        },
    );
    // Keep timer alive for the lifetime of the app
    std::mem::forget(timer);
}

fn connect_search(window: &MainWindow, state: SharedState, rt: Handle) {
    let debounce_timer = Rc::new(slint::Timer::default());

    // Live search — debounced 300ms after each keystroke
    {
        let state = state.clone();
        let rt = rt.clone();
        let weak = window.as_weak();
        let weak_clear = window.as_weak();
        let timer = debounce_timer.clone();

        window.on_search_changed(move |query| {
            let query = query.to_string();
            if query.trim().len() < 2 {
                timer.stop();
                if let Some(w) = weak_clear.upgrade() {
                    w.set_searching(false);
                    w.set_results_collapsing(true);
                }
                return;
            }

            let state = state.clone();
            let weak = weak.clone();
            let rt = rt.clone();

            timer.start(
                slint::TimerMode::SingleShot,
                std::time::Duration::from_millis(300),
                move || {
                    perform_search(query.clone(), state.clone(), weak.clone(), rt.clone());
                },
            );
        });
    }

    // Immediate search on Enter — cancels pending debounce
    {
        let state = state.clone();
        let weak = window.as_weak();
        let rt = rt.clone();
        let timer = debounce_timer;

        window.on_search(move |query| {
            timer.stop();
            let query = query.to_string();
            if query.trim().is_empty() {
                return;
            }
            perform_search(query, state.clone(), weak.clone(), rt.clone());
        });
    }

}

fn perform_search(query: String, state: SharedState, weak: slint::Weak<MainWindow>, rt: Handle) {
    {
        let weak = weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(window) = weak.upgrade() {
                window.set_searching(true);
            }
        });
    }

    rt.spawn(async move {
        tracing::info!("Searching for: {query}");
        match tendril_core::youtube::search::search(&query).await {
            Ok(results) => {
                let count = results.len();

                // Store results in state immediately (before UI update)
                // so fetch_thumbnails can read them.
                {
                    let mut s = state.lock().unwrap();
                    s.search_results = results.clone();
                }

                let output_dir = {
                    state.lock().unwrap().config.output_dir.clone()
                };

                let weak_inner = weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(window) = weak_inner.upgrade() {
                        window.set_searching(false);
                        window.set_results_collapsing(false);
                        window.set_enqueued_index(-1);
                        let model = crate::models::search_results_model(&results, &output_dir);
                        window.set_search_results(model);
                        window.set_status_message("Search complete".into());
                    }
                });

                // Fetch all thumbnails
                fetch_thumbnails(&state, &weak, 0, count).await;
            }
            Err(e) => {
                tracing::error!("Search failed: {e}");
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(window) = weak.upgrade() {
                        window.set_searching(false);
                        window.set_status_message(
                            slint::SharedString::from(format!("Search failed: {e}")),
                        );
                    }
                });
            }
        }
    });
}

/// Fetch and apply thumbnails for results[start..start+count].
async fn fetch_thumbnails(
    state: &SharedState,
    weak: &slint::Weak<MainWindow>,
    start: usize,
    count: usize,
) {
    let batch: Vec<_> = {
        let s = state.lock().unwrap();
        s.search_results.iter().skip(start).take(count).cloned().collect()
    };

    if batch.is_empty() {
        return;
    }

    let video_ids: Vec<String> = batch.iter().map(|r| r.video_id.clone()).collect();
    let urls: Vec<String> = video_ids
        .iter()
        .map(|vid| format!("https://i.ytimg.com/vi/{vid}/hqdefault.jpg"))
        .collect();
    let futs: Vec<_> = urls
        .iter()
        .map(|url| crate::models::fetch_thumbnail_bytes(url))
        .collect();
    let thumbnail_bytes = futures::future::join_all(futs).await;

    // Cache
    {
        let mut s = state.lock().unwrap();
        for (vid, bytes) in video_ids.iter().zip(thumbnail_bytes.iter()) {
            if let Some(data) = bytes {
                s.thumbnail_cache.insert(vid.clone(), data.clone());
            }
        }
    }

    // Apply to UI
    let weak = weak.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(window) = weak.upgrade() {
            let model = window.get_search_results();
            let Some(vec_model) = model
                .as_any()
                .downcast_ref::<slint::VecModel<crate::SearchResultData>>()
            else {
                return;
            };
            for (i, bytes) in thumbnail_bytes.into_iter().enumerate() {
                let row = start + i;
                if let Some(data) = bytes {
                    if let Some(img) = crate::models::decode_image_bytes(&data) {
                        if let Some(mut item) = vec_model.row_data(row) {
                            item.thumbnail = img;
                            vec_model.set_row_data(row, item);
                        }
                    }
                }
            }
        }
    });
}

fn connect_enqueue(window: &MainWindow, state: SharedState) {
    let weak = window.as_weak();
    window.on_enqueue_result(move |idx| {
        let mut state = state.lock().unwrap();
        if let Some(result) = state.search_results.get(idx as usize) {
            let source = tendril_core::pipeline::job::JobSource::Youtube {
                video_id: result.video_id.clone(),
                title: result.title.clone(),
            };
            let id = state.queue.enqueue(source);
            tracing::info!("Enqueued job {id}");
        }
        if let Some(window) = weak.upgrade() {
            let model = crate::models::queue_items_model(&state.queue, &state.thumbnail_cache);
            window.set_queue_items(model);

            // Phase 1: Highlight the clicked row, clear search text
            window.set_enqueued_index(idx);
            window.set_search_text("".into());

            // Phase 2: Collapse all rows together
            let weak2 = weak.clone();
            let t1 = slint::Timer::default();
            t1.start(
                slint::TimerMode::SingleShot,
                std::time::Duration::from_millis(200),
                move || {
                    if let Some(w) = weak2.upgrade() {
                        w.set_results_collapsing(true);
                    }
                },
            );
            std::mem::forget(t1);

            // Phase 3: Clean up after animation completes
            let weak3 = weak.clone();
            let t2 = slint::Timer::default();
            t2.start(
                slint::TimerMode::SingleShot,
                std::time::Duration::from_millis(550),
                move || {
                    if let Some(w) = weak3.upgrade() {
                        w.set_search_results(slint::ModelRc::default());
                        w.set_results_collapsing(false);
                        w.set_enqueued_index(-1);
                    }
                },
            );
            std::mem::forget(t2);
        }
    });
}

fn connect_remove(window: &MainWindow, state: SharedState) {
    let weak = window.as_weak();
    window.on_remove_job(move |job_id| {
        let mut state = state.lock().unwrap();
        // Signal cancellation before removing (stops running subprocess)
        if let Some(job) = state.queue.iter().find(|j| j.id == job_id as u64) {
            let _ = job.cancel_tx.send(true);
        }
        state.queue.remove(job_id as u64);
        tracing::info!("Removed job {job_id}");
        if let Some(window) = weak.upgrade() {
            let model = crate::models::queue_items_model(&state.queue, &state.thumbnail_cache);
            window.set_queue_items(model);
        }
    });
}

fn connect_open_folder(window: &MainWindow, state: SharedState) {
    window.on_open_result_folder(move |idx| {
        let s = state.lock().unwrap();
        if let Some(result) = s.search_results.get(idx as usize) {
            let folder = tendril_core::pipeline::job::output_folder_name(
                &result.title,
                Some(&result.video_id),
            );
            let path = s.config.output_dir.join(&folder);
            if path.exists() {
                tracing::info!("Opening folder: {}", path.display());
                #[cfg(target_os = "macos")]
                {
                    let _ = std::process::Command::new("open").arg(&path).spawn();
                }
                #[cfg(target_os = "windows")]
                {
                    let _ = std::process::Command::new("explorer").arg(&path).spawn();
                }
                #[cfg(target_os = "linux")]
                {
                    let _ = std::process::Command::new("xdg-open").arg(&path).spawn();
                }
            }
        }
    });
}

fn connect_settings(window: &MainWindow, state: SharedState) {
    let weak = window.as_weak();
    window.on_settings_changed(move || {
        let Some(w) = weak.upgrade() else { return };
        let mut s = state.lock().unwrap();

        s.config.output_format = match w.get_format_index() {
            0 => tendril_core::config::OutputFormat::Wav,
            2 => tendril_core::config::OutputFormat::Mp3,
            3 => tendril_core::config::OutputFormat::Aac,
            _ => tendril_core::config::OutputFormat::Flac,
        };
        s.config.gpu_backend = match w.get_gpu_index() {
            1 => tendril_core::config::GpuBackend::Cpu,
            _ => tendril_core::config::GpuBackend::Auto,
        };
        s.config.model_variant = match w.get_model_index() {
            0 => tendril_core::config::ModelVariant::Htdemucs,
            _ => tendril_core::config::ModelVariant::HtdemucsFineTuned,
        };
        s.config.preserve_full_mix = w.get_preserve_full_mix();

        if let Err(e) = s.config.save(&s.dirs.config_dir) {
            tracing::error!("Failed to save config: {e}");
        }
    });
}

fn connect_deps(window: &MainWindow, state: SharedState, rt: Handle) {
    // Download all deps
    {
        let state = state.clone();
        let weak = window.as_weak();
        let rt = rt.clone();

        window.on_download_deps(move || {
            let state = state.clone();
            let weak = weak.clone();

            // Set downloading state
            if let Some(w) = weak.upgrade() {
                w.set_deps_downloading(true);
                w.set_deps_status("Starting download...".into());
            }

            rt.spawn(async move {
                let dirs = { state.lock().unwrap().dirs.clone() };
                let mgr = tendril_core::deps::DependencyManager::new(&dirs);

                let (progress_tx, mut progress_rx) =
                    tokio::sync::watch::channel(tendril_core::deps::DownloadProgress::default());

                // Progress UI updater task
                let weak_progress = weak.clone();
                let progress_task = tokio::spawn(async move {
                    while progress_rx.changed().await.is_ok() {
                        let p = progress_rx.borrow().clone();
                        let weak_inner = weak_progress.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(w) = weak_inner.upgrade() {
                                w.set_deps_progress(p.fraction);
                                w.set_deps_status(p.message.into());
                            }
                        });
                    }
                });

                let result = mgr.ensure_all(Some(progress_tx)).await;
                drop(progress_task);

                match result {
                    Ok(()) => {
                        let statuses = mgr.check_status();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(w) = weak.upgrade() {
                                w.set_deps_downloading(false);
                                w.set_dep_items(dep_status_model(&statuses));
                                w.set_status_message("Ready".into());
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!("Dependency download failed: {e}");
                        let msg = e.to_string();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(w) = weak.upgrade() {
                                w.set_deps_downloading(false);
                                w.set_deps_status(
                                    slint::SharedString::from(format!("Failed: {msg}")),
                                );
                            }
                        });
                    }
                }
            });
        });
    }

    // Update individual dep
    {
        let state = state.clone();
        let weak = window.as_weak();

        window.on_update_dep(move |dep_name| {
            let dep_name = dep_name.to_string();
            let state = state.clone();
            let weak = weak.clone();

            rt.spawn(async move {
                let dirs = { state.lock().unwrap().dirs.clone() };
                let mgr = tendril_core::deps::DependencyManager::new(&dirs);

                let result = match dep_name.as_str() {
                    "demucs" => mgr.update_demucs().await,
                    "yt-dlp" => mgr.update_ytdlp().await,
                    "ffmpeg" => mgr.update_ffmpeg().await,
                    _ => {
                        tracing::warn!("Unknown dep to update: {dep_name}");
                        Ok(())
                    }
                };

                match result {
                    Ok(()) => {
                        tracing::info!("Updated {dep_name}");
                        let statuses = mgr.check_status();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(w) = weak.upgrade() {
                                w.set_dep_items(dep_status_model(&statuses));
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!("Failed to update {dep_name}: {e}");
                    }
                }
            });
        });
    }
}

fn connect_browse_output(window: &MainWindow, state: SharedState) {
    let weak = window.as_weak();
    window.on_browse_output_dir(move || {
        let current = {
            let s = state.lock().unwrap();
            s.config.output_dir.clone()
        };
        let weak = weak.clone();
        let state = state.clone();
        std::thread::spawn(move || {
            let folder = rfd::FileDialog::new()
                .set_directory(&current)
                .pick_folder();
            if let Some(path) = folder {
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = weak.upgrade() {
                        w.set_output_dir(path.display().to_string().into());
                        let mut s = state.lock().unwrap();
                        s.config.output_dir = path;
                        if let Err(e) = s.config.save(&s.dirs.config_dir) {
                            tracing::error!("Failed to save config: {e}");
                        }
                    }
                });
            }
        });
    });
}

/// Convert dependency statuses to a Slint model.
fn dep_status_model(
    statuses: &[tendril_core::deps::DependencyStatus],
) -> slint::ModelRc<crate::DepItemData> {
    let items: Vec<crate::DepItemData> = statuses
        .iter()
        .map(|s| crate::DepItemData {
            name: s.name.clone().into(),
            version: s.version.clone().unwrap_or_default().into(),
            status: match s.state {
                tendril_core::deps::DepState::Installed => "installed",
                tendril_core::deps::DepState::System => "system",
                tendril_core::deps::DepState::Missing => "missing",
            }
            .into(),
            update_available: s.update_available,
        })
        .collect();
    slint::ModelRc::new(slint::VecModel::from(items))
}

#[cfg(target_os = "windows")]
fn ytdlp_binary_name() -> &'static str {
    "yt-dlp.exe"
}

#[cfg(not(target_os = "windows"))]
fn ytdlp_binary_name() -> &'static str {
    "yt-dlp"
}

/// Resolve ffmpeg binary path: check bin_dir first, fall back to system PATH.
fn resolve_ffmpeg(dirs: &tendril_core::dirs::AppDirs) -> std::path::PathBuf {
    let managed = dirs.bin_dir().join(ffmpeg_binary_name());
    if managed.exists() {
        return managed;
    }
    // Resolve from system PATH to get an absolute path
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(ffmpeg_binary_name());
        if candidate.is_file() {
            return candidate;
        }
    }
    // Last resort — let OS try to resolve it
    std::path::PathBuf::from(ffmpeg_binary_name())
}

#[cfg(target_os = "windows")]
fn ffmpeg_binary_name() -> &'static str {
    "ffmpeg.exe"
}

#[cfg(not(target_os = "windows"))]
fn ffmpeg_binary_name() -> &'static str {
    "ffmpeg"
}
