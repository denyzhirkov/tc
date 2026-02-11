$ErrorActionPreference = "Stop"

$Repo = "denyzhirkov/tc"
$Asset = "tc-client-windows-amd64.exe"
$Url = "https://github.com/$Repo/releases/latest/download/$Asset"

$InstallDir = "$env:LOCALAPPDATA\tc"
$ExePath = "$InstallDir\tc.exe"

if (-not (Test-Path $InstallDir)) {
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
}

Write-Host "Downloading $Asset..."
Invoke-WebRequest -Uri $Url -OutFile $ExePath -UseBasicParsing

# Add to user PATH if not already present
$UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($UserPath -notlike "*$InstallDir*") {
    [Environment]::SetEnvironmentVariable("Path", "$UserPath;$InstallDir", "User")
    $env:Path = "$env:Path;$InstallDir"
    Write-Host "Added $InstallDir to user PATH."
}

Write-Host "Installed tc to $ExePath"
Write-Host "Restart your terminal, then run 'tc' to start."
