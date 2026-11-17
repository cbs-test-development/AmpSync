//! Browser link set-up on this computer: the native-messaging host registration, the unpacked
//! extension folder, the zip for people who cannot reach that folder, and starting the browser on
//! its extensions page.
//!
//! Everything here is idempotent and per-user (HKCU on Windows, `~/.config` on Linux).

use std::io;
use std::path::{Path, PathBuf};

use ampsync_core::Error;
use ampsync_core::auth::BrowserKind;
use ampsync_core::browser_link;

include!(concat!(env!("OUT_DIR"), "/extension_files.rs"));

/// The data directory the browser link uses (honours `AMPSYNC_DATA_DIR`, like core's purge).
pub fn data_dir() -> PathBuf {
    ampsync_core::appdirs::data_dir().unwrap_or_else(crate::paths::data_dir)
}

/// Where the unpacked extension is extracted.
pub fn extension_folder() -> PathBuf {
    browser_link::extension_dir(&data_dir())
}

// ---------------------------------------------------------------- extension files

/// Write `files` below `dir` when `manifest.json` is missing or differs from the embedded one.
/// The manifest goes last, so an interrupted extraction is noticed and redone next time.
/// Returns whether anything was written.
fn extract_to(dir: &Path, files: &[(&str, &[u8])]) -> io::Result<bool> {
    let Some((_, manifest)) = files.iter().find(|(n, _)| *n == "manifest.json") else {
        return Ok(false);
    };
    if std::fs::read(dir.join("manifest.json")).is_ok_and(|on_disk| on_disk == *manifest) {
        return Ok(false);
    }
    for (name, bytes) in files.iter().filter(|(n, _)| *n != "manifest.json") {
        write_file(&dir.join(name), bytes)?;
    }
    write_file(&dir.join("manifest.json"), manifest)?;
    Ok(true)
}

fn write_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)
}

/// Unpack the embedded extension into the extension folder if needed.
pub fn extract_extension() -> Result<bool, Error> {
    let dir = extension_folder();
    extract_to(&dir, EXTENSION_FILES).map_err(|e| Error::io(&dir, e))
}

// ---------------------------------------------------------------- zip

/// CRC-32 (IEEE) as the zip format wants it.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// A zip archive with every file stored uncompressed. The extension is a few tens of kilobytes,
/// so this avoids a compression dependency for a feature used once. Timestamps are fixed
/// (1980-01-01), which also makes the archive reproducible.
fn zip_bytes(files: &[(&str, &[u8])]) -> Vec<u8> {
    const DOS_DATE: u16 = (1 << 5) | 1; // 1980-01-01
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    for (name, data) in files {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let size = data.len() as u32;
        let name = name.as_bytes();
        // Local file header.
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&0u16.to_le_bytes()); // method: stored
        out.extend_from_slice(&0u16.to_le_bytes()); // time
        out.extend_from_slice(&DOS_DATE.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra length
        out.extend_from_slice(name);
        out.extend_from_slice(data);
        // Central directory entry.
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes()); // version made by
        central.extend_from_slice(&20u16.to_le_bytes()); // version needed
        central.extend_from_slice(&0u16.to_le_bytes()); // flags
        central.extend_from_slice(&0u16.to_le_bytes()); // method
        central.extend_from_slice(&0u16.to_le_bytes()); // time
        central.extend_from_slice(&DOS_DATE.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // extra length
        central.extend_from_slice(&0u16.to_le_bytes()); // comment length
        central.extend_from_slice(&0u16.to_le_bytes()); // disk number
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
        central.extend_from_slice(&0u32.to_le_bytes()); // external attributes
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name);
    }
    let cd_offset = out.len() as u32;
    let cd_size = central.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // this disk
    out.extend_from_slice(&0u16.to_le_bytes()); // disk with the directory
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // comment length
    out
}

/// Write the embedded extension as a zip to `dest`.
pub fn save_zip(dest: &Path) -> Result<(), Error> {
    if EXTENSION_FILES.is_empty() {
        return Err(Error::State("this build does not include the browser extension".into()));
    }
    std::fs::write(dest, zip_bytes(EXTENSION_FILES)).map_err(|e| Error::io(dest, e))
}

// ---------------------------------------------------------------- native host registration

/// Write the host manifest (only when its text changed) and point the browsers at it.
fn register_host(data_dir: &Path, exe: &Path) -> Result<(), Error> {
    let manifest = browser_link::host_manifest_path(data_dir);
    let text = browser_link::host_manifest_json(exe);
    if std::fs::read_to_string(&manifest).ok().as_deref() != Some(text.as_str()) {
        write_file(&manifest, text.as_bytes()).map_err(|e| Error::io(&manifest, e))?;
    }
    register_platform(&manifest, &text)
}

#[cfg(windows)]
fn register_platform(manifest: &Path, _text: &str) -> Result<(), Error> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let value = manifest.to_string_lossy().to_string();
    for target in browser_link::registry_targets() {
        let (key, _) = hkcu
            .create_subkey(&target.subkey)
            .map_err(|e| Error::State(format!("cannot register the browser link ({}): {e}", target.subkey)))?;
        // The default value (empty name) is the path of the manifest.
        key.set_value("", &value)
            .map_err(|e| Error::State(format!("cannot register the browser link ({}): {e}", target.subkey)))?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn register_platform(_manifest: &Path, text: &str) -> Result<(), Error> {
    let Some(config_home) = directories::BaseDirs::new().map(|d| d.config_dir().to_path_buf()) else {
        return Err(Error::State("no per-user config directory".into()));
    };
    for target in browser_link::linux_manifest_targets(&config_home) {
        if std::fs::read_to_string(&target.path).ok().as_deref() == Some(text) {
            continue;
        }
        write_file(&target.path, text.as_bytes()).map_err(|e| Error::io(&target.path, e))?;
    }
    Ok(())
}

#[cfg(not(any(windows, target_os = "linux")))]
fn register_platform(_manifest: &Path, _text: &str) -> Result<(), Error> {
    Ok(())
}

/// Remove the browser registrations (registry keys / manifest files). Returns how many existed
/// and the problems met; never stops at the first.
pub fn unregister_host() -> (usize, Vec<String>) {
    let mut removed = 0;
    let mut errors = Vec::new();
    #[cfg(windows)]
    {
        use winreg::RegKey;
        use winreg::enums::HKEY_CURRENT_USER;
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        for target in browser_link::registry_targets() {
            match hkcu.delete_subkey_all(&target.subkey) {
                Ok(()) => removed += 1,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => errors.push(format!("{}: {e}", target.subkey)),
            }
        }
    }
    #[cfg(target_os = "linux")]
    if let Some(config_home) = directories::BaseDirs::new().map(|d| d.config_dir().to_path_buf()) {
        for target in browser_link::linux_manifest_targets(&config_home) {
            match std::fs::remove_file(&target.path) {
                Ok(()) => removed += 1,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => errors.push(format!("{}: {e}", target.path.display())),
            }
        }
    }
    (removed, errors)
}

/// Unlink: drop the registrations and the host manifest file; the extension folder stays.
pub fn unlink() -> Result<(), Error> {
    let (_, errors) = unregister_host();
    let manifest = browser_link::host_manifest_path(&data_dir());
    if let Err(e) = std::fs::remove_file(&manifest)
        && e.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!("could not remove the browser link host manifest: {e}");
    }
    match errors.first() {
        Some(first) => Err(Error::State(format!("could not remove the browser link registration: {first}"))),
        None => Ok(()),
    }
}

/// Register the native host for this exe and unpack the extension; returns the extension folder.
/// Run at launch when the link is on, and by `link_prepare`.
pub fn prepare() -> Result<PathBuf, Error> {
    let exe = std::env::current_exe().map_err(|e| Error::State(format!("cannot find AmpSync's own path: {e}")))?;
    register_host(&data_dir(), &exe)?;
    extract_extension()?;
    Ok(extension_folder())
}

// ---------------------------------------------------------------- starting the browser

/// Start the browser on its extensions page. `false` when it cannot be started; the guide then
/// shows the address to copy instead.
pub fn open_extensions_page(kind: BrowserKind) -> bool {
    let (page, exe) = match kind {
        BrowserKind::Edge => ("edge://extensions", browser_exe(kind)),
        BrowserKind::Chrome => ("chrome://extensions", browser_exe(kind)),
    };
    let Some(exe) = exe else {
        return false;
    };
    std::process::Command::new(exe)
        .arg(page)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

#[cfg(windows)]
fn browser_exe(kind: BrowserKind) -> Option<PathBuf> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    let (file, rel) = match kind {
        BrowserKind::Edge => ("msedge.exe", r"Microsoft\Edge\Application"),
        BrowserKind::Chrome => ("chrome.exe", r"Google\Chrome\Application"),
    };
    // Registered install location first ("App Paths" is what Start > Run uses).
    let key = format!(r"Software\Microsoft\Windows\CurrentVersion\App Paths\{file}");
    for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        if let Ok(k) = RegKey::predef(hive).open_subkey(&key)
            && let Ok(v) = k.get_value::<String, _>("")
        {
            let path = PathBuf::from(v.trim().trim_matches('"'));
            if path.is_file() {
                return Some(path);
            }
        }
    }
    // Then the usual install folders.
    ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"]
        .iter()
        .filter_map(|var| std::env::var_os(var).map(|base| PathBuf::from(base).join(rel).join(file)))
        .find(|p| p.is_file())
}

#[cfg(not(windows))]
fn browser_exe(kind: BrowserKind) -> Option<PathBuf> {
    let names: &[&str] = match kind {
        BrowserKind::Edge => &["microsoft-edge", "microsoft-edge-stable"],
        BrowserKind::Chrome => &["google-chrome", "google-chrome-stable"],
    };
    let path = std::env::var_os("PATH")?;
    names
        .iter()
        .flat_map(|n| std::env::split_paths(&path).map(move |d| d.join(n)))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!("ampsync-linksetup-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn zip_has_the_expected_structure() {
        let files: &[(&str, &[u8])] = &[("manifest.json", b"{}"), ("icons/a.png", b"PNG")];
        let z = zip_bytes(files);
        // End-of-central-directory record: 22 bytes, entry count 2.
        let eocd = &z[z.len() - 22..];
        assert_eq!(&eocd[..4], &0x0605_4b50u32.to_le_bytes());
        assert_eq!(u16::from_le_bytes([eocd[10], eocd[11]]), 2);
        let cd_offset = u32::from_le_bytes([eocd[16], eocd[17], eocd[18], eocd[19]]) as usize;
        assert_eq!(&z[cd_offset..cd_offset + 4], &0x0201_4b50u32.to_le_bytes());
        // First local header is at 0 and holds the name and data.
        assert_eq!(&z[..4], &0x0403_4b50u32.to_le_bytes());
        assert_eq!(&z[30..43], b"manifest.json");
        assert_eq!(&z[43..45], b"{}");
        // CRC of "{}" is stored in the local header.
        assert_eq!(u32::from_le_bytes([z[14], z[15], z[16], z[17]]), crc32(b"{}"));
    }

    #[test]
    fn extraction_writes_when_missing_and_when_the_manifest_differs() {
        let dir = temp_dir("extract");
        let v1: &[(&str, &[u8])] = &[("manifest.json", b"v1"), ("sub/a.js", b"a1")];
        assert!(extract_to(&dir, v1).unwrap());
        assert_eq!(std::fs::read(dir.join("sub").join("a.js")).unwrap(), b"a1");
        // Same manifest: nothing to do, even if a file was changed by hand.
        std::fs::write(dir.join("sub").join("a.js"), b"edited").unwrap();
        assert!(!extract_to(&dir, v1).unwrap());
        // New manifest: everything is written again.
        let v2: &[(&str, &[u8])] = &[("manifest.json", b"v2"), ("sub/a.js", b"a2")];
        assert!(extract_to(&dir, v2).unwrap());
        assert_eq!(std::fs::read(dir.join("manifest.json")).unwrap(), b"v2");
        assert_eq!(std::fs::read(dir.join("sub").join("a.js")).unwrap(), b"a2");
    }

    #[test]
    fn nothing_is_extracted_without_a_manifest() {
        let dir = temp_dir("nomanifest");
        assert!(!extract_to(&dir, &[("a.js", b"x")]).unwrap());
        assert!(!dir.join("a.js").exists());
    }

    #[test]
    fn host_manifest_is_written_once_per_change() {
        let dir = temp_dir("host");
        let manifest = browser_link::host_manifest_path(&dir);
        let text = browser_link::host_manifest_json(Path::new("/x/ampsync"));
        write_file(&manifest, text.as_bytes()).unwrap();
        assert_eq!(std::fs::read_to_string(&manifest).unwrap(), text);
    }

    #[test]
    fn the_embedded_extension_has_a_manifest_and_no_tests() {
        // The extension folder ships with the repository, so a build without it is a mistake.
        assert!(EXTENSION_FILES.iter().any(|(n, _)| *n == "manifest.json"));
        assert!(EXTENSION_FILES.iter().all(|(n, _)| !n.starts_with("test/") && *n != "README.md"));
    }
}
