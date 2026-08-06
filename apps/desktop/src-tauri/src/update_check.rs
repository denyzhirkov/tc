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
    /// Browser URL of the release page — release notes, every platform.
    pub url: String,
    /// Direct link to the build for *this* machine, when the release carries
    /// one. `None` on an unrecognised platform or a release without a matching
    /// asset, in which case the UI offers only the release page.
    pub download_url: Option<String>,
    /// File name of that build, so the UI can say what it is about to fetch.
    pub download_name: Option<String>,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize, Clone)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

/// Asset name suffixes for the running platform, best first.
///
/// Matching is by suffix because the file name carries the version
/// (`tc__1.10.0_aarch64.dmg`). Installers win over raw archives, and on Linux
/// the AppImage wins over distro packages because it runs anywhere.
fn wanted_suffixes() -> &'static [&'static str] {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => &["_aarch64.dmg"],
        ("macos", _) => &["_x64.dmg"],
        ("windows", _) => &["-setup.exe", "_en-US.msi", ".msi"],
        ("linux", "aarch64") => &["_aarch64.AppImage", "_arm64.deb", ".aarch64.rpm"],
        ("linux", _) => &["_amd64.AppImage", "_amd64.deb", ".x86_64.rpm"],
        _ => &[],
    }
}

fn pick_asset<'a>(assets: &'a [GhAsset], suffixes: &[&str]) -> Option<&'a GhAsset> {
    suffixes
        .iter()
        .find_map(|s| assets.iter().find(|a| a.name.ends_with(s)))
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
    let asset = pick_asset(&rel.assets, wanted_suffixes());
    Ok(Some(UpdateInfo {
        version: rel.tag_name.trim_start_matches('v').to_string(),
        url: rel.html_url,
        download_url: asset.map(|a| a.browser_download_url.clone()),
        download_name: asset.map(|a| a.name.clone()),
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
    use super::{is_newer, pick_asset, GhAsset};

    /// Every asset the 1.10.0 release actually shipped.
    fn release_assets() -> Vec<GhAsset> {
        [
            "tc-client-linux-amd64",
            "tc-client-macos-arm64",
            "tc-server-windows-amd64.exe",
            "tc_-1.10.0-1.aarch64.rpm",
            "tc_-1.10.0-1.x86_64.rpm",
            "tc_.app.zip",
            "tc__1.10.0_aarch64.AppImage",
            "tc__1.10.0_aarch64.dmg",
            "tc__1.10.0_amd64.AppImage",
            "tc__1.10.0_amd64.deb",
            "tc__1.10.0_arm64.deb",
            "tc__1.10.0_x64-setup.exe",
            "tc__1.10.0_x64.dmg",
            "tc__1.10.0_x64_en-US.msi",
        ]
        .into_iter()
        .map(|name| GhAsset {
            name: name.to_string(),
            browser_download_url: format!("https://example.invalid/{name}"),
        })
        .collect()
    }

    fn picked(suffixes: &[&str]) -> Option<String> {
        pick_asset(&release_assets(), suffixes).map(|a| a.name.clone())
    }

    #[test]
    fn picks_the_build_for_each_platform() {
        assert_eq!(
            picked(&["_aarch64.dmg"]).as_deref(),
            Some("tc__1.10.0_aarch64.dmg")
        );
        assert_eq!(picked(&["_x64.dmg"]).as_deref(), Some("tc__1.10.0_x64.dmg"));
        assert_eq!(
            picked(&["_amd64.AppImage", "_amd64.deb"]).as_deref(),
            Some("tc__1.10.0_amd64.AppImage")
        );
        assert_eq!(
            picked(&["_aarch64.AppImage", "_arm64.deb"]).as_deref(),
            Some("tc__1.10.0_aarch64.AppImage")
        );
    }

    /// The installer must win over the msi, and an arm64 mac must never be
    /// handed the x64 dmg — `_x64.dmg` is a suffix of neither.
    #[test]
    fn respects_priority_and_never_crosses_arch() {
        assert_eq!(
            picked(&["-setup.exe", "_en-US.msi"]).as_deref(),
            Some("tc__1.10.0_x64-setup.exe")
        );
        let arm_only: Vec<GhAsset> = release_assets()
            .into_iter()
            .filter(|a| !a.name.ends_with("_x64.dmg"))
            .collect();
        assert!(pick_asset(&arm_only, &["_x64.dmg"]).is_none());
    }

    /// A release with nothing for us (or an unknown platform) degrades to the
    /// release page rather than guessing.
    #[test]
    fn no_match_is_none() {
        assert!(picked(&[]).is_none());
        assert!(picked(&["_riscv64.dmg"]).is_none());
    }

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
