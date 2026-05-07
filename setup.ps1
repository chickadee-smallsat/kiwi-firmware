# Setup script for kiwi-firmware on Windows
# Installs the Rust toolchain and probe-rs

$ErrorActionPreference = "Stop"

# --- Visual Studio Build Tools (MSVC linker) ---
$vswhere    = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vsInstaller = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vs_installer.exe"

$hasVCTools = $false
if (Test-Path $vswhere) {
    $vcPath = & $vswhere -latest -products * `
        -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
        -property installationPath 2>$null
    if ($vcPath) { $hasVCTools = $true }
}

if ($hasVCTools) {
    Write-Host "MSVC C++ tools already installed -- skipping."
} elseif (Test-Path $vsInstaller) {
    # VS is present but missing C++ tools -- modify the existing install
    $installPath = & $vswhere -latest -products * -property installationPath
    Write-Host "Visual Studio found at '$installPath'. Adding C++ workload..."
    & $vsInstaller modify `
        --installPath $installPath `
        --add Microsoft.VisualStudio.Workload.VCTools `
        --includeRecommended --quiet --norestart
    if ($LASTEXITCODE -ne 0) {
        Write-Error "Failed to add C++ tools to existing Visual Studio (exit code $LASTEXITCODE). Open the Visual Studio Installer and add the 'Desktop development with C++' workload manually."
    }
    Write-Host "C++ workload added."
} else {
    # No VS at all -- install Build Tools via winget
    if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
        Write-Error "winget is not available. Please install Visual Studio 2022 Build Tools manually from https://visualstudio.microsoft.com/downloads/#build-tools-for-visual-studio-2022 and select the 'Desktop development with C++' workload."
    }
    Write-Host "Installing Visual Studio 2022 Build Tools with C++ workload..."
    winget install --id Microsoft.VisualStudio.2022.BuildTools --exact --silent --force `
        --override "--quiet --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended" `
        --accept-source-agreements --accept-package-agreements
    if ($LASTEXITCODE -ne 0) {
        Write-Error "Visual Studio Build Tools installation failed (exit code $LASTEXITCODE). Please install manually from https://visualstudio.microsoft.com/downloads/#build-tools-for-visual-studio-2022"
    }
    Write-Host "Visual Studio Build Tools installed."
}

# --- Rust ---
if (Get-Command rustup -ErrorAction SilentlyContinue) {
    Write-Host "rustup already installed -- updating..."
    rustup update stable
} else {
    Write-Host "Installing Rust via rustup..."
    $rustupInit = Join-Path $env:TEMP "rustup-init.exe"
    Invoke-WebRequest -Uri "https://win.rustup.rs/x86_64" -OutFile $rustupInit
    & $rustupInit -y --default-toolchain stable
    Remove-Item $rustupInit

    # Make cargo/rustup available in this session
    $env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
}

# --- Target for RP2350 ---
Write-Host "Adding thumbv8m.main-none-eabihf target..."
rustup target add thumbv8m.main-none-eabihf

# --- probe-rs ---
Write-Host "Installing probe-rs..."
Invoke-RestMethod `
    -Uri "https://github.com/probe-rs/probe-rs/releases/latest/download/probe-rs-tools-installer.ps1" `
    | Invoke-Expression

# Reload PATH from Machine and User environment so new tools are available immediately
$env:PATH = [System.Environment]::GetEnvironmentVariable("Path", "Machine") + ";" +
            [System.Environment]::GetEnvironmentVariable("Path", "User")

Write-Host ""
Write-Host "Setup complete."
Write-Host "  Please close this PowerShell window and open a new one before building."
Write-Host "  Then connect your debug probe and run:  test.bat"