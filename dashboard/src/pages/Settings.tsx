import { useEffect, useState, type FormEvent } from 'react'
import { Check, LockKeyhole, Save } from 'lucide-react'
import {
  api,
  labels,
  registries,
  put,
  type Registry,
  type RegistryConfig,
  type Settings,
  type DashboardSession,
} from '../api'
import { Button } from '../components/ui/button'
import { NumberField } from '../components/number-field'
import { Input } from '../components/ui/input'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '../components/ui/card'
import { PasswordForm } from '../auth-ui'
import { ErrorMessage, Field, Loading, Toggle, useData } from '../common'
export function SettingsPage({
  onCredentialsChanged,
}: {
  onCredentialsChanged: (session: DashboardSession) => void
}) {
  const resource = useData<Settings>('/settings')
  const [settings, setSettings] = useState<Settings>()
  const [tab, setTab] = useState<Registry>('pypi')
  const [error, setError] = useState('')
  const [saved, setSaved] = useState(false)
  const [busy, setBusy] = useState(false)
  useEffect(() => {
    if (resource.data) setSettings(resource.data)
  }, [resource.data])
  function update<Key extends keyof Settings>(key: Key, value: Settings[Key]) {
    setSettings((previous) => (previous ? { ...previous, [key]: value } : previous))
    setSaved(false)
  }
  function registry<Key extends keyof RegistryConfig>(key: Key, value: RegistryConfig[Key]) {
    if (settings)
      update('registries', {
        ...settings.registries,
        [tab]: { ...settings.registries[tab], [key]: value },
      })
  }
  async function save(event: FormEvent) {
    event.preventDefault()
    if (!settings) return
    setBusy(true)
    setError('')
    try {
      const payload = {
        ...settings,
        registries: Object.fromEntries(
          registries.map((kind) => [
            kind,
            {
              ...settings.registries[kind],
              allowed_artifact_hosts:
                settings.registries[kind].allowed_artifact_hosts.filter(Boolean),
            },
          ]),
        ),
      }
      await api('/settings', put(payload))
      const refreshed = await api<Settings>('/settings')
      setSettings(refreshed)
      setSaved(true)
    } catch (error) {
      setError((error as Error).message)
    } finally {
      setBusy(false)
    }
  }
  if (!settings) return resource.loading ? <Loading /> : <ErrorMessage message={resource.error} />
  const config = settings.registries[tab]
  return (
    <>
      <form onSubmit={save}>
        <div className="page-heading">
          <div>
            <h1>Settings</h1>
            <p>Changes apply immediately and are preserved across restarts.</p>
          </div>
          <Button disabled={busy}>
            {saved ? <Check /> : <Save />}
            {busy ? 'Saving…' : saved ? 'Saved' : 'Save changes'}
          </Button>
        </div>
        <ErrorMessage message={error || resource.error} />
        {saved && (
          <div role="status" className="notice success">
            <Check size={16} />
            Settings saved.
          </div>
        )}
        <Card className="settings-card">
          <CardHeader>
            <CardTitle>Public address</CardTitle>
            <CardDescription>
              The address clients use, including HTTPS when TLS is terminated by your reverse proxy.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <Field
              label="Public base URL"
              hint="Use an origin without a path, e.g. https://mirror.example.com."
            >
              <Input
                type="url"
                required
                value={settings.public_base_url}
                onChange={(e) => update('public_base_url', e.target.value)}
              />
            </Field>
          </CardContent>
        </Card>
        <div className="tabs" role="tablist" aria-label="Registry settings">
          {registries.map((kind) => (
            <button
              role="tab"
              aria-selected={tab === kind}
              key={kind}
              type="button"
              className={tab === kind ? 'selected' : ''}
              onClick={() => setTab(kind)}
            >
              <span className={`registry-tag ${kind}`}>{kind}</span>
              {labels[kind]}
            </button>
          ))}
        </div>
        <Card className="settings-card">
          <CardHeader>
            <CardTitle>{labels[tab]} registry</CardTitle>
            <CardDescription>
              Configure access, upstream connectivity, and cache retention.
            </CardDescription>
          </CardHeader>
          <CardContent className="form">
            <Toggle
              label="Registry enabled"
              hint="Accept metadata and download requests for this registry."
              checked={config.enabled}
              onChange={(value) => registry('enabled', value)}
            />
            <Toggle
              label="Require mirror authentication"
              hint="Allow access only to enabled mirror users with a valid token."
              checked={config.auth_required}
              onChange={(value) => registry('auth_required', value)}
            />
            <div className="form-divider" />
            <Field label="Upstream URL">
              <Input
                type="url"
                required
                value={config.upstream_url}
                onChange={(e) => registry('upstream_url', e.target.value)}
              />
            </Field>
            <Field
              label="Allowed artifact hosts"
              hint="Comma-separated hostnames. The upstream hostname is always allowed."
            >
              <Input
                value={config.allowed_artifact_hosts.join(', ')}
                onChange={(e) =>
                  registry(
                    'allowed_artifact_hosts',
                    e.target.value.split(',').map((s) => s.trim()),
                  )
                }
              />
            </Field>
            <div className="two-columns">
              <NumberField
                label="Artifact cache lifetime (seconds)"
                hint="Blank means forever. Changes recalculate expiry for files still stored."
                min={1}
                placeholder="Forever"
                value={config.cache_lifetime_seconds}
                onValueChange={(value) => registry('cache_lifetime_seconds', value)}
              />
              <NumberField
                label="Metadata freshness (seconds)"
                hint="Retained metadata can be used when upstream is unavailable."
                required
                min={0}
                value={config.metadata_freshness_seconds}
                onValueChange={(value) => registry('metadata_freshness_seconds', value ?? 0)}
              />
            </div>
            <div className="form-divider" />
            <Toggle
              label="Use an upstream proxy"
              hint="Route metadata, artifacts, and audit requests through this proxy."
              checked={!!config.proxy}
              onChange={(value) =>
                registry(
                  'proxy',
                  value
                    ? { url: 'socks5h://127.0.0.1:1080', username: null, password: null }
                    : null,
                )
              }
            />
            {config.proxy && (
              <div className="proxy-form">
                <Field
                  label="Proxy URL"
                  hint="socks5h resolves upstream DNS at the proxy. socks5 uses local DNS."
                >
                  <Input
                    required
                    value={config.proxy.url}
                    onChange={(e) => registry('proxy', { ...config.proxy!, url: e.target.value })}
                    placeholder="socks5h://proxy.example.com:1080"
                  />
                </Field>
                <div className="two-columns">
                  <Field label="Proxy username (optional)">
                    <Input
                      value={config.proxy.username || ''}
                      autoComplete="off"
                      onChange={(e) =>
                        registry('proxy', { ...config.proxy!, username: e.target.value || null })
                      }
                    />
                  </Field>
                  <Field
                    label="Proxy password (optional)"
                    hint="A masked value preserves the current secret."
                  >
                    <Input
                      type="password"
                      autoComplete="new-password"
                      value={config.proxy.password || ''}
                      onChange={(e) =>
                        registry('proxy', { ...config.proxy!, password: e.target.value || null })
                      }
                    />
                  </Field>
                </div>
              </div>
            )}
          </CardContent>
        </Card>
        <Card className="settings-card">
          <CardHeader>
            <CardTitle>Service behavior</CardTitle>
            <CardDescription>Listener IP and port are configured at startup.</CardDescription>
          </CardHeader>
          <CardContent className="two-columns">
            {(
              [
                ['connect_timeout_seconds', 'Connection timeout (seconds)', 1],
                ['download_timeout_seconds', 'Download timeout (seconds)', 1],
                ['download_concurrency', 'Concurrent downloads', 1],
                ['cleanup_interval_seconds', 'Cleanup interval (seconds)', 1],
                ['admin_session_lifetime_seconds', 'Admin session lifetime (seconds)', 60],
                ['user_session_lifetime_seconds', 'User session lifetime (seconds)', 60],
              ] as const
            ).map(([key, label, min]) => (
              <NumberField
                key={key}
                label={label}
                required
                min={min}
                max={
                  key === 'download_concurrency'
                    ? 256
                    : key === 'admin_session_lifetime_seconds' ||
                        key === 'user_session_lifetime_seconds'
                      ? 2592000
                      : undefined
                }
                value={settings[key]}
                onValueChange={(value) => update(key, value ?? min)}
              />
            ))}
          </CardContent>
        </Card>
      </form>
      <Card className="settings-card">
        <CardHeader>
          <CardTitle>
            <span className="flex items-center gap-2">
              <LockKeyhole size={16} />
              Admin account
            </span>
          </CardTitle>
          <CardDescription>
            The administrator username is admin. Password changes sign out other admin sessions.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <PasswordForm role="admin" onChanged={onCredentialsChanged} />
        </CardContent>
      </Card>
    </>
  )
}
