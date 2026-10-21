#!/bin/sh
set -eu
apk add --no-cache abuild openssl >/dev/null
abuild-keygen -a -n >/dev/null
cp /root/.abuild/*.rsa.pub /etc/apk/keys/
mkdir -p /work/apk/fixture/main/x86_64
for name in speculum-fixture speculum-cold speculum-tampered; do
  mkdir -p "/tmp/build/$name"
  cd "/tmp/build/$name"
  cat > APKBUILD <<EOF
pkgname=$name
pkgver=1.0
pkgrel=0
pkgdesc="Signed Speculum client fixture"
url="https://example.invalid"
arch="noarch"
license="MIT"
options="!check"
package() {
  mkdir -p "\$pkgdir/usr/share/$name"
  printf 'Speculum fixture\\n' > "\$pkgdir/usr/share/$name/installed"
}
EOF
  # These data-only fixtures have no build dependencies or compiler steps.
  REPODEST=/tmp/packages abuild -F -d >/dev/null
done
find /tmp/packages -name '*.apk' -exec cp '{}' /work/apk/fixture/main/x86_64/ ';'
mkdir -p /work/apk/fixture/main/noarch
cp /work/apk/fixture/main/x86_64/*.apk /work/apk/fixture/main/noarch/
cp /root/.abuild/*.rsa.pub /work/apk-key.rsa.pub
cd /work/apk/fixture/main/x86_64
apk index -o APKINDEX.tar.gz ./*.apk >/dev/null
abuild-sign APKINDEX.tar.gz
mkdir -p /work/apk/bad-signature/main/x86_64
cp APKINDEX.tar.gz /work/apk/bad-signature/main/x86_64/APKINDEX.tar.gz
printf 'invalid signed index' > /work/apk/bad-signature/main/x86_64/APKINDEX.tar.gz
printf 'tampered package' > speculum-tampered-1.0-r0.apk
printf 'tampered package' > ../noarch/speculum-tampered-1.0-r0.apk
