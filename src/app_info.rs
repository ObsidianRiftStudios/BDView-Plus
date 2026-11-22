//! Program identity, repository address and the update check (std only; the check runs `curl`).

/// Home of the TuxInfo repository, e.g. "https://github.com/OWNER/tuxinfo". Empty until it is known.
pub const REPO_URL: &str = "https://github.com/ObsidianRiftStudios/TuxInfo";

/// Developer name and support address shown in the About window.
pub const STUDIO: &str = "Obsidian Rift Studios";
pub const SUPPORT_EMAIL: &str = "support@obsidianrift.net";

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[allow(dead_code)]
pub enum Update {
    Current,
    Newer(String),
}

/// Ask the repository's latest release and compare it with this version.
#[allow(dead_code)]
pub fn check() -> Result<Update, String> {
    let slug = REPO_URL
        .trim_end_matches('/')
        .strip_prefix("https://github.com/")
        .ok_or("The repository address is not set yet.")?;
    let url = format!("https://api.github.com/repos/{slug}/releases/latest");
    let out = std::process::Command::new("curl")
        .args([
            "-sS",
            "-L",
            "-m",
            "15",
            "-w",
            "\n%{http_code}",
            "-H",
            "Accept: application/vnd.github+json",
            "-A",
            "TuxInfo",
            &url,
        ])
        .output()
        .map_err(|_| "Could not run curl (install it to check for updates).".to_string())?;
    let body = String::from_utf8_lossy(&out.stdout).to_string();
    let (json, code) = body.rsplit_once('\n').unwrap_or((&body, ""));
    match code.trim() {
        "200" => {}
        "404" => return Err("No release has been published yet.".into()),
        "" => {
            return Err(format!(
                "Could not reach the server: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
        c => return Err(format!("The server answered with HTTP {c}.")),
    }
    let tag = json
        .split("\"tag_name\"")
        .nth(1)
        .and_then(|s| s.split('"').nth(1))
        .ok_or("The answer had no version number.")?
        .to_string();
    if newer(&tag, version()) {
        Ok(Update::Newer(tag))
    } else {
        Ok(Update::Current)
    }
}

#[allow(dead_code)]
fn newer(tag: &str, cur: &str) -> bool {
    let nums = |s: &str| -> Vec<u64> {
        s.trim_start_matches(['v', 'V'])
            .split(['.', '-', '+'])
            .map_while(|p| p.parse().ok())
            .collect()
    };
    nums(tag) > nums(cur)
}
