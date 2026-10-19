"""Derive CI toolchains and publishing metadata from the checked-out source."""

import os
from pathlib import Path
import re


def metadata(ref, sha, event, repository, dockerfile):
    if not re.fullmatch(r"[0-9a-f]{40}", sha):
        raise ValueError("Expected a full Git commit SHA")
    image = f"ghcr.io/{repository.lower()}"
    tags = []
    if ref.startswith("refs/tags/"):
        match = re.fullmatch(r"refs/tags/v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", ref)
        if not match:
            raise ValueError("Release tags must use vX.Y.Z (no prerelease suffix or leading zeros)")
        major, minor, patch = match.groups()
        version = f"{major}.{minor}.{patch}"
        if event == "push":
            tags = [version, f"{major}.{minor}", "latest"]
    elif ref == "refs/heads/main":
        version = "main"
        if event == "push":
            tags = ["main"]
    else:
        version = f"sha-{sha}"
    if tags:
        tags.append(f"sha-{sha}")

    rust = re.search(r"^FROM rust:([0-9.]+)-", dockerfile, re.MULTILINE)
    bun = re.search(r"^FROM oven/bun:([0-9.]+)-", dockerfile, re.MULTILINE)
    if not rust or not bun:
        raise ValueError("Expected numeric Rust and Bun image versions in Dockerfile")
    return {
        "rust": rust[1],
        "bun": bun[1],
        "image": image,
        "version": version,
        "publish": str(bool(tags)).lower(),
        "tags": "\n".join(f"{image}:{tag}" for tag in tags),
    }


if __name__ == "__main__":
    values = metadata(
        os.environ["GITHUB_REF"],
        os.environ["GITHUB_SHA"],
        os.environ["GITHUB_EVENT_NAME"],
        os.environ["GITHUB_REPOSITORY"],
        Path("Dockerfile").read_text(),
    )
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        for key, value in values.items():
            output.write(f"{key}<<SPECULUM_OUTPUT\n{value}\nSPECULUM_OUTPUT\n")
