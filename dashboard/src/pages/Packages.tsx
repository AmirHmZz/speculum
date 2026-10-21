import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '../components/ui/table'
import {
  ArrowLeft,
  ChevronLeft,
  ChevronRight,
  Package as PackageIcon,
  Search,
  Trash2,
} from 'lucide-react'
import { useState } from 'react'
import { api, labels, registries, isDistro, type Artifact, type Package } from '../api'
import { Button } from '../components/ui/button'
import { Input } from '../components/ui/input'
import { Card, CardContent, CardHeader, CardTitle } from '../components/ui/card'
import { RegistrySelect } from '../components/registry-select'
import { ErrorMessage, Loading, Stat, useConfirm, useData } from '../common'
import { bytes, date, number } from '../lib/utils'
export function Packages() {
  const [search, setSearch] = useState('')
  const [registry, setRegistry] = useState('')
  const [page, setPage] = useState(1)
  const [selected, setSelected] = useState('')
  const [revision, setRevision] = useState(0)
  const data = useData<{ items: Package[]; total: number; page_size: number }>(
    `/packages?${new URLSearchParams({ q: search, registry, page: String(page) })}`,
    revision,
  )
  const confirm = useConfirm()
  if (selected)
    return (
      <PackageDetails
        id={selected}
        onBack={() => {
          setSelected('')
          setRevision(revision + 1)
        }}
      />
    )
  return (
    <>
      <div className="page-heading">
        <div>
          <h1>Packages</h1>
          <p>Inspect packages, repository files, and their footprint.</p>
        </div>
        <Button variant="outline" onClick={() => setRevision(revision + 1)}>
          Refresh
        </Button>
      </div>
      <ErrorMessage message={data.error} />
      <Card>
        <div className="table-toolbar">
          <div className="search">
            <Search size={16} />
            <Input
              aria-label="Search packages"
              placeholder="Search packages…"
              value={search}
              onChange={(e) => {
                setSearch(e.target.value)
                setPage(1)
              }}
            />
          </div>
          <RegistrySelect
            value={registry}
            onValueChange={(value) => {
              setRegistry(value)
              setPage(1)
            }}
          />
        </div>
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Package</TableHead>
              <TableHead>Registry</TableHead>
              <TableHead>Versions</TableHead>
              <TableHead>Storage</TableHead>
              <TableHead>Requests</TableHead>
              <TableHead>Hit rate</TableHead>
              <TableHead>Last access</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {data.data?.items.map((item) => (
              <TableRow key={item.id}>
                <TableCell>
                  <button className="package-link" onClick={() => setSelected(item.id)}>
                    <PackageIcon size={16} />
                    {item.name}
                    <ChevronRight size={13} />
                  </button>
                </TableCell>
                <TableCell>
                  <span className={`registry-tag ${item.registry}`}>{item.registry}</span>
                </TableCell>
                <TableCell>{isDistro(item.registry) ? '—' : number(item.versions)}</TableCell>
                <TableCell>{bytes(item.stored_bytes)}</TableCell>
                <TableCell>{number(item.requests)}</TableCell>
                <TableCell>
                  {item.requests ? `${Math.round((item.hits / item.requests) * 100)}%` : '—'}
                </TableCell>
                <TableCell className="muted text-xs">{date(item.last_access)}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
        {data.loading && !data.data && <Loading />}
        {data.data?.items.length === 0 && (
          <div className="empty">
            <PackageIcon size={30} />
            <h3>No packages yet</h3>
            <p>Packages appear when clients request their metadata or files.</p>
          </div>
        )}
        <div className="table-footer">
          <span>{number(data.data?.total || 0)} known packages</span>
          <div>
            <Button
              variant="outline"
              size="icon"
              aria-label="Previous page"
              disabled={page === 1}
              onClick={() => setPage(page - 1)}
            >
              <ChevronLeft />
            </Button>
            <span>Page {page}</span>
            <Button
              variant="outline"
              size="icon"
              aria-label="Next page"
              disabled={!data.data || page * data.data.page_size >= data.data.total}
              onClick={() => setPage(page + 1)}
            >
              <ChevronRight />
            </Button>
          </div>
        </div>
      </Card>
      <Card className="mt-6">
        <CardHeader>
          <CardTitle>Clear a registry cache</CardTitle>
        </CardHeader>
        <CardContent>
          <p className="muted text-sm mb-4">
            Remove stored files and metadata. Historical traffic statistics are preserved. Requested
            packages will be downloaded again.
          </p>
          <div className="flex flex-wrap gap-3">
            {registries.map((kind) => (
              <Button
                key={kind}
                variant="outline"
                onClick={() =>
                  confirm({
                    title: `Clear ${labels[kind]} cache?`,
                    description:
                      'All cached artifacts and metadata in this registry will be removed.',
                    action: async () => {
                      await api(`/cache/${kind}`, { method: 'DELETE' })
                      setRevision(revision + 1)
                    },
                  })
                }
              >
                <Trash2 />
                Clear {kind}
              </Button>
            ))}
          </div>
        </CardContent>
      </Card>
    </>
  )
}
function PackageDetails({ id, onBack }: { id: string; onBack: () => void }) {
  const [revision, setRevision] = useState(0)
  const result = useData<{ package: Package; artifacts: Artifact[] }>(`/packages/${id}`, revision)
  const confirm = useConfirm()
  if (!result.data) return result.loading ? <Loading /> : <ErrorMessage message={result.error} />
  const item = result.data.package
  const versions = Array.from(new Set(result.data.artifacts.map((a) => a.version)))
  function remove(version?: string) {
    confirm({
      title: version ? `Delete version ${version}?` : `Clear ${item.name}?`,
      description: 'Stored files will be removed. Traffic statistics will remain available.',
      action: async () => {
        await api(`/packages/${id}${version ? `?version=${encodeURIComponent(version)}` : ''}`, {
          method: 'DELETE',
        })
        setRevision(revision + 1)
      },
    })
  }
  return (
    <>
      <Button variant="ghost" className="back-button" onClick={onBack}>
        <ArrowLeft />
        All packages
      </Button>
      <div className="page-heading">
        <div>
          <div className="eyebrow">{labels[item.registry].toUpperCase()} {isDistro(item.registry) ? 'REPOSITORY FILE' : 'PACKAGE'}</div>
          <h1>{item.name}</h1>
          <p>Last requested {date(item.last_access)}</p>
        </div>
        <Button variant="outline" onClick={() => remove()}>
          <Trash2 />
          Clear package
        </Button>
      </div>
      <ErrorMessage message={result.error} />
      <div className="stats-grid">
        <Stat
          label="Stored files"
          value={number(item.files)}
          detail={isDistro(item.registry) ? bytes(item.stored_bytes) : `${item.versions} versions · ${bytes(item.stored_bytes)}`}
        />
        <Stat
          label="Download requests"
          value={number(item.requests)}
          detail={`${item.hits} hits · ${item.misses} misses`}
        />
        <Stat
          label="Upstream traffic"
          value={bytes(item.upstream_bytes)}
          detail={`${item.upstream_downloads} upstream downloads`}
        />
        <Stat
          label="Client traffic"
          value={bytes(item.client_bytes)}
          detail={`${item.failures} failed downloads`}
        />
      </div>
      {versions.length === 0 && (
        <Card>
          <div className="empty">
            <PackageIcon size={30} />
            <h3>No cached files</h3>
            <p>Files will be cached when requested by a client.</p>
          </div>
        </Card>
      )}
      {versions.map((version) => (
        <Card className="version-card" key={version}>
          <CardHeader>
            <div className="section-heading">
              <CardTitle>{isDistro(item.registry) ? 'Repository file' : `Version ${version}`}</CardTitle>
              <Button variant="ghost" size="sm" onClick={() => remove(version)}>
                <Trash2 />
                {isDistro(item.registry) ? 'Delete file' : 'Delete version'}
              </Button>
            </div>
          </CardHeader>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Artifact</TableHead>
                <TableHead>Size</TableHead>
                <TableHead>Downloaded</TableHead>
                <TableHead>Expires</TableHead>
                <TableHead>
                  <span className="sr-only">Actions</span>
                </TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {result
                .data!.artifacts.filter((a) => a.version === version)
                .map((artifact) => (
                  <TableRow key={artifact.id}>
                    <TableCell>
                      <span className="artifact-name" title={`Upstream: ${artifact.upstream}`}>
                        {artifact.filename}
                      </span>
                      {artifact.is_sidecar && <span className="badge ml-2">Metadata</span>}
                    </TableCell>
                    <TableCell>{bytes(artifact.size)}</TableCell>
                    <TableCell className="text-xs">{date(artifact.downloaded_at)}</TableCell>
                    <TableCell className="text-xs">
                      {artifact.expires_at === null ? 'Forever' : date(artifact.expires_at)}
                    </TableCell>
                    <TableCell>
                      <Button
                        variant="ghost"
                        size="icon"
                        aria-label={`Delete ${artifact.filename}`}
                        onClick={() =>
                          confirm({
                            title: 'Delete artifact?',
                            description: artifact.filename,
                            action: async () => {
                              await api(`/artifacts/${artifact.id}`, { method: 'DELETE' })
                              setRevision(revision + 1)
                            },
                          })
                        }
                      >
                        <Trash2 />
                      </Button>
                    </TableCell>
                  </TableRow>
                ))}
            </TableBody>
          </Table>
        </Card>
      ))}
    </>
  )
}
