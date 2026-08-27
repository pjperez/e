#Requires -Version 7.0

<#
.SYNOPSIS
Installs a verified Windows release of the e agent harness.

.DESCRIPTION
Downloads an immutable GitHub release, verifies its signed manifest and
installer SHA-256 digest, then starts the current-user NSIS installer.
This script never elevates, changes execution policy, or installs build tools.

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

$PublicKey = @'
-----BEGIN PUBLIC KEY-----
MIIBojANBgkqhkiG9w0BAQEFAAOCAY8AMIIBigKCAYEAt8jLqGk6YdfIvjKGs6Sc
5T0Vzxhxf1mOhFqOoW9PDnfmhhM+EaslL3xUvQAMtDsOOVwnQWQ3IMdVR9dlX8vD
sdzgN6TvnrdVXMEjw3G881+ilBHEwgcb8/r0WhBBO0aJ2RmR8e3Q5hilJWOqsGWN
kptDr55jlb0XDAXPwc4Xd79hMR/v1aYt5i+n7/P3CnWXuQBwWiRZSeSwaEXnusm+
5/9Cs1rzNw49L7AhVmOpPCFWzQgIIuGeUeAHUpAlD2a+nzmQtKLsdaIoexZ9nmDm
qBmxx4UpXhBvG5dFy2yYMu8ntjW0yEZkHGROaCXu6MoOZXxQ/0yiU/tn4/suHGmU
iovBmkBqvlFniEcWVMmDdnH2VulwRm+Ped0sfO0+N49+J+bLZtPc2UVPy+2oRXCn
qOAl0Qa9vqf3oM54otncjsKRZr1PV0dgppVHIP8wtmqa7SWM+YyPjNJvz8QhB7Jq
vsC0U5dDamYdCwVwWqeUUTVW9yPYUxFAV8i8Pcyu4HzpAgMBAAE=
-----END PUBLIC KEY-----
'@

function Invoke-Download {
    param(
        [Parameter(Mandatory)]
        [uri] $Uri,

        [Parameter(Mandatory)]
        [string] $Destination,

        [Parameter(Mandatory)]
        [hashtable] $Headers
    )

    Invoke-WebRequest `
        -Uri $Uri `
        -OutFile $Destination `
        -Headers $Headers `
        -MaximumRetryCount 2 `
        -RetryIntervalSec 2
}

function Get-SemanticVersion {
    param([Parameter(Mandatory)][string] $Value)

    return [System.Management.Automation.SemanticVersion]::new($Value)
}

if (-not $PSCommandPath) {
    throw 'Download install.ps1 and run it as a file; piping remote code into PowerShell is intentionally unsupported.'
}

if (-not [System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
    [System.Runtime.InteropServices.OSPlatform]::Windows
)) {
    throw 'This installer currently supports Windows only.'
}

$identity = [System.Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [System.Security.Principal.WindowsPrincipal]::new($identity)
if ($principal.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run install.ps1 from a non-elevated PowerShell window. e installs for the current user.'
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
$repositoryApi = 'https://api.github.com/repos/pjperez/e'
$releaseUri = if ($Version) {
    "$repositoryApi/releases/tags/v$Version"
}
else {
    "$repositoryApi/releases/latest"
}

Write-Host 'Resolving the e release...'
$release = Invoke-RestMethod -Uri $releaseUri -Headers $apiHeaders -MaximumRetryCount 2
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

    $manifestAsset = @($release.assets) | Where-Object name -EQ 'checksums.json'
    $signatureAsset = @($release.assets) | Where-Object name -EQ 'checksums.json.sig'
    if ($manifestAsset.Count -ne 1 -or $signatureAsset.Count -ne 1) {
        throw "Release '$($release.tag_name)' does not contain one signed checksum manifest."
    }

    $manifestPath = Join-Path $stagingDirectory 'checksums.json'
    $signaturePath = Join-Path $stagingDirectory 'checksums.json.sig'
    Invoke-Download -Uri $manifestAsset[0].browser_download_url -Destination $manifestPath -Headers $apiHeaders
    Invoke-Download -Uri $signatureAsset[0].browser_download_url -Destination $signaturePath -Headers $apiHeaders

    $rsa = [System.Security.Cryptography.RSA]::Create()
    try {
        $rsa.ImportFromPem($PublicKey)
        $manifestBytes = [System.IO.File]::ReadAllBytes($manifestPath)
        $signatureBytes = [Convert]::FromBase64String(
            [System.IO.File]::ReadAllText($signaturePath).Trim()
        )
        $verified = $rsa.VerifyData(
            $manifestBytes,
            $signatureBytes,
            [System.Security.Cryptography.HashAlgorithmName]::SHA256,
            [System.Security.Cryptography.RSASignaturePadding]::Pkcs1
        )
        if (-not $verified) {
            throw 'The release manifest signature is invalid.'
        }
    }
    finally {
        $rsa.Dispose()
    }

    $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    if ($manifest.schemaVersion -ne 1) {
        throw "Unsupported release manifest schema '$($manifest.schemaVersion)'."
    }
    if ($manifest.tag -ne $release.tag_name) {
        throw 'The release tag does not match the signed manifest.'
    }

    $releaseVersion = Get-SemanticVersion -Value ([string]$manifest.version)
    if ($Version -and $releaseVersion -ne (Get-SemanticVersion -Value $Version)) {
        throw "Requested version '$Version' does not match manifest version '$releaseVersion'."
    }

    if (Test-Path -LiteralPath $statePath) {
        $state = Get-Content -LiteralPath $statePath -Raw | ConvertFrom-Json
        $installedVersion = Get-SemanticVersion -Value ([string]$state.version)
        if ($releaseVersion -lt $installedVersion) {
            throw "Version '$releaseVersion' is older than installed version '$installedVersion'. Downgrades are not supported."
        }
    }

    $artifact = @($manifest.artifacts) | Where-Object arch -EQ $architecture
    if ($artifact.Count -ne 1) {
        throw "The signed manifest does not contain one '$architecture' installer."
    }
    if ($artifact[0].name -notmatch '^e_[0-9A-Za-z.-]+_(x64|arm64)-setup\.exe$') {
        throw "Unsafe installer name '$($artifact[0].name)' in release manifest."
    }

    $releaseAsset = @($release.assets) | Where-Object name -EQ $artifact[0].name
    if ($releaseAsset.Count -ne 1) {
        throw "Release asset '$($artifact[0].name)' is missing or ambiguous."
    }

    $installerPath = Join-Path $stagingDirectory $artifact[0].name
    Write-Host "Downloading e $releaseVersion for Windows $architecture..."
    Invoke-Download -Uri $releaseAsset[0].browser_download_url -Destination $installerPath -Headers $apiHeaders

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

    Write-Host "Verified e $releaseVersion. Starting the current-user installer..."
    $process = if ($Quiet) {
        Start-Process -FilePath $installerPath -ArgumentList '/S' -Wait -PassThru
    }
    else {
        Start-Process -FilePath $installerPath -Wait -PassThru
    }
    if ($process.ExitCode -ne 0) {
        throw "The installer exited with code $($process.ExitCode)."
    }

    New-Item -ItemType Directory -Path $stateDirectory -Force | Out-Null
    $stateTempPath = Join-Path $stateDirectory "install-state-$([Guid]::NewGuid()).json"
    [ordered]@{
        version = $releaseVersion.ToString()
        arch = $architecture
        installedAt = [DateTimeOffset]::UtcNow.ToString('O')
    } | ConvertTo-Json | Set-Content -LiteralPath $stateTempPath -Encoding utf8NoBOM
    Move-Item -LiteralPath $stateTempPath -Destination $statePath -Force

    Write-Host "e $releaseVersion installed successfully."
}
finally {
    if (Test-Path -LiteralPath $stagingDirectory) {
        Remove-Item -LiteralPath $stagingDirectory -Recurse -Force
    }
}
