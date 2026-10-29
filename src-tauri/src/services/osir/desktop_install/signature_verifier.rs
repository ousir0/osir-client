//! Fixed trust values copied from vendored codesign.rs and sys.rs. No overrides.
use super::{Error, Result, Verifier};
use std::path::Path;
pub const TEAM_ID: &str = "2DC432GLL2";
pub const BUNDLE_ID: &str = "com.openai.codex";
pub struct SystemVerifier;

fn exact_field(text: &str, prefix: &str, expected: &str) -> bool {
    let values: Vec<_> = text
        .lines()
        .filter_map(|l| l.strip_prefix(prefix))
        .collect();
    values.len() == 1 && values[0].trim() == expected
}
#[cfg(target_os = "macos")]
fn run(program: &str, args: &[&str], path: &Path) -> Result<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    const LIMIT: u64 = 64 * 1024;
    let mut child = Command::new(program)
        .args(args)
        .arg(path)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| Error::Verification)?;
    let stdout = child.stdout.take().ok_or(Error::Verification)?;
    let stderr = child.stderr.take().ok_or(Error::Verification)?;
    let read = |stream: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut data = Vec::new();
            let result = stream.take(LIMIT + 1).read_to_end(&mut data);
            if result.is_err() || data.len() as u64 > LIMIT {
                Err(Error::Verification)
            } else {
                Ok(data)
            }
        })
    };
    let out = read(Box::new(stdout));
    let err = read(Box::new(stderr));
    let start = Instant::now();
    let success = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s.success(),
            Ok(None) => {}
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
        if start.elapsed() > Duration::from_secs(120) {
            let _ = child.kill();
            let _ = child.wait();
            break false;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let out = out.join().map_err(|_| Error::Verification)??;
    let err = err.join().map_err(|_| Error::Verification)??;
    if !success {
        return Err(Error::Verification);
    }
    let mut bytes = out;
    bytes.extend(err);
    String::from_utf8(bytes).map_err(|_| Error::Verification)
}
impl Verifier for SystemVerifier {
    fn verify(&self, path: &Path, build: u64) -> Result<()> {
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (path, build);
            Err(Error::Verification)
        }
        #[cfg(target_os = "macos")]
        {
            if build == 0 || !super::clean_absolute(path) {
                return Err(Error::Verification);
            }
            // Reject symlink components before external tools resolve the path.
            let mut current = std::path::PathBuf::new();
            for c in path.components() {
                current.push(c.as_os_str());
                let m = std::fs::symlink_metadata(&current).map_err(|_| Error::Verification)?;
                if !m.is_dir() || m.file_type().is_symlink() {
                    return Err(Error::Verification);
                }
            }
            let before = std::fs::metadata(path).map_err(|_| Error::Verification)?;
            run(
                "/usr/bin/codesign",
                &["--verify", "--deep", "--strict"],
                path,
            )?;
            let signature = run("/usr/bin/codesign", &["-dv", "--verbose=2"], path)?;
            if !exact_field(&signature, "TeamIdentifier=", TEAM_ID)
                || !exact_field(&signature, "Identifier=", BUNDLE_ID)
            {
                return Err(Error::Verification);
            }
            let contents = path.join("Contents");
            let plist = contents.join("Info.plist");
            for p in [&contents, &plist] {
                let m = std::fs::symlink_metadata(p).map_err(|_| Error::Verification)?;
                if m.file_type().is_symlink() {
                    return Err(Error::Verification);
                }
            }
            if run(
                "/usr/libexec/PlistBuddy",
                &["-c", "Print :CFBundleIdentifier"],
                &plist,
            )?
            .trim()
                != BUNDLE_ID
            {
                return Err(Error::Verification);
            }
            let version = run(
                "/usr/libexec/PlistBuddy",
                &["-c", "Print :CFBundleVersion"],
                &plist,
            )?;
            if version.trim().parse::<u64>().ok() != Some(build) {
                return Err(Error::Verification);
            }
            run("/usr/sbin/spctl", &["--assess", "--type", "execute"], path)?;
            run(
                "/usr/bin/codesign",
                &["--verify", "--deep", "--strict"],
                path,
            )?;
            use std::os::unix::fs::MetadataExt;
            let after = std::fs::metadata(path).map_err(|_| Error::Verification)?;
            if (before.dev(), before.ino()) != (after.dev(), after.ino()) {
                return Err(Error::Verification);
            }
            Ok(())
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_identity_rejects_duplicates_and_wrong_product() {
        assert!(exact_field(
            "TeamIdentifier=2DC432GLL2",
            "TeamIdentifier=",
            TEAM_ID
        ));
        assert!(!exact_field(
            "Identifier=com.openai.chat",
            "Identifier=",
            BUNDLE_ID
        ));
        assert!(!exact_field(
            "Identifier=com.openai.codex
Identifier=com.openai.chat",
            "Identifier=",
            BUNDLE_ID
        ));
    }
}
