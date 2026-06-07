#Requires -Version 5.1
<#
.SYNOPSIS
    Set the AllTheThings version in every file that carries it, in lockstep.

.DESCRIPTION
    Updates the version in package.json, src-tauri/Cargo.toml and
    src-tauri/tauri.conf.json, then syncs Cargo.lock. Keeps the three sources
    of truth from drifting. Optionally commits the change and creates the
    matching git tag.

.PARAMETER Version
    The new semantic version, e.g. 0.2.0.

.PARAMETER Tag
    Also commit the bump and create a "v<Version>" git tag (not pushed).

.EXAMPLE
    .\scripts\bump-version.ps1 0.2.0

.EXAMPLE
    .\scripts\bump-version.ps1 0.2.0 -Tag
    git push origin main --follow-tags   # triggers the release workflow
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory, Position = 0)]
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string]$Version,

    [switch]$Tag
)

$ErrorActionPreference = 'Stop'

# --- Data: which files hold a version, and how to find it ---
function Get-VersionTargets {
    [CmdletBinding()]
    param([string]$Root)

    @(
        [pscustomobject]@{
            Path    = Join-Path $Root 'package.json'
            Pattern = '(?<pre>"version"\s*:\s*")(?<ver>\d+\.\d+\.\d+)(?<post>")'
        }
        [pscustomobject]@{
            Path    = Join-Path $Root 'src-tauri\tauri.conf.json'
            Pattern = '(?<pre>"version"\s*:\s*")(?<ver>\d+\.\d+\.\d+)(?<post>")'
        }
        [pscustomobject]@{
            # Anchored to line start so dependency `version = ` lines are untouched.
            Path    = Join-Path $Root 'src-tauri\Cargo.toml'
            Pattern = '(?m)(?<pre>^version\s*=\s*")(?<ver>\d+\.\d+\.\d+)(?<post>")'
        }
    )
}

# --- Processing: replace the version in one file, return what changed ---
function Set-FileVersion {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)] [string]$Path,
        [Parameter(Mandatory)] [string]$Pattern,
        [Parameter(Mandatory)] [string]$NewVersion
    )

    if (-not (Test-Path -LiteralPath $Path)) {
        throw "File not found: $Path"
    }

    $text = Get-Content -Raw -LiteralPath $Path
    $match = [regex]::Match($text, $Pattern)
    if (-not $match.Success) {
        throw "No version field matched in $Path"
    }

    $old = $match.Groups['ver'].Value
    $updated = [regex]::Replace($text, $Pattern, "`${pre}$NewVersion`${post}")

    # Write UTF-8 without BOM and preserve existing newlines (PS 5.1 safe).
    [System.IO.File]::WriteAllText($Path, $updated, (New-Object System.Text.UTF8Encoding $false))

    [pscustomobject]@{
        File = Split-Path -Leaf $Path
        From = $old
        To   = $NewVersion
    }
}

# --- Orchestration ---
$root = Split-Path -Parent $PSScriptRoot

$results = foreach ($target in Get-VersionTargets -Root $root) {
    Set-FileVersion -Path $target.Path -Pattern $target.Pattern -NewVersion $Version
}

# Sync Cargo.lock's workspace entry (best-effort; a build would fix it anyway).
$cargo = Get-Command cargo -ErrorAction SilentlyContinue
if ($cargo) {
    & $cargo.Source update --manifest-path (Join-Path $root 'src-tauri\Cargo.toml') --workspace --quiet 2>&1 | Out-Null
}

$results | Format-Table -AutoSize

if ($Tag) {
    $files = @(
        'package.json'
        'src-tauri/Cargo.toml'
        'src-tauri/tauri.conf.json'
        'src-tauri/Cargo.lock'
    )
    git -C $root add $files
    git -C $root commit -m "Release v$Version"
    git -C $root tag "v$Version"
    Write-Host "Committed and tagged v$Version. Push with:" -ForegroundColor Green
    Write-Host "  git push origin main --follow-tags"
}
else {
    Write-Host "Bumped to $Version. Review, then commit and tag to release." -ForegroundColor Green
}
