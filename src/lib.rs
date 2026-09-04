#![cfg(target_os = "windows")]

use std::ffi::c_void;
use std::fs;
use std::mem::size_of;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use sha2::{Digest, Sha256};
use windows_sys::Win32::Networking::WinHttp::*;

const API_HOST: &str = "api.github.com";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const METADATA_MAX_SIZE: usize = 4 * 1024 * 1024;
const CHECKSUM_MAX_SIZE: usize = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct UpdateConfig {
    pub repository: String,
    pub asset_name: String,
    pub current_version: String,
    pub app_name: String,
    pub checksum_asset_name: Option<String>,
    pub max_download_size: usize,
    pub min_executable_size: usize,
    pub require_checksum: bool,
}

impl UpdateConfig {
    pub fn new(
        repository: impl Into<String>,
        asset_name: impl Into<String>,
        current_version: impl Into<String>,
    ) -> Self {
        let asset_name = asset_name.into();
        let app_name = asset_name
            .strip_suffix(".exe")
            .unwrap_or(&asset_name)
            .to_owned();
        Self {
            repository: repository.into(),
            asset_name,
            current_version: current_version.into(),
            app_name,
            checksum_asset_name: None,
            max_download_size: 100 * 1024 * 1024,
            min_executable_size: 100_000,
            require_checksum: false,
        }
    }

    pub fn with_app_name(mut self, app_name: impl Into<String>) -> Self {
        self.app_name = app_name.into();
        self
    }

    pub fn with_checksum_asset(mut self, asset_name: impl Into<String>) -> Self {
        self.checksum_asset_name = Some(asset_name.into());
        self
    }

    pub fn with_max_download_size(mut self, bytes: usize) -> Self {
        self.max_download_size = bytes;
        self
    }

    pub fn with_min_executable_size(mut self, bytes: usize) -> Self {
        self.min_executable_size = bytes;
        self
    }

    pub fn with_required_checksum(mut self, required: bool) -> Self {
        self.require_checksum = required;
        self
    }
}

#[derive(Clone, Debug)]
pub struct UpdateInfo {
    pub version: String,
    pub release_url: String,
    pub download_url: String,
    pub checksum_url: Option<String>,
    pub asset_digest: Option<String>,
}

#[derive(Clone, Debug)]
pub enum UpdateStatus {
    Idle,
    Checking,
    UpToDate,
    Available(UpdateInfo),
    Downloading {
        info: UpdateInfo,
        downloaded: u64,
        total: Option<u64>,
    },
    Ready(UpdateInfo, PathBuf),
    Failed(String),
}

#[derive(Clone)]
pub struct UpdateManager {
    config: Arc<UpdateConfig>,
    state: Arc<Mutex<UpdateStatus>>,
}

impl UpdateManager {
    pub fn new(config: UpdateConfig) -> Self {
        Self {
            config: Arc::new(config),
            state: Arc::new(Mutex::new(UpdateStatus::Idle)),
        }
    }

    pub fn status(&self) -> UpdateStatus {
        self.state.lock().unwrap().clone()
    }

    pub fn start_check(&self, auto_download: bool) -> bool {
        {
            let mut state = self.state.lock().unwrap();
            if matches!(
                *state,
                UpdateStatus::Checking | UpdateStatus::Downloading { .. }
            ) {
                return false;
            }
            *state = UpdateStatus::Checking;
        }

        let state = Arc::clone(&self.state);
        let config = Arc::clone(&self.config);
        std::thread::spawn(move || {
            let next = match check_latest_release(&config) {
                Ok(Some(info)) if auto_download => {
                    *state.lock().unwrap() = UpdateStatus::Downloading {
                        info: info.clone(),
                        downloaded: 0,
                        total: None,
                    };
                    let progress_state = Arc::clone(&state);
                    let progress_info = info.clone();
                    match download_update(&config, &info, move |downloaded, total| {
                        *progress_state.lock().unwrap() = UpdateStatus::Downloading {
                            info: progress_info.clone(),
                            downloaded,
                            total,
                        };
                    }) {
                        Ok(path) => UpdateStatus::Ready(info, path),
                        Err(message) => UpdateStatus::Failed(message),
                    }
                }
                Ok(Some(info)) => UpdateStatus::Available(info),
                Ok(None) => UpdateStatus::UpToDate,
                Err(message) => UpdateStatus::Failed(message),
            };
            *state.lock().unwrap() = next;
        });
        true
    }

    pub fn start_download(&self) -> bool {
        let info = match self.status() {
            UpdateStatus::Available(info) => info,
            _ => return false,
        };

        *self.state.lock().unwrap() = UpdateStatus::Downloading {
            info: info.clone(),
            downloaded: 0,
            total: None,
        };

        let state = Arc::clone(&self.state);
        let config = Arc::clone(&self.config);
        std::thread::spawn(move || {
            let progress_state = Arc::clone(&state);
            let progress_info = info.clone();
            let next = match download_update(&config, &info, move |downloaded, total| {
                *progress_state.lock().unwrap() = UpdateStatus::Downloading {
                    info: progress_info.clone(),
                    downloaded,
                    total,
                };
            }) {
                Ok(path) => UpdateStatus::Ready(info, path),
                Err(message) => UpdateStatus::Failed(message),
            };
            *state.lock().unwrap() = next;
        });
        true
    }

    pub fn apply_ready(&self) -> Result<bool, String> {
        let source = match self.status() {
            UpdateStatus::Ready(_, source) => source,
            _ => return Ok(false),
        };
        apply_update(&self.config, &source)?;
        Ok(true)
    }
}

#[derive(Clone, Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    assets: Vec<GithubAsset>,
}

#[derive(Clone, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    digest: Option<String>,
}

struct InternetHandle(*mut c_void);

impl Drop for InternetHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                WinHttpCloseHandle(self.0);
            }
        }
    }
}

enum HttpPayload {
    Data(Vec<u8>),
    NotFound,
}

pub fn check_latest_release(config: &UpdateConfig) -> Result<Option<UpdateInfo>, String> {
    validate_config(config)?;
    let path = format!("/repos/{}/releases/latest", config.repository);
    let body = match http_get(API_HOST, &path, METADATA_MAX_SIZE, None)? {
        HttpPayload::Data(body) => body,
        HttpPayload::NotFound => return Ok(None),
    };

    let release: GithubRelease = serde_json::from_slice(&body)
        .map_err(|err| format!("Invalid GitHub release response: {err}"))?;
    if !is_newer(&release.tag_name, &config.current_version) {
        return Ok(None);
    }

    let executable = release
        .assets
        .iter()
        .find(|asset| asset.name.eq_ignore_ascii_case(&config.asset_name))
        .ok_or_else(|| {
            format!(
                "Release {} has no {} asset.",
                release.tag_name, config.asset_name
            )
        })?;

    let checksum_url = config.checksum_asset_name.as_deref().and_then(|name| {
        release
            .assets
            .iter()
            .find(|asset| asset.name.eq_ignore_ascii_case(name))
            .map(|asset| asset.browser_download_url.clone())
    });

    let asset_digest = executable.digest.clone();
    if config.require_checksum
        && parse_github_sha256(asset_digest.as_deref()).is_none()
        && checksum_url.is_none()
    {
        let expected = config
            .checksum_asset_name
            .as_deref()
            .unwrap_or("a SHA-256 digest");
        return Err(format!(
            "Release {} has no usable checksum ({expected}).",
            release.tag_name
        ));
    }

    Ok(Some(UpdateInfo {
        version: release
            .tag_name
            .trim_start_matches(['v', 'V'])
            .to_owned(),
        release_url: release.html_url,
        download_url: executable.browser_download_url.clone(),
        checksum_url,
        asset_digest,
    }))
}

pub fn download_update<F>(
    config: &UpdateConfig,
    info: &UpdateInfo,
    progress: F,
) -> Result<PathBuf, String>
where
    F: Fn(u64, Option<u64>),
{
    validate_config(config)?;
    let expected_checksum = expected_sha256(config, info)?;
    let (host, path) = split_https_url(&info.download_url)
        .ok_or_else(|| "Update URL is not a valid HTTPS URL.".to_owned())?;
    let payload = http_get(host, path, config.max_download_size, Some(&progress))?;
    let bytes = match payload {
        HttpPayload::Data(bytes) => bytes,
        HttpPayload::NotFound => return Err("Update asset returned HTTP 404.".to_owned()),
    };

    if bytes.len() < config.min_executable_size || !bytes.starts_with(b"MZ") {
        return Err("Downloaded update is not a valid Windows executable.".to_owned());
    }

    if let Some(expected_checksum) = expected_checksum {
        let actual_checksum = sha256_hex(&bytes);
        if !actual_checksum.eq_ignore_ascii_case(&expected_checksum) {
            return Err(format!(
                "Update integrity check failed: expected {expected_checksum}, got {actual_checksum}."
            ));
        }
    }

    let safe_version = sanitize_component(&info.version);
    let safe_app = sanitize_component(&config.app_name);
    let path = std::env::temp_dir().join(format!(
        "{safe_app}-update-{}-{safe_version}.exe",
        std::process::id()
    ));
    fs::write(&path, bytes).map_err(|err| format!("Cannot store downloaded update: {err}"))?;
    Ok(path)
}

pub fn apply_update(config: &UpdateConfig, source: &Path) -> Result<(), String> {
    validate_config(config)?;
    if !source.is_file() {
        return Err("Downloaded update file does not exist.".to_owned());
    }
    let current_exe = std::env::current_exe()
        .map_err(|err| format!("Cannot locate current executable: {err}"))?;
    let safe_app = sanitize_component(&config.app_name);
    let script = std::env::temp_dir().join(format!(
        "{safe_app}-updater-{}.ps1",
        std::process::id()
    ));
    fs::write(&script, updater_script())
        .map_err(|err| format!("Cannot create updater script: {err}"))?;
    launch_updater(&script, source, &current_exe)
}

fn validate_config(config: &UpdateConfig) -> Result<(), String> {
    if config.repository.trim().is_empty() || !config.repository.contains('/') {
        return Err("GitHub repository must be in owner/name format.".to_owned());
    }
    if config.asset_name.trim().is_empty() {
        return Err("Update asset name cannot be empty.".to_owned());
    }
    if config.current_version.trim().is_empty() {
        return Err("Current application version cannot be empty.".to_owned());
    }
    if config.max_download_size == 0 {
        return Err("Maximum download size must be greater than zero.".to_owned());
    }
    Ok(())
}

fn expected_sha256(config: &UpdateConfig, info: &UpdateInfo) -> Result<Option<String>, String> {
    if let Some(checksum) = parse_github_sha256(info.asset_digest.as_deref()) {
        return Ok(Some(checksum));
    }

    if let Some(url) = info.checksum_url.as_deref() {
        let (host, path) = split_https_url(url)
            .ok_or_else(|| "Update checksum URL is not a valid HTTPS URL.".to_owned())?;
        let payload = http_get(host, path, CHECKSUM_MAX_SIZE, None)?;
        let bytes = match payload {
            HttpPayload::Data(bytes) => bytes,
            HttpPayload::NotFound => {
                return Err("Update checksum asset returned HTTP 404.".to_owned());
            }
        };
        return parse_sha256_checksum(&bytes).map(Some);
    }

    if config.require_checksum {
        Err("Update release does not provide a usable SHA-256 checksum.".to_owned())
    } else {
        Ok(None)
    }
}

fn parse_github_sha256(digest: Option<&str>) -> Option<String> {
    let digest = digest?.trim();
    let checksum = digest.strip_prefix("sha256:")?;
    if checksum.len() == 64 && checksum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Some(checksum.to_ascii_lowercase())
    } else {
        None
    }
}

fn parse_sha256_checksum(bytes: &[u8]) -> Result<String, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| "Update checksum file is not UTF-8/ASCII text.".to_owned())?;
    let checksum = text
        .split_whitespace()
        .next()
        .ok_or_else(|| "Update checksum file is empty.".to_owned())?;
    if checksum.len() != 64 || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Update checksum file does not contain a valid SHA-256 digest.".to_owned());
    }
    Ok(checksum.to_ascii_lowercase())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut text = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut text, "{byte:02x}");
    }
    text
}

fn launch_updater(script: &Path, source: &Path, destination: &Path) -> Result<(), String> {
    Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-WindowStyle",
            "Hidden",
            "-File",
        ])
        .arg(script)
        .arg("-TargetPid")
        .arg(std::process::id().to_string())
        .arg("-Source")
        .arg(source)
        .arg("-Destination")
        .arg(destination)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .map_err(|err| format!("Cannot launch updater: {err}"))
}

fn updater_script() -> &'static str {
    r#"param([int]$TargetPid, [string]$Source, [string]$Destination)
Wait-Process -Id $TargetPid -ErrorAction SilentlyContinue
for ($attempt = 0; $attempt -lt 30; $attempt++) {
    try {
        Copy-Item -LiteralPath $Source -Destination $Destination -Force -ErrorAction Stop
        Start-Process -FilePath $Destination
        Remove-Item -LiteralPath $Source -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $PSCommandPath -Force -ErrorAction SilentlyContinue
        exit 0
    } catch {
        Start-Sleep -Milliseconds 500
    }
}
"#
}

fn split_https_url(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("https://")?;
    let slash = rest.find('/')?;
    Some((&rest[..slash], &rest[slash..]))
}

fn sanitize_component(value: &str) -> String {
    let cleaned = value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(*ch, '.' | '-' | '_'))
        .collect::<String>();
    if cleaned.is_empty() {
        "app".to_owned()
    } else {
        cleaned
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum VersionIdentifier {
    Numeric(u64),
    Text(String),
}

fn parse_version(value: &str) -> Option<((u32, u32, u32), Vec<VersionIdentifier>)> {
    let normalized = value.trim().trim_start_matches(['v', 'V']);
    let normalized = normalized.split('+').next()?;
    let (core, prerelease) = normalized
        .split_once('-')
        .map(|(core, prerelease)| (core, Some(prerelease)))
        .unwrap_or((normalized, None));
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let prerelease = prerelease
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .split('.')
                .map(|identifier| {
                    identifier
                        .parse::<u64>()
                        .map(VersionIdentifier::Numeric)
                        .unwrap_or_else(|_| VersionIdentifier::Text(identifier.to_ascii_lowercase()))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Some(((major, minor, patch), prerelease))
}

fn compare_prerelease(
    remote: &[VersionIdentifier],
    current: &[VersionIdentifier],
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (remote.is_empty(), current.is_empty()) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        (false, false) => {}
    }
    for index in 0..remote.len().max(current.len()) {
        match (remote.get(index), current.get(index)) {
            (Some(VersionIdentifier::Numeric(left)), Some(VersionIdentifier::Numeric(right))) => {
                let ordering = left.cmp(right);
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            (Some(VersionIdentifier::Numeric(_)), Some(VersionIdentifier::Text(_))) => {
                return Ordering::Less;
            }
            (Some(VersionIdentifier::Text(_)), Some(VersionIdentifier::Numeric(_))) => {
                return Ordering::Greater;
            }
            (Some(VersionIdentifier::Text(left)), Some(VersionIdentifier::Text(right))) => {
                let ordering = left.cmp(right);
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
            (None, None) => return Ordering::Equal,
        }
    }
    Ordering::Equal
}

fn compare_versions(remote: &str, current: &str) -> Option<std::cmp::Ordering> {
    let (remote_core, remote_pre) = parse_version(remote)?;
    let (current_core, current_pre) = parse_version(current)?;
    let core_ordering = remote_core.cmp(&current_core);
    if core_ordering != std::cmp::Ordering::Equal {
        return Some(core_ordering);
    }
    Some(compare_prerelease(&remote_pre, &current_pre))
}

fn is_newer(remote: &str, current: &str) -> bool {
    matches!(compare_versions(remote, current), Some(std::cmp::Ordering::Greater))
}

fn http_get(
    host: &str,
    path: &str,
    max_size: usize,
    progress: Option<&dyn Fn(u64, Option<u64>)>,
) -> Result<HttpPayload, String> {
    unsafe {
        let agent = wide("update-via-github/0.1");
        let session = InternetHandle(WinHttpOpen(
            agent.as_ptr(),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            null(),
            null(),
            0,
        ));
        if session.0.is_null() {
            return Err("Cannot initialize WinHTTP.".to_owned());
        }

        let host_wide = wide(host);
        let connection = InternetHandle(WinHttpConnect(
            session.0,
            host_wide.as_ptr(),
            INTERNET_DEFAULT_HTTPS_PORT,
            0,
        ));
        if connection.0.is_null() {
            return Err("Cannot connect to update server.".to_owned());
        }

        let verb = wide("GET");
        let path_wide = wide(path);
        let request = InternetHandle(WinHttpOpenRequest(
            connection.0,
            verb.as_ptr(),
            path_wide.as_ptr(),
            null(),
            null(),
            null(),
            WINHTTP_FLAG_SECURE | WINHTTP_FLAG_REFRESH,
        ));
        if request.0.is_null() {
            return Err("Cannot create update request.".to_owned());
        }
        if WinHttpSendRequest(request.0, null(), 0, null(), 0, 0, 0) == 0
            || WinHttpReceiveResponse(request.0, null_mut()) == 0
        {
            return Err("Update request failed.".to_owned());
        }

        let mut status_code = 0u32;
        let mut status_size = size_of::<u32>() as u32;
        let mut index = 0u32;
        if WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            null(),
            &mut status_code as *mut u32 as *mut c_void,
            &mut status_size,
            &mut index,
        ) == 0
        {
            return Err("Cannot read update HTTP status.".to_owned());
        }
        if status_code == 404 {
            return Ok(HttpPayload::NotFound);
        }
        if !(200..300).contains(&status_code) {
            return Err(format!("Update server returned HTTP {status_code}."));
        }

        let mut content_length = 0u32;
        let mut content_length_size = size_of::<u32>() as u32;
        let mut content_index = 0u32;
        let total = if WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER,
            null(),
            &mut content_length as *mut u32 as *mut c_void,
            &mut content_length_size,
            &mut content_index,
        ) != 0
            && content_length > 0
        {
            let total = u64::from(content_length);
            if total > max_size as u64 {
                return Err(format!(
                    "Update response is too large ({total} bytes; limit is {max_size})."
                ));
            }
            Some(total)
        } else {
            None
        };

        let mut body = Vec::new();
        if let Some(callback) = progress {
            callback(0, total);
        }
        loop {
            let mut available = 0u32;
            if WinHttpQueryDataAvailable(request.0, &mut available) == 0 {
                return Err("Cannot read update response.".to_owned());
            }
            if available == 0 {
                break;
            }
            if body.len().saturating_add(available as usize) > max_size {
                return Err(format!("Update response exceeds the {max_size}-byte limit."));
            }
            let start = body.len();
            body.resize(start + available as usize, 0);
            let mut read = 0u32;
            if WinHttpReadData(
                request.0,
                body[start..].as_mut_ptr() as *mut c_void,
                available,
                &mut read,
            ) == 0
            {
                return Err("Cannot read update response.".to_owned());
            }
            body.truncate(start + read as usize);
            if let Some(callback) = progress {
                callback(body.len() as u64, total);
            }
        }
        Ok(HttpPayload::Data(body))
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_release_versions() {
        assert!(is_newer("v0.3.1", "0.3.0"));
        assert!(is_newer("3.0.0", "2.9.9"));
        assert!(!is_newer("v2.2.0", "2.2.0"));
        assert!(!is_newer("v2.1.9", "2.2.0"));
        assert!(is_newer("v0.3.0", "0.3.0-alpha.6"));
        assert!(is_newer("v0.3.0-alpha.7", "0.3.0-alpha.6"));
        assert!(is_newer("v0.3.0-beta.1", "0.3.0-alpha.99"));
        assert!(!is_newer("v0.3.0-alpha.5", "0.3.0-alpha.6"));
        assert!(!is_newer("v0.3.0-alpha.6", "0.3.0"));
    }

    #[test]
    fn parses_github_asset_digest() {
        let digest = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(parse_github_sha256(Some(digest)).unwrap().len(), 64);
        assert!(parse_github_sha256(Some("md5:1234")).is_none());
    }

    #[test]
    fn sanitizes_temp_file_components() {
        assert_eq!(sanitize_component("My App/1"), "MyApp1");
        assert_eq!(sanitize_component(""), "app");
    }
}
