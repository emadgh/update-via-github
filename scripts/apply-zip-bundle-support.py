from pathlib import Path

path = Path("src/lib.rs")
text = path.read_text(encoding="utf-8")


def replace_once(old: str, new: str) -> None:
    global text
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"expected one match, found {count}: {old[:80]!r}")
    text = text.replace(old, new, 1)


replace_once(
'''    if bytes.len() < config.min_executable_size || !bytes.starts_with(b"MZ") {
        return Err("Downloaded update is not a valid Windows executable.".to_owned());
    }
''',
'''    validate_downloaded_asset(config, &bytes)?;
''')

replace_once(
'''    let safe_version = sanitize_component(&info.version);
    let safe_app = sanitize_component(&config.app_name);
    let path = std::env::temp_dir().join(format!(
        "{safe_app}-update-{}-{safe_version}.exe",
        std::process::id()
    ));
''',
'''    let safe_version = sanitize_component(&info.version);
    let safe_app = sanitize_component(&config.app_name);
    let extension = if is_zip_bundle(config) { "zip" } else { "exe" };
    let path = std::env::temp_dir().join(format!(
        "{safe_app}-update-{}-{safe_version}.{extension}",
        std::process::id()
    ));
''')

replace_once(
'''    fs::write(&script, updater_script())
        .map_err(|err| format!("Cannot create updater script: {err}"))?;
    launch_updater(&script, source, &current_exe)
}

fn validate_config(config: &UpdateConfig) -> Result<(), String> {
''',
'''    let script_body = if is_zip_bundle(config) {
        bundle_updater_script()
    } else {
        updater_script()
    };
    fs::write(&script, script_body)
        .map_err(|err| format!("Cannot create updater script: {err}"))?;
    launch_updater(&script, source, &current_exe)
}

fn is_zip_bundle(config: &UpdateConfig) -> bool {
    config.asset_name.to_ascii_lowercase().ends_with(".zip")
}

fn validate_downloaded_asset(config: &UpdateConfig, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() < config.min_executable_size {
        return Err(format!(
            "Downloaded update is too small ({} bytes; minimum is {}).",
            bytes.len(), config.min_executable_size
        ));
    }
    if is_zip_bundle(config) {
        let zip_magic = bytes.starts_with(b"PK\\x03\\x04")
            || bytes.starts_with(b"PK\\x05\\x06")
            || bytes.starts_with(b"PK\\x07\\x08");
        if !zip_magic {
            return Err("Downloaded update is not a valid ZIP bundle.".to_owned());
        }
    } else if !bytes.starts_with(b"MZ") {
        return Err("Downloaded update is not a valid Windows executable.".to_owned());
    }
    Ok(())
}

fn validate_config(config: &UpdateConfig) -> Result<(), String> {
''')

replace_once(
'''fn updater_script() -> &'static str {
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
''',
'''fn updater_script() -> &'static str {
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

fn bundle_updater_script() -> &'static str {
    r#"param([int]$TargetPid, [string]$Source, [string]$Destination)
$Stage = Join-Path ([IO.Path]::GetTempPath()) ("update-via-github-bundle-{0}-{1}" -f $TargetPid, [Guid]::NewGuid().ToString("N"))
try {
    New-Item -ItemType Directory -Force -Path $Stage | Out-Null
    Expand-Archive -LiteralPath $Source -DestinationPath $Stage -Force -ErrorAction Stop
    $ExeName = [IO.Path]::GetFileName($Destination)
    $StagedExe = Join-Path $Stage $ExeName
    if (-not (Test-Path -LiteralPath $StagedExe -PathType Leaf)) {
        throw "Update ZIP does not contain $ExeName at its root."
    }
    Wait-Process -Id $TargetPid -ErrorAction SilentlyContinue
    $DestinationDir = Split-Path -Parent $Destination
    for ($attempt = 0; $attempt -lt 30; $attempt++) {
        try {
            Get-ChildItem -LiteralPath $Stage -Force | Where-Object { $_.Name -ne $ExeName } | ForEach-Object {
                Copy-Item -LiteralPath $_.FullName -Destination $DestinationDir -Recurse -Force -ErrorAction Stop
            }
            Copy-Item -LiteralPath $StagedExe -Destination $Destination -Force -ErrorAction Stop
            Start-Process -FilePath $Destination
            Remove-Item -LiteralPath $Source -Force -ErrorAction SilentlyContinue
            Remove-Item -LiteralPath $Stage -Recurse -Force -ErrorAction SilentlyContinue
            Remove-Item -LiteralPath $PSCommandPath -Force -ErrorAction SilentlyContinue
            exit 0
        } catch {
            Start-Sleep -Milliseconds 500
        }
    }
} catch {
    Remove-Item -LiteralPath $Stage -Recurse -Force -ErrorAction SilentlyContinue
}
"#
}

fn split_https_url(url: &str) -> Option<(&str, &str)> {
''')

replace_once(
'''    #[test]
    fn sanitizes_temp_file_components() {
        assert_eq!(sanitize_component("My App/1"), "MyApp1");
        assert_eq!(sanitize_component(""), "app");
    }
}
''',
'''    #[test]
    fn sanitizes_temp_file_components() {
        assert_eq!(sanitize_component("My App/1"), "MyApp1");
        assert_eq!(sanitize_component(""), "app");
    }

    #[test]
    fn infers_zip_bundle_from_asset_name() {
        let zip = UpdateConfig::new("owner/repo", "app-win64.zip", "1.0.0");
        let exe = UpdateConfig::new("owner/repo", "app.exe", "1.0.0");
        assert!(is_zip_bundle(&zip));
        assert!(!is_zip_bundle(&exe));
    }

    #[test]
    fn validates_executable_and_zip_payload_magic() {
        let exe = UpdateConfig::new("owner/repo", "app.exe", "1.0.0")
            .with_min_executable_size(2);
        let zip = UpdateConfig::new("owner/repo", "app-win64.zip", "1.0.0")
            .with_min_executable_size(4);
        assert!(validate_downloaded_asset(&exe, b"MZ").is_ok());
        assert!(validate_downloaded_asset(&exe, b"PK").is_err());
        assert!(validate_downloaded_asset(&zip, b"PK\\x03\\x04payload").is_ok());
        assert!(validate_downloaded_asset(&zip, b"MZpayload").is_err());
    }

    #[test]
    fn bundle_script_copies_sidecars_before_replacing_executable() {
        let script = bundle_updater_script();
        let sidecars = script.find("Get-ChildItem").unwrap();
        let executable = script.find("Copy-Item -LiteralPath $StagedExe").unwrap();
        assert!(sidecars < executable);
        assert!(script.contains("Expand-Archive"));
    }
}
''')

path.write_text(text, encoding="utf-8")
