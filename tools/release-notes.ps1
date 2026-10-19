<#
.SYNOPSIS
  Writes the GitHub release notes for a version: its section of CHANGELOG.md.

.EXAMPLE
  powershell -File tools\release-notes.ps1 -Version 0.4.0 -OutFile release-notes.md
#>
param(
    [Parameter(Mandatory = $true)][string] $Version,
    [Parameter(Mandatory = $true)][string] $OutFile
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$lines = Get-Content (Join-Path $root "CHANGELOG.md")

$on = $false
$section = New-Object System.Collections.Generic.List[string]
foreach ($line in $lines) {
    if ($line -match '^##\s+v?(\S+)') {
        if ($on) { break }
        $on = ($Matches[1] -eq $Version)
        continue
    }
    if ($on) { $section.Add($line) }
}
$text = ($section -join "`n").Trim()
if (-not $text) { throw "CHANGELOG.md has no section for version $Version. Add '## $Version' with a line for each change." }

Set-Content -Path $OutFile -Value $text -Encoding UTF8
Write-Host "Release notes for $Version written to $OutFile"
