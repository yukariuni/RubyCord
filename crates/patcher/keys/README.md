The patcher's signing identity. Keep these: every patched APK must be signed by them to install
over an earlier one.

- `phoenix.pk8`, `phoenix.der`: phoenix's key (`CN=phoenix-unlock`), the one APKs are signed with.
- `lineage`: the v3 proof of rotation from the key earlier builds used (`CN=Valenced Test`, the
  Valenced test keystore) to phoenix's, so installs signed with either update in place. It holds
  certificates and signatures only, no private key; the old key is not in this repo.

APKs carry only a v3 signature (v1/v2 would need the old private key), so they install on Android 9+.
The key is public: it only keeps patched installs updatable, it does not vouch for anything.
