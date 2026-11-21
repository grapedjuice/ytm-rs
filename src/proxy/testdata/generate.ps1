# Test-only CA and identities. These are never installed in a system trust store.
# Regenerate with: pwsh -File src/proxy/testdata/generate.ps1
$ErrorActionPreference = 'Stop'
$before = [DateTimeOffset]::Parse('2020-01-01T00:00:00Z')
$after = [DateTimeOffset]::Parse('2049-12-31T23:59:59Z')
$hash = [System.Security.Cryptography.HashAlgorithmName]::SHA256
$padding = [System.Security.Cryptography.RSASignaturePadding]::Pkcs1
$caKey = [System.Security.Cryptography.RSA]::Create(2048)
$caRequest = [System.Security.Cryptography.X509Certificates.CertificateRequest]::new('CN=ytm-rs proxy TEST ONLY CA', $caKey, $hash, $padding)
$caRequest.CertificateExtensions.Add([System.Security.Cryptography.X509Certificates.X509BasicConstraintsExtension]::new($true, $false, 0, $true))
$caUsage = [System.Security.Cryptography.X509Certificates.X509KeyUsageFlags]::KeyCertSign -bor [System.Security.Cryptography.X509Certificates.X509KeyUsageFlags]::CrlSign
$caRequest.CertificateExtensions.Add([System.Security.Cryptography.X509Certificates.X509KeyUsageExtension]::new($caUsage, $true))
$ca = $caRequest.CreateSelfSigned($before, $after)
[System.IO.File]::WriteAllBytes((Join-Path $PSScriptRoot 'ca.der'), $ca.Export([System.Security.Cryptography.X509Certificates.X509ContentType]::Cert))
foreach ($name in @('proxy', 'expired')) {
    $key = [System.Security.Cryptography.RSA]::Create(2048)
    $request = [System.Security.Cryptography.X509Certificates.CertificateRequest]::new('CN=localhost', $key, $hash, $padding)
    $request.CertificateExtensions.Add([System.Security.Cryptography.X509Certificates.X509BasicConstraintsExtension]::new($false, $false, 0, $true))
    $usage = [System.Security.Cryptography.X509Certificates.X509KeyUsageFlags]::DigitalSignature -bor [System.Security.Cryptography.X509Certificates.X509KeyUsageFlags]::KeyEncipherment
    $request.CertificateExtensions.Add([System.Security.Cryptography.X509Certificates.X509KeyUsageExtension]::new($usage, $true))
    $eku = [System.Security.Cryptography.OidCollection]::new()
    [void]$eku.Add([System.Security.Cryptography.Oid]::new('1.3.6.1.5.5.7.3.1'))
    $request.CertificateExtensions.Add([System.Security.Cryptography.X509Certificates.X509EnhancedKeyUsageExtension]::new($eku, $true))
    $san = [System.Security.Cryptography.X509Certificates.SubjectAlternativeNameBuilder]::new()
    $san.AddDnsName('localhost')
    $request.CertificateExtensions.Add($san.Build())
    $serial = [byte[]]::new(16)
    [System.Security.Cryptography.RandomNumberGenerator]::Fill($serial)
    $serial[0] = 1
    $expires = if ($name -eq 'expired') { [DateTimeOffset]::Parse('2021-01-01T00:00:00Z') } else { $after }
    $public = $request.Create($ca, $before, $expires, $serial)
    $identity = [System.Security.Cryptography.X509Certificates.RSACertificateExtensions]::CopyWithPrivateKey($public, $key)
    [System.IO.File]::WriteAllBytes((Join-Path $PSScriptRoot "$name.p12"), $identity.Export([System.Security.Cryptography.X509Certificates.X509ContentType]::Pfx, 'test-only'))
    $identity.Dispose()
    $public.Dispose()
    $key.Dispose()
}
$ca.Dispose()
$caKey.Dispose()
