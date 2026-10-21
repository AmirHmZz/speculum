#!/bin/sh
set -eu
export DEBIAN_FRONTEND=noninteractive
# Only fixture generation uses public repositories. Client installs use /work exclusively.
apt-get update -qq
apt-get install -y -qq --no-install-recommends apt-utils gnupg >/dev/null
mkdir -p /work/apt/pool/main/s/speculum /work/apt/dists/fixture/main/binary-amd64
export GNUPGHOME=/tmp/speculum-gnupg
mkdir -m 700 "$GNUPGHOME"
gpg --batch --pinentry-mode loopback --passphrase '' --quick-gen-key 'Speculum Fixture <fixture@example.invalid>' rsa2048 sign 0
gpg --batch --export > /work/apt-key.gpg
for name in speculum-fixture speculum-cold speculum-tampered; do
  dir="/tmp/$name"
  mkdir -p "$dir/DEBIAN" "$dir/usr/share/$name"
  printf 'Package: %s\nVersion: 1.0\nArchitecture: all\nMaintainer: Fixture <fixture@example.invalid>\nDescription: Signed Speculum client fixture\n' "$name" > "$dir/DEBIAN/control"
  printf 'Speculum fixture\n' > "$dir/usr/share/$name/installed"
  dpkg-deb --build "$dir" "/work/apt/pool/main/s/speculum/${name}_1.0_all.deb" >/dev/null
done
cd /work/apt
apt-ftparchive packages pool > dists/fixture/main/binary-amd64/Packages
gzip -n -k dists/fixture/main/binary-amd64/Packages
for file in dists/fixture/main/binary-amd64/Packages*; do
  for algorithm in SHA256 SHA512; do
    command=$(printf '%s' "$algorithm" | tr 'A-Z' 'a-z')sum
    digest=$($command "$file" | cut -d ' ' -f 1)
    mkdir -p "$(dirname "$file")/by-hash/$algorithm"
    cp "$file" "$(dirname "$file")/by-hash/$algorithm/$digest"
  done
done
apt-ftparchive -o APT::FTPArchive::Release::Suite=fixture -o APT::FTPArchive::Release::Codename=fixture \
  -o APT::FTPArchive::Release::Architectures=amd64 -o APT::FTPArchive::Release::Components=main \
  -o APT::FTPArchive::Release::Acquire-By-Hash=true release dists/fixture > dists/fixture/Release
gpg --batch --yes --pinentry-mode loopback --passphrase '' --clearsign -o dists/fixture/InRelease dists/fixture/Release
gpg --batch --yes --pinentry-mode loopback --passphrase '' --detach-sign -o dists/fixture/Release.gpg dists/fixture/Release
cp -r dists/fixture dists/bad-signature
sed -i 's/Suite: fixture/Suite: forged!/' dists/bad-signature/InRelease
# Retain the original digest in Packages, but serve corrupt package bytes.
printf 'tampered package' > pool/main/s/speculum/speculum-tampered_1.0_all.deb
