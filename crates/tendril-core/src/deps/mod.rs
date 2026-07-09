pub mod demucs_bundle;
pub mod ffmpeg;
pub mod github_release;
pub mod status;
pub mod update_check;
pub mod version_compare;
pub mod versions;
pub mod ytdlp;

use std::sync::OnceLock;
use std::time::Duration;

use tokio::sync::Mutex;

use crate::dirs::AppDirs;
use crate::error::DependencyError;

pub use demucs_bundle::DownloadProgress;
pub use status::{DepState, DependencyStatus};
pub use version_compare::version_eq_normalized;

/// HTTP client timeouts for dependency downloads/checks.
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Process-wide mutex serializing dep-manager mutating operations.
///
/// The UI re-creates a `DependencyManager` per task, so a per-instance
/// lock wouldn't actually serialize concurrent button-click flows. The
/// dep filesystem is shared across the whole process, so a static lock
/// is the right granularity.
fn op_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Build a reqwest client with sane timeouts.
///
/// Default `reqwest::Client::new()` has *no* timeout, so a stalled
/// connection wedges the dep manager indefinitely.
pub(crate) fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Manages downloading and updating external binaries.
pub struct DependencyManager {
    dirs: AppDirs,
    client: reqwest::Client,
}

impl DependencyManager {
    pub fn new(dirs: &AppDirs) -> Self {
        Self {
            dirs: dirs.clone(),
            client: build_client(),
        }
    }

    /// Ensure lightweight deps (ffmpeg + yt-dlp) are present.
    /// These are small enough to download silently at startup.
    ///
    /// Always re-queries version + source on each call, so the recorded
    /// state stays in sync when the user installs/upgrades/removes a
    /// system ffmpeg between launches.
    pub async fn ensure_lightweight(&self) -> Result<(), DependencyError> {
        let _guard = op_lock().lock().await;
        self.ensure_ffmpeg().await?;
        self.ensure_ytdlp().await?;
        self.refresh_versions().await;
        Ok(())
    }

    /// Ensure ALL deps including the heavy demucs bundle.
    /// Call this from the UI download button.
    pub async fn ensure_all(
        &self,
        progress_tx: Option<tokio::sync::watch::Sender<DownloadProgress>>,
    ) -> Result<(), DependencyError> {
        let _guard = op_lock().lock().await;
        // Heavy dep first (Python + torch + demucs)
        demucs_bundle::ensure(&self.client, &self.dirs, progress_tx).await?;

        // Lightweight deps
        self.ensure_ffmpeg().await?;
        self.ensure_ytdlp().await?;
        self.refresh_versions().await;

        Ok(())
    }

    /// Re-query yt-dlp version and persist.
    ///
    /// ffmpeg is intentionally not refreshed here — its state and version
    /// are derived live in `status::check_all` on every read.
    async fn refresh_versions(&self) {
        let mut versions = versions::InstalledVersions::load(&self.dirs.data_dir);
        versions.ytdlp = query_ytdlp_version(&self.dirs).await;
        let _ = versions.save(&self.dirs.data_dir);
    }

    pub async fn ensure_ffmpeg(&self) -> Result<std::path::PathBuf, DependencyError> {
        ffmpeg::ensure(&self.client, &self.dirs.bin_dir()).await
    }

    pub async fn ensure_ytdlp(&self) -> Result<std::path::PathBuf, DependencyError> {
        ytdlp::ensure(&self.client, &self.dirs.bin_dir()).await
    }

    /// Check if the demucs Python environment is installed.
    ///
    /// Requires the recorded torch + demucs versions, not just the Python
    /// binary — a partially-completed install must read as "not ready" so
    /// the next `ensure_all` repairs it instead of splitting against a
    /// broken env.
    pub fn is_demucs_ready(&self) -> bool {
        let versions = versions::InstalledVersions::load(&self.dirs.data_dir);
        self.dirs.python_bin().exists() && versions.torch.is_some() && versions.demucs.is_some()
    }

    /// Get installation status of all dependencies (fast, local-only).
    pub fn check_status(&self) -> Vec<DependencyStatus> {
        status::check_all(&self.dirs)
    }

    /// Check for available updates (hits network).
    ///
    /// Issues the three latest-version checks concurrently so a slow
    /// upstream doesn't block the spinner. Skips the ffmpeg upstream
    /// check whenever ffmpeg resolves to the system PATH — system
    /// installs are the user's to manage, never Tendril's.
    pub async fn check_updates(&self) -> Vec<DependencyStatus> {
        let _guard = op_lock().lock().await;
        let mut statuses = self.check_status();

        let check_ffmpeg = statuses
            .iter()
            .find(|s| s.name == "ffmpeg")
            .is_some_and(|s| s.state == DepState::Installed);

        let (demucs_latest, ytdlp_latest, ffmpeg_latest) = tokio::join!(
            update_check::check_demucs_latest(&self.client),
            update_check::check_ytdlp_latest(&self.client),
            async {
                if check_ffmpeg {
                    update_check::check_ffmpeg_latest(&self.client).await
                } else {
                    None
                }
            },
        );

        if let Some(latest) = demucs_latest {
            apply_latest(&mut statuses, "demucs", &latest);
        }
        if let Some(latest) = ytdlp_latest {
            apply_latest(&mut statuses, "yt-dlp", &latest);
        }
        if let Some(latest) = ffmpeg_latest {
            apply_latest(&mut statuses, "ffmpeg", &latest);
        }

        statuses
    }

    /// Update demucs to latest via pip.
    pub async fn update_demucs(&self) -> Result<(), DependencyError> {
        let _guard = op_lock().lock().await;
        demucs_bundle::update_demucs(&self.dirs).await
    }

    /// Re-download latest yt-dlp atomically.
    ///
    /// Downloads to a temp file, verifies it, then renames over the
    /// existing binary. On any failure the existing install is preserved.
    pub async fn update_ytdlp(&self) -> Result<(), DependencyError> {
        let _guard = op_lock().lock().await;
        let bin_dir = self.dirs.bin_dir();
        let final_path = bin_dir.join(ytdlp_binary_name());
        let temp_path = bin_dir.join(format!("{}.new", ytdlp_binary_name()));

        // Cleanup any leftover from a prior aborted run.
        let _ = tokio::fs::remove_file(&temp_path).await;

        let download_result = ytdlp::download_to(&self.client, &temp_path).await;
        if let Err(e) = download_result {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(e);
        }

        // Sanity check: file should be at least 1 MB (real yt-dlp is ~30 MB).
        match tokio::fs::metadata(&temp_path).await {
            Ok(meta) if meta.len() >= 1_000_000 => {}
            Ok(meta) => {
                let _ = tokio::fs::remove_file(&temp_path).await;
                return Err(DependencyError::GitHubApi {
                    message: format!(
                        "yt-dlp download too small ({} bytes); aborting update",
                        meta.len()
                    ),
                });
            }
            Err(e) => {
                let _ = tokio::fs::remove_file(&temp_path).await;
                return Err(DependencyError::Extract(e));
            }
        }

        // Functional check: `yt-dlp --version` should succeed.
        let version_check = tokio::process::Command::new(&temp_path)
            .arg("--version")
            .output()
            .await;
        match version_check {
            Ok(out) if out.status.success() => {}
            Ok(out) => {
                let _ = tokio::fs::remove_file(&temp_path).await;
                return Err(DependencyError::GitHubApi {
                    message: format!(
                        "downloaded yt-dlp failed --version check: {}",
                        String::from_utf8_lossy(&out.stderr)
                    ),
                });
            }
            Err(e) => {
                let _ = tokio::fs::remove_file(&temp_path).await;
                return Err(DependencyError::Extract(e));
            }
        }

        // Atomic swap. On Windows rename-over-existing requires the
        // existing file to be removed first.
        #[cfg(target_os = "windows")]
        let _ = tokio::fs::remove_file(&final_path).await;
        tokio::fs::rename(&temp_path, &final_path)
            .await
            .map_err(DependencyError::Extract)?;

        let mut versions = versions::InstalledVersions::load(&self.dirs.data_dir);
        versions.ytdlp = query_ytdlp_version(&self.dirs).await;
        let _ = versions.save(&self.dirs.data_dir);
        Ok(())
    }

    /// Re-download latest ffmpeg atomically (only if managed, not system).
    ///
    /// Resolves "is this a managed ffmpeg?" the same way `check_status`
    /// does, so the button's behavior matches what the UI shows.
    pub async fn update_ffmpeg(&self) -> Result<(), DependencyError> {
        let _guard = op_lock().lock().await;
        let ffmpeg_state = self
            .check_status()
            .into_iter()
            .find(|s| s.name == "ffmpeg")
            .map(|s| s.state);
        if !matches!(ffmpeg_state, Some(DepState::Installed)) {
            return Ok(());
        }

        let bin_dir = self.dirs.bin_dir();
        let staging = bin_dir.join(".ffmpeg-update-staging");

        // Wipe staging from any prior aborted run.
        if staging.exists() {
            let _ = tokio::fs::remove_dir_all(&staging).await;
        }
        tokio::fs::create_dir_all(&staging)
            .await
            .map_err(DependencyError::Extract)?;

        // Download into the staging directory.
        if let Err(e) = ffmpeg::download_into(&self.client, &staging).await {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            return Err(e);
        }

        // Verify staged ffmpeg works.
        let staged_ffmpeg = staging.join(ffmpeg_binary_name());
        match tokio::fs::metadata(&staged_ffmpeg).await {
            Ok(meta) if meta.len() >= 100_000 => {}
            Ok(meta) => {
                let _ = tokio::fs::remove_dir_all(&staging).await;
                return Err(DependencyError::GitHubApi {
                    message: format!(
                        "ffmpeg download too small ({} bytes); aborting update",
                        meta.len()
                    ),
                });
            }
            Err(e) => {
                let _ = tokio::fs::remove_dir_all(&staging).await;
                return Err(DependencyError::Extract(e));
            }
        }
        let version_check = tokio::process::Command::new(&staged_ffmpeg)
            .arg("-version")
            .output()
            .await;
        match version_check {
            Ok(out) if out.status.success() => {}
            Ok(out) => {
                let _ = tokio::fs::remove_dir_all(&staging).await;
                return Err(DependencyError::GitHubApi {
                    message: format!(
                        "downloaded ffmpeg failed -version check: {}",
                        String::from_utf8_lossy(&out.stderr)
                    ),
                });
            }
            Err(e) => {
                let _ = tokio::fs::remove_dir_all(&staging).await;
                return Err(DependencyError::Extract(e));
            }
        }

        // Atomic swap: move every staged file into bin_dir, replacing
        // any existing version. We do this per-file (rather than swapping
        // directories) so unrelated files in bin_dir survive.
        if let Err(e) = swap_in_directory(&staging, &bin_dir).await {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            return Err(DependencyError::Extract(e));
        }
        let _ = tokio::fs::remove_dir_all(&staging).await;

        // No persisted ffmpeg state to update — `check_status` will pick
        // up the freshly-swapped binary on the next read.
        Ok(())
    }
}

/// Update the matching status entry's `latest_version` and
/// `update_available` flag using normalized version comparison.
fn apply_latest(statuses: &mut [DependencyStatus], name: &str, latest: &str) {
    if let Some(dep) = statuses.iter_mut().find(|d| d.name == name) {
        dep.latest_version = Some(latest.to_string());
        if dep.state != DepState::Missing
            && !version_compare::opt_version_eq(dep.version.as_deref(), latest)
        {
            dep.update_available = true;
        }
    }
}

/// Move every file from `src` over the matching name in `dest`,
/// replacing existing files atomically (per-file rename).
async fn swap_in_directory(src: &std::path::Path, dest: &std::path::Path) -> std::io::Result<()> {
    let mut entries = tokio::fs::read_dir(src).await?;
    while let Some(entry) = entries.next_entry().await? {
        let from = entry.path();
        let Some(name) = from.file_name() else {
            continue;
        };
        let to = dest.join(name);

        // On Windows, rename fails if dest exists; remove first.
        #[cfg(target_os = "windows")]
        let _ = tokio::fs::remove_file(&to).await;

        tokio::fs::rename(&from, &to).await?;
    }
    Ok(())
}

async fn query_ytdlp_version(dirs: &AppDirs) -> Option<String> {
    let bin = dirs.bin_dir().join(ytdlp_binary_name());
    if !bin.exists() {
        return None;
    }
    let output = tokio::process::Command::new(&bin)
        .arg("--version")
        .output()
        .await
        .ok()?;
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(target_os = "windows")]
pub fn ytdlp_binary_name() -> &'static str {
    "yt-dlp.exe"
}
#[cfg(not(target_os = "windows"))]
pub fn ytdlp_binary_name() -> &'static str {
    "yt-dlp"
}

#[cfg(target_os = "windows")]
pub fn ffmpeg_binary_name() -> &'static str {
    "ffmpeg.exe"
}
#[cfg(not(target_os = "windows"))]
pub fn ffmpeg_binary_name() -> &'static str {
    "ffmpeg"
}
