import { ArrowDownToLine, ArrowRight, HardDrive, Package, ShieldCheck, Zap } from 'lucide-react'
import { labels, registries, type Settings, type Summary } from '../api'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '../components/ui/card'
import { Button } from '../components/ui/button'
import { ErrorMessage, Loading, Stat, useData } from '../common'
import { bytes, number } from '../lib/utils'
import { useState } from 'react'
import { ClientConfiguration } from '../components/client-configuration'
export function Overview({ onPackages }: { onPackages: () => void }) {
  const [revision, setRevision] = useState(0)
  const summary = useData<Summary>('/summary', revision)
  const settings = useData<Settings>('/settings')
  if (summary.loading && !summary.data) return <Loading />
  if (!summary.data) return <ErrorMessage message={summary.error} />
  const { total } = summary.data
  const hitRate = total.requests ? `${Math.round((total.hits / total.requests) * 100)}%` : '—'
  return (
    <>
      <div className="page-heading">
        <div>
          <h1>Overview</h1>
          <p>A local home for the packages your projects depend on.</p>
        </div>
        <Button variant="outline" onClick={() => setRevision(revision + 1)}>
          Refresh
        </Button>
      </div>
      <ErrorMessage message={summary.error || settings.error} />
      <div className="stats-grid">
        <Stat
          label="Cache storage"
          value={bytes(total.stored_bytes)}
          detail={`${number(total.files)} cached files`}
          icon={<HardDrive size={16} />}
        />
        <Stat
          label="Cached packages"
          value={number(total.packages)}
          detail={`${number(total.versions)} downloaded versions`}
          icon={<Package size={16} />}
        />
        <Stat
          label="Download requests"
          value={number(total.requests)}
          detail={`${number(total.hits)} served from cache`}
          icon={<ArrowDownToLine size={16} />}
        />
        <Stat
          label="Cache hit rate"
          value={hitRate}
          detail="Across all package downloads"
          icon={<Zap size={16} />}
        />
      </div>
      <div className="section-heading">
        <h2>Registries</h2>
      </div>
      <div className="registry-grid">
        {registries.map((kind) => {
          const stats = summary.data!.registries[kind]
          const config = settings.data?.registries[kind]
          return (
            <Card key={kind}>
              <CardHeader>
                <div className="registry-heading">
                  <span className={`registry-logo ${kind}`}>
                    {kind === 'pypi' ? 'Py' : kind === 'npm' ? 'JS' : 'Rs'}
                  </span>
                  <div>
                    <CardTitle>{labels[kind]}</CardTitle>
                    <CardDescription>
                      {kind === 'cargo' ? 'crates.io' : kind === 'pypi' ? 'PyPI' : 'npm registry'}
                    </CardDescription>
                  </div>
                  <span className={`badge ${config?.enabled === false ? 'muted-badge' : 'green'}`}>
                    {config?.enabled === false ? 'Disabled' : 'Enabled'}
                  </span>
                </div>
              </CardHeader>
              <CardContent>
                <div className="registry-metrics">
                  <div>
                    <strong>{number(stats.packages)}</strong>
                    <span>packages</span>
                  </div>
                  <div>
                    <strong>{bytes(stats.stored_bytes)}</strong>
                    <span>stored</span>
                  </div>
                  <div>
                    <strong>{number(stats.requests)}</strong>
                    <span>requests</span>
                  </div>
                </div>
                <div className="registry-footer">
                  <span>
                    <ShieldCheck size={13} />
                    {config?.auth_required ? 'Authentication required' : 'Public access'}
                  </span>
                  <span>{config?.proxy ? 'Proxy' : 'Direct'}</span>
                </div>
              </CardContent>
            </Card>
          )
        })}
      </div>
      <Card className="traffic-card">
        <CardHeader>
          <CardTitle>Traffic & reliability</CardTitle>
          <CardDescription>
            Lifetime totals, preserved across restarts and cache deletion.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="traffic-grid">
            {[
              ['Upstream downloads', number(total.upstream_downloads)],
              ['Upstream traffic', bytes(total.upstream_bytes)],
              ['Client traffic', bytes(total.client_bytes)],
              ['Cache misses', number(total.misses)],
              ['Failed downloads', number(total.failures)],
            ].map(([label, value]) => (
              <div key={label}>
                <span>{label}</span>
                <strong>{value}</strong>
              </div>
            ))}
          </div>
        </CardContent>
      </Card>
      <div className="section-heading">
        <h2>Connect your projects</h2>
        <Button variant="ghost" size="sm" onClick={onPackages}>
          Browse packages
          <ArrowRight />
        </Button>
      </div>
      <ClientConfiguration config={settings.data} />
    </>
  )
}
