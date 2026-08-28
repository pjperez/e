#Requires -Version 5.1

<#
.SYNOPSIS
Installs a verified Windows release of the e agent harness.

.DESCRIPTION
Downloads an immutable GitHub release, verifies its signed manifest and
installer SHA-256 digest, then starts the installer. Windows asks for the
usual approval because e installs into Program Files.

.PARAMETER Version
Install a specific release version instead of the latest stable release.

.PARAMETER Quiet
Run the NSIS installer without interactive prompts.

.PARAMETER AllowPrerelease
Allow a prerelease when Version explicitly names one.
#>

[CmdletBinding()]
param(
    [ValidatePattern('^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$')]
    [string] $Version,

    [switch] $Quiet,

    [switch] $AllowPrerelease
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
# Invoke-WebRequest renders a progress bar that can slow a download to a crawl
# in an interactive console, and it makes the script look wedged.
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = `
    [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$PublicKeyModulus = 't8jLqGk6YdfIvjKGs6Sc5T0Vzxhxf1mOhFqOoW9PDnfmhhM+EaslL3xUvQAMtDsOOVwnQWQ3IMdVR9dlX8vDsdzgN6TvnrdVXMEjw3G881+ilBHEwgcb8/r0WhBBO0aJ2RmR8e3Q5hilJWOqsGWNkptDr55jlb0XDAXPwc4Xd79hMR/v1aYt5i+n7/P3CnWXuQBwWiRZSeSwaEXnusm+5/9Cs1rzNw49L7AhVmOpPCFWzQgIIuGeUeAHUpAlD2a+nzmQtKLsdaIoexZ9nmDmqBmxx4UpXhBvG5dFy2yYMu8ntjW0yEZkHGROaCXu6MoOZXxQ/0yiU/tn4/suHGmUiovBmkBqvlFniEcWVMmDdnH2VulwRm+Ped0sfO0+N49+J+bLZtPc2UVPy+2oRXCnqOAl0Qa9vqf3oM54otncjsKRZr1PV0dgppVHIP8wtmqa7SWM+YyPjNJvz8QhB7JqvsC0U5dDamYdCwVwWqeUUTVW9yPYUxFAV8i8Pcyu4Hzp'
$PublicKeyExponent = 'AQAB'

function Invoke-WithRetry {
    param(
        [Parameter(Mandatory)]
        [scriptblock] $Operation,

        [Parameter(Mandatory)]
        [string] $Description,

        [int] $Attempts = 4
    )

    for ($attempt = 1; $attempt -le $Attempts; $attempt++) {
        try {
            return & $Operation
        }
        catch {
            if ($attempt -eq $Attempts) {
                throw
            }
            Write-Warning "$Description failed (attempt $attempt of $Attempts): $($_.Exception.Message). Retrying..."
            Start-Sleep -Seconds 3
        }
    }
}

function Invoke-Download {
    param(
        [Parameter(Mandatory)]
        [uri] $Uri,

        [Parameter(Mandatory)]
        [string] $Destination,

        [Parameter(Mandatory)]
        [hashtable] $Headers
    )

    $null = Invoke-WithRetry -Description "Download from $Uri" -Operation {
        Remove-Item -LiteralPath $Destination -Force -ErrorAction SilentlyContinue
        Invoke-WebRequest `
            -UseBasicParsing `
            -Uri $Uri `
            -OutFile $Destination `
            -Headers $Headers `
            -TimeoutSec 120
    }
}

function Get-SemanticVersionParts {
    param(
        [Parameter(Mandatory)]
        [string] $Value
    )

    $match = [regex]::Match(
        $Value,
        '^(?<core>\d+\.\d+\.\d+)(?:-(?<pre>[0-9A-Za-z.-]+))?$'
    )
    if (-not $match.Success) {
        throw "Invalid semantic version '$Value'."
    }

    [pscustomobject] @{
        Core = [version] $match.Groups['core'].Value
        Prerelease = $match.Groups['pre'].Value
    }
}

function Compare-SemanticVersion {
    param(
        [Parameter(Mandatory)]
        [string] $Left,

        [Parameter(Mandatory)]
        [string] $Right
    )

    $leftVersion = Get-SemanticVersionParts -Value $Left
    $rightVersion = Get-SemanticVersionParts -Value $Right
    $coreComparison = $leftVersion.Core.CompareTo($rightVersion.Core)
    if ($coreComparison -ne 0) {
        return [Math]::Sign($coreComparison)
    }
    if (-not $leftVersion.Prerelease -and -not $rightVersion.Prerelease) {
        return 0
    }
    if (-not $leftVersion.Prerelease) {
        return 1
    }
    if (-not $rightVersion.Prerelease) {
        return -1
    }

    $leftParts = @($leftVersion.Prerelease -split '\.')
    $rightParts = @($rightVersion.Prerelease -split '\.')
    $count = [Math]::Min($leftParts.Count, $rightParts.Count)
    for ($i = 0; $i -lt $count; $i++) {
        $leftPart = $leftParts[$i]
        $rightPart = $rightParts[$i]
        if ($leftPart -ceq $rightPart) {
            continue
        }

        $leftNumeric = $leftPart -match '^\d+$'
        $rightNumeric = $rightPart -match '^\d+$'
        if ($leftNumeric -and -not $rightNumeric) {
            return -1
        }
        if (-not $leftNumeric -and $rightNumeric) {
            return 1
        }
        if ($leftNumeric) {
            $leftNumber = $leftPart.TrimStart('0')
            $rightNumber = $rightPart.TrimStart('0')
            if (-not $leftNumber) {
                $leftNumber = '0'
            }
            if (-not $rightNumber) {
                $rightNumber = '0'
            }
            if ($leftNumber.Length -ne $rightNumber.Length) {
                return $(if ($leftNumber.Length -lt $rightNumber.Length) { -1 } else { 1 })
            }
            $partComparison = [string]::CompareOrdinal($leftNumber, $rightNumber)
        }
        else {
            $partComparison = [string]::CompareOrdinal($leftPart, $rightPart)
        }
        if ($partComparison -ne 0) {
            return [Math]::Sign($partComparison)
        }
    }

    return [Math]::Sign($leftParts.Count - $rightParts.Count)
}

function Test-ManifestSignature {
    param(
        [Parameter(Mandatory)]
        [string] $ManifestPath,

        [Parameter(Mandatory)]
        [string] $SignaturePath
    )

    $parameters = New-Object System.Security.Cryptography.RSAParameters
    $parameters.Modulus = [Convert]::FromBase64String($PublicKeyModulus)
    $parameters.Exponent = [Convert]::FromBase64String($PublicKeyExponent)

    $rsa = New-Object System.Security.Cryptography.RSACryptoServiceProvider
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
        $rsa.ImportParameters($parameters)
        $manifestBytes = [System.IO.File]::ReadAllBytes($ManifestPath)
        $signatureBytes = [Convert]::FromBase64String(
            [System.IO.File]::ReadAllText($SignaturePath).Trim()
        )
        return $rsa.VerifyData($manifestBytes, $sha256, $signatureBytes)
    }
    finally {
        $sha256.Dispose()
        $rsa.Dispose()
    }
}

if (-not $PSCommandPath) {
    throw 'Download install.ps1 and run it as a file; piping remote code into PowerShell is intentionally unsupported.'
}

if (-not [System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
    [System.Runtime.InteropServices.OSPlatform]::Windows
)) {
    throw 'This installer currently supports Windows only.'
}

$architecture = switch (
    [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
) {
    'X64' { 'x64' }
    'Arm64' { 'arm64' }
    default { throw "Unsupported Windows architecture '$($_)'." }
}

$apiHeaders = @{
    Accept = 'application/vnd.github+json'
    'X-GitHub-Api-Version' = '2022-11-28'
    'User-Agent' = 'e-release-installer'
}
# Release assets are binaries, not API documents.
$assetHeaders = @{
    Accept = 'application/octet-stream'
    'User-Agent' = 'e-release-installer'
}
$repositoryApi = 'https://api.github.com/repos/pjperez/e'
$releaseUri = if ($Version) {
    "$repositoryApi/releases/tags/v$Version"
}
else {
    "$repositoryApi/releases/latest"
}

Write-Host 'Resolving the e release...'
$release = Invoke-WithRetry -Description 'GitHub release lookup' -Operation {
    Invoke-RestMethod `
        -UseBasicParsing `
        -Uri $releaseUri `
        -Headers $apiHeaders `
        -TimeoutSec 60
}
if ($release.draft) {
    throw "Release '$($release.tag_name)' is still a draft."
}
if ($release.prerelease -and -not $AllowPrerelease) {
    throw "Release '$($release.tag_name)' is a prerelease. Pass -AllowPrerelease to install it."
}

$stagingDirectory = Join-Path ([System.IO.Path]::GetTempPath()) "e-install-$([Guid]::NewGuid())"
$stateDirectory = Join-Path $env:LOCALAPPDATA 'e-harness'
$statePath = Join-Path $stateDirectory 'install-state.json'

New-Item -ItemType Directory -Path $stagingDirectory -ErrorAction Stop | Out-Null

try {
    $stagingItem = Get-Item -LiteralPath $stagingDirectory -Force
    if ($stagingItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) {
        throw 'Refusing to use a reparse-point staging directory.'
    }

    $manifestAsset = @(@($release.assets) | Where-Object name -EQ 'checksums.json')
    $signatureAsset = @(@($release.assets) | Where-Object name -EQ 'checksums.json.sig')
    if ($manifestAsset.Count -ne 1 -or $signatureAsset.Count -ne 1) {
        throw "Release '$($release.tag_name)' does not contain one signed checksum manifest."
    }

    $manifestPath = Join-Path $stagingDirectory 'checksums.json'
    $signaturePath = Join-Path $stagingDirectory 'checksums.json.sig'
    Invoke-Download -Uri $manifestAsset[0].browser_download_url -Destination $manifestPath -Headers $assetHeaders
    Invoke-Download -Uri $signatureAsset[0].browser_download_url -Destination $signaturePath -Headers $assetHeaders

    if (-not (Test-ManifestSignature -ManifestPath $manifestPath -SignaturePath $signaturePath)) {
        throw 'The release manifest signature is invalid.'
    }

    $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    if ($manifest.schemaVersion -ne 1) {
        throw "Unsupported release manifest schema '$($manifest.schemaVersion)'."
    }
    if ($manifest.tag -ne $release.tag_name) {
        throw 'The release tag does not match the signed manifest.'
    }

    $releaseVersion = [string] $manifest.version
    $null = Get-SemanticVersionParts -Value $releaseVersion
    if ($Version -and (Compare-SemanticVersion -Left $releaseVersion -Right $Version) -ne 0) {
        throw "Requested version '$Version' does not match manifest version '$releaseVersion'."
    }

    if (Test-Path -LiteralPath $statePath) {
        $state = Get-Content -LiteralPath $statePath -Raw | ConvertFrom-Json
        $installedVersion = [string] $state.version
        if ((Compare-SemanticVersion -Left $releaseVersion -Right $installedVersion) -lt 0) {
            throw "Version '$releaseVersion' is older than installed version '$installedVersion'. Downgrades are not supported."
        }
    }

    $artifact = @(@($manifest.artifacts) | Where-Object arch -EQ $architecture)
    if ($artifact.Count -ne 1) {
        throw "The signed manifest does not contain one '$architecture' installer."
    }
    if ($artifact[0].name -notmatch '^e_[0-9A-Za-z.-]+_(x64|arm64)-setup\.exe$') {
        throw "Unsafe installer name '$($artifact[0].name)' in release manifest."
    }

    $releaseAsset = @(@($release.assets) | Where-Object name -EQ $artifact[0].name)
    if ($releaseAsset.Count -ne 1) {
        throw "Release asset '$($artifact[0].name)' is missing or ambiguous."
    }

    $installerPath = Join-Path $stagingDirectory $artifact[0].name
    $hasSize = $artifact[0].PSObject.Properties.Name -contains 'size'
    $scale = if ($hasSize) { " ($([math]::Round($artifact[0].size / 1MB, 1)) MB)" } else { '' }
    Write-Host "Downloading e $releaseVersion for Windows $architecture$scale..."
    Invoke-Download -Uri $releaseAsset[0].browser_download_url -Destination $installerPath -Headers $assetHeaders
    Write-Host 'Verifying the download...'

    $actualHash = (Get-FileHash -LiteralPath $installerPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $expectedHash = ([string]$artifact[0].sha256).ToLowerInvariant()
    if ($actualHash -ne $expectedHash) {
        throw 'The installer SHA-256 digest does not match the signed release manifest.'
    }
    $hasGitHubDigest = $releaseAsset[0].PSObject.Properties.Name -contains 'digest'
    if (
        $hasGitHubDigest -and
        $releaseAsset[0].digest -and
        $releaseAsset[0].digest -ne "sha256:$actualHash"
    ) {
        throw 'The installer SHA-256 digest does not match GitHub release metadata.'
    }

    $authenticode = Get-AuthenticodeSignature -FilePath $installerPath
    if ($authenticode.Status -eq 'Valid') {
        Write-Host "Authenticode publisher: $($authenticode.SignerCertificate.Subject)"
    }
    elseif ($authenticode.Status -eq 'NotSigned') {
        Write-Warning 'This release is authenticated by the signed manifest but does not yet carry a trusted Authenticode publisher signature.'
    }
    else {
        throw "The installer has an invalid Authenticode status: $($authenticode.Status)."
    }

    Write-Host "Verified e $releaseVersion. Starting the installer (Windows will ask for approval)..."
    # Deliberately not -Wait: that waits for the process *and its descendants*,
    # so the installer's "Run e" finish option would keep this script blocked
    # for as long as the app stays open. Wait for the installer alone.
    $process = if ($Quiet) {
        Start-Process -FilePath $installerPath -ArgumentList '/S' -PassThru
    }
    else {
        Start-Process -FilePath $installerPath -PassThru
    }
    $process.WaitForExit()

    if ($process.ExitCode -ne 0) {
        throw "The installer exited with code $($process.ExitCode). If you cancelled it, run this script again to retry."
    }

    New-Item -ItemType Directory -Path $stateDirectory -Force | Out-Null
    $stateTempPath = Join-Path $stateDirectory "install-state-$([Guid]::NewGuid()).json"
    $stateJson = [ordered]@{
        version = $releaseVersion.ToString()
        arch = $architecture
        installedAt = [DateTimeOffset]::UtcNow.ToString('O')
    } | ConvertTo-Json
    $utf8NoBom = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText(
        $stateTempPath,
        $stateJson + [Environment]::NewLine,
        $utf8NoBom
    )
    Move-Item -LiteralPath $stateTempPath -Destination $statePath -Force

    Write-Host "e $releaseVersion installed successfully."
}
finally {
    if (Test-Path -LiteralPath $stagingDirectory) {
        Remove-Item -LiteralPath $stagingDirectory -Recurse -Force
    }
}
