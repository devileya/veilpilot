//! On-device speech to text. The recorder hands over 16 kHz mono WAV bytes;
//! this writes them to a temp file and runs the whisper.cpp CLI over it, so
//! meeting audio never leaves the machine.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use futures::StreamExt as _;
use regex::Regex;

use crate::ai::claude_code::is_executable;
use crate::config::{WhisperCppConfig, data_dir};

pub struct ModelInfo {
    pub file: &'static str,
    pub label: &'static str,
    pub bytes: u64,
}

pub static MODELS: &[ModelInfo] = &[
    ModelInfo { file: "ggml-tiny.bin", label: "tiny · 78 MB", bytes: 77_691_713 },
    ModelInfo { file: "ggml-base.bin", label: "base · 148 MB", bytes: 147_951_465 },
    ModelInfo { file: "ggml-small.bin", label: "small · 488 MB", bytes: 487_601_967 },
    ModelInfo { file: "ggml-medium.bin", label: "medium · 1.5 GB", bytes: 1_533_763_059 },
];

const MODEL_BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/";

const BINARY_NAMES: &[&str] = if cfg!(windows) {
    &["whisper-cli.exe", "whisper-cpp.exe", "main.exe"]
} else {
    &["whisper-cli", "whisper-cpp", "whisper"]
};

pub fn model_dir() -> PathBuf {
    data_dir().join("models")
}

fn extra_binary_dirs() -> Vec<PathBuf> {
    if cfg!(windows) {
        let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
        vec![PathBuf::from(local).join("Programs").join("whisper.cpp"), PathBuf::from(r"C:\Program Files\whisper.cpp")]
    } else {
        ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].iter().map(PathBuf::from).collect()
    }
}

/// Same PATH problem as the Claude Code CLI: a GUI app on macOS does not see
/// Homebrew's bin directory, so the usual locations are probed too.
pub fn resolve_binary(configured: &str) -> anyhow::Result<PathBuf> {
    if !configured.is_empty() {
        let path = PathBuf::from(configured);
        if is_executable(&path) {
            return Ok(path);
        }
        bail!("whisper.cpp not found at {configured}. Fix the path in settings › Voice.");
    }
    let path_dirs = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default();
    for dir in path_dirs.into_iter().chain(extra_binary_dirs()) {
        for name in BINARY_NAMES {
            let file = dir.join(name);
            if is_executable(&file) {
                return Ok(file);
            }
        }
    }
    bail!(
        "whisper.cpp is not installed. On macOS: brew install whisper-cpp. Or set the binary path in settings › Voice."
    )
}

fn model_candidates() -> Vec<PathBuf> {
    let mut dirs = vec![model_dir()];
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".cache").join("whisper.cpp"));
    }
    if !cfg!(windows) {
        dirs.push(PathBuf::from("/opt/homebrew/share/whisper-cpp"));
    }
    let mut found = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("ggml-") && name.ends_with(".bin") {
                found.push(entry.path());
            }
        }
    }
    found
}

pub fn resolve_model(configured: &str) -> anyhow::Result<PathBuf> {
    if !configured.is_empty() {
        let path = PathBuf::from(configured);
        if path.exists() {
            return Ok(path);
        }
        bail!("Model file not found at {configured}.");
    }
    let mut found = model_candidates();
    if found.is_empty() {
        bail!("No Whisper model found. Download one in settings › Voice.");
    }
    // Bigger file means a bigger model, which means better transcription.
    found.sort_by_key(|p| std::cmp::Reverse(std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)));
    Ok(found.remove(0))
}

#[derive(Clone, Debug)]
pub struct ModelStatus {
    pub file: &'static str,
    pub label: &'static str,
    pub installed: Option<String>,
}

pub fn list_models() -> Vec<ModelStatus> {
    let available = model_candidates();
    MODELS
        .iter()
        .map(|m| ModelStatus {
            file: m.file,
            label: m.label,
            installed: available
                .iter()
                .find(|p| p.file_name().is_some_and(|n| n == m.file))
                .map(|p| p.display().to_string()),
        })
        .collect()
}

#[derive(Clone, Debug, Default)]
pub struct Detection {
    pub ok: bool,
    pub binary_path: Option<String>,
    pub model_path: Option<String>,
    pub model_name: Option<String>,
    pub error: Option<String>,
    pub models: Vec<ModelStatus>,
}

/// Every probe here is a filesystem stat, so this stays synchronous.
pub fn detect(cfg: &WhisperCppConfig) -> Detection {
    let binary = match resolve_binary(&cfg.binary_path) {
        Ok(b) => b,
        Err(err) => return Detection { error: Some(err.to_string()), models: list_models(), ..Default::default() },
    };
    let (model, error) = match resolve_model(&cfg.model_path) {
        Ok(m) => (Some(m), None),
        Err(err) => (None, Some(err.to_string())),
    };
    Detection {
        ok: model.is_some(),
        binary_path: Some(binary.display().to_string()),
        model_name: model.as_ref().and_then(|m| m.file_name()).map(|n| n.to_string_lossy().to_string()),
        model_path: model.map(|m| m.display().to_string()),
        error,
        models: list_models(),
    }
}

/* ---------- transcription ---------- */

/// whisper.cpp prints the transcript to stdout; -np drops the banner and -nt
/// drops the [00:00.000 --> ...] prefixes, leaving just the words.
pub fn build_args(model: &Path, wav: &Path, language: &str, translate: bool, threads: u32) -> Vec<String> {
    let mut args = vec![
        "-m".into(),
        model.display().to_string(),
        "-f".into(),
        wav.display().to_string(),
        "--no-timestamps".into(),
        "--no-prints".into(),
        "-l".into(),
        if language.is_empty() { "auto".into() } else { language.to_string() },
    ];
    if translate {
        args.push("--translate".into());
    }
    if threads > 0 {
        args.extend(["-t".into(), threads.to_string()]);
    }
    args
}

// Diagnostics normally go to stderr, but the exact split has moved between
// whisper.cpp releases, so known noise is dropped from stdout as well.
static NOISE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(read_audio_data|whisper_\w+|ggml_\w+|main|system_info|error|warning|usage):").unwrap()
});
// Bracketed and parenthesised markers are Whisper's non-speech labels
// ([BLANK_AUDIO], (music)) — not something anyone said.
static MARKER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\[[^\]]*\]|\([^)]*\))$").unwrap());
static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

pub fn clean(stdout: &str) -> String {
    let joined = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !NOISE.is_match(line) && !MARKER.is_match(line))
        .collect::<Vec<_>>()
        .join(" ");
    SPACES.replace_all(&joined, " ").trim().to_string()
}

/// The model is re-read on every invocation, so a cold page cache costs ~10s
/// against ~1s warm. Reading the file through once at startup removes that
/// from the first chunk, and costs no CPU.
pub fn warm(cfg: &WhisperCppConfig) {
    let Ok(model) = resolve_model(&cfg.model_path) else { return };
    std::thread::Builder::new()
        .name("whisper-warm".into())
        .spawn(move || {
            use std::io::Read as _;
            if let Ok(mut f) = std::fs::File::open(model) {
                let mut buf = vec![0u8; 4 << 20];
                while matches!(f.read(&mut buf), Ok(n) if n > 0) {}
            }
        })
        .ok();
}

pub async fn transcribe(wav_bytes: Vec<u8>, cfg: &WhisperCppConfig, language: &str) -> anyhow::Result<String> {
    let binary = resolve_binary(&cfg.binary_path)?;
    let model = resolve_model(&cfg.model_path)?;

    let wav = std::env::temp_dir().join(format!("veilpilot-{}.wav", uuid::Uuid::new_v4()));
    tokio::fs::write(&wav, &wav_bytes).await.context("writing the recording")?;
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _cleanup = Cleanup(wav.clone());

    let mut command = tokio::process::Command::new(&binary);
    command
        .args(build_args(&model, &wav, language, cfg.translate, cfg.threads))
        .stdin(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);

    let output = tokio::time::timeout(Duration::from_secs(600), command.output())
        .await
        .map_err(|_| anyhow!("whisper.cpp failed: timed out after 10 minutes"))?
        .map_err(|e| anyhow!("whisper.cpp failed: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: Vec<&str> = stderr.trim().lines().rev().take(3).collect::<Vec<_>>().into_iter().rev().collect();
        let detail = if tail.is_empty() { format!("exited with {}", output.status) } else { tail.join(" ") };
        bail!("whisper.cpp failed: {detail}");
    }
    Ok(clean(&String::from_utf8_lossy(&output.stdout)))
}

/* ---------- model download ---------- */

static DOWNLOADING: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug)]
pub struct Progress {
    pub file: String,
    pub percent: u32,
    pub received: u64,
    pub total: u64,
}

pub async fn download_model(file: &str, mut on_progress: impl FnMut(Progress)) -> anyhow::Result<PathBuf> {
    let info = MODELS.iter().find(|m| m.file == file).ok_or_else(|| anyhow!("Unknown model {file}"))?;
    if DOWNLOADING.swap(true, Ordering::SeqCst) {
        bail!("A model download is already running.");
    }
    struct Release;
    impl Drop for Release {
        fn drop(&mut self) {
            DOWNLOADING.store(false, Ordering::SeqCst);
        }
    }
    let _release = Release;

    let target = model_dir().join(file);
    // Written to .part first so an interrupted download is never mistaken for
    // an installed model.
    let partial = target.with_extension("bin.part");
    let result = async {
        tokio::fs::create_dir_all(model_dir()).await?;
        let response = crate::runtime::http().get(format!("{MODEL_BASE_URL}{file}")).send().await?;
        if !response.status().is_success() {
            bail!(
                "Download failed: {} {}",
                response.status().as_u16(),
                response.status().canonical_reason().unwrap_or("")
            );
        }
        let total = response.content_length().filter(|n| *n > 0).unwrap_or(info.bytes);
        let mut out = tokio::fs::File::create(&partial).await?;
        let mut received = 0u64;
        let mut last = 0u32;
        let mut body = response.bytes_stream();
        while let Some(chunk) = body.next().await {
            let chunk = chunk?;
            received += chunk.len() as u64;
            tokio::io::AsyncWriteExt::write_all(&mut out, &chunk).await?;
            let percent = ((received as f64 / total as f64) * 100.0).floor() as u32;
            if percent > last {
                last = percent;
                on_progress(Progress { file: file.to_string(), percent, received, total });
            }
        }
        tokio::io::AsyncWriteExt::flush(&mut out).await?;
        drop(out);
        tokio::fs::rename(&partial, &target).await?;
        Ok(target.clone())
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&partial).await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_drops_noise_and_markers() {
        let out =
            "whisper_init_from_file: loading\n [BLANK_AUDIO]\n Halo semua,\n (music)\n  apa kabar?  \nmain: done\n";
        assert_eq!(clean(out), "Halo semua, apa kabar?");
    }

    #[test]
    fn args_default_to_autodetect() {
        let a = build_args(Path::new("/m.bin"), Path::new("/a.wav"), "", false, 0);
        assert!(a.windows(2).any(|w| w == ["-l", "auto"]));
        let a = build_args(Path::new("/m.bin"), Path::new("/a.wav"), "id", true, 4);
        assert!(a.windows(2).any(|w| w == ["-l", "id"]));
        assert!(a.contains(&"--translate".to_string()));
        assert!(a.windows(2).any(|w| w == ["-t", "4"]));
    }
}
