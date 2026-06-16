//! Checks GitHub Releases for a newer tc_ build and reports it to the UI.
//!
//! Best-effort and silent on failure: an offline client simply never sees the
//! prompt. Comparison is a plain numeric semver compare against the release
//! `tag_name` (`v1.9.20` → `1.9.20`); drafts and pre-releases are skipped.

use serde::{Deserialize, Serialize};

const RELEASES_URL: &str = "https://api.github.com/repos/denyzhirkov/tc/releases/latest";

#[derive(Debug, Clone, Serialize)]
pub struct UpdateInfo {
    /// Latest release version, without the leading `v` (e.g. "1.9.20").
    pub version: String,
    /// Browser URL of the release page.
    pub url: String,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// Returns `Some(info)` when the latest GitHub release is newer than the
/// running build, `None` otherwise — including any network/parse failure, so
/// the caller never has to handle the offline case specially.
#[tauri::command]
pub async fn check_for_update() -> Result<Option<UpdateInfo>, String> {
    let current = env!("CARGO_PKG_VERSION");
    match fetch_latest().await {
        Ok(Some(info)) if is_newer(&info.version, current) => Ok(Some(info)),
        Ok(_) => Ok(None),
        Err(e) => {
            tracing::debug!("update check failed: {}", e);
            Ok(None)
        }
    }
}

async fn fetch_latest() -> anyhow::Result<Option<UpdateInfo>> {
    // GitHub requires a User-Agent; the Accept header pins the v3 JSON schema.
    let client = reqwest::Client::builder()
        .user_agent(concat!("tc-desktop/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let rel: GhRelease = client
        .get(RELEASES_URL)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if rel.draft || rel.prerelease {
        return Ok(None);
    }
    Ok(Some(UpdateInfo {
        version: rel.tag_name.trim_start_matches('v').to_string(),
        url: rel.html_url,
    }))
}

/// `true` when `candidate` is a strictly higher version than `current`. Both
/// are dot-separated numeric strings; a missing or non-numeric part reads as 0
/// (a `1.9.20-rc1` part keeps its leading number), so a malformed tag can never
/// spuriously prompt for "newer".
fn is_newer(candidate: &str, current: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.split('.')
            .map(|p| p.split('-').next().unwrap_or("").parse().unwrap_or(0))
            .collect()
    };
    let (a, b) = (parse(candidate), parse(current));
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

/// Open the release page in the user's default browser.
#[tauri::command]
pub async fn open_release(url: String) -> Result<(), String> {
    open::that(url).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::is_newer;

    #[test]
    fn detects_newer() {
        assert!(is_newer("1.9.20", "1.9.19"));
        assert!(is_newer("1.10.0", "1.9.19"));
        assert!(is_newer("2.0.0", "1.9.19"));
    }

    #[test]
    fn ignores_same_or_older() {
        assert!(!is_newer("1.9.19", "1.9.19"));
        assert!(!is_newer("1.9.18", "1.9.19"));
        assert!(!is_newer("1.8.99", "1.9.0"));
    }

    #[test]
    fn malformed_never_newer() {
        assert!(!is_newer("garbage", "1.9.19"));
        assert!(!is_newer("1.9.20-rc1", "1.9.20"));
    }
}
