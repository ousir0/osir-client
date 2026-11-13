//! NSIS identity from electron-builder's UUIDv5(appId, builder namespace), not
//! display-name substring search, an upstream product, or a bundled CLI runner.
pub(super) const APP_ID: &str = "com.osir.anywhere.desktop";
pub(super) const NSIS_GUID: &str = "d78e534b-2cbd-5c14-9c2e-5fdd65be38df";
pub(super) const DISCOVERY: &str = r#"
function Assert-NoLinks([string]$path) {
  $item = Get-Item -LiteralPath $path -Force -ErrorAction Stop
  while ($null -ne $item) {
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'reparse installation' }
    if ($item -is [IO.FileInfo]) { $item = $item.Directory } else { $item = $item.Parent }
  }
}
function Read-AsarPackage([string]$path) {
  Assert-NoLinks $path
  $stream = [IO.File]::Open($path, 'Open', 'Read', 'Read')
  $reader = [IO.BinaryReader]::new($stream)
  try {
    if ($reader.ReadUInt32() -ne 4) { throw 'asar prefix' }
    $headerSize = $reader.ReadUInt32()
    $null = $reader.ReadUInt32()
    $jsonSize = $reader.ReadUInt32()
    if ($headerSize -lt 8 -or $headerSize -gt 8388608 -or $jsonSize -gt ($headerSize - 8)) { throw 'asar header' }
    $raw = $reader.ReadBytes($jsonSize)
    if ($raw.Length -ne $jsonSize) { throw 'asar truncated' }
    $header = [Text.Encoding]::UTF8.GetString($raw) | ConvertFrom-Json -ErrorAction Stop
    $entry = $header.files.'package.json'
    if ($null -eq $entry -or $entry.unpacked -or $entry.link -or $entry.offset -notmatch '^\d+$' -or $entry.size -lt 2 -or $entry.size -gt 262144) { throw 'asar package' }
    $offset = [long]8 + [long]$headerSize + [long]$entry.offset
    if ($offset -lt 0 -or $offset + [long]$entry.size -gt $stream.Length) { throw 'asar bounds' }
    $null = $stream.Seek($offset, 'Begin')
    $raw = $reader.ReadBytes([int]$entry.size)
    if ($raw.Length -ne $entry.size) { throw 'asar package truncated' }
    return ([Text.Encoding]::UTF8.GetString($raw) | ConvertFrom-Json -ErrorAction Stop)
  } finally { $reader.Dispose(); $stream.Dispose() }
}
$guid = 'd78e534b-2cbd-5c14-9c2e-5fdd65be38df'
$roots = @{}
foreach ($hive in @([Microsoft.Win32.RegistryHive]::CurrentUser, [Microsoft.Win32.RegistryHive]::LocalMachine)) {
  foreach ($view in @([Microsoft.Win32.RegistryView]::Registry64, [Microsoft.Win32.RegistryView]::Registry32)) {
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey($hive, $view)
    try {
      $install = $base.OpenSubKey("Software\$guid")
      $uninstall = $base.OpenSubKey("Software\Microsoft\Windows\CurrentVersion\Uninstall\$guid")
      try {
        if ($null -ne $install -or $null -ne $uninstall) {
          if ($null -eq $install -or $null -eq $uninstall -or $uninstall.GetValue('DisplayName') -cne 'OSIR Anywhere') { throw 'incomplete NSIS identity' }
          $root = [string]$install.GetValue('InstallLocation')
          if (-not [IO.Path]::IsPathRooted($root) -or $root -notmatch '^[A-Za-z]:\\') { throw 'NSIS path' }
          $root = [IO.Path]::GetFullPath($root).TrimEnd('\')
          $version = [string]$uninstall.GetValue('DisplayVersion')
          if ($roots.ContainsKey($root) -and $roots[$root] -cne $version) { throw 'conflicting NSIS records' }
          $roots[$root] = $version
        }
      } finally { if ($install) { $install.Dispose() }; if ($uninstall) { $uninstall.Dispose() } }
    } finally { $base.Dispose() }
  }
}
# electron-builder assisted NSIS defaults; custom paths are taken only from the
# exact app GUID above. Do not execute UninstallString or search PATH/runners.
foreach ($parent in @((Join-Path $env:LOCALAPPDATA 'Programs'), $env:ProgramFiles, ${env:ProgramFiles(x86)})) {
  if (-not $parent) { continue }
  $root = Join-Path $parent 'OSIR Anywhere'
  if ((Test-Path -LiteralPath $root -PathType Container) -and -not $roots.ContainsKey($root)) { $roots[$root] = $null }
}
$result = @()
foreach ($root in $roots.Keys) {
  Assert-NoLinks $root
  $exe = Join-Path $root 'OSIR Anywhere.exe'
  Assert-NoLinks $exe
  $v = (Get-Item -LiteralPath $exe -ErrorAction Stop).VersionInfo
  if ($v.ProductName -cne 'OSIR Anywhere') { throw 'wrong PE product' }
  $package = Read-AsarPackage (Join-Path $root 'resources\app.asar')
  if ($package.name -cne 'osir-anywhere-desktop-workbench' -or $package.productName -cne 'OSIR Anywhere' -or $package.main -cne 'dist/electron/main.js') { throw 'wrong app package' }
  $version = [string]$package.version
  if ($version -notmatch '^\d+\.\d+\.\d+$' -or [string]$v.ProductVersion -cne $version -or ($roots[$root] -and $roots[$root] -cne $version)) { throw 'version mismatch' }
  $result += @{
    root = [string]$root; executable = [string]$exe; relative = 'OSIR Anywhere.exe'
    version = $version; family = $null; appId = $null; protocol = $false
    package = 'com.osir.anywhere.desktop'; publisher = 'OSIR Anywhere'
  }
}
ConvertTo-Json -InputObject @($result) -Depth 5 -Compress
"#;

pub(super) fn validate(entry: &super::Installation) -> Result<(), String> {
    let parts: Vec<_> = entry.version.split('.').collect();
    if parts.len() != 3
        || parts.iter().any(|p| {
            p.is_empty() || !p.bytes().all(|c| c.is_ascii_digit()) || p.parse::<u64>().is_err()
        })
        || entry.package.as_deref() != Some(APP_ID)
        || entry.relative != "OSIR Anywhere.exe"
        || entry.publisher != "OSIR Anywhere"
        || entry.family.is_some()
        || entry.app_id.is_some()
        || entry.protocol
    {
        return Err("无法确认 OSIR Anywhere 的安装身份与版本，请修复安装".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed() -> super::super::Installation {
        // Literal NSIS fixture, independent of policy constants. This is not
        // evidence that a real Windows installation was detected or launched.
        serde_json::from_value(serde_json::json!({
            "root":r"C:\Users\Fixture\AppData\Local\Programs\OSIR Anywhere",
            "executable":r"C:\Users\Fixture\AppData\Local\Programs\OSIR Anywhere\OSIR Anywhere.exe",
            "relative":"OSIR Anywhere.exe","version":"2.1.0","family":null,
            "appId":null,"protocol":false,"package":"com.osir.anywhere.desktop",
            "publisher":"OSIR Anywhere"
        }))
        .unwrap()
    }

    #[test]
    fn anywhere_windows_accepts_nsis_identity_and_rejects_other_products() {
        installed().validate("osir-anywhere").unwrap();
        for package in ["OpenAI.Codex", "com.anywhere.desktop", "com.osir.client"] {
            let mut entry = installed();
            entry.package = Some(package.into());
            assert!(entry.validate("osir-anywhere").is_err());
        }
        let mut entry = installed();
        entry.relative = "runner.exe".into();
        entry.executable = format!(r"{}\runner.exe", entry.root);
        assert!(entry.validate("osir-anywhere").is_err());
        let mut entry = installed();
        entry.version = "2.1.0-beta".into();
        assert!(entry.validate("osir-anywhere").is_err());
        let mut entry = installed();
        entry.executable = r"C:\Other\OSIR Anywhere.exe".into();
        assert!(entry.validate("osir-anywhere").is_err());
    }

    #[test]
    fn anywhere_windows_rejects_cli_or_upstream_metadata_even_with_expected_filename() {
        let mut entry = installed();
        entry.publisher = "Agents Anywhere".into();
        assert!(entry.validate("osir-anywhere").is_err());
        let mut entry = installed();
        entry.app_id = Some("App".into());
        assert!(entry.validate("osir-anywhere").is_err());
        assert_eq!(NSIS_GUID, "d78e534b-2cbd-5c14-9c2e-5fdd65be38df");
    }
}
