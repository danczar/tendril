//! Version-string normalization for update detection.
//!
//! Different upstream sources tag their releases inconsistently:
//!   * yt-dlp ships dates like `2024.05.27` or `2024.05.27.232815`
//!   * eugeneware/ffmpeg-static uses bare semver like `b7.1`
//!   * BtbN/FFmpeg-Builds uses tags like `n7.1` or `n7.1.1`
//!   * PyPI demucs uses `4.0.1`
//!   * Local installs may store `4.0.1+cpu` or similar build metadata
//!
//! Bare string comparison is fragile. This module strips leading
//! letter-prefixes (v/V/b/n) and trailing `+local.segments`, then
//! compares numeric segments as integers.

/// Strip a single leading ASCII letter prefix (`v`, `V`, `b`, `n`)
/// and trailing `+localmeta` from a version string.
fn strip_prefix_and_local(s: &str) -> &str {
    let s = s.trim();
    let stripped = match s.chars().next() {
        Some(c) if matches!(c, 'v' | 'V' | 'b' | 'n') => &s[c.len_utf8()..],
        _ => s,
    };
    stripped.find('+').map_or(stripped, |i| &stripped[..i])
}

/// Normalize a version string: drop letter prefix and `+local` metadata,
/// trim whitespace.
pub fn normalize(version: &str) -> &str {
    strip_prefix_and_local(version)
}

/// Compare two version strings for equality, ignoring leading letter
/// prefixes and trailing `+local` metadata, comparing numeric segments
/// as integers (so `7.1` == `7.1.0` is *not* asserted, but `07.1` == `7.1` is).
pub fn version_eq_normalized(installed: &str, latest: &str) -> bool {
    let a = normalize(installed);
    let b = normalize(latest);
    if a == b {
        return true;
    }

    let segs_a: Vec<&str> = a.split('.').collect();
    let segs_b: Vec<&str> = b.split('.').collect();
    if segs_a.len() != segs_b.len() {
        return false;
    }
    segs_a.iter().zip(segs_b.iter()).all(|(x, y)| {
        match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(a), Ok(b)) => a == b,
            _ => x == y,
        }
    })
}

/// Convenience: compare an `Option<&str>` installed version against a
/// known-Some latest. Returns true only if installed is Some and equal.
pub fn opt_version_eq(installed: Option<&str>, latest: &str) -> bool {
    installed.is_some_and(|i| version_eq_normalized(i, latest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_v_prefix() {
        assert!(version_eq_normalized("v1.2.3", "1.2.3"));
        assert!(version_eq_normalized("V1.2.3", "1.2.3"));
    }

    #[test]
    fn strips_b_and_n_prefix() {
        assert!(version_eq_normalized("b7.1", "7.1"));
        assert!(version_eq_normalized("n7.1", "b7.1"));
        assert!(version_eq_normalized("n7.1.1", "7.1.1"));
    }

    #[test]
    fn strips_local_metadata() {
        assert!(version_eq_normalized("4.0.1+cpu", "4.0.1"));
        assert!(version_eq_normalized("v1.2.3+build5", "1.2.3"));
    }

    #[test]
    fn normalizes_numeric_segments() {
        assert!(version_eq_normalized("01.02.03", "1.2.3"));
    }

    #[test]
    fn segments_must_match_in_count() {
        assert!(!version_eq_normalized("1.2", "1.2.0"));
        assert!(!version_eq_normalized("1.2.3", "1.2"));
    }

    #[test]
    fn detects_real_difference() {
        assert!(!version_eq_normalized("1.2.3", "1.2.4"));
        assert!(!version_eq_normalized("v1.2.3", "v1.3.3"));
    }

    #[test]
    fn opt_version_eq_handles_none() {
        assert!(!opt_version_eq(None, "1.2.3"));
        assert!(opt_version_eq(Some("v1.2.3"), "1.2.3"));
    }
}
