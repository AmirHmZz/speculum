"""Real uv, npm, Bun, and Cargo installs against generated fixture registries.

Requires a built Rust binary and uv/npm/Bun/Cargo on PATH. No external packages are
downloaded. --keep retains the populated demo mirror for dashboard browser QA.
"""
from __future__ import annotations
import argparse
import base64
from contextlib import closing
import hashlib
import http.server
import io
import json
import os
from pathlib import Path
import shutil
import socket
import sqlite3
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
import urllib.parse
import urllib.request
import zipfile


def wheel(name, dependency=None, version="1.0.0"):
    normalized = name.replace("-", "_")
    metadata = f"Metadata-Version: 2.1\nName: {name}\nVersion: {version}\nRequires-Python: >=3.9\n"
    if dependency:
        metadata += f"Requires-Dist: {dependency}==1.0.0\n"
    metadata += "\nFixture package.\n"
    prefix = f"{normalized}-{version}.dist-info"
    files = {
        f"{normalized}/__init__.py": b"VALUE = 42\n",
        f"{prefix}/METADATA": metadata.encode(),
        f"{prefix}/WHEEL": b"Wheel-Version: 1.0\nGenerator: speculum-test\nRoot-Is-Purelib: true\nTag: py3-none-any\n",
    }
    record = "".join(f"{name},sha256={base64.urlsafe_b64encode(hashlib.sha256(data).digest()).decode().rstrip('=')},{len(data)}\n" for name, data in files.items())
    files[f"{prefix}/RECORD"] = (record + f"{prefix}/RECORD,,\n").encode()
    stream = io.BytesIO()
    with zipfile.ZipFile(stream, "w", zipfile.ZIP_DEFLATED) as archive:
        for path, data in files.items():
            archive.writestr(path, data)
    return f"{normalized}-{version}-py3-none-any.whl", stream.getvalue(), metadata.encode()


def archive(files):
    stream = io.BytesIO()
    with tarfile.open(fileobj=stream, mode="w:gz") as tar:
        for name, data in files.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            info.mtime = 0
            tar.addfile(info, io.BytesIO(data))
    return stream.getvalue()


class Registries(http.server.ThreadingHTTPServer):
    daemon_threads = True
    def __init__(self):
        super().__init__(("127.0.0.1", 0), Handler)
        self.base = f"http://127.0.0.1:{self.server_port}"
        self.offline = False
        self.files = {}
        self.documents = {}
        self.downloads = []
        self.auth_leaked = False
        for name, dependency in [("mirror-demo", "mirror-dep"), ("mirror-dep", None)]:
            entries = []
            for version in ["1.0.0", "2.0.0"]:
                filename, body, metadata = wheel(name, dependency, version)
                self.files[f"/files/{filename}"] = body
                self.files[f"/files/{filename}.metadata"] = metadata
                entries.append({"filename": filename, "url": f"{self.base}/files/{filename}", "hashes": {"sha256": hashlib.sha256(body).hexdigest()}, "requires-python": ">=3.9", "core-metadata": {"sha256": hashlib.sha256(metadata).hexdigest()}})
            self.documents[f"/simple/{name}/"] = {"meta": {"api-version": "1.0"}, "name": name, "files": entries}
        for name, dependency in [("mirror-demo", "@fixture/mirror-dep"), ("@fixture/mirror-dep", None)]:
            versions = {}
            for version in ["1.0.0", "2.0.0"]:
                document = {"name": name, "version": version, "main": "index.js"}
                if dependency:
                    document["dependencies"] = {dependency: "1.0.0"}
                body = archive({"package/package.json": json.dumps(document).encode(), "package/index.js": b"module.exports = 42;\n"})
                filename = f"{name.rsplit('/', 1)[-1]}-{version}.tgz"
                self.files[f"/files/{filename}"] = body
                versions[version] = {**document, "dist": {"tarball": f"{self.base}/files/{filename}", "integrity": "sha512-" + base64.b64encode(hashlib.sha512(body).digest()).decode(), "shasum": hashlib.sha1(body).hexdigest()}}
            self.documents[f"/npm/{name}"] = {"name": name, "dist-tags": {"latest": "1.0.0"}, "versions": versions}
        self.documents["/index/config.json"] = {"dl": self.base + "/files/{crate}-{version}.crate"}
        for name, dependency in [("mirror-demo", "mirror-dep"), ("mirror-dep", None)]:
            entries = []
            for version in ["1.0.0", "2.0.0"]:
                manifest = f'[package]\nname = "{name}"\nversion = "{version}"\nedition = "2021"\n'
                if dependency:
                    manifest += f'\n[dependencies]\n{dependency} = "=1.0.0"\n'
                body = archive({f"{name}-{version}/Cargo.toml": manifest.encode(), f"{name}-{version}/src/lib.rs": b"pub const VALUE: u32 = 42;\n"})
                self.files[f"/files/{name}-{version}.crate"] = body
                deps = [{"name": dependency, "req": "=1.0.0", "features": [], "optional": False, "default_features": True, "target": None, "kind": "normal", "registry": None}] if dependency else []
                entries.append({"name": name, "vers": version, "deps": deps, "cksum": hashlib.sha256(body).hexdigest(), "features": {}, "yanked": False})
            self.documents[f"/index/{name[:2]}/{name[2:4]}/{name}"] = "\n".join(json.dumps(entry) for entry in entries) + "\n"


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass
    def do_HEAD(self):
        self.respond(True)
    def do_GET(self):
        self.respond(False)
    def do_POST(self):
        self.rfile.read(int(self.headers.get("content-length", 0)))
        self.send_body(b"{}", "application/json", False)
    def respond(self, head):
        state = self.server
        state.auth_leaked |= bool(self.headers.get("authorization"))
        if state.offline:
            self.send_error(503)
            return
        path = urllib.parse.unquote(urllib.parse.urlsplit(self.path).path)
        if path in state.files:
            if not head:
                state.downloads.append(path)
            self.send_body(state.files[path], "application/octet-stream", head)
        elif path in state.documents:
            document = state.documents[path]
            body = document.encode() if isinstance(document, str) else json.dumps(document).encode()
            self.send_body(body, "text/plain" if isinstance(document, str) else "application/json", head)
        else:
            self.send_error(404)
    def send_body(self, body, content_type, head):
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if not head:
            self.wfile.write(body)


def call(base, path, method="GET", body=None, cookie=None, csrf=None):
    headers = {}
    if body is not None:
        headers["Content-Type"] = "application/json"
    if cookie:
        headers["Cookie"] = cookie
    if csrf:
        headers["X-CSRF-Token"] = csrf
    request = urllib.request.Request(base + path, data=json.dumps(body).encode() if body is not None else None, method=method, headers=headers)
    with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(request, timeout=20) as response:
        data = response.read()
        return json.loads(data) if data else None, response.headers


def run(command, cwd, env):
    completed = subprocess.run(command, cwd=cwd, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=180, creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
    if completed.returncode:
        raise RuntimeError(f"{Path(command[0]).name} failed:\n{completed.stdout}")
    print(f"PASS: {Path(command[0]).name} {' '.join(command[1:3])}", flush=True)


def clients(root, base, token, phase, upstream):
    env = {**os.environ, "NO_PROXY": "127.0.0.1,localhost", "no_proxy": "127.0.0.1,localhost", "UV_NO_PROGRESS": "1"}
    uv = shutil.which("uv")
    npm = shutil.which("npm.cmd" if os.name == "nt" else "npm")
    bun = shutil.which("bun")
    cargo = shutil.which("cargo")
    if not all((uv, npm, bun, cargo)):
        raise RuntimeError("uv, npm, Bun, and Cargo are required for native-client validation")
    folder = root / phase
    folder.mkdir()
    run([uv, "venv", str(folder / "venv"), "--python", sys.executable, "--no-managed-python"], folder, env)
    interpreter = folder / "venv" / ("Scripts/python.exe" if os.name == "nt" else "bin/python")
    index = "speculum=" + base + "/pypi/simple/"
    pythonenv = {**env, "UV_INDEX_SPECULUM_USERNAME": "developer", "UV_INDEX_SPECULUM_PASSWORD": token}
    run([uv, "pip", "install", "--no-cache", "--python", str(interpreter), "--default-index", index, "mirror-demo==1.0.0"], folder, pythonenv)
    run([str(interpreter), "-c", "import mirror_demo, mirror_dep; assert mirror_demo.VALUE == mirror_dep.VALUE == 42"], folder, env)
    js = folder / "js"
    js.mkdir()
    (js / "package.json").write_text(json.dumps({"name": "fixture-client", "version": "1.0.0", "dependencies": {"mirror-demo": "1.0.0"}}))
    (js / ".npmrc").write_text(f"registry={base}/npm/\n{base.removeprefix('http:')}/npm/:_authToken={token}\n")
    bunjs = folder / "bun"
    bunjs.mkdir()
    (bunjs / "package.json").write_text((js / "package.json").read_text())
    (bunjs / ".npmrc").write_text((js / ".npmrc").read_text())
    # Public-registry lockfiles omit custom tarball URLs. Bun reconstructs the
    # conventional /package/-/package-version.tgz paths at the configured mirror.
    packages = {}
    for name, dependency in [("mirror-demo", "@fixture/mirror-dep"), ("@fixture/mirror-dep", None)]:
        package = {"dependencies": {dependency: "1.0.0"}} if dependency else {}
        integrity = upstream.documents[f"/npm/{name}"]["versions"]["1.0.0"]["dist"]["integrity"]
        packages[name] = [f"{name}@1.0.0", "", package, integrity]
    lockfile = {"lockfileVersion": 2, "configVersion": 1, "workspaces": {"": json.loads((bunjs / "package.json").read_text())}, "packages": packages}
    (bunjs / "bun.lock").write_text(json.dumps(lockfile))
    run([bun, "install", "--frozen-lockfile", "--ignore-scripts", "--cache-dir", str(folder / "bun-cache")], bunjs, env)
    assert (bunjs / "node_modules/@fixture/mirror-dep/package.json").exists()
    run([bun, "-e", "if (require('mirror-demo') !== 42 || require('@fixture/mirror-dep') !== 42) throw new Error('Invalid fixture packages')"], bunjs, env)
    run([npm, "install", "--ignore-scripts", "--no-fund", "--cache", str(folder / "npm-cache")], js, env)
    assert (js / "node_modules/@fixture/mirror-dep/package.json").exists()
    rust = folder / "rust"
    (rust / ".cargo").mkdir(parents=True)
    (rust / "src").mkdir()
    (rust / "src/main.rs").write_text("fn main() {}\n")
    (rust / "Cargo.toml").write_text('[package]\nname="fixture-client"\nversion="0.1.0"\nedition="2021"\n[dependencies]\nmirror-demo="=1.0.0"\n')
    (rust / ".cargo/config.toml").write_text(f'[source.crates-io]\nreplace-with="speculum"\n[registries.speculum]\nindex="sparse+{base}/cargo/index/"\ncredential-provider="cargo:token"\n')
    rustenv = {**env, "CARGO_HOME": str(folder / "cargo-home"), "CARGO_REGISTRIES_SPECULUM_TOKEN": token}
    run([cargo, "fetch"], rust, rustenv)
    assert "mirror-dep" in (rust / "Cargo.lock").read_text()


def wait_ready(base, process, log_path):
    deadline = time.monotonic() + 30
    while True:
        try:
            call(base, "/healthz")
            return
        except (OSError, ValueError):
            if process.poll() is not None or time.monotonic() > deadline:
                raise RuntimeError(log_path.read_text())
            time.sleep(0.1)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=Path(__file__).resolve().parents[1] / "target/debug" / ("speculum.exe" if os.name == "nt" else "speculum"))
    parser.add_argument("--keep", action="store_true")
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.exists():
        raise RuntimeError("Build the Rust binary first")
    temp = tempfile.TemporaryDirectory(prefix="speculum-clients-")
    root = Path(temp.name).resolve()
    # Keep Windows executable replacement independent of a running QA preview.
    runtime_binary = root / binary.name
    shutil.copy2(binary, runtime_binary)
    upstream = Registries()
    threading.Thread(target=upstream.serve_forever, daemon=True).start()
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    base = f"http://127.0.0.1:{port}"
    env = {**os.environ, "SPECULUM_LISTEN_IP": "127.0.0.1", "SPECULUM_LISTEN_PORT": str(port), "RUST_LOG": "speculum=warn"}
    with (root / "server.log").open("w") as log:
        process = subprocess.Popen([str(runtime_binary)], cwd=root, env=env, stdout=log, stderr=log, creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
        try:
            wait_ready(base, process, root / "server.log")
            password = next(line.partition("=")[2] for line in (root / "server.log").read_text().splitlines() if line.startswith("SPECULUM_INITIAL_ADMIN_PASSWORD="))
            session, headers = call(base, "/api/login", "POST", {"username": "admin", "password": password})
            assert session["role"] == "admin"
            assert session["must_change_password"] is True
            cookie = headers["Set-Cookie"].split(";")[0]
            session, headers = call(base, "/api/admin/credentials", "PUT", {"current_password": password, "password": "admin-password"}, cookie, session["csrf_token"])
            assert session["must_change_password"] is False
            cookie = headers["Set-Cookie"].split(";")[0]
            csrf = session["csrf_token"]
            # Recovery uses stdin, forces another change, and never regenerates bootstrap credentials.
            process.terminate()
            process.wait(timeout=10)
            reset = subprocess.run([str(runtime_binary), "admin", "reset-password"], input="recovery-fixture-password\n", text=True, cwd=root, env=env, capture_output=True, check=True, creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
            assert "recovery-fixture-password" not in reset.stdout + reset.stderr
            log.flush()
            log.seek(0, os.SEEK_END)
            process = subprocess.Popen([str(runtime_binary)], cwd=root, env=env, stdout=log, stderr=log, creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
            wait_ready(base, process, root / "server.log")
            assert (root / "server.log").read_text().count("SPECULUM_INITIAL_ADMIN_PASSWORD=") == 1
            session, headers = call(base, "/api/login", "POST", {"username": "admin", "password": "recovery-fixture-password"})
            assert session["must_change_password"] is True
            cookie = headers["Set-Cookie"].split(";")[0]
            session, headers = call(base, "/api/admin/credentials", "PUT", {"current_password": "recovery-fixture-password", "password": "admin-password"}, cookie, session["csrf_token"])
            assert session["must_change_password"] is False
            cookie = headers["Set-Cookie"].split(";")[0]
            csrf = session["csrf_token"]
            settings, _ = call(base, "/api/admin/settings", cookie=cookie)
            settings["public_base_url"] = base
            for kind, path in [("pypi", "simple"), ("npm", "npm"), ("cargo", "index")]:
                settings["registries"][kind]["upstream_url"] = f"{upstream.base}/{path}/"
                settings["registries"][kind]["metadata_freshness_seconds"] = 0
            call(base, "/api/admin/settings", "PUT", settings, cookie, csrf)
            user, _ = call(base, "/api/admin/users", "POST", {"username": "developer", "enabled": True}, cookie, csrf)
            user_session, headers = call(base, "/api/login", "POST", {"username": "developer", "password": user["temporary_password"]})
            assert user_session["role"] == "user"
            assert user_session["must_change_password"] is True
            user_cookie = headers["Set-Cookie"].split(";")[0]
            user_session, headers = call(base, "/api/user/password", "PUT", {"current_password": user["temporary_password"], "password": "developer-password"}, user_cookie, user_session["csrf_token"])
            assert user_session["must_change_password"] is False
            user_cookie = headers["Set-Cookie"].split(";")[0]
            issued, _ = call(base, "/api/user/tokens", "POST", {"name": "fixture-clients"}, user_cookie, user_session["csrf_token"])
            token_list, _ = call(base, "/api/user/tokens", cookie=user_cookie)
            assert token_list[0]["last_used_at"] is None
            clients(root, base, issued["token"], "online", upstream)
            assert all("2.0.0" not in path for path in upstream.downloads), upstream.downloads
            assert not upstream.auth_leaked, "Mirror credentials leaked upstream"
            initial = len(upstream.downloads)
            upstream.offline = True
            clients(root, base, issued["token"], "offline", upstream)
            assert len(upstream.downloads) == initial
            with closing(sqlite3.connect(root / "data/speculum.sqlite")) as database:
                assert database.execute("SELECT COUNT(*) FROM artifacts WHERE downloaded_at IS NOT NULL AND version='2.0.0'").fetchone()[0] == 0
            summary, _ = call(base, "/api/admin/summary", cookie=cookie)
            assert summary["total"]["packages"] == 6, summary
            assert summary["total"]["hits"] >= 6, summary
            personal, _ = call(base, "/api/user/summary", cookie=user_cookie)
            assert personal["total"]["requests"] == summary["total"]["requests"], personal
            assert personal["total"]["packages"] == 6, personal
            token_list, _ = call(base, "/api/user/tokens", cookie=user_cookie)
            assert isinstance(token_list[0]["last_used_at"], int)
            print(f"PASS: on-demand caching, dependencies, scopes, authenticated Cargo, offline installs; {initial} upstream files", flush=True)
            if args.keep:
                upstream.offline = False
                print(f"PREVIEW: {base}/admin (admin / admin-password; user developer / developer-password)", flush=True)
                print(f"Runtime directory: {root}", flush=True)
                threading.Event().wait()
        finally:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
            upstream.shutdown()
            log.close()
            temp.cleanup()


if __name__ == "__main__":
    main()
