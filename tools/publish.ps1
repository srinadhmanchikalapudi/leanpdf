<#
.SYNOPSIS
  Builds a release of LeanPDF (the native app): LeanPDF.exe with pdfium.dll and the license files, a zip of them,
  and with -Installer a Windows setup program (LeanPDF-Setup-<version>.exe) made with Inno Setup.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File tools\publish.ps1
  powershell -ExecutionPolicy Bypass -File tools\publish.ps1 -Installer
  powershell -ExecutionPolicy Bypass -File tools\publish.ps1 -Version 0.2.0 -Installer -SkipTests

  Inno Setup is needed for -Installer:  winget install JRSoftware.InnoSetup
  pdfium.dll is downloaded automatically (pdfium-binaries, Chromium build 7881) when pdfium\pdfium.dll is missing.
#>
param(
    [string] $Version = "0.4.3",
    [switch] $Installer,
    [switch] $SkipTests
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# Find Inno Setup first, so a missing tool is reported before the long build, not after it.
$iscc = $null
if ($Installer) {
    $candidates = @(
        (Get-Command ISCC.exe -ErrorAction SilentlyContinue).Source,
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "$env:ProgramFiles\Inno Setup 6\ISCC.exe",
        "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
    ) | Where-Object { $_ -and (Test-Path $_) }
    $iscc = $candidates | Select-Object -First 1
    if (-not $iscc) { throw "Inno Setup was not found. Install it with:  winget install JRSoftware.InnoSetup" }
}

# PDFium: the prebuilt library the app loads at run time.
$pdfiumDir = Join-Path $root "pdfium"
if (-not (Test-Path (Join-Path $pdfiumDir "pdfium.dll"))) {
    Write-Host "Downloading pdfium.dll (pdfium-binaries, Chromium 7881)..." -ForegroundColor Cyan
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("pdfium-" + [Guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory $tmp | Out-Null
    $tgz = Join-Path $tmp "pdfium-win-x64.tgz"
    Invoke-WebRequest "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F7881/pdfium-win-x64.tgz" -OutFile $tgz
    $x = Join-Path $tmp "x"
    New-Item -ItemType Directory $x | Out-Null
    tar -xzf $tgz -C $x
    Copy-Item (Join-Path $x "bin\pdfium.dll") $pdfiumDir -Force
    Remove-Item $tmp -Recurse -Force
}

# Every version needs notes written for people (they are shown inside the app and become the GitHub release notes).
if (-not (Select-String -Path (Join-Path $root "CHANGELOG.md") -Pattern ("^##\s+v?" + [regex]::Escape($Version) + "\b") -Quiet)) {
    throw "CHANGELOG.md has no section for version $Version. Add '## $Version' with a line for each change, then publish again."
}

if (-not $SkipTests) {
    Write-Host "Running the tests first..." -ForegroundColor Cyan
    cargo test --manifest-path core/Cargo.toml
    if ($LASTEXITCODE -ne 0) { throw "The core tests failed, so nothing was published." }
    cargo test --manifest-path native/Cargo.toml
    if ($LASTEXITCODE -ne 0) { throw "The native tests failed, so nothing was published." }
}

Write-Host "Building LeanPDF $Version..." -ForegroundColor Cyan
cargo build --release --manifest-path native/Cargo.toml
if ($LASTEXITCODE -ne 0) { throw "cargo build failed." }

$name = "LeanPDF-$Version-win-x64"
$out = Join-Path $root "dist\$name"
if (Test-Path $out) { Remove-Item $out -Recurse -Force }
New-Item -ItemType Directory $out | Out-Null

Copy-Item (Join-Path $root "native\target\release\leanpdf-native.exe") (Join-Path $out "LeanPDF.exe")
Copy-Item (Join-Path $pdfiumDir "pdfium.dll") $out
Copy-Item (Join-Path $pdfiumDir "licenses") (Join-Path $out "licenses") -Recurse
Copy-Item (Join-Path $pdfiumDir "PDFIUM-LICENSE.txt") (Join-Path $out "licenses")
Copy-Item (Join-Path $root "LICENSE"), (Join-Path $root "THIRD-PARTY-NOTICES.md") -Destination $out

$zip = Join-Path $root "dist\$name.zip"
if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path (Join-Path $out "*") -DestinationPath $zip

$exe = Get-Item (Join-Path $out "LeanPDF.exe")
Write-Host ""
Write-Host ("Done. {0} ({1:N1} MB)" -f $exe.FullName, ($exe.Length / 1MB)) -ForegroundColor Green
Write-Host ("Zip:  {0}" -f $zip) -ForegroundColor Green

if ($Installer) {
    Write-Host "Building the installer with Inno Setup..." -ForegroundColor Cyan
    & $iscc "/DAppVersion=$Version" "/DSourceDir=$out" "/DOutputDir=$(Join-Path $root 'dist')" (Join-Path $root "installer\LeanPDF.iss")
    if ($LASTEXITCODE -ne 0) { throw "Inno Setup failed." }
    $setup = Get-Item (Join-Path $root "dist\LeanPDF-Setup-$Version.exe")
    Write-Host ("Installer: {0} ({1:N1} MB)" -f $setup.FullName, ($setup.Length / 1MB)) -ForegroundColor Green
}
