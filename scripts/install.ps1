$ErrorActionPreference = "Stop"

$Repo = "Mai0313/VibeCodingTracker"
$BinaryName = "vibe_coding_tracker"

[System.Net.ServicePointManager]::SecurityProtocol = [System.Net.SecurityProtocolType]::Tls12

# The request already reports the underlying reason; this only says what to do about it.
$TlsFailureHint = @(
    "If the error above is a trust or certificate problem, this machine does not trust the",
    "server's issuer. Import the missing root (or your proxy's CA) into the Windows",
    "certificate store and retry. This installer never skips certificate verification."
) -join "`r`n"

function Invoke-Download {
    param(
        [Parameter(Mandatory = $true)][string]$Uri,
        [string]$OutFile
    )

    $params = @{ Uri = $Uri; UseBasicParsing = $true }
    if ($OutFile) {
        $params.OutFile = $OutFile
    }

    try {
        return Invoke-WebRequest @params
    }
    catch {
        # The outer message is just "see inner exception"; the certificate error is nested.
        $reason = @()
        for ($e = $_.Exception; $e; $e = $e.InnerException) {
            $reason += $e.Message
        }
        throw "Download failed: $Uri`r`n$($reason -join "`r`n")`r`n$TlsFailureHint"
    }
}

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
    $response = Invoke-Download -Uri "https://api.github.com/repos/$Repo/releases/latest"
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
    # Set before the try so the finally can always test them; Remove-Item rejects a null
    # LiteralPath at parameter binding, which -ErrorAction does not suppress.
    $stageDir = $null
    $newMarker = $null

    try {
        $archive = Join-Path $tempDir $filename
        Invoke-Download -Uri $url -OutFile $archive

        Expand-Archive -Path $archive -DestinationPath $tempDir -Force
        $binary = Get-ChildItem -Path $tempDir -Filter "$BinaryName.exe" -Recurse | Select-Object -First 1
        if (-not $binary) {
            throw "Binary not found in archive."
        }

        $installDir = Get-InstallDirectory
        if (-not (Test-Path $installDir)) {
            New-Item -ItemType Directory -Path $installDir | Out-Null
        }

        # Staged beside the targets so each move into place is a same-volume rename, and inside one
        # directory of its own so a single Remove-Item in the finally reaps whatever a failed run
        # staged.
        $stageDir = Join-Path $installDir ".$BinaryName.$PID.staging"
        New-Item -ItemType Directory -Path $stageDir -Force | Out-Null

        $target = Join-Path $installDir "$BinaryName.exe"
        $stagedTarget = Join-Path $stageDir "$BinaryName.exe"
        Copy-Item -Path $binary.FullName -Destination $stagedTarget -Force

        $wrapper = Join-Path $installDir "vct.cmd"
        $stagedWrapper = Join-Path $stageDir "vct.cmd"
        [System.IO.File]::WriteAllText(
            $stagedWrapper,
            "@echo off`r`n`"%~dp0$BinaryName.exe`" %*`r`n",
            [System.Text.Encoding]::ASCII
        )

        $markerPath = [System.IO.Path]::GetFullPath($target) + ".vct-managed"
        $stagedMarker = Join-Path $stageDir "$BinaryName.exe.vct-managed"
        [System.IO.File]::WriteAllBytes(
            $stagedMarker,
            [System.Text.Encoding]::ASCII.GetBytes("vct-release-installer-v1`n")
        )

        # Every file is ready before any of them lands, and the marker lands first. Renames cannot
        # be made atomic across files, so this is the order whose half-done state is survivable: a
        # binary installed without its marker is one the startup auto-update silently never fires
        # for again, whereas an unaccompanied marker is undone in the finally. Only one this run
        # put down is undone, since a marker that was already there belongs to the install that is
        # already there.
        if (-not (Test-Path -LiteralPath $markerPath)) {
            $newMarker = $markerPath
        }
        Move-Item -LiteralPath $stagedMarker -Destination $markerPath -Force
        Move-Item -LiteralPath $stagedTarget -Destination $target -Force
        # The binary the marker claims is in place, so the marker is no longer this run's to undo.
        $newMarker = $null
        Move-Item -LiteralPath $stagedWrapper -Destination $wrapper -Force

        $legacyAlias = Join-Path $installDir "vct.exe"
        if (Test-Path -LiteralPath $legacyAlias) {
            Remove-Item -LiteralPath $legacyAlias -Force
        }

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
        if ($stageDir) {
            Remove-Item -LiteralPath $stageDir -Recurse -Force -ErrorAction SilentlyContinue
        }
        if ($newMarker) {
            Remove-Item -LiteralPath $newMarker -Force -ErrorAction SilentlyContinue
        }
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
