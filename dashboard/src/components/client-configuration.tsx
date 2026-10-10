import { type ClientConfig } from '../api'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from './ui/card'
import { Code } from '../common'
import { Database } from 'lucide-react'
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
            Package metadata is fetched on demand. Wheels, tarballs, and crates are cached only when
            a client requests them.
          </span>
        </div>
      </CardContent>
    </Card>
  )
}
