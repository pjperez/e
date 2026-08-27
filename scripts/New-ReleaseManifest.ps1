[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string] $ArtifactDirectory,

    [Parameter(Mandatory)]
    [ValidatePattern('^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$')]
    [string] $Version,

    [Parameter(Mandatory)]
    [string] $Tag,

    [Parameter(Mandatory)]
    [string] $PrivateKey,

    [string] $ManifestPath = (Join-Path $PWD 'checksums.json'),

    [string] $SignaturePath = (Join-Path $PWD 'checksums.json.sig')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$installers = @(Get-ChildItem -LiteralPath $ArtifactDirectory -Filter '*-setup.exe' -File)
if ($installers.Count -eq 0) {
    throw "No NSIS installers were found in '$ArtifactDirectory'."
}

$artifacts = foreach ($installer in ($installers | Sort-Object Name)) {
    if ($installer.Name -notmatch '^e_[0-9A-Za-z.-]+_(x64|arm64)-setup\.exe$') {
        throw "Unexpected installer name '$($installer.Name)'."
    }

    [ordered]@{
        arch = $Matches[1]
        name = $installer.Name
        sha256 = (Get-FileHash -LiteralPath $installer.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        size = $installer.Length
    }
}

$manifest = [ordered]@{
    schemaVersion = 1
    version = $Version
    tag = $Tag
    artifacts = @($artifacts)
}

$utf8 = [System.Text.UTF8Encoding]::new($false)
$json = ($manifest | ConvertTo-Json -Depth 5 -Compress) + "`n"
[System.IO.File]::WriteAllText($ManifestPath, $json, $utf8)

$rsa = [System.Security.Cryptography.RSA]::Create()
try {
    $rsa.ImportFromPem($PrivateKey)
    $manifestBytes = [System.IO.File]::ReadAllBytes($ManifestPath)
    $signature = $rsa.SignData(
        $manifestBytes,
        [System.Security.Cryptography.HashAlgorithmName]::SHA256,
        [System.Security.Cryptography.RSASignaturePadding]::Pkcs1
    )
    [System.IO.File]::WriteAllText(
        $SignaturePath,
        [Convert]::ToBase64String($signature),
        $utf8
    )
}
finally {
    $rsa.Dispose()
}

Write-Host "Signed release manifest for $($artifacts.Count) installer(s)."
