"""Signed APT/APK installs through Speculum, using disposable Docker clients.

Public images/tools are fetched during fixture setup only. Every tested install
uses a generated repository with no fallback sources or trust-bypass flags.
"""
from __future__ import annotations

import argparse
import functools
import http.server
import hashlib
import gzip
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import threading
import zlib
import uuid

from native_clients import call, wait_ready


IMAGES = ["debian:12-slim", "ubuntu:24.04", "alpine:3.22", "alpine:3.23"]


def docker(args, script=None, *, check=True):
    # Binary stdin prevents Windows from translating shell scripts to CRLF.
    container = f"speculum-distro-{uuid.uuid4().hex[:12]}" if args[0] == "run" else None
    if container:
        args = ["run", "--name", container, *args[1:]]
    try:
        result = subprocess.run(["docker", *args], input=None if script is None else script.encode(), capture_output=True, timeout=600)
    except BaseException:
        if container:
            subprocess.run(["docker", "rm", "--force", container], capture_output=True, timeout=30, check=False)
        raise
    result.stdout = result.stdout.decode("utf-8", errors="replace")
    result.stderr = result.stderr.decode("utf-8", errors="replace")
    if check and result.returncode:
        raise RuntimeError(f"Docker failed ({args[0]}):\n{result.stdout}\n{result.stderr}")
    return result


class Fixture(http.server.ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, root):
        super().__init__(("127.0.0.1", 0), functools.partial(Handler, directory=str(root)))
        self.offline = False
        self.auth_leaked = False
        self.downloads = []


class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        self.server.auth_leaked |= bool(self.headers.get("Authorization"))
        if self.server.offline:
            self.send_error(503)
        else:
            self.server.downloads.append(self.path)
            super().do_GET()

    def log_message(self, *_):
        pass


def run_client(root, image, base, phase):
    common = ["run", "--rm", "--add-host=host.docker.internal:host-gateway",
              "-v", f"{root}:/work:ro", "-e", f"MIRROR={base}", "-e", f"PHASE={phase}",
              "-e", "http_proxy=", "-e", "https_proxy=", "-e", "HTTP_PROXY=", "-e", "HTTPS_PROXY=",
              "-e", "ALL_PROXY=", "-e", "NO_PROXY=host.docker.internal", "-i", image, "sh", "-s"]
    if image.startswith(("debian", "ubuntu")):
        kind = "debian" if image.startswith("debian") else "ubuntu"
        script = r'''
set -eu
rm -f /etc/apt/sources.list /etc/apt/sources.list.d/*
cp /work/apt-key.gpg /usr/share/keyrings/speculum-fixture.gpg
cp /work/apt-auth.conf /etc/apt/auth.conf.d/speculum.conf
chmod 600 /etc/apt/auth.conf.d/speculum.conf
printf 'Types: deb\nURIs: %s/KIND/\nSuites: fixture\nComponents: main\nSigned-By: /usr/share/keyrings/speculum-fixture.gpg\n' "$MIRROR" > /etc/apt/sources.list.d/speculum.sources
apt-get update -qq
apt-get install -y -qq --no-install-recommends speculum-fixture
test "$(cat /usr/share/speculum-fixture/installed)" = 'Speculum fixture'
if [ "$PHASE" = online ]; then
  if apt-get install -y -qq speculum-tampered; then echo 'Accepted tampered package'; exit 1; fi
  sed -i 's/Suites: fixture/Suites: bad-signature/' /etc/apt/sources.list.d/speculum.sources
  rm -rf /var/lib/apt/lists/*
  if apt-get update -qq; then echo 'Accepted invalid signature'; exit 1; fi
else
  if apt-get install -y -qq speculum-cold; then echo 'Uncached package worked offline'; exit 1; fi
fi
'''.replace("KIND", kind)
    else:
        script = r'''
set -eu
cp /work/apk-key.rsa.pub /etc/apk/keys/speculum-fixture.rsa.pub
# libfetch selects the key by its original filename, recorded by fixture setup.
cp /work/apk-key.rsa.pub "/etc/apk/keys/$(cat /work/apk-key-name)"
cp /work/apk.netrc /root/.netrc
chmod 600 /root/.netrc
printf '%s/alpine/fixture/main\n' "$MIRROR" > /etc/apk/repositories
apk update
apk add --no-cache speculum-fixture
test "$(cat /usr/share/speculum-fixture/installed)" = 'Speculum fixture'
if [ "$PHASE" = online ]; then
  if apk add --no-cache speculum-tampered; then echo 'Accepted tampered package'; exit 1; fi
  printf '%s/alpine/bad-signature/main\n' "$MIRROR" > /etc/apk/repositories
  if apk update; then echo 'Accepted invalid signature'; exit 1; fi
else
  if apk add --no-cache speculum-cold; then echo 'Uncached package worked offline'; exit 1; fi
fi
'''
    result = docker(common, script)
    print(f"PASS: {image} {phase}: signed update/install and negative checks", flush=True)
    (root / f"{image.replace(':', '-')}-{phase}.log").write_text(result.stdout + result.stderr, encoding="utf-8")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=Path("target/debug/speculum.exe" if os.name == "nt" else "target/debug/speculum"))
    parser.add_argument("--keep", action="store_true", help="Leave the populated mirror running for browser QA")
    parser.add_argument("--client", action="append", choices=IMAGES, help="Run selected clients while debugging; the default runs all four")
    args = parser.parse_args()
    images = args.client or IMAGES
    binary = args.binary.resolve()
    scripts = Path(__file__).resolve().parent / "distro-fixtures"
    runtime = tempfile.TemporaryDirectory(prefix="speculum-distros-")
    root = Path(runtime.name).resolve()
    process = None
    upstream = None
    log = None
    try:
        for image in images:
            if docker(["image", "inspect", image], check=False).returncode:
                docker(["pull", image])
        for image, name in [(IMAGES[0], "apt"), (IMAGES[2], "apk")]:
            script = (scripts / f"{name}.sh").read_text()
            fingerprint = hashlib.sha256((image + script).encode()).hexdigest()[:16]
            cache = scripts.parent / ".runtime" / "distro-fixtures" / f"{name}-{fingerprint}"
            if not cache.exists():
                print(f"Generating signed {name.upper()} fixture", flush=True)
                cache.parent.mkdir(parents=True, exist_ok=True)
                with tempfile.TemporaryDirectory(prefix=f"{name}-", dir=cache.parent) as staging:
                    ownership = []
                    generation_script = script
                    if hasattr(os, "getuid"):
                        ownership = ["-e", f"FIXTURE_UID={os.getuid()}", "-e", f"FIXTURE_GID={os.getgid()}"]
                        # Root-created bind-mount files must remain removable by Linux CI's user,
                        # including when a generator fails partway through its work.
                        generation_script = "trap 'chown -R \"$FIXTURE_UID:$FIXTURE_GID\" /work' EXIT\n" + script
                    result = docker(["run", "--rm", "-v", f"{Path(staging).resolve()}:/work", *ownership, "-i", image, "sh", "-s"], generation_script)
                    shutil.copytree(staging, cache)
                (cache / f"{name}-generation.log").write_text(result.stdout + result.stderr, encoding="utf-8")
            for entry in cache.iterdir():
                if entry.is_dir():
                    shutil.copytree(entry, root / entry.name)
                else:
                    shutil.copy2(entry, root / entry.name)
        # The signer embeds the original public key name in APK signature members.
        import tarfile
        with tarfile.open(root / "apk/fixture/main/x86_64/APKINDEX.tar.gz", "r:gz") as archive:
            signature = next(member for member in archive if member.name.startswith(".SIGN.RSA."))
        (root / "apk-key-name").write_text(signature.name.removeprefix(".SIGN.RSA."))
        # Corrupt only the RSA signature, retaining a valid compressed index and tar layout.
        signed_index = (root / "apk/fixture/main/x86_64/APKINDEX.tar.gz").read_bytes()
        decoder = zlib.decompressobj(16 + zlib.MAX_WBITS)
        signature_tar = bytearray(decoder.decompress(signed_index))
        signature_tar[signature.offset_data] ^= 1
        (root / "apk/bad-signature/main/x86_64/APKINDEX.tar.gz").write_bytes(gzip.compress(signature_tar, mtime=0) + decoder.unused_data)
        upstream = Fixture(root)
        threading.Thread(target=upstream.serve_forever, daemon=True).start()
        with socket.socket() as probe:
            probe.bind(("0.0.0.0", 0))
            port = probe.getsockname()[1]
        local_base = f"http://127.0.0.1:{port}"
        client_base = f"http://host.docker.internal:{port}"
        executable = root / binary.name
        shutil.copy2(binary, executable)
        env = {**os.environ, "SPECULUM_LISTEN_IP": "0.0.0.0", "SPECULUM_LISTEN_PORT": str(port)}
        log = (root / "server.log").open("w+", encoding="utf-8")
        process = subprocess.Popen([str(executable)], cwd=root, env=env, stdout=log, stderr=log,
                                   creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
        wait_ready(local_base, process, root / "server.log")
        password = next(line.split("=", 1)[1] for line in (root / "server.log").read_text().splitlines() if line.startswith("SPECULUM_INITIAL_ADMIN_PASSWORD="))
        session, headers = call(local_base, "/api/login", "POST", {"username": "admin", "password": password})
        cookie = headers["Set-Cookie"].split(";", 1)[0]
        session, headers = call(local_base, "/api/admin/credentials", "PUT", {"current_password": password, "password": "fixture-admin-password"}, cookie, session["csrf_token"])
        cookie = headers["Set-Cookie"].split(";", 1)[0]
        csrf = session["csrf_token"]
        settings, _ = call(local_base, "/api/admin/settings", cookie=cookie)
        settings["public_base_url"] = client_base
        origin = f"http://127.0.0.1:{upstream.server_port}"
        for kind in ["debian", "debian-security", "ubuntu", "ubuntu-security", "ubuntu-ports", "alpine"]:
            settings["registries"][kind]["upstream_url"] = f"{origin}/{'apk' if kind == 'alpine' else 'apt'}/"
        call(local_base, "/api/admin/settings", "PUT", settings, cookie, csrf)
        user, _ = call(local_base, "/api/admin/users", "POST", {"username": "distro-user", "enabled": True}, cookie, csrf)
        token, _ = call(local_base, f"/api/admin/users/{user['id']}/tokens", "POST", {"name": "native-distro-client"}, cookie, csrf)
        # Secrets stay in temporary mounted files, not Docker command arguments or image layers.
        (root / "apt-auth.conf").write_text("\n\n".join(f"machine {client_base}/{kind}/\nlogin distro-user\npassword {token['token']}" for kind in ["debian", "ubuntu"]))
        (root / "apk.netrc").write_text(f"machine host.docker.internal\nlogin distro-user\npassword {token['token']}\n")
        for image in images:
            run_client(root, image, client_base, "online")
        assert not upstream.auth_leaked, "Client credentials reached the fixture upstream"
        initial_downloads = len(upstream.downloads)
        upstream.offline = True
        for image in images:
            run_client(root, image, client_base, "offline")
        assert len(upstream.downloads) == initial_downloads
        print("PASS: signed APT/APK fixtures, clean-cache offline installs, missing packages and tampering rejected", flush=True)
        if args.keep:
            upstream.offline = False
            print(f"PREVIEW: {local_base}/admin (admin / fixture-admin-password)", flush=True)
            print(f"Runtime directory: {root}", flush=True)
            threading.Event().wait()
    except Exception:
        if upstream is not None:
            print(f"Fixture requests: {upstream.downloads[-16:]}", flush=True)
        if log is not None:
            log.flush()
            lines = (root / 'server.log').read_text(encoding='utf-8', errors='replace').splitlines()
            print('\n'.join(line for line in lines[-35:] if 'SPECULUM_INITIAL_ADMIN_PASSWORD=' not in line), flush=True)
        raise
    finally:
        if process is not None:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        if upstream is not None:
            upstream.shutdown()
            upstream.server_close()
        if log is not None:
            log.close()
        runtime.cleanup()


if __name__ == "__main__":
    main()
