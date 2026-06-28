use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::Ordering;

use slint::{ComponentHandle, Model};
use tokio::runtime::Handle;

use tendril_core::pipeline::orchestrator::PipelineContext;
use tendril_core::progress::PipelineStage;

use crate::MainWindow;
use crate::state::SharedState;

thread_local! {
    /// UI-thread-only cache of decoded queue thumbnails keyed by
    /// `JobSource::thumbnail_key()`. `slint::Image` is `!Send`, but every site
    /// that builds the queue model already runs on the UI thread (Slint
    /// callbacks, the progress timer, and `invoke_from_event_loop` closures),
    /// so a thread-local cache is sufficient and avoids needing `Send`.
    static QUEUE_THUMB_CACHE: RefCell<HashMap<String, slint::Image>> =
        RefCell::new(HashMap::new());
    /// Persistent queue model, mutated in place so the Slint repeater keeps
    /// each `QueueItem` instance across progress ticks. Replacing the model
    /// wholesale (via `set_queue_items`) recreates the instances and drops
    /// per-row state like `TouchArea::has-hover`, causing the hover highlight
    /// to die every 250ms while a job is animating.
    static QUEUE_MODEL: RefCell<Option<Rc<slint::VecModel<crate::QueueItemData>>>> =
        const { RefCell::new(None) };
}

/// Wire all Slint callbacks to their Rust implementations.
pub fn connect_callbacks(window: &MainWindow, state: SharedState, rt: Handle) {
    connect_search(window, state.clone(), rt.clone());
    connect_enqueue(window, state.clone());
    connect_remove(window, state.clone());
    connect_open_folder(window, state.clone());
    connect_open_queue_folder(window, state.clone());
    connect_settings(window, state.clone());
    connect_deps(window, state.clone(), rt.clone());
    connect_check_deps(window, state.clone(), rt.clone());
    connect_update_all_deps(window, state.clone(), rt.clone());
    connect_browse_output(window, state.clone());
    connect_file_dropped(window, state, rt);
}

/// Patch the persistent queue VecModel to match the current job queue.
///
/// On first call: creates the VecModel and binds it to the window. On
/// subsequent calls: diffs the new rows against the model and only calls
/// `set_row_data` / `push` / `remove` for actual changes. Rows whose data
/// is unchanged are not touched, so the underlying `QueueItem` Slint
/// instance — and its `TouchArea::has-hover` — is preserved.
///
/// MUST be called from the UI thread.
fn apply_queue_to_model(
    window: &MainWindow,
    queue: &tendril_core::pipeline::queue::JobQueue,
    raw_thumbnails: &HashMap<String, Vec<u8>>,
) {
    let model = QUEUE_MODEL.with(|m| {
        let mut borrow = m.borrow_mut();
        if borrow.is_none() {
            let vm: Rc<slint::VecModel<crate::QueueItemData>> = Rc::new(slint::VecModel::default());
            window.set_queue_items(slint::ModelRc::from(vm.clone()));
            *borrow = Some(vm);
        }
        borrow.as_ref().unwrap().clone()
    });

    let new_items = QUEUE_THUMB_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let live_keys: std::collections::HashSet<String> =
            queue.iter().map(|j| j.source.thumbnail_key()).collect();
        cache.retain(|k, _| live_keys.contains(k));
        crate::models::build_queue_items(queue, raw_thumbnails, &mut cache)
    });

    // Walk the model in parallel with new_items. When the row at position i
    // is a different job, it means the job that was there has been removed
    // (X button); drop the row and retry the same position. New jobs only
    // appear at the end (push_back enqueue), handled by the trailing push.
    let mut i = 0;
    while i < new_items.len() {
        if i >= model.row_count() {
            model.push(new_items[i].clone());
            i += 1;
            continue;
        }
        let old = model.row_data(i).expect("row_data within row_count");
        if old.job_id == new_items[i].job_id {
            if old.progress != new_items[i].progress
                || old.stage != new_items[i].stage
                || old.stage_color != new_items[i].stage_color
            {
                model.set_row_data(i, new_items[i].clone());
            }
            i += 1;
        } else {
            model.remove(i);
        }
    }
    while model.row_count() > new_items.len() {
        model.remove(model.row_count() - 1);
    }
}

/// Populate initial dependency status on the UI.
pub fn init_dep_status(window: &MainWindow, state: &SharedState) {
    let s = state.lock().unwrap();
    let mgr = tendril_core::deps::DependencyManager::new(&s.dirs);
    let statuses = mgr.check_status();
    let all_installed = statuses
        .iter()
        .all(|s| s.state != tendril_core::deps::DepState::Missing);
    window.set_dep_items(dep_status_model(&statuses));
    window.set_deps_all_installed(all_installed);
}

/// Start the background pipeline runner that processes queued jobs.
pub fn start_pipeline_runner(state: SharedState, rt: Handle, weak: slint::Weak<MainWindow>) {
    rt.spawn(async move {
        loop {
            // Find the first Queued job
            let job_info = {
                let s = state.lock().unwrap();
                let found = s
                    .queue
                    .iter()
                    .find(|job| job.progress_rx.borrow().stage == PipelineStage::Queued);
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
                        ytdlp_bin: s
                            .dirs
                            .bin_dir()
                            .join(tendril_core::deps::ytdlp_binary_name()),
                        ffmpeg_bin: resolve_ffmpeg(&s.dirs),
                        python_bin: s.dirs.python_bin(),
                        models_dir: s.dirs.models_dir(),
                        cache_dir: s.dirs.cache_dir.clone(),
                        output_dir: s.config.output_dir.clone(),
                        output_format: s.config.output_format,
                        gpu_backend: s.config.gpu_backend,
                        model_name: s.config.model_variant.model_name().to_string(),
                        preserve_full_mix: s.config.preserve_full_mix,
                        create_instrumental: s.config.create_instrumental,
                        target_lufs: s.config.target_lufs,
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
                    Ok(()) => {}
                    Err(e) => {
                        if matches!(e, tendril_core::error::PipelineError::Cancelled) {
                            tracing::info!("Job {job_id} cancelled");
                            // Cancelled jobs were explicitly X'd by the user; the
                            // remove handler already pulled them from the queue.
                        } else {
                            tracing::error!("Pipeline failed for job {job_id}: {e}");
                            let _ = progress_tx.send(tendril_core::progress::ProgressEvent {
                                stage: PipelineStage::Failed,
                                fraction: 0.0,
                                message: e.to_string(),
                            });
                        }
                    }
                }

                // Completed and failed jobs stay in the queue so the user can
                // click Done items to open the output folder and review failures.
                // The runner skips non-Queued items, so they never re-run.
                let state_c = state.clone();
                let weak_c = weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(window) = weak_c.upgrade() {
                        let s = state_c.lock().unwrap();
                        apply_queue_to_model(&window, &s.queue, &s.thumbnail_cache);
                    }
                });
            }

            // Poll interval
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    });
}

/// Start a Slint timer that periodically refreshes queue progress in the UI.
///
/// `apply_queue_to_model` patches the existing model in place: rows whose
/// progress/stage haven't changed are left untouched, so the Slint repeater
/// keeps each `QueueItem` instance and its `TouchArea::has-hover` state.
pub fn start_progress_timer(window: &MainWindow, state: SharedState) {
    let weak = window.as_weak();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(250),
        move || {
            let Some(window) = weak.upgrade() else { return };
            let s = state.lock().unwrap();
            if s.queue.is_empty() {
                return;
            }
            apply_queue_to_model(&window, &s.queue, &s.thumbnail_cache);
        },
    );
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
        let state_for_short = state.clone();

        window.on_search_changed(move |query| {
            let query = query.to_string();
            if query.trim().len() < 2 {
                timer.stop();
                // Invalidate any in-flight search/thumbnail fetch so its
                // results don't land after the user has cleared the field.
                bump_search_generation(&state_for_short);
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
                bump_search_generation(&state);
                return;
            }
            perform_search(query, state.clone(), weak.clone(), rt.clone());
        });
    }

    // Explicit clear (Escape key or close button) — cancel any in-flight work
    {
        let state = state.clone();
        window.on_search_cleared(move || {
            bump_search_generation(&state);
        });
    }
}

/// Increment the search-generation counter so any in-flight search or
/// thumbnail-fetch task with an older generation will skip its mutations.
fn bump_search_generation(state: &SharedState) {
    let s = state.lock().unwrap();
    s.search_generation.fetch_add(1, Ordering::SeqCst);
}

fn perform_search(query: String, state: SharedState, weak: slint::Weak<MainWindow>, rt: Handle) {
    // Bump the generation: any in-flight search or thumbnail fetch with an
    // older token will see this new value and bail out before mutating
    // shared state or the UI.
    let (my_gen, gen_counter) = {
        let s = state.lock().unwrap();
        let new_gen = s.search_generation.fetch_add(1, Ordering::SeqCst) + 1;
        (new_gen, s.search_generation.clone())
    };

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
                // Drop stale results.
                if gen_counter.load(Ordering::SeqCst) != my_gen {
                    tracing::debug!("Discarding stale search results for: {query}");
                    return;
                }

                let count = results.len();

                // Store results in state (recheck generation under the lock
                // to avoid racing a concurrent newer search).
                {
                    let mut s = state.lock().unwrap();
                    if s.search_generation.load(Ordering::SeqCst) != my_gen {
                        return;
                    }
                    s.search_results = results.clone();
                }

                let output_dir = { state.lock().unwrap().config.output_dir.clone() };

                let weak_inner = weak.clone();
                let gen_for_ui = gen_counter.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    // Final guard on the UI thread: a newer search may have
                    // bumped the generation between our spawn and now.
                    if gen_for_ui.load(Ordering::SeqCst) != my_gen {
                        return;
                    }
                    if let Some(window) = weak_inner.upgrade() {
                        window.set_searching(false);
                        window.set_results_collapsing(false);
                        window.set_enqueued_index(-1);
                        let model = crate::models::search_results_model(&results, &output_dir);
                        window.set_search_results(model);
                        window.set_status_message("Search complete".into());
                    }
                });

                // Fetch all thumbnails — gated by the same generation token.
                fetch_thumbnails(&state, &weak, 0, count, my_gen, &gen_counter).await;
            }
            Err(e) => {
                tracing::error!("Search failed: {e}");
                if gen_counter.load(Ordering::SeqCst) != my_gen {
                    return;
                }
                let gen_for_ui = gen_counter.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if gen_for_ui.load(Ordering::SeqCst) != my_gen {
                        return;
                    }
                    if let Some(window) = weak.upgrade() {
                        window.set_searching(false);
                        window.set_status_message(slint::SharedString::from(format!(
                            "Search failed: {e}"
                        )));
                    }
                });
            }
        }
    });
}

/// Fetch and apply thumbnails for results[start..start+count].
///
/// Gated by `my_gen`: if a newer search/clear has bumped the counter, the
/// fetch silently bails out so stale thumbnails can't be pinned to the wrong
/// rows of a freshly-replaced result list.
async fn fetch_thumbnails(
    state: &SharedState,
    weak: &slint::Weak<MainWindow>,
    start: usize,
    count: usize,
    my_gen: u64,
    gen_counter: &std::sync::Arc<std::sync::atomic::AtomicU64>,
) {
    // Snapshot the batch under the lock with a generation check so we don't
    // index into a freshly-replaced search_results.
    let batch: Vec<_> = {
        let s = state.lock().unwrap();
        if s.search_generation.load(Ordering::SeqCst) != my_gen {
            return;
        }
        s.search_results
            .iter()
            .skip(start)
            .take(count)
            .cloned()
            .collect()
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

    if gen_counter.load(Ordering::SeqCst) != my_gen {
        return;
    }

    // Cache (still useful even if we don't push to UI — the bytes are keyed
    // by video_id, not row index, so they remain correct).
    {
        let mut s = state.lock().unwrap();
        if s.search_generation.load(Ordering::SeqCst) != my_gen {
            return;
        }
        for (vid, bytes) in video_ids.iter().zip(thumbnail_bytes.iter()) {
            if let Some(data) = bytes {
                s.thumbnail_cache.insert(vid.clone(), data.clone());
            }
        }
    }

    // Decode images off the UI thread (JPEG decoding is CPU-intensive).
    // slint::Image is !Send, so pass raw RGBA pixels and create Images on the UI thread.
    let decoded: Vec<Option<(Vec<u8>, u32, u32)>> = tokio::task::spawn_blocking(move || {
        thumbnail_bytes
            .into_iter()
            .map(|bytes| crate::models::decode_to_rgba(bytes.as_deref()))
            .collect()
    })
    .await
    .unwrap_or_default();

    if gen_counter.load(Ordering::SeqCst) != my_gen {
        return;
    }

    // Apply to UI (cheap — just wrapping pixel buffers into Slint Images).
    let weak = weak.clone();
    let gen_for_ui = gen_counter.clone();
    let _ = slint::invoke_from_event_loop(move || {
        // Final UI-thread guard: a newer search may have replaced the model.
        if gen_for_ui.load(Ordering::SeqCst) != my_gen {
            return;
        }
        if let Some(window) = weak.upgrade() {
            let model = window.get_search_results();
            let Some(vec_model) = model
                .as_any()
                .downcast_ref::<slint::VecModel<crate::SearchResultData>>()
            else {
                return;
            };
            for (i, pixels) in decoded.into_iter().enumerate() {
                let row = start + i;
                if let Some((rgba, w, h)) = pixels {
                    let img = crate::models::rgba_to_slint_image(&rgba, w, h);
                    if let Some(mut item) = vec_model.row_data(row) {
                        item.thumbnail = img;
                        vec_model.set_row_data(row, item);
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
            let title = if result.channel.is_empty() {
                result.title.clone()
            } else {
                format!("{} - {}", result.channel, result.title)
            };
            let source = tendril_core::pipeline::job::JobSource::Youtube {
                video_id: result.video_id.clone(),
                title,
            };
            let id = state.queue.enqueue(source);
            tracing::info!("Enqueued job {id}");
        }
        if let Some(window) = weak.upgrade() {
            apply_queue_to_model(&window, &state.queue, &state.thumbnail_cache);

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
            apply_queue_to_model(&window, &state.queue, &state.thumbnail_cache);
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
                open_in_file_browser(&path);
            }
        }
    });
}

fn connect_open_queue_folder(window: &MainWindow, state: SharedState) {
    window.on_open_queue_folder(move |job_id| {
        let s = state.lock().unwrap();
        let Some(job) = s.queue.iter().find(|j| j.id == job_id as u64) else {
            return;
        };
        let folder = tendril_core::pipeline::job::output_folder_name(
            job.source.display_name(),
            job.source.video_id(),
        );
        let path = s.config.output_dir.join(&folder);
        if path.exists() {
            tracing::info!("Opening folder: {}", path.display());
            open_in_file_browser(&path);
        }
    });
}

fn open_in_file_browser(path: &std::path::Path) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
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
        s.config.create_instrumental = w.get_create_instrumental();
        s.config.target_lufs = w.get_target_lufs() as f32;

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
                        // Use check_updates so latest_version / update_available
                        // get refreshed alongside install state. Without this,
                        // the modal goes blank after a download completes.
                        let statuses = mgr.check_updates().await;
                        let all_installed = statuses
                            .iter()
                            .all(|s| s.state != tendril_core::deps::DepState::Missing);
                        let any_updates = statuses.iter().any(|s| s.update_available);
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(w) = weak.upgrade() {
                                w.set_deps_downloading(false);
                                w.set_dep_items(dep_status_model(&statuses));
                                w.set_deps_all_installed(all_installed);
                                w.set_deps_any_updates(any_updates);
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
                                w.set_deps_status(slint::SharedString::from(format!(
                                    "Failed: {msg}"
                                )));
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

fn connect_check_deps(window: &MainWindow, state: SharedState, rt: Handle) {
    let weak = window.as_weak();
    window.on_check_deps(move || {
        let state = state.clone();
        let weak = weak.clone();

        if let Some(w) = weak.upgrade() {
            w.set_deps_checking(true);
        }

        rt.spawn(async move {
            let dirs = { state.lock().unwrap().dirs.clone() };
            let mgr = tendril_core::deps::DependencyManager::new(&dirs);
            let statuses = mgr.check_updates().await;
            let any_updates = statuses.iter().any(|s| s.update_available);

            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = weak.upgrade() {
                    w.set_dep_items(dep_status_model(&statuses));
                    w.set_deps_checking(false);
                    w.set_deps_any_updates(any_updates);
                }
            });
        });
    });
}

fn connect_update_all_deps(window: &MainWindow, state: SharedState, rt: Handle) {
    let weak = window.as_weak();
    window.on_update_all_deps(move || {
        let state = state.clone();
        let weak = weak.clone();

        if let Some(w) = weak.upgrade() {
            w.set_deps_downloading(true);
            w.set_deps_status("Updating dependencies...".into());
        }

        rt.spawn(async move {
            let dirs = { state.lock().unwrap().dirs.clone() };
            let mgr = tendril_core::deps::DependencyManager::new(&dirs);

            // Get current statuses to know what needs updating
            let statuses = mgr.check_updates().await;
            let updatable: Vec<String> = statuses
                .iter()
                .filter(|s| s.update_available)
                .map(|s| s.name.clone())
                .collect();

            for name in &updatable {
                let weak_inner = weak.clone();
                let msg = format!("Updating {}...", name);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = weak_inner.upgrade() {
                        w.set_deps_status(msg.into());
                    }
                });

                let result = match name.as_str() {
                    "demucs" => mgr.update_demucs().await,
                    "yt-dlp" => mgr.update_ytdlp().await,
                    "ffmpeg" => mgr.update_ffmpeg().await,
                    _ => Ok(()),
                };
                if let Err(e) = result {
                    tracing::error!("Failed to update {name}: {e}");
                }
            }

            // Re-check after all updates
            let statuses = mgr.check_updates().await;
            let any_updates = statuses.iter().any(|s| s.update_available);

            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = weak.upgrade() {
                    w.set_dep_items(dep_status_model(&statuses));
                    w.set_deps_downloading(false);
                    w.set_deps_any_updates(any_updates);
                    w.set_deps_status("".into());
                }
            });
        });
    });
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
            let folder = rfd::FileDialog::new().set_directory(&current).pick_folder();
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

fn connect_file_dropped(window: &MainWindow, state: SharedState, rt: Handle) {
    let weak = window.as_weak();
    window.on_file_dropped(move |path_str| {
        let path = std::path::PathBuf::from(path_str.as_str());
        if !path.is_file() {
            tracing::warn!("Dropped path is not a file: {}", path.display());
            return;
        }
        tracing::info!("Enqueuing dropped file: {}", path.display());
        let cache_key = path.to_string_lossy().to_string();
        let mut s = state.lock().unwrap();
        let source = tendril_core::pipeline::job::JobSource::LocalFile { path: path.clone() };
        s.queue.enqueue(source);
        if let Some(w) = weak.upgrade() {
            apply_queue_to_model(&w, &s.queue, &s.thumbnail_cache);
        }

        // Extract album art in background
        let ffmpeg_bin = resolve_ffmpeg(&s.dirs);
        let state = state.clone();
        let weak = weak.clone();
        rt.spawn(async move {
            if let Some(art) = extract_album_art(&ffmpeg_bin, &path).await {
                {
                    let mut s = state.lock().unwrap();
                    s.thumbnail_cache.insert(cache_key, art);
                }
                // UI will pick it up on next progress timer tick
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = weak.upgrade() {
                        let s = state.lock().unwrap();
                        apply_queue_to_model(&w, &s.queue, &s.thumbnail_cache);
                    }
                });
            }
        });
    });
}

/// Extract embedded album art from an audio file using ffmpeg.
async fn extract_album_art(
    ffmpeg_bin: &std::path::Path,
    input: &std::path::Path,
) -> Option<Vec<u8>> {
    let output = tokio::process::Command::new(ffmpeg_bin)
        .arg("-i")
        .arg(input)
        .arg("-an")
        .arg("-vcodec")
        .arg("copy")
        .arg("-f")
        .arg("image2pipe")
        .arg("pipe:1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .await
        .ok()?;

    if output.status.success() && !output.stdout.is_empty() {
        Some(output.stdout)
    } else {
        None
    }
}

/// Convert dependency statuses to a Slint model.
pub(crate) fn dep_status_model(
    statuses: &[tendril_core::deps::DependencyStatus],
) -> slint::ModelRc<crate::DepItemData> {
    let items: Vec<crate::DepItemData> = statuses
        .iter()
        .map(|s| crate::DepItemData {
            name: s.name.clone().into(),
            version: s.version.clone().unwrap_or_default().into(),
            latest_version: s.latest_version.clone().unwrap_or_default().into(),
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

/// Resolve ffmpeg binary path with the same priority as `deps::ffmpeg::ensure`
/// and `status::check_all`, so the pipeline runs the binary the UI advertises.
///
/// macOS/Linux: system PATH > managed > bare name.
/// Windows: managed > system PATH > bare name (managed shared-build DLLs
/// are required by torchcodec).
fn resolve_ffmpeg(dirs: &tendril_core::dirs::AppDirs) -> std::path::PathBuf {
    let managed = dirs
        .bin_dir()
        .join(tendril_core::deps::ffmpeg_binary_name());
    // Only accept a system ffmpeg that actually runs and has a co-located
    // ffprobe — otherwise prefer the managed static build. This keeps the
    // pipeline working on machines whose Homebrew/system ffmpeg is broken.
    let system = tendril_core::deps::ffmpeg::find_working_system_ffmpeg();

    #[cfg(target_os = "windows")]
    {
        if managed.exists() {
            return managed;
        }
        if let Some(s) = system {
            return s;
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        if let Some(s) = system {
            return s;
        }
        if managed.exists() {
            return managed;
        }
    }
    std::path::PathBuf::from(tendril_core::deps::ffmpeg_binary_name())
}
