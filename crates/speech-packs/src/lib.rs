//! Pronto speech packs.
//!
//! A *pack* is one pinned download: a model file, an unpacked model folder, or
//! an unpacked runtime. Each speech backend ([`AsrModel`]) needs a set of packs.
//! The installer and the in-app model switcher both go through this crate, so
//! a pack installed by either one is recognised by the other.
//!
//! Layout under the Pronto data root (`%LOCALAPPDATA%\Pronto` on Windows):
//!
//! ```text
//! models/parakeet-tdt-0.6b-v3.q8_0.gguf   kind=file
//! models/phonon-2/                        kind=zip (+ .pack-sha256 marker)
//! runtimes/nemo-speech-cuda/              kind=zip
//! runtimes/phonon-cpu/                    kind=zip
//! downloads/                              resumable *.part files
//! ```

mod download;
mod gpu;

pub use download::{sha256_file, verify_file};
pub use gpu::{detect_gpus, GpuAdapter, GpuInfo, GpuVendor};

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const MANIFEST: &str = include_str!("../speech-packs.manifest");
const MARKER: &str = ".pack-sha256";
const MIB: u64 = 1024 * 1024;
/// Head-room kept free beyond the download itself (unpacking + temp files).
const DISK_HEADROOM: u64 = 200 * MIB;

/// The speech backends Pronto can run.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AsrModel {
    /// NVIDIA Parakeet TDT 0.6B v3 on CUDA (NeMo-Speech.cpp runtime).
    #[default]
    Parakeet,
    /// Fermion Research Phonon-2 on the CPU (fermion-research runtime).
    Phonon,
}

impl AsrModel {
    pub const ALL: [AsrModel; 2] = [AsrModel::Parakeet, AsrModel::Phonon];

    pub fn id(self) -> &'static str {
        match self {
            AsrModel::Parakeet => "parakeet",
            AsrModel::Phonon => "phonon",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "parakeet" => Some(AsrModel::Parakeet),
            "phonon" => Some(AsrModel::Phonon),
            _ => None,
        }
    }

    /// Short product name used in status messages ("Parakeet is warm and ready").
    pub fn short_name(self) -> &'static str {
        match self {
            AsrModel::Parakeet => "Parakeet",
            AsrModel::Phonon => "Phonon",
        }
    }

    /// Full backend description for the engine row.
    pub fn backend_label(self) -> &'static str {
        match self {
            AsrModel::Parakeet => "NVIDIA Parakeet TDT 0.6B v3 · CUDA",
            AsrModel::Phonon => "Fermion Phonon-2 · CPU",
        }
    }

    pub fn uses_gpu(self) -> bool {
        matches!(self, AsrModel::Parakeet)
    }

    pub fn english_only(self) -> bool {
        matches!(self, AsrModel::Phonon)
    }
}

/// Which backend to suggest for this machine: Parakeet only where CUDA can run.
pub fn recommended_model(gpu: &GpuInfo) -> AsrModel {
    if gpu.has_nvidia() {
        AsrModel::Parakeet
    } else {
        AsrModel::Phonon
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackKind {
    File,
    Zip,
}

#[derive(Clone, Debug)]
pub struct Pack {
    pub id: String,
    pub model: AsrModel,
    pub kind: PackKind,
    /// Relative to the data root, with forward slashes.
    pub dest: String,
    pub url: String,
    pub sha256: Option<String>,
    pub size: u64,
    pub approx_bytes: u64,
    windows_only: bool,
}

impl Pack {
    pub fn is_published(&self) -> bool {
        self.sha256.is_some() && self.size > 0
    }

    /// Size to show before downloading.
    pub fn display_bytes(&self) -> u64 {
        if self.size > 0 {
            self.size
        } else {
            self.approx_bytes
        }
    }

    pub fn dest_path(&self, root: &Path) -> PathBuf {
        root.join(self.dest.replace('/', std::path::MAIN_SEPARATOR_STR))
    }

    fn applies_here(&self) -> bool {
        !self.windows_only || cfg!(windows)
    }
}

/// Every pack in the manifest that applies to this platform.
pub fn catalog() -> &'static [Pack] {
    static CATALOG: OnceLock<Vec<Pack>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        parse_manifest(MANIFEST)
            .expect("speech-packs.manifest is invalid")
            .into_iter()
            .filter(Pack::applies_here)
            .collect()
    })
}

pub fn pack(id: &str) -> Option<&'static Pack> {
    catalog().iter().find(|pack| pack.id == id)
}

pub fn packs_for(model: AsrModel) -> impl Iterator<Item = &'static Pack> {
    catalog().iter().filter(move |pack| pack.model == model)
}

fn parse_manifest(text: &str) -> Result<Vec<Pack>, String> {
    let mut packs = Vec::new();
    let mut current: Option<(String, Vec<(String, String)>)> = None;
    let mut finish = |section: Option<(String, Vec<(String, String)>)>| -> Result<(), String> {
        let Some((id, values)) = section else {
            return Ok(());
        };
        let get = |key: &str| {
            values
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
                .ok_or_else(|| format!("[{id}] lacks {key}"))
        };
        let url = get("url")?.to_string();
        if !url.starts_with("https://") {
            return Err(format!("[{id}] url must use HTTPS"));
        }
        let dest = get("dest")?.to_string();
        if dest.contains([':', '\\']) || !is_safe_relative(Path::new(&dest)) {
            return Err(format!(
                "[{id}] dest must be a relative path inside the data root"
            ));
        }
        let sha = get("sha256")?.to_ascii_lowercase();
        let sha256 = match sha.as_str() {
            "pending" => None,
            value if value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()) => {
                Some(value.to_string())
            }
            _ => {
                return Err(format!(
                    "[{id}] sha256 must be 64 hex characters or `pending`"
                ))
            }
        };
        let parse_u64 = |key: &str| {
            get(key)?
                .parse::<u64>()
                .map_err(|_| format!("[{id}] {key} must be a number"))
        };
        packs.push(Pack {
            model: AsrModel::parse(get("model")?).ok_or_else(|| format!("[{id}] unknown model"))?,
            kind: match get("kind")? {
                "file" => PackKind::File,
                "zip" => PackKind::Zip,
                _ => return Err(format!("[{id}] kind must be file or zip")),
            },
            windows_only: match get("platform")? {
                "windows" => true,
                "any" => false,
                _ => return Err(format!("[{id}] platform must be windows or any")),
            },
            dest,
            url,
            sha256,
            size: parse_u64("size")?,
            approx_bytes: parse_u64("approx_mb")? * MIB,
            id,
        });
        Ok(())
    };
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            finish(current.take())?;
            current = Some((name.trim().to_string(), Vec::new()));
        } else if let Some((key, value)) = line.split_once('=') {
            let (_, values) = current
                .as_mut()
                .ok_or_else(|| format!("`{line}` appears before any [pack] section"))?;
            values.push((key.trim().to_string(), value.trim().to_string()));
        } else {
            return Err(format!("unrecognised manifest line `{line}`"));
        }
    }
    finish(current.take())?;
    Ok(packs)
}

fn is_safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

/// True when the pack is present and was installed from the pinned download.
///
/// A file pack is accepted on an exact size match: it was hash-verified when
/// it was installed (by this crate or by the legacy NSIS installer), and
/// re-hashing 700 MB on every launch would delay startup for no gain.
pub fn is_installed(root: &Path, pack: &Pack) -> bool {
    let dest = pack.dest_path(root);
    match pack.kind {
        PackKind::File => fs::metadata(&dest)
            .map(|meta| meta.is_file() && (pack.size == 0 || meta.len() == pack.size))
            .unwrap_or(false),
        PackKind::Zip => match fs::read_to_string(dest.join(MARKER)) {
            Ok(marker) => match &pack.sha256 {
                Some(sha) => marker.trim() == sha,
                // A pending pack can only be present when sideloaded by a developer.
                None => true,
            },
            Err(_) => false,
        },
    }
}

/// Packs `model` still needs, in install order.
pub fn missing_packs(root: &Path, model: AsrModel) -> Vec<&'static Pack> {
    packs_for(model)
        .filter(|pack| !is_installed(root, pack))
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum InstallPhase {
    Preparing,
    Downloading,
    Verifying,
    Unpacking,
    Done,
}

/// Progress across every pack of one install, so a UI can show a single bar.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub phase: InstallPhase,
    pub pack_id: String,
    pub pack_index: usize,
    pub pack_count: usize,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub bytes_per_sec: u64,
    /// `None` until the transfer rate has settled.
    pub eta_secs: Option<u64>,
}

/// Download, verify, and install every missing pack for `model`.
///
/// Partial downloads survive cancellation and failures and are resumed on the
/// next attempt. `skip` lets a caller treat extra packs as present (for
/// example a CUDA runtime bundled beside the executable).
pub fn install_model(
    root: &Path,
    model: AsrModel,
    skip: impl Fn(&Pack) -> bool,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> Result<(), String> {
    let packs: Vec<&Pack> = missing_packs(root, model)
        .into_iter()
        .filter(|pack| !skip(pack))
        .collect();
    if packs.iter().any(|pack| !pack.is_published()) {
        return Err(format!(
            "The {} download is not published yet. Try again after the next Pronto update.",
            model.short_name()
        ));
    }
    let total: u64 = packs.iter().map(|pack| pack.size).sum();
    check_disk_space(root, total)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("Could not start the downloader: {e}"))?;
    let mut meter = RateMeter::new();
    let mut completed = 0u64;
    for (index, pack) in packs.iter().enumerate() {
        let report = |phase, done: u64, meter: &RateMeter| Progress {
            phase,
            pack_id: pack.id.clone(),
            pack_index: index,
            pack_count: packs.len(),
            downloaded_bytes: completed + done,
            total_bytes: total,
            bytes_per_sec: meter.rate(),
            eta_secs: meter.eta(total.saturating_sub(completed + done)),
        };
        progress(report(InstallPhase::Preparing, 0, &meter));
        runtime.block_on(install_pack(root, pack, cancel, |phase, done| {
            if phase == InstallPhase::Downloading {
                meter.sample(completed + done);
            }
            progress(report(phase, done, &meter));
        }))?;
        completed += pack.size;
    }
    progress(Progress {
        phase: InstallPhase::Done,
        pack_id: String::new(),
        pack_index: packs.len(),
        pack_count: packs.len(),
        downloaded_bytes: total,
        total_bytes: total,
        bytes_per_sec: 0,
        eta_secs: Some(0),
    });
    Ok(())
}

fn check_disk_space(root: &Path, bytes: u64) -> Result<(), String> {
    fs::create_dir_all(root).map_err(|e| format!("Could not create {}: {e}", root.display()))?;
    let free =
        fs2::available_space(root).map_err(|e| format!("Could not check free disk space: {e}"))?;
    // Zip packs need room for the archive and its unpacked copy at once.
    let needed = bytes.saturating_mul(2).saturating_add(DISK_HEADROOM);
    if free < needed {
        return Err(format!(
            "Not enough free disk space. Free up {} MB and try again.",
            (needed - free).div_ceil(MIB)
        ));
    }
    Ok(())
}

async fn install_pack(
    root: &Path,
    pack: &Pack,
    cancel: &AtomicBool,
    mut progress: impl FnMut(InstallPhase, u64),
) -> Result<(), String> {
    let sha = pack
        .sha256
        .as_deref()
        .ok_or_else(|| format!("{} is not published", pack.id))?;
    let downloads = root.join("downloads");
    fs::create_dir_all(&downloads).map_err(|e| format!("Could not create download folder: {e}"))?;
    let file_name = pack.url.rsplit('/').next().unwrap_or(&pack.id);
    let partial = downloads.join(format!("{file_name}.part"));
    let mut offset = fs::metadata(&partial).map(|m| m.len()).unwrap_or(0);
    if offset > pack.size {
        fs::remove_file(&partial).map_err(|e| e.to_string())?;
        offset = 0;
    }
    if offset < pack.size {
        if cancel.load(Ordering::Acquire) {
            return Err(download::CANCELLED.into());
        }
        download::download_partial(cancel, &pack.url, pack.size, &partial, offset, |done| {
            progress(InstallPhase::Downloading, done)
        })
        .await?;
    }
    progress(InstallPhase::Verifying, pack.size);
    if !verify_file(&partial, sha, pack.size)? {
        let _ = fs::remove_file(&partial);
        return Err("The download was corrupted and has been discarded. Try again.".into());
    }
    let dest = pack.dest_path(root);
    match pack.kind {
        PackKind::File => {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            replace_path(&partial, &dest)?;
        }
        PackKind::Zip => {
            progress(InstallPhase::Unpacking, pack.size);
            unpack_zip(&partial, &dest, sha)?;
            let _ = fs::remove_file(&partial);
        }
    }
    Ok(())
}

/// Atomically-ish move `from` over `to`: the old copy is moved aside first so a
/// failed rename never leaves both missing.
fn replace_path(from: &Path, to: &Path) -> Result<(), String> {
    let aside = to.with_extension("old");
    if aside.exists() {
        remove_any(&aside)?;
    }
    if to.exists() {
        fs::rename(to, &aside).map_err(|e| {
            format!(
                "Could not replace {} (is Pronto still running?): {e}",
                to.display()
            )
        })?;
    }
    if let Err(error) = fs::rename(from, to) {
        let _ = fs::rename(&aside, to);
        return Err(format!("Could not install {}: {error}", to.display()));
    }
    let _ = remove_any(&aside);
    Ok(())
}

fn remove_any(path: &Path) -> Result<(), String> {
    let result = if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    result.map_err(|e| format!("Could not remove {}: {e}", path.display()))
}

/// Unpack into a staging folder beside `dest`, stamp it with the archive hash,
/// then swap it into place.
pub fn unpack_zip(archive: &Path, dest: &Path, sha: &str) -> Result<(), String> {
    let staging = dest.with_extension("installing");
    if staging.exists() {
        remove_any(&staging)?;
    }
    fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    let result = (|| {
        let file = fs::File::open(archive).map_err(|e| e.to_string())?;
        let mut zip =
            zip::ZipArchive::new(file).map_err(|e| format!("Invalid pack archive: {e}"))?;
        for index in 0..zip.len() {
            let mut entry = zip.by_index(index).map_err(|e| e.to_string())?;
            let Some(relative) = entry.enclosed_name() else {
                return Err(format!("Pack archive has an unsafe path: {}", entry.name()));
            };
            let target = staging.join(relative);
            if entry.is_dir() {
                fs::create_dir_all(&target).map_err(|e| e.to_string())?;
                continue;
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut out = fs::File::create(&target).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut out)
                .map_err(|e| format!("Could not unpack pack: {e}"))?;
        }
        fs::write(staging.join(MARKER), sha).map_err(|e| e.to_string())
    })();
    if let Err(error) = result {
        let _ = remove_any(&staging);
        return Err(error);
    }
    replace_path(&staging, dest)
}

/// Remove an installed pack (used when uninstalling).
pub fn remove_pack(root: &Path, pack: &Pack) -> Result<(), String> {
    let dest = pack.dest_path(root);
    if dest.exists() {
        remove_any(&dest)?;
    }
    Ok(())
}

/// Smoothed transfer rate. ETA is withheld for the first couple of seconds so
/// the UI never flashes wild estimates.
struct RateMeter {
    started: Instant,
    last: Option<(Instant, u64)>,
    rate: f64,
}

impl RateMeter {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            last: None,
            rate: 0.0,
        }
    }

    fn sample(&mut self, bytes: u64) {
        let now = Instant::now();
        match self.last {
            None => self.last = Some((now, bytes)),
            Some((at, previous)) => {
                let elapsed = now.duration_since(at).as_secs_f64();
                if elapsed < 0.25 {
                    return;
                }
                let instant = bytes.saturating_sub(previous) as f64 / elapsed;
                self.rate = if self.rate == 0.0 {
                    instant
                } else {
                    self.rate * 0.8 + instant * 0.2
                };
                self.last = Some((now, bytes));
            }
        }
    }

    fn rate(&self) -> u64 {
        self.rate as u64
    }

    fn eta(&self, remaining: u64) -> Option<u64> {
        if self.started.elapsed() < Duration::from_secs(2) || self.rate < 1.0 {
            return None;
        }
        Some((remaining as f64 / self.rate).ceil() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_parses_and_pins_parakeet() {
        let packs = parse_manifest(MANIFEST).unwrap();
        let parakeet = packs.iter().find(|p| p.id == "parakeet-model").unwrap();
        assert_eq!(parakeet.model, AsrModel::Parakeet);
        assert_eq!(parakeet.kind, PackKind::File);
        assert!(parakeet.is_published());
        assert!(packs.iter().any(|p| p.model == AsrModel::Phonon));
        for pack in &packs {
            assert!(pack.display_bytes() > 0, "{} needs a size hint", pack.id);
        }
    }

    #[test]
    fn manifest_rejects_unsafe_entries() {
        let base = "[x]\nmodel=phonon\nplatform=any\nkind=zip\nurl=https://e.test/a\nsha256=pending\nsize=0\napprox_mb=1\n";
        assert!(parse_manifest(&format!("{base}dest=models/x\n")).is_ok());
        assert!(parse_manifest(&format!("{base}dest=../x\n")).is_err());
        assert!(parse_manifest(&format!("{base}dest=C:/x\n")).is_err());
        assert!(parse_manifest(
            &base
                .replace("https://", "http://")
                .replace("approx_mb=1", "approx_mb=1\ndest=m")
        )
        .is_err());
    }

    #[test]
    fn recommends_parakeet_only_with_nvidia() {
        let adapter = |vendor| GpuAdapter {
            name: "GPU".into(),
            vendor,
            dedicated_mb: 4096,
        };
        assert_eq!(recommended_model(&GpuInfo::default()), AsrModel::Phonon);
        let amd = GpuInfo {
            adapters: vec![adapter(GpuVendor::Amd), adapter(GpuVendor::Intel)],
        };
        assert_eq!(recommended_model(&amd), AsrModel::Phonon);
        let nvidia = GpuInfo {
            adapters: vec![adapter(GpuVendor::Intel), adapter(GpuVendor::Nvidia)],
        };
        assert_eq!(recommended_model(&nvidia), AsrModel::Parakeet);
    }

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("speech-packs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn zip_fixture(path: &Path, entries: &[(&str, &[u8])]) {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(fs::File::create(path).unwrap());
        for (name, data) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn zip_pack_installs_with_marker_and_replaces_previous_copy() {
        let root = temp_root("zip");
        let archive = root.join("pack.zip");
        zip_fixture(&archive, &[("bin/tool.txt", b"v1")]);
        let dest = root.join("runtimes/tool");
        unpack_zip(&archive, &dest, "aa").unwrap();
        assert_eq!(fs::read(dest.join("bin/tool.txt")).unwrap(), b"v1");
        let pack = Pack {
            id: "tool".into(),
            model: AsrModel::Phonon,
            kind: PackKind::Zip,
            dest: "runtimes/tool".into(),
            url: "https://e.test/pack.zip".into(),
            sha256: Some("aa".into()),
            size: 1,
            approx_bytes: 1,
            windows_only: false,
        };
        assert!(is_installed(&root, &pack));
        zip_fixture(&archive, &[("bin/new.txt", b"v2")]);
        unpack_zip(&archive, &dest, "bb").unwrap();
        assert!(!dest.join("bin/tool.txt").exists());
        assert!(!is_installed(&root, &pack));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn zip_pack_refuses_path_traversal() {
        let root = temp_root("slip");
        let archive = root.join("evil.zip");
        zip_fixture(&archive, &[("../escape.txt", b"x")]);
        let dest = root.join("inner/dest");
        assert!(unpack_zip(&archive, &dest, "aa").is_err());
        assert!(!root.join("inner/escape.txt").exists());
        assert!(!dest.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn file_pack_is_installed_on_exact_size() {
        let root = temp_root("file");
        let pack = Pack {
            id: "m".into(),
            model: AsrModel::Parakeet,
            kind: PackKind::File,
            dest: "models/m.bin".into(),
            url: "https://e.test/m.bin".into(),
            sha256: Some("x".repeat(64)),
            size: 3,
            approx_bytes: 3,
            windows_only: false,
        };
        assert!(!is_installed(&root, &pack));
        fs::create_dir_all(root.join("models")).unwrap();
        fs::write(root.join("models/m.bin"), b"ab").unwrap();
        assert!(!is_installed(&root, &pack));
        fs::write(root.join("models/m.bin"), b"abc").unwrap();
        assert!(is_installed(&root, &pack));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn eta_waits_for_a_settled_rate() {
        let mut meter = RateMeter::new();
        meter.sample(0);
        assert_eq!(meter.eta(1000), None);
        meter.started -= Duration::from_secs(5);
        meter.last = Some((Instant::now() - Duration::from_secs(1), 0));
        meter.sample(1000);
        assert!(meter.rate() > 500);
        assert!(meter.eta(10_000).is_some());
    }
}
