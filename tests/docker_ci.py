"""Smoke-test an already built image in a disposable Compose project.

Usage: python tests/docker_ci.py --image speculum:ci
Only this script's unique project, network, and volume are removed on exit.
"""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import uuid

ROOT = Path(__file__).resolve().parents[1]
BOOTSTRAP = re.compile(r"SPECULUM_INITIAL_ADMIN_PASSWORD=([^\s]+)")


def run(args, **kwargs):
    result = subprocess.run(args, text=True, capture_output=True, timeout=240, **kwargs)
    if result.returncode:
        detail = BOOTSTRAP.sub("SPECULUM_INITIAL_ADMIN_PASSWORD=[redacted]", result.stdout + result.stderr)
        raise RuntimeError(f"{args[0]} failed ({result.returncode}):\n{detail}")
    return result.stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True)
    args = parser.parse_args()
    project = "speculum-ci-" + uuid.uuid4().hex[:12]
    client_name = project + "-client"
    print(f"Smoke-test project: {project}", flush=True)
    with tempfile.TemporaryDirectory(prefix="speculum-compose-") as folder:
        override = Path(folder) / "image.yaml"
        override.write_text(json.dumps({"services": {"speculum": {"image": args.image}}}))
        compose = ["docker", "compose", "--project-name", project,
                   "--file", str(ROOT / "compose.yaml"), "--file", str(override)]
        up = ["up", "--detach", "--no-build", "--pull", "never", "--wait", "--wait-timeout", "120"]
        try:
            run(["docker", "pull", "node:24-bookworm-slim"])
            run(compose + up)
            container = run(compose + ["ps", "--quiet", "speculum"]).strip()
            info = json.loads(run(["docker", "inspect", container]))[0]
            assert info["Config"]["User"] == "10001:10001", "Container must run as the application user"
            assert info["HostConfig"]["ReadonlyRootfs"], "Container root filesystem must be read-only"
            assert not any(info["NetworkSettings"]["Ports"].values()), "Service must not publish host ports"
            assert info["State"]["Health"]["Status"] == "healthy", "Container must be healthy"
            logs = run(compose + ["logs", "--no-color", "speculum"])
            passwords = BOOTSTRAP.findall(logs)
            assert len(passwords) == 1, "Fresh storage must generate exactly one initial password"
            if os.environ.get("GITHUB_ACTIONS") == "true":
                print(f"::add-mask::{passwords[0]}", flush=True)
            smoke = (ROOT / "tests/docker_smoke.js").read_text()
            client = ["docker", "run", "--rm", "--name", client_name,
                      "--interactive", "--network", project + "_default"]
            env = {**os.environ, "SPECULUM_TEST_PASSWORD": passwords[0]}
            print(run(client + ["--env", "SPECULUM_TEST_PASSWORD", "node:24-bookworm-slim",
                                "node", "--input-type=module"], input=smoke, env=env), end="")
            run(compose + ["restart", "speculum"])
            run(compose + up)
            print(run(client + ["--env", "SPECULUM_TEST_PHASE=restart", "node:24-bookworm-slim",
                                "node", "--input-type=module"], input=smoke), end="")
            logs = run(compose + ["logs", "--no-color", "speculum"])
            assert len(BOOTSTRAP.findall(logs)) == 1, "Restart must not generate another initial password"
        except Exception:
            try:
                logs = run(compose + ["logs", "--no-color", "--tail", "100", "speculum"])
                print(BOOTSTRAP.sub("SPECULUM_INITIAL_ADMIN_PASSWORD=[redacted]", logs), flush=True)
            except Exception as error:
                print(f"Could not collect container logs: {error}", flush=True)
            raise
        finally:
            try:
                # A timed-out Docker CLI may leave its client container running.
                subprocess.run(["docker", "rm", "--force", client_name],
                               capture_output=True, timeout=30, check=False)
            finally:
                run(compose + ["down", "--volumes", "--remove-orphans", "--timeout", "10"])


if __name__ == "__main__":
    main()
