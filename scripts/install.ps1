$ErrorActionPreference = "Stop"

$Repo = "Mai0313/VibeCodingTracker"
$BinaryName = "vibe_coding_tracker"

[System.Net.ServicePointManager]::SecurityProtocol = [System.Net.SecurityProtocolType]::Tls12
[System.Net.ServicePointManager]::ServerCertificateValidationCallback = { $true }

function Get-Architecture {
    switch ($env:PROCESSOR_ARCHITECTURE) {
        "AMD64" { "x64"; return }
        "ARM64" { "arm64"; return }
        default {
            Write-Error "Unsupported architecture: $($env:PROCESSOR_ARCHITECTURE)"
            exit 1
        }
    }
}

function Get-LatestVersion {
    $response = Invoke-WebRequest -Uri "https://api.github.com/repos/$Repo/releases/latest" -UseBasicParsing
    $tag = ($response.Content | ConvertFrom-Json).tag_name
    if (-not $tag) {
        Write-Error "Failed to determine latest release."
        exit 1
    }
    return $tag
}

function Get-InstallDirectory {
    return (Join-Path $env:LOCALAPPDATA "Programs\VibeCodingTracker")
}

function Install-Binary {
    param(
        [string]$Version,
        [string]$Arch
    )

    $filename = "$BinaryName-$Version-windows-$Arch.zip"
    $url = "https://github.com/$Repo/releases/download/$Version/$filename"

    $tempDir = Join-Path ([System.IO.Path]::GetTempPath()) ([System.Guid]::NewGuid())
    New-Item -ItemType Directory -Path $tempDir | Out-Null

    try {
        $archive = Join-Path $tempDir $filename
        Invoke-WebRequest -Uri $url -OutFile $archive -UseBasicParsing

        Expand-Archive -Path $archive -DestinationPath $tempDir -Force
        $binary = Get-ChildItem -Path $tempDir -Filter "$BinaryName.exe" -Recurse | Select-Object -First 1
        if (-not $binary) {
            throw "Binary not found in archive."
        }

        $installDir = Get-InstallDirectory
        if (-not (Test-Path $installDir)) {
            New-Item -ItemType Directory -Path $installDir | Out-Null
        }

        $target = Join-Path $installDir "$BinaryName.exe"
        Copy-Item -Path $binary.FullName -Destination $target -Force
        Copy-Item -Path $target -Destination (Join-Path $installDir "vct.exe") -Force

        Write-Host "Installed $BinaryName $Version to $installDir"
        if ($env:Path -notlike "*$installDir*") {
            Write-Host "Add $installDir to your PATH if the command is not found."
        }
    }
    catch {
        Write-Error "Installation failed: $($_.Exception.Message)"
        exit 1
    }
    finally {
        Remove-Item -Path $tempDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Main {
    Write-Host "Vibe Coding Tracker Installer"

    $arch = Get-Architecture
    Write-Host "Detected architecture: $arch"

    $version = Get-LatestVersion
    Write-Host "Latest version: $version"

    Install-Binary -Version $version -Arch $arch
}

Main
