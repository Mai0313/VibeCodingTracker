# Vibe Coding Tracker Installer for Windows
# This script downloads and installs the latest version of vibe_coding_tracker

$ErrorActionPreference = "Stop"

# GitHub repository information
$Repo = "Mai0313/VibeCodingTracker"
$BinaryName = "vibe_coding_tracker"

# Disable SSL certificate validation
[System.Net.ServicePointManager]::ServerCertificateValidationCallback = {$true}
[System.Net.ServicePointManager]::SecurityProtocol = [System.Net.SecurityProtocolType]::Tls12

function Write-ColorOutput {
    param(
        [string]$Message,
        [string]$Color = "White"
    )
    Write-Host $Message -ForegroundColor $Color
}

function Get-Architecture {
    $arch = $env:PROCESSOR_ARCHITECTURE
    switch ($arch) {
        "AMD64" { return "x64" }
        "ARM64" { return "arm64" }
        default {
            Write-ColorOutput "Error: Unsupported architecture $arch" "Red"
            exit 1
        }
    }
}

function Get-LatestVersion {
    Write-ColorOutput "Fetching latest release version..." "Yellow"

    try {
        $response = Invoke-WebRequest -Uri "https://api.github.com/repos/$Repo/releases/latest" -UseBasicParsing
        $json = $response.Content | ConvertFrom-Json
        $version = $json.tag_name

        if (-not $version) {
            throw "Version not found in response"
        }

        return $version
    }
    catch {
        Write-ColorOutput "Error: Failed to fetch latest version - $($_.Exception.Message)" "Red"
        exit 1
    }
}

function Install-Binary {
    param(
        [string]$Arch,
        [string]$Version
    )

    # Construct download URL
    $filename = "${BinaryName}-${Version}-windows-${Arch}.zip"
    $downloadUrl = "https://github.com/$Repo/releases/download/$Version/$filename"

    Write-ColorOutput "Downloading $filename..." "Yellow"

    # Create temporary directory
    $tempDir = [System.IO.Path]::Combine([System.IO.Path]::GetTempPath(), [System.Guid]::NewGuid().ToString())
    New-Item -ItemType Directory -Path $tempDir | Out-Null

    $archivePath = Join-Path $tempDir $filename

    try {
        # Download file
        Invoke-WebRequest -Uri $downloadUrl -OutFile $archivePath -UseBasicParsing

        # Extract archive
        Write-ColorOutput "Extracting archive..." "Yellow"
        Expand-Archive -Path $archivePath -DestinationPath $tempDir -Force

        # Find binary
        $binaryPath = Get-ChildItem -Path $tempDir -Recurse -Filter "$BinaryName.exe" | Select-Object -First 1

        if (-not $binaryPath) {
            throw "Binary not found in archive"
        }

        # Determine install directory
        $installDir = Join-Path $env:LOCALAPPDATA "Programs\VibeCodingTracker"

        # Create install directory if it doesn't exist
        if (-not (Test-Path $installDir)) {
            New-Item -ItemType Directory -Path $installDir -Force | Out-Null
        }

        # Install binary
        Write-ColorOutput "Installing to $installDir..." "Yellow"
        $targetPath = Join-Path $installDir "$BinaryName.exe"
        Copy-Item -Path $binaryPath.FullName -Destination $targetPath -Force

        # Create short alias (vct.exe)
        $vctPath = Join-Path $installDir "vct.exe"
        Copy-Item -Path $targetPath -Destination $vctPath -Force

        # Add to PATH if not already present
        $currentPath = [Environment]::GetEnvironmentVariable("Path", "User")
        if ($currentPath -notlike "*$installDir*") {
            Write-ColorOutput "Adding to PATH..." "Yellow"
            [Environment]::SetEnvironmentVariable(
                "Path",
                "$currentPath;$installDir",
                "User"
            )
            $env:Path = "$env:Path;$installDir"
        }

        # Clean up
        Remove-Item -Path $tempDir -Recurse -Force

        Write-ColorOutput "" "Green"
        Write-ColorOutput "✓ Installation complete!" "Green"
        Write-ColorOutput "Run 'vct --help' or 'vibe_coding_tracker --help' to get started" "Green"
        Write-ColorOutput "" "Yellow"
        Write-ColorOutput "Note: You may need to restart your terminal for PATH changes to take effect" "Yellow"
    }
    catch {
        Write-ColorOutput "Error: Installation failed - $($_.Exception.Message)" "Red"
        Remove-Item -Path $tempDir -Recurse -Force -ErrorAction SilentlyContinue
        exit 1
    }
}

# Main installation flow
function Main {
    Write-ColorOutput "Vibe Coding Tracker Installer" "Green"
    Write-ColorOutput "" "White"

    $arch = Get-Architecture
    Write-ColorOutput "Detected architecture: $arch" "Green"

    $version = Get-LatestVersion
    Write-ColorOutput "Latest version: $version" "Green"
    Write-ColorOutput "" "White"

    Install-Binary -Arch $arch -Version $version
}

# Run installer
Main
