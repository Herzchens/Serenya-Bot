use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{error, info, warn};

fn dependency_dir_from(cwd: &Path) -> PathBuf {
    cwd.join("bin")
}

pub fn dependency_dir() -> PathBuf {
    env::current_dir()
        .map(|cwd| dependency_dir_from(&cwd))
        .unwrap_or_else(|_| PathBuf::from("bin"))
}

fn path_with_dependency_dir(bin_dir: &Path, current_path: Option<&OsStr>) -> Option<OsString> {
    let mut paths = vec![bin_dir.to_path_buf()];
    if let Some(current_path) = current_path {
        paths.extend(env::split_paths(current_path).filter(|path| path != bin_dir));
    }
    env::join_paths(paths).ok()
}

pub fn configure_dependency_path() {
    let bin_dir = dependency_dir();
    if let Some(path) = path_with_dependency_dir(&bin_dir, env::var_os("PATH").as_deref()) {
        unsafe { env::set_var("PATH", path) };
    }
}

pub async fn ensure_dependencies() {
    let os = env::consts::OS;
    let bin_dir = dependency_dir();
    let ffmpeg_filename = if os == "windows" {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    let ytdlp_filename = if os == "windows" {
        "yt-dlp.exe"
    } else {
        "yt-dlp"
    };

    let ffmpeg_exists = check_command("ffmpeg") || bin_dir.join(ffmpeg_filename).is_file();
    let ytdlp_exists = check_command("yt-dlp") || bin_dir.join(ytdlp_filename).is_file();

    if ffmpeg_exists && ytdlp_exists {
        info!("All dependencies (ffmpeg, yt-dlp) are present.");
        return;
    }

    if let Err(error) = tokio::fs::create_dir_all(&bin_dir).await {
        error!(path = %bin_dir.display(), %error, "Failed to create dependency directory");
        return;
    }

    warn!(path = %bin_dir.display(), "Missing dependencies; attempting auto-install");

    if !ytdlp_exists {
        info!("Installing yt-dlp...");
        if let Err(error) = install_ytdlp(os, &bin_dir).await {
            error!(%error, "Failed to install yt-dlp; please install it manually");
        } else {
            info!("yt-dlp installed successfully");
        }
    }

    if !ffmpeg_exists {
        info!("Installing ffmpeg...");
        if let Err(error) = install_ffmpeg(os, &bin_dir).await {
            error!(%error, "Failed to install ffmpeg; please install it manually");
        } else {
            info!("ffmpeg installed successfully");
        }
    }
}

fn check_command(cmd: &str) -> bool {
    Command::new(cmd).arg("-version").output().is_ok()
}

async fn download_file(
    http_client: &reqwest::Client,
    url: &str,
    destination: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = http_client.get(url).send().await?.error_for_status()?;
    let bytes = response.bytes().await?;
    let temporary = destination.with_extension("download");
    tokio::fs::write(&temporary, &bytes).await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = tokio::fs::metadata(&temporary).await?.permissions();
        permissions.set_mode(0o755);
        tokio::fs::set_permissions(&temporary, permissions).await?;
    }

    if let Err(error) = tokio::fs::rename(&temporary, destination).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(error.into());
    }
    Ok(())
}

async fn install_ytdlp(os: &str, bin_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let (url, filename) = if os == "windows" {
        (
            "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe",
            "yt-dlp.exe",
        )
    } else {
        (
            "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp",
            "yt-dlp",
        )
    };
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(60))
        .build()?;
    download_file(&client, url, &bin_dir.join(filename)).await
}

async fn install_ffmpeg(os: &str, bin_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if os == "windows" {
        let script = r#"
            $ErrorActionPreference = 'Stop'
            $dest = $args[0]
            $archive = Join-Path $dest 'ffmpeg.zip'
            $extract = Join-Path $dest 'ffmpeg_extracted'
            Invoke-WebRequest -Uri 'https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip' -OutFile $archive
            Expand-Archive -Path $archive -DestinationPath $extract -Force
            Move-Item -Path (Join-Path $extract 'ffmpeg-master-latest-win64-gpl\bin\ffmpeg.exe') -Destination (Join-Path $dest 'ffmpeg.exe') -Force
            Remove-Item $archive
            Remove-Item $extract -Recurse -Force
        "#;
        let status = Command::new("powershell")
            .arg("-Command")
            .arg(script)
            .arg(bin_dir)
            .status()?;
        if !status.success() {
            return Err("PowerShell ffmpeg install script failed".into());
        }
    } else {
        let script = r#"
            set -eu
            dest="$1"
            archive="$dest/ffmpeg.tar.xz"
            wget -qO "$archive" "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-amd64-static.tar.xz"
            tar -xf "$archive" --strip-components=1 -C "$dest" "*/ffmpeg"
            rm -f "$archive"
            chmod +x "$dest/ffmpeg"
        "#;
        let status = Command::new("sh")
            .arg("-c")
            .arg(script)
            .arg("serenya-ffmpeg-install")
            .arg(bin_dir)
            .status()?;
        if !status.success() {
            return Err("ffmpeg install script failed; consider installing ffmpeg from your package manager".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_path_is_prepended_even_before_directory_exists() {
        let cwd = Path::new("/tmp/serenya-fresh-install");
        let bin = dependency_dir_from(cwd);
        let existing = env::join_paths([PathBuf::from("/usr/bin"), PathBuf::from("/bin")])
            .expect("build test PATH");
        let updated = path_with_dependency_dir(&bin, Some(&existing)).expect("join PATH");
        let paths: Vec<_> = env::split_paths(&updated).collect();
        assert_eq!(paths.first(), Some(&bin));
        assert_eq!(paths.iter().filter(|path| *path == &bin).count(), 1);
    }

    async fn spawn_http_response(status: &str, body: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind installer test server");
        let address = listener.local_addr().expect("installer test address");
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept installer request");
            let mut request = [0_u8; 2048];
            let _ = socket
                .read(&mut request)
                .await
                .expect("read installer request");
            socket
                .write_all(response.as_bytes())
                .await
                .expect("write installer response");
        });
        format!("http://{address}/asset")
    }

    #[tokio::test]
    async fn http_error_body_is_not_installed_as_executable() {
        let url = spawn_http_response("404 Not Found", "not an executable").await;
        let unique = format!(
            "serenya-installer-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        );
        let dir = env::temp_dir().join(unique);
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create test dir");
        let destination = dir.join("yt-dlp");
        let result = download_file(&reqwest::Client::new(), &url, &destination).await;
        assert!(result.is_err());
        assert!(!destination.exists());
        let _ = tokio::fs::remove_dir_all(dir).await;
    }
}
