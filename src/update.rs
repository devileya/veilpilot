//! Checking for, and installing, a newer release from GitHub.
//!
//! The check asks GitHub for the latest release and compares its tag with
//! this build's version. On macOS the update installs in place: the release's
//! disk image is downloaded, its app copied next to the running one, the two
//! swapped, and the new one started once this one has quit. Elsewhere the
//! download opens in the browser.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use futures::StreamExt as _;
use serde::Deserialize;

pub const REPO: &str = "devileya/veilpilot";

/// This build's version (`VEILPILOT_PRETEND_VERSION` stands in for it in tests).
pub fn current_version() -> String {
    std::env::var("VEILPILOT_PRETEND_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string())
}

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// Without the leading `v`.
    pub version: String,
    /// The release page.
    pub page: String,
    /// This platform's download, if the release has one.
    pub asset: Option<Asset>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
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

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

/// The file name ending of this platform's download.
fn asset_suffix() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some("-macos-universal.dmg")
    } else if cfg!(all(windows, target_arch = "x86_64")) {
        Some("-windows-x64.zip")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("-linux-x64.tar.gz")
    } else {
        None
    }
}

/// `1.10.0` > `1.9.2`; a pre-release suffix (`-rc.1`) is ignored.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    fn parts(v: &str) -> Vec<u64> {
        v.trim()
            .trim_start_matches(['v', 'V'])
            .split(['-', '+'])
            .next()
            .unwrap_or("")
            .split('.')
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    }
    let (a, b) = (parts(candidate), parts(current));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

fn api_url() -> String {
    std::env::var("VEILPILOT_UPDATE_URL")
        .unwrap_or_else(|_| format!("https://api.github.com/repos/{REPO}/releases/latest"))
}

fn parse_release(body: &str) -> anyhow::Result<Option<Release>> {
    let gh: GhRelease = serde_json::from_str(body).context("reading GitHub's answer")?;
    if gh.draft || gh.prerelease {
        return Ok(None);
    }
    let asset = asset_suffix().and_then(|suffix| {
        gh.assets.iter().find(|a| a.name.ends_with(suffix)).map(|a| Asset {
            name: a.name.clone(),
            url: a.browser_download_url.clone(),
            size: a.size,
        })
    });
    Ok(Some(Release { version: gh.tag_name.trim_start_matches(['v', 'V']).to_string(), page: gh.html_url, asset }))
}

/// The latest release, if it is newer than this build.
pub async fn check() -> anyhow::Result<Option<Release>> {
    let response = crate::runtime::http()
        .get(api_url())
        .header("accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .context("reaching GitHub")?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None); // no release published yet
    }
    if !response.status().is_success() {
        bail!("GitHub answered {}", response.status());
    }
    let release = parse_release(&response.text().await?)?;
    Ok(release.filter(|r| is_newer(&r.version, &current_version())))
}

/// Can this build replace itself? Only an app bundle on macOS can.
pub fn can_install() -> bool {
    cfg!(target_os = "macos") && running_bundle().is_some()
}

/// The `.app` this process runs from.
fn running_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    // …/VeilPilot.app/Contents/MacOS/VeilPilot
    let bundle = exe.parent()?.parent()?.parent()?;
    bundle.extension().is_some_and(|e| e == "app").then(|| bundle.to_path_buf())
}

async fn download(url: &str, to: &Path) -> anyhow::Result<()> {
    let response = crate::runtime::http().get(url).send().await.context("downloading the update")?;
    if !response.status().is_success() {
        bail!("the download answered {}", response.status());
    }
    let mut file = tokio::fs::File::create(to).await?;
    let mut stream = response.bytes_stream();
    use tokio::io::AsyncWriteExt as _;
    while let Some(chunk) = stream.next().await {
        file.write_all(&chunk.context("downloading the update")?).await?;
    }
    file.flush().await?;
    Ok(())
}

async fn run(program: &str, args: &[&str]) -> anyhow::Result<()> {
    let out = tokio::process::Command::new(program)
        .args(args)
        .output()
        .await
        .with_context(|| format!("running {program}"))?;
    if !out.status.success() {
        bail!("{program} failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Download `release` and put it in place of the running app. Returns the
/// app to start once this one has quit.
pub async fn install(release: &Release) -> anyhow::Result<PathBuf> {
    let asset = release.asset.as_ref().ok_or_else(|| anyhow!("this release has no download for this computer"))?;
    let bundle = running_bundle().ok_or_else(|| anyhow!("VeilPilot is not running from an app bundle"))?;
    let parent = bundle.parent().ok_or_else(|| anyhow!("no folder around {}", bundle.display()))?.to_path_buf();
    let work = std::env::temp_dir().join(format!("veilpilot-update-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&work).await?;
    let result = async {
        let dmg = work.join(&asset.name);
        download(&asset.url, &dmg).await?;
        let mount = work.join("mnt");
        run(
            "hdiutil",
            &[
                "attach",
                "-nobrowse",
                "-readonly",
                "-noautoopen",
                "-mountpoint",
                &mount.to_string_lossy(),
                &dmg.to_string_lossy(),
            ],
        )
        .await?;
        // Copied next to the old app (same volume), so the swap is two renames.
        let staged = parent.join(format!(".VeilPilot-{}.app", release.version));
        let copied = async {
            let app = std::fs::read_dir(&mount)?
                .flatten()
                .map(|e| e.path())
                .find(|p| p.extension().is_some_and(|e| e == "app"))
                .ok_or_else(|| anyhow!("no app in the disk image"))?;
            let _ = tokio::fs::remove_dir_all(&staged).await;
            run("ditto", &[&app.to_string_lossy(), &staged.to_string_lossy()]).await
        }
        .await;
        let _ = run("hdiutil", &["detach", "-quiet", &mount.to_string_lossy()]).await;
        copied.context("copying the new version")?;

        let old = parent.join(format!(".VeilPilot-old-{}.app", uuid::Uuid::new_v4()));
        tokio::fs::rename(&bundle, &old)
            .await
            .with_context(|| format!("no permission to replace {}", bundle.display()))?;
        if let Err(err) = tokio::fs::rename(&staged, &bundle).await {
            let _ = tokio::fs::rename(&old, &bundle).await;
            return Err(err).context("putting the new version in place");
        }
        let _ = tokio::fs::remove_dir_all(&old).await;
        anyhow::Ok(bundle.clone())
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&work).await;
    result
}

/// Start `bundle`'s app once this process has exited (the app allows one
/// instance, so the new one must not start while this one runs).
pub fn relaunch_after_exit(bundle: &Path) -> anyhow::Result<()> {
    let exe = std::fs::read_dir(bundle.join("Contents/MacOS"))?
        .flatten()
        .map(|e| e.path())
        .next()
        .ok_or_else(|| anyhow!("no program in {}", bundle.display()))?;
    let script = format!("while kill -0 {pid} 2>/dev/null; do sleep 0.2; done; exec \"$0\"", pid = std::process::id());
    let mut command = std::process::Command::new("/bin/sh");
    command.arg("-c").arg(script).arg(&exe);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    command.spawn().context("starting the new version")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        assert!(is_newer("0.2.1", "0.2.0"));
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("1.0", "0.99.0"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.1.9", "0.2.0"));
        assert!(!is_newer("0.2.0-rc.1", "0.2.0"));
    }

    #[test]
    fn reads_a_github_release() {
        let body = r#"{"tag_name":"v0.3.0","html_url":"https://github.com/devileya/veilpilot/releases/tag/v0.3.0",
            "draft":false,"prerelease":false,"assets":[
            {"name":"VeilPilot-0.3.0-macos-universal.dmg","browser_download_url":"https://x/mac.dmg","size":10},
            {"name":"VeilPilot-0.3.0-windows-x64.zip","browser_download_url":"https://x/win.zip","size":20},
            {"name":"VeilPilot-0.3.0-linux-x64.tar.gz","browser_download_url":"https://x/linux.tgz","size":30}]}"#;
        let r = parse_release(body).unwrap().unwrap();
        assert_eq!(r.version, "0.3.0");
        let asset = r.asset.expect("an asset for this platform");
        assert!(asset.name.ends_with(asset_suffix().unwrap()));
        let pre = body.replace(r#""prerelease":false"#, r#""prerelease":true"#);
        assert_eq!(parse_release(&pre).unwrap(), None, "pre-releases are not offered");
    }
}
