# Speculum

A self-hosted, on-demand package mirror for **PyPI, npm, and crates.io**, with a
React/shadcn admin and user dashboards embedded in a single Rust service. Speculum listens
on HTTP; place your own TLS-terminating reverse proxy in front of it.

## Start with Docker Compose

1. Run `docker compose pull` followed by `docker compose up -d --wait`.
   Compose uses the prebuilt Linux AMD64 image `ghcr.io/amirhmzz/speculum:latest`;
   no registry login or credential environment variables are needed once the package is public.
2. Read the generated initial password with `docker compose logs speculum`.
   Look for `SPECULUM_INITIAL_ADMIN_PASSWORD=...`. It is printed once on a fresh
   database, even when `RUST_LOG` filters other startup messages.
3. Connect your reverse proxy to the Compose network and route it to
   `http://speculum:8080`. Open `/admin` on your public URL and sign in with the
   username **admin** and the generated password. Choose a new password before
   accessing the dashboard.
4. In **Settings**, set the public base URL to the HTTPS origin your clients use.
5. Configure each registry's proxy, authentication, and cache lifetime as needed.
6. Add accounts under **Mirror users**. Copy each generated temporary password
   and share it with that user. They sign in at `/account`, change their password,
   and create package tokens. Client configuration is available in both dashboards.

The container listens on `0.0.0.0:8080`, accessible within its Docker network.
Compose does not publish any ports to the host. Attach your reverse proxy to the
same Docker network and forward to `http://speculum:8080`, preserving the request
path. Do not strip the `/pypi/`, `/npm/`, `/cargo/`, or `/api/` prefixes. Allow enough request time
for a cold large artifact: it is verified completely before being served.

The named `speculum-data` volume stores configuration, users, sessions, statistics,
metadata, artifacts, and `secret.key`. Runtime settings are edited through the
dashboard and take effect without restarting. The administrator username is always
`admin`. Restarts preserve the chosen password and never print it. Bootstrap
passwords are stored only as hashes; use password recovery if the initial log is lost.

### Updates and image selection

From the same directory and Compose project used for installation, update manually:

```sh
docker compose pull
docker compose up -d --wait
```

Updates reuse the existing data volume. Back up the complete volume before an
upgrade, especially when the release changes the database schema (see Operations
and recovery). Speculum does not migrate existing schemas automatically.

The `latest` tag follows successful stable release-tag builds. To pin a version or
digest, set `SPECULUM_IMAGE` in the `.env` file beside `compose.yaml`, then run the
commands above:

```dotenv
SPECULUM_IMAGE=ghcr.io/amirhmzz/speculum:0.2.1
# Or use the digest shown in the publishing workflow summary:
# SPECULUM_IMAGE=ghcr.io/amirhmzz/speculum@sha256:<published-digest>
```

Use `ghcr.io/amirhmzz/speculum:main` to follow tested development builds. To build
your checkout locally instead, explicitly include the development override on
every Compose command (including logs, restart, and recovery):

```sh
docker compose -f compose.yaml -f compose.build.yaml up --build -d --wait
```

This builds `speculum:local` and preserves the same service settings and data
volume. The standard Compose file never builds from source.

## GitHub CI and container publishing

GitHub Actions validates pull requests targeting `main`, pushes to `main`, and
version-tag pushes. It builds the dashboard using its frozen Bun lockfile, runs
Rust formatting/Clippy/tests, and tests real uv/npm/Bun/Cargo installs against
fixture registries, including offline cache use. Rust and Bun versions are read
from the Dockerfile. A separate job builds the Linux AMD64 image and smoke-tests
its health, embedded dashboards, account setup, and restart persistence using a
unique temporary Compose project. Test storage is removed afterward.

Run the publishing-boundary and container checks locally with:

```sh
python -m unittest discover -s tests -p 'test_ci_*.py'
docker compose -f compose.yaml -f compose.build.yaml build
python tests/docker_ci.py --image speculum:local
```

Publishing runs only after every check passes on a main or release-tag push.
It loads and verifies the tested image from a workflow artifact rather than
rebuilding it. Pull requests never publish. Published image tags and the registry
digest appear in the publishing job's summary:

| Trigger | Tags |
| --- | --- |
| Push to `main` | `main`, `sha-<full-commit>` |
| Push `v0.2.1` | `0.2.1`, `0.2`, `latest`, `sha-<full-commit>` |

New release tags must use `vX.Y.Z`, without prerelease suffixes or leading zeros.
Existing tags are unchanged. After updating package versions/changelog as needed
and merging the intended release commit to `main`, create a new, unused tag:

```sh
git tag v0.2.1
git push origin v0.2.1
```

Replace the example with your intended version. The workflow does not change
package versions, create GitHub releases, or update running servers. Each successful
release-tag push updates its minor alias and `latest`, including a backport; pin
an exact version or digest when that behavior is undesirable. The SHA tag identifies
the source commit; main and release builds of that commit may have different
version labels. Use a digest to identify the exact image.

### First publication and public access

Enable GitHub Actions for the repository and allow the pinned actions used by
the workflow. The publishing job uses the built-in `GITHUB_TOKEN` with
`packages: write`; no personal access token or repository secret is required.
The OCI source label links the image to this repository. If a package with this
name already exists, ensure this repository has Actions write access to it.

GHCR initially creates packages as **private**, even for public repositories.
After the first successful publication, open your GitHub profile's **Packages**,
select **speculum**, open **Package settings**, and change its visibility to
**Public**. This is a one-time package-owner setting; the workflow does not change
visibility. See [GitHub's Container registry documentation](https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry).

Verify an anonymous pull using an empty Docker credential configuration:

```sh
docker --config /tmp/speculum-ghcr-anonymous pull ghcr.io/amirhmzz/speculum:main
```

Use a fresh, empty directory for `--config` (on Windows, use a temporary Windows
path). `latest` becomes available after the first successful `vX.Y.Z` build; a
main-only first publication provides `main` and SHA tags. Until a stable image is
published, set `SPECULUM_IMAGE=ghcr.io/amirhmzz/speculum:main` or use the local-build
override.

## Package-manager configuration

All registries initially require authentication. Mirror credentials are separate
from dashboard passwords. Disabling registry authentication opens only
that registry; the admin API always requires an admin session.
Mirror-user passwords authenticate only the user dashboard. All protected package
downloads use tokens, including Python Basic-auth passwords. Disabling a user
immediately blocks dashboard access and all their tokens; deletion also removes
their tokens, sessions, and personal statistics. Admins can still issue tokens
for users. Password resets generate a new temporary password, require another
password change, and sign out user sessions while preserving package tokens.

Both dashboard roles sign in at `/login` using one form. Speculum detects the
account role and redirects administrators to `/admin` and mirror users to
`/account`, including when a password change is required.

The user dashboard exposes only the signed-in user's token management and usage.
Its `/api/user/` APIs provide login/session/logout, password changes, tokens,
summary, paginated package activity, and public client configuration. Admin
sessions and user sessions are separate; users cannot access `/api/admin/`.
The Tokens table shows when each token last authenticated a mirror request,
including metadata and HEAD requests. Unused tokens show **Never**. Invalid
credentials and disabled users do not update this timestamp.

**uv** — use a named index in `pyproject.toml`, with credentials in environment
variables:

```toml
[[tool.uv.index]]
name = "speculum"
url = "https://mirror.example.com/pypi/simple/"
default = true
```

Set `UV_INDEX_SPECULUM_USERNAME` to the mirror username and
`UV_INDEX_SPECULUM_PASSWORD` to a token belonging to that user. Standard pip
clients use the same username and token through HTTP Basic authentication
(for example through their credential store or `.netrc`). The token occupies
the client's password field; Speculum does not store or accept mirror-user
passwords. Renaming a user requires updating that username in Python clients.
HTML and JSON Simple API responses, metadata sidecars, and byte ranges
are supported. The index root lists discovered projects; named projects are
resolved lazily without downloading the entire upstream project catalogue.

**npm** — put the following in `.npmrc` and set `SPECULUM_TOKEN`:

```ini
registry=https://mirror.example.com/npm/
//mirror.example.com/npm/:_authToken=${SPECULUM_TOKEN}
```

Scoped packages, exact versions, tags, full/abbreviated metadata, and npm audit
requests are supported. Conventional tarball paths (`/npm/name/-/name-version.tgz`
and `/npm/@scope/name/-/name-version.tgz`) resolve on demand and share the cache
with metadata-generated download URLs, including requests from existing Bun
lockfiles. npm login/publishing are not supported; issue tokens in
the dashboard. Existing lockfiles that contain direct upstream tarball URLs can
bypass the mirror: regenerate those lockfiles using the mirror registry.

Mirror roots also accept URLs without a trailing slash. `/npm` and
`/pypi/simple` redirect to their canonical roots; `/cargo/index` redirects to
the sparse registry's `config.json`. Authentication and registry settings
apply to these aliases.

**Bun** — use the same npm endpoint. Put this in `bunfig.toml` at the project
root, set `SPECULUM_TOKEN` to a mirror-user token, and run `bun install`:

```toml
[install]
registry = { url = "https://mirror.example.com/npm/", token = "$SPECULUM_TOKEN" }
```

Bun also reads the `.npmrc` configuration above. When npm authentication is
disabled, use `registry = "https://mirror.example.com/npm/"` without a token.
See [Bun registry configuration](https://bun.sh/guides/install/custom-registry).

**Deno** — put this `.npmrc` in the project root and set `SPECULUM_TOKEN` to a
mirror-user token:

```ini
registry=https://mirror.example.com/npm/
//mirror.example.com/npm/:_authToken=${SPECULUM_TOKEN}
```

Declare npm dependencies in `deno.json`, for example:

```json
{
  "imports": {
    "chalk": "npm:chalk@5"
  }
}
```

Run `deno install`, then import the configured alias in your code (for example
`import chalk from "chalk"`). Existing `package.json` dependencies also use the
configured npm registry. When npm authentication is disabled, omit the
`_authToken` line. Only Deno's npm dependencies use Speculum; `jsr:` packages and
HTTP imports use their original sources. See
[Deno npm registry configuration](https://docs.deno.com/runtime/fundamentals/node/#private-registries).

**Cargo** — put this in `.cargo/config.toml` and set
`CARGO_REGISTRIES_SPECULUM_TOKEN`:

```toml
[source.crates-io]
replace-with = "speculum"

[registries.speculum]
index = "sparse+https://mirror.example.com/cargo/index/"
credential-provider = "cargo:token"
```

Cargo's `config.json` is public discovery data containing the mirror download
URL and authentication flag. Index entries and crate files are protected when
Cargo authentication is enabled. Only the sparse index is supported. Git/path
dependencies and package publishing are outside the mirror's scope.

## Cache and proxy behavior

- Metadata requests never prefetch package archives. Clients resolve normal
  dependencies, and each requested wheel, sdist, tarball, or crate is cached
  independently. Python metadata sidecars are downloaded only when requested.
- Concurrent requests for an artifact share one upstream download. Downloads
  stream to disk, verify advertised SHA-256/SHA-512/SHA-1 checksums, and become
  visible only after completion. No full archive is buffered in memory.
- An empty artifact lifetime means **forever**. Otherwise expiry is the original
  completed download timestamp plus the **current** registry lifetime. Extending
  the setting extends every artifact still stored; switching to forever removes
  expiry. Reads do not reset the timestamp. Already deleted files are downloaded
  again when requested.
- Expired artifacts cannot be served even before background cleanup deletes
  them. Manual deletion waits for active readers/downloads and preserves traffic
  statistics. A later client request may populate the cache again.
- Metadata is fresh for five minutes by default. Conditional refreshes reuse
  upstream ETags/Last-Modified. Retained metadata and unexpired files remain
  available during upstream failures; uncached/expired files fail with 502.
  Upstream 404s are returned as 404s rather than silently using old metadata.
- Changing an upstream URL selects a different cache identity. Download URLs
  from the previous upstream return 410 and require refreshed client metadata.
  Old files remain visible in the dashboard until cleared or expired.
- Each registry has an independent optional proxy. `socks5h://` resolves DNS at
  the proxy; `socks5://` resolves it locally. HTTP(S) proxies are also accepted.
  Proxy credentials go in the separate username/password fields. In Docker,
  use `host.docker.internal` to reach a proxy running on the host, rather than
  `127.0.0.1`. Disabling the proxy uses direct connections, ignoring system proxy
  environment variables.
- Upstream and redirect hosts must match the configured upstream host or an
  explicitly allowed artifact host. The default CDN hosts are already included.
  Client credentials, cookies, and admin sessions are never forwarded upstream.

Overview and package pages show persistent lifetime request/hit/miss/download/
failure and bandwidth totals. HEAD and index metadata requests do not count as
file downloads; Python metadata sidecar GETs do. Upstream traffic includes bytes
received during failed downloads. Client traffic counts file bytes emitted by
the response stream; it is not an acknowledgement of client receipt. Storage
totals reflect files currently on disk, including sidecars, and can briefly
include expired files while cleanup is running. There are no daily history charts.

Dashboard passwords use Argon2id. Tokens and session IDs are stored hashed. Only
the first 12 characters of a token (including `spm_`) are also stored in plaintext
for identification in the user's Tokens table. The full token is shown only when
it is created. Dashboard mutations require CSRF tokens. Temporary passwords must
be changed before accessing dashboard features. Both session lifetimes are configurable,
defaulting to 12 hours. Proxy secrets are encrypted with the persisted
data-volume key, masked in settings responses, and never logged. Back up that
key together with the database. Ten failed login attempts per dashboard role from one direct
peer IP block login for five minutes; forwarded client-IP headers are not trusted.
Dashboard session cookies are HttpOnly/SameSite and Secure when the public URL uses
HTTPS. All public URLs come from the explicit setting rather than forwarded headers.

Personal lifetime statistics count artifact requests made with valid tokens,
including on public registries. Anonymous or invalid credentials on public
registries contribute only to global totals. Personal views show requests, cache
hits/misses, upstream downloads, failures, traffic, and package activity without
shared cache storage or other users' usage. Cache deletion and token revocation
preserve history. Upstream downloads and bytes belong to the requesting user who
performs the fetch; waiting users are counted only for their own requests and
delivered bytes. Deleting a user preserves global traffic totals.

Speculum does not run database migrations. Changes to existing table schemas
require a fresh application database. Token usage is stored in a separate
table created automatically at startup, so enabling this feature preserves
existing users and tokens.

## Local development and checks

Requires Rust 1.89+ (including a native C toolchain for SQLite/TLS dependencies),
Bun, and Node 20.19+ or 22.12+.
On Windows with the GNU toolchain, include your MinGW `bin` directory on PATH
(for example `C:\msys64\mingw64\bin`).

```text
cd dashboard
bun install --frozen-lockfile
bun run build
cd ..
cargo build --locked
```

Optionally set `SPECULUM_LISTEN_IP` and `SPECULUM_LISTEN_PORT`, then run
`cargo run --locked`. First startup generates and prints the initial admin
password; change it at `/admin`. Mirror users sign in at `/account`.
The process uses a fixed `data/` directory beneath its working directory; only
one instance may open that directory. The Docker working directory is `/app`.
For UI development, run the backend on 8080 and `bun run dev` in `dashboard/`.
Rebuild dashboard assets and Rust to update the embedded production dashboard.

```text
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cd dashboard
bun run typecheck
bun run build
```

`tests/native_clients.py` generates valid wheel/tarball/crate fixtures, installs
them with real uv/npm/Bun/Cargo clients and isolated caches, then repeats the
installs with upstream unavailable. It checks dependencies, npm scopes, Bun
lockfile tarball paths, authenticated Cargo source replacement, credential
isolation, and absence of unrelated cached versions. Run it after `cargo build`,
using an existing Python interpreter:

```text
uv run --no-project --python 3.12 tests/native_clients.py
```

Use `--keep` for a populated local dashboard preview with disposable fixture
credentials. Rust integration tests additionally cover controlled-clock expiry,
retention changes, ranges/HEAD, corruption, concurrency, eviction coordination,
settings encryption, restart persistence, users/CSRF, and SOCKS5 authentication/DNS.

## Operations and recovery

`/healthz` checks database availability. The binary's `healthcheck` subcommand
provides the Docker health check. Structured operational logs go to stdout;
`RUST_LOG` controls verbosity, including when running through Compose. Set it in
your shell or in a `.env` file alongside `compose.yaml` (see `.env.example`):

```dotenv
RUST_LOG=speculum=debug,tower_http=info
```

The default is `speculum=info,tower_http=info`. Upstream metadata fetches,
artifact downloads, and HEAD probes log at `INFO`; registry requests and cache
outcomes log at `DEBUG`. Upstream failures and stale metadata fallback log at
`WARN`, while internal database and storage failures log at `ERROR`. Logs omit
credentials, headers, request bodies, and URL query strings. Recreate the
container with `docker compose up -d` after changing `RUST_LOG`.

A cold download failure is returned clearly and
leaves no reusable partial cache file.

For a consistent backup, stop the service, copy **all of `/app/data`** with
`docker compose cp speculum:/app/data ./speculum-backup`, then restart it. Restore
the directory into the mounted volume while the service is stopped, preserving
ownership for container UID/GID **10001:10001**. Do not omit `secret.key` or SQLite
WAL files from a copy. Do not run `docker compose down --volumes` on data you want
to retain.

To recover an admin password, stop Speculum and pass the replacement password on
stdin to `speculum admin reset-password`. With Compose, pipe the password from a
shell variable populated with a hidden password prompt:

```sh
docker compose stop speculum
printf '%s' "$NEW_ADMIN_PASSWORD" | docker compose run --rm -T speculum admin reset-password
docker compose start speculum
```

The recovery command invalidates every admin session and requires a password
change on the next login. It leaves mirror users, configuration, and cached
packages intact. Dashboard password changes invalidate prior sessions and issue
a fresh session for the caller.
