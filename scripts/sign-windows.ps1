param(
    [string]$PfxPath = $env:WINDOWS_PFX_PATH,
    [string]$PfxPassword = $env:WINDOWS_PFX_PASSWORD,
    [string]$TimestampUrl = "http://timestamp.digicert.com",
    [switch]$CreateLocalCertIfMissing,
    [switch]$InstallLocalTrust
)

$ErrorActionPreference = "Stop"
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$ReleaseDir = Join-Path $RepoRoot "release"
$PortableDir = Join-Path $ReleaseDir "PDFForge-1.0.1-Windows-x64-Portable"
$CertSubject = "CN=Md. Alamin (PDFForge Studio), O=PDFForge Studio, E=alaminprincesarker@gmail.com"
$FriendlyName = "PDFForge Studio Code Signing"

$Targets = @(
    (Join-Path $ReleaseDir "PDFForge-Rust.exe"),
    (Join-Path $ReleaseDir "PDFForge.exe"),
    (Join-Path $ReleaseDir "PDFForge-Setup-1.0.1-Windows-x64.exe"),
    (Join-Path $ReleaseDir "Uninstall-PDFForge.exe"),
    (Join-Path $PortableDir "PDFForge.exe")
) | Where-Object { Test-Path $_ }

if ($Targets.Count -eq 0) {
    throw "No release executables found in $ReleaseDir"
}

$cert = $null
if ($PfxPath -and (Test-Path $PfxPath)) {
    Write-Host "Loading Authenticode certificate from PFX: $PfxPath"
    $cert = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2
    if ($PfxPassword) {
        $cert.Import($PfxPath, $PfxPassword, [System.Security.Cryptography.X509Certificates.X509KeyStorageFlags]::Exportable)
    } else {
        $cert.Import($PfxPath)
    }
} else {
    $cert = Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert |
        Where-Object { $_.Subject -like "*Md. Alamin (PDFForge Studio)*" -and $_.NotAfter -gt (Get-Date).AddDays(7) } |
        Sort-Object NotAfter -Descending |
        Select-Object -First 1

    if (-not $cert -and $CreateLocalCertIfMissing) {
        Write-Host "Creating Code Signing certificate for: $CertSubject"
        $cert = New-SelfSignedCertificate `
            -Type CodeSigningCert `
            -Subject $CertSubject `
            -FriendlyName $FriendlyName `
            -CertStoreLocation "Cert:\CurrentUser\My" `
            -KeyAlgorithm RSA `
            -KeyLength 3072 `
            -HashAlgorithm SHA256 `
            -NotAfter (Get-Date).AddYears(5)
    }
}

if (-not $cert) {
    throw "No Code Signing certificate available. Pass -PfxPath or -CreateLocalCertIfMissing."
}

$CerOut = Join-Path $ReleaseDir "Md-Alamin-PDFForge-Publisher.cer"
Export-Certificate -Cert $cert -FilePath $CerOut -Force | Out-Null
Write-Host "Exported public publisher certificate: $CerOut (Thumbprint=$($cert.Thumbprint))"

if ($InstallLocalTrust) {
    Import-Certificate -FilePath $CerOut -CertStoreLocation "Cert:\CurrentUser\Root" | Out-Null
    Import-Certificate -FilePath $CerOut -CertStoreLocation "Cert:\CurrentUser\TrustedPublisher" | Out-Null
    Write-Host "Installed publisher trust into CurrentUser\Root & CurrentUser\TrustedPublisher."
}

foreach ($exe in $Targets) {
    Unblock-File -Path $exe -ErrorAction SilentlyContinue
    $sig = $null
    if ($TimestampUrl) {
        $sig = Set-AuthenticodeSignature -FilePath $exe -Certificate $cert -HashAlgorithm SHA256 -TimestampServer $TimestampUrl -ErrorAction SilentlyContinue
    }
    if (-not $sig -or $sig.Status -eq [System.Management.Automation.SignatureStatus]::UnknownError) {
        $sig = Set-AuthenticodeSignature -FilePath $exe -Certificate $cert -HashAlgorithm SHA256
    }
    Write-Host ("SIGNED " + (Split-Path $exe -Leaf) + " -> Status=" + $sig.Status + " Subject=" + $sig.SignerCertificate.Subject)
}

$wv = Join-Path $PortableDir "WebView2Loader.dll"
if (Test-Path $wv) {
    Unblock-File -Path $wv -ErrorAction SilentlyContinue
    (Get-Item $wv).LastWriteTime = Get-Date
}
$zipPath = Join-Path $ReleaseDir "PDFForge-1.0.1-Windows-x64-Portable.zip"
Remove-Item $zipPath -Force -ErrorAction SilentlyContinue
tar -a -c -f $zipPath -C $ReleaseDir "PDFForge-1.0.1-Windows-x64-Portable"
Write-Host "Repacked signed Portable ZIP: $zipPath"

