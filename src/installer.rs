use std::env;
use std::path::Path;
use std::process::Command;
use tracing::{error, info, warn};

pub async fn ensure_dependencies() {
    let os = env::consts::OS;

    let ffmpeg_exists =
        check_command("ffmpeg") || Path::new("ffmpeg.exe").exists() || Path::new("ffmpeg").exists();
    let ytdlp_exists =
        check_command("yt-dlp") || Path::new("yt-dlp.exe").exists() || Path::new("yt-dlp").exists();

    if ffmpeg_exists && ytdlp_exists {
        info!("All dependencies (ffmpeg, yt-dlp) are present.");
        return;
    }

    warn!("Missing dependencies! Attempting to auto-install...");

    if !ytdlp_exists {
        info!("Installing yt-dlp...");
        if let Err(e) = install_ytdlp(os).await {
            error!("Failed to install yt-dlp: {e}. Please install it manually.");
        } else {
            info!("yt-dlp installed successfully!");
        }
    }

    if !ffmpeg_exists {
        info!("Installing ffmpeg...");
        if let Err(e) = install_ffmpeg(os).await {
            error!("Failed to install ffmpeg: {e}. Please install it manually.");
        } else {
            info!("ffmpeg installed successfully!");
        }
    }
}

fn check_command(cmd: &str) -> bool {
    Command::new(cmd).arg("-version").output().is_ok()
}

async fn install_ytdlp(os: &str) -> Result<(), Box<dyn std::error::Error>> {
    let url = if os == "windows" {
        "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe"
    } else {
        "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp"
    };

    let filename = if os == "windows" {
        "yt-dlp.exe"
    } else {
        "yt-dlp"
    };

    let response = reqwest::get(url).await?.bytes().await?;
    tokio::fs::write(filename, &response).await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = tokio::fs::metadata(filename).await?.permissions();
        perms.set_mode(0o755);
        tokio::fs::set_permissions(filename, perms).await?;
    }

    Ok(())
}

async fn install_ffmpeg(os: &str) -> Result<(), Box<dyn std::error::Error>> {
    if os == "windows" {
        let script = r#"
            $ErrorActionPreference = 'Stop'
            Write-Host "Downloading FFmpeg..."
            Invoke-WebRequest -Uri "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip" -OutFile "ffmpeg.zip"
            Write-Host "Extracting FFmpeg..."
            Expand-Archive -Path "ffmpeg.zip" -DestinationPath "ffmpeg_extracted" -Force
            Move-Item -Path "ffmpeg_extracted\ffmpeg-master-latest-win64-gpl\bin\ffmpeg.exe" -Destination ".\ffmpeg.exe" -Force
            Write-Host "Cleaning up..."
            Remove-Item "ffmpeg.zip"
            Remove-Item "ffmpeg_extracted" -Recurse -Force
        "#;

        let status = Command::new("powershell")
            .arg("-Command")
            .arg(script)
            .status()?;

        if !status.success() {
            return Err("PowerShell script failed".into());
        }
    } else {
        // Linux (Assuming AMD64)
        let script = r#"
            echo "Downloading FFmpeg..."
            wget -qO ffmpeg.tar.xz "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-amd64-static.tar.xz"
            echo "Extracting FFmpeg..."
            tar -xf ffmpeg.tar.xz --strip-components=1 -C . "*/ffmpeg"
            rm ffmpeg.tar.xz
            chmod +x ffmpeg
        "#;

        let status = Command::new("sh").arg("-c").arg(script).status()?;

        if !status.success() {
            return Err("Shell script failed. Consider running: sudo apt install ffmpeg".into());
        }
    }

    Ok(())
}
