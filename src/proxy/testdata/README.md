These identities are **test-only**, signed by a private test CA that is never installed in a system trust store. The PKCS#12 password is `test-only`; the identities contain no production credentials.

- `ca.der`: test CA, valid from 2020 through 2049.
- `proxy.p12`: server identity for DNS name `localhost`, valid from 2020 through 2049. It deliberately has no IP SAN, so connecting by `127.0.0.1` tests hostname rejection.
- `expired.p12`: the same DNS identity with validity ending in 2021.

Tests add the CA only to an explicitly constructed connector. Production uses the platform's default certificate trust and hostname verification.

Regenerate the fixtures with `pwsh -File src/proxy/testdata/generate.ps1` from the repository root.
