import { type ClientConfig } from '../api'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from './ui/card'
import { Code } from '../common'
import { Database } from 'lucide-react'
import { aptSources, aptCredentials, apkCredentials } from '../lib/distro-config'
import type { Registry } from '../api'
export function ClientConfiguration({
  config,
  username,
}: {
  config?: ClientConfig
  username?: string
}) {
  const base = config?.public_base_url.replace(/\/$/, '') || 'https://mirror.example.com'
  const npmrc = `registry=${base}/npm/\n${base.replace(/^https?:/, '')}/npm/:_authToken=\${SPECULUM_TOKEN}`
  return (
    <Card>
      <CardHeader>
        <CardTitle>Client configuration</CardTitle>
        <CardDescription>
          Use a generated mirror-user token for every protected registry. Python clients use your
          mirror username and the token as the password. Set SPECULUM_TOKEN for JavaScript clients.
        </CardDescription>
      </CardHeader>
      <CardContent className="client-configs">
        <div>
          <h3>Python · uv</h3>
          <Code>{`[[tool.uv.index]]\nname = "speculum"\nurl = "${base}/pypi/simple/"\ndefault = true\n\n# Set environment variables when authentication is enabled:\n# UV_INDEX_SPECULUM_USERNAME=${username || '<username>'}\n# UV_INDEX_SPECULUM_PASSWORD=<token>`}</Code>
        </div>
        <div>
          <h3>JavaScript · npm (.npmrc)</h3>
          <Code>{npmrc}</Code>
        </div>
        <div>
          <h3>JavaScript · Bun (bunfig.toml)</h3>
          <Code>{`[install]\nregistry = { url = "${base}/npm/", token = "$SPECULUM_TOKEN" }`}</Code>
          <p className="muted">
            Run <code>bun install</code>. Bun also accepts the npm configuration above. For public
            access, use a plain registry URL without the token.
          </p>
        </div>
        <div>
          <h3>JavaScript · Deno (.npmrc)</h3>
          <Code>{npmrc}</Code>
          <p className="muted">Declare npm dependencies in deno.json:</p>
          <Code>{`{\n  "imports": {\n    "chalk": "npm:chalk@5"\n  }\n}`}</Code>
          <p className="muted">
            Run <code>deno install</code>. Only npm dependencies use this mirror; JSR and HTTP
            imports use their original sources. For public access, omit the auth token line.
          </p>
        </div>
        <div className="client-config-wide">
          <h3>Rust · .cargo/config.toml</h3>
          <Code>{`[source.crates-io]\nreplace-with = "speculum"\n\n[registries.speculum]\nindex = "sparse+${base}/cargo/index/"\ncredential-provider = "cargo:token"\n\n# Set CARGO_REGISTRIES_SPECULUM_TOKEN=<token>`}</Code>
        </div>
        <div className="info-box">
          <Database size={17} />
          <span>
            Files are cached only when requested. Offline use requires warmed files, and clients
            continue enforcing distribution signatures and metadata expiry.
          </span>
        </div>
        {(['debian', 'ubuntu', 'ubuntu-ports'] as const).map((kind) => {
          const distro = kind === 'debian' ? 'debian' : 'ubuntu'
          const kinds: Registry[] = kind === 'ubuntu-ports' ? [kind] : [distro, `${distro}-security`]
          const auth = aptCredentials(base, kinds, config, username)
          return (
            <div className="client-config-wide" key={kind}>
              <h3>{kind === 'debian' ? 'Debian' : kind === 'ubuntu' ? 'Ubuntu · AMD64 / i386' : 'Ubuntu ports · ARM64 and other ports'} · APT</h3>
              <p className="muted">Save as /etc/apt/sources.list.d/speculum.sources. Replace &lt;release&gt; with your installed release codename and replace matching existing sources to avoid bypassing the mirror.</p>
              <Code>{aptSources(base, distro, kind === 'ubuntu-ports')}</Code>
              {auth ? <>
                <p className="muted">Save credentials as /etc/apt/auth.conf.d/speculum.conf, owned by root with permissions 0600. Use a generated token as the password.</p>
                <Code>{auth}</Code>
              </> : <p className="muted">Authentication is disabled for these mirrors; no credential file is needed.</p>}
            </div>
          )
        })}
        <div className="client-config-wide">
          <h3>Alpine · APK</h3>
          <p className="muted">Replace matching entries in /etc/apk/repositories. Replace &lt;branch&gt; with your installed branch, such as v3.23.</p>
          <Code>{`${base}/alpine/<branch>/main\n${base}/alpine/<branch>/community`}</Code>
          {config?.registries.alpine.auth_required !== false ? <>
            <p className="muted">Save credentials in root’s .netrc with permissions 0600. APK also accepts a separate file selected with NETRC=/path/to/file.</p>
            <Code>{apkCredentials(base, username)}</Code>
          </> : <p className="muted">Authentication is disabled for Alpine; no credential file is needed.</p>}
        </div>
      </CardContent>
    </Card>
  )
}
