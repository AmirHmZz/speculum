import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from './components/ui/table'
import { useEffect, useState, type FormEvent } from 'react'
import {
  ChevronLeft,
  ChevronRight,
  KeyRound,
  LayoutDashboard,
  LogOut,
  Moon,
  Plus,
  Search,
  Sun,
  Trash2,
  UserRound,
  ArrowDownToLine,
  Zap,
  Package,
  Activity,
} from 'lucide-react'
import {
  userApi,
  post,
  setCsrf,
  labels,
  registries,
  registryIcon,
  registryDescription,
  type DashboardSession,
  type PersonalSummary,
  type PersonalPackage,
  type ClientConfig,
  type UserToken,
} from './api'
import { Login, PasswordForm, PasswordGate } from './auth-ui'
import { BrandIcon } from './components/brand-icon'
import { ClientConfiguration } from './components/client-configuration'
import { RegistrySelect } from './components/registry-select'
import { Button } from './components/ui/button'
import { Input } from './components/ui/input'
import { Card, CardContent, CardHeader, CardTitle, CardDescription } from './components/ui/card'
import { Dialog } from './components/ui/dialog'
import {
  Code,
  ConfirmProvider,
  ErrorMessage,
  Field,
  Loading,
  Stat,
  useConfirm,
  useData,
} from './common'
import { bytes, date, number } from './lib/utils'

type View = 'overview' | 'tokens' | 'account'
const pages = {
  overview: { label: 'Overview', icon: LayoutDashboard },
  tokens: { label: 'Tokens', icon: KeyRound },
  account: { label: 'Account', icon: UserRound },
}
export function UserApp() {
  const [session, setSession] = useState<DashboardSession | null>(null)
  const [checking, setChecking] = useState(true)
  const [view, setView] = useState<View>('overview')
  const [dark, setDark] = useState(() => localStorage.getItem('theme') === 'dark')
  const [error, setError] = useState('')
  function acceptSession(session: DashboardSession) {
    setSession(session)
    setCsrf(session.csrf_token, 'user')
  }
  useEffect(() => {
    document.documentElement.classList.toggle('dark', dark)
    localStorage.setItem('theme', dark ? 'dark' : 'light')
  }, [dark])
  useEffect(() => {
    userApi<DashboardSession>('/session')
      .then(acceptSession)
      .catch(() => {})
      .finally(() => setChecking(false))
    const expire = () => {
      setSession(null)
      setCsrf('', 'user')
    }
    window.addEventListener('user-session-expired', expire)
    return () => window.removeEventListener('user-session-expired', expire)
  }, [])
  async function logout() {
    await userApi('/session', { method: 'DELETE' })
    setSession(null)
    setCsrf('', 'user')
  }
  if (checking) return <Loading />
  if (!session) return <Login />
  if (session.must_change_password)
    return <PasswordGate role="user" onChanged={acceptSession} onLogout={logout} />
  return (
    <ConfirmProvider>
      <div className="shell">
        <aside className="sidebar">
          <a className="brand" href="/account">
            <span className="brand-icon">
              <BrandIcon size={21} />
            </span>
            <span>
              speculum
            </span>
          </a>
          <div className="nav-label">YOUR WORKSPACE</div>
          <nav>
            {(Object.keys(pages) as View[]).map((key) => {
              const Icon = pages[key].icon
              return (
                <button
                  key={key}
                  className={`nav-item ${view === key ? 'active' : ''}`}
                  onClick={() => setView(key)}
                >
                  <Icon size={18} />
                  {pages[key].label}
                  {view === key && <ChevronRight size={14} className="nav-arrow" />}
                </button>
              )
            })}
          </nav>
          <div className="sidebar-bottom">
            <div className="admin-profile">
              <span className="avatar">{session.username.slice(0, 1).toUpperCase()}</span>
              <span>
                <strong>{session.username}</strong>
                <small>Mirror user</small>
              </span>
              <Button
                variant="ghost"
                size="icon"
                aria-label="Sign out"
                onClick={() => logout().catch((error: Error) => setError(error.message))}
              >
                <LogOut />
              </Button>
            </div>
          </div>
        </aside>
        <main className="main">
          <header className="topbar">
            <span>
              Your workspace <ChevronRight size={13} />
              <strong>{pages[view].label}</strong>
            </span>
            <div className="topbar-actions">
              <Button
                variant="ghost"
                size="icon"
                aria-label={dark ? 'Use light theme' : 'Use dark theme'}
                onClick={() => setDark(!dark)}
              >
                {dark ? <Sun /> : <Moon />}
              </Button>
            </div>
          </header>
          <div className="content">
            <ErrorMessage message={error} />
            {view === 'overview' && <UserOverview username={session.username} />}
            {view === 'tokens' && <UserTokens />}
            {view === 'account' && (
              <>
                <div className="page-heading">
                  <div>
                    <h1>Account</h1>
                    <p>Signed in as {session.username}.</p>
                  </div>
                </div>
                <Card className="settings-card">
                  <CardHeader>
                    <CardTitle>Change password</CardTitle>
                    <CardDescription>
                      This changes dashboard access. Your package tokens stay valid.
                    </CardDescription>
                  </CardHeader>
                  <CardContent>
                    <PasswordForm role="user" onChanged={acceptSession} />
                  </CardContent>
                </Card>
              </>
            )}
          </div>
        </main>
      </div>
    </ConfirmProvider>
  )
}
function UserOverview({ username }: { username: string }) {
  const [revision, setRevision] = useState(0)
  const summary = useData<PersonalSummary>('/summary', revision, 'user')
  const config = useData<ClientConfig>('/config', revision, 'user')
  if (!summary.data) return summary.loading ? <Loading /> : <ErrorMessage message={summary.error} />
  const total = summary.data.total
  return (
    <div className="user-overview">
      <div className="page-heading">
        <div>
          <h1>Overview</h1>
          <p>Your lifetime usage across all your package tokens.</p>
        </div>
        <Button variant="outline" onClick={() => setRevision(revision + 1)}>
          Refresh
        </Button>
      </div>
      <ErrorMessage message={summary.error || config.error} />
      <div className="stats-grid">
        <Stat
          label="Download requests"
          value={number(total.requests)}
          detail={`${number(total.hits)} served from cache`}
          icon={<ArrowDownToLine size={16} />}
        />
        <Stat
          label="Cache hit rate"
          value={total.requests ? `${Math.round((total.hits / total.requests) * 100)}%` : '—'}
          detail="For your artifact requests"
          icon={<Zap size={16} />}
        />
        <Stat
          label="Client traffic"
          value={bytes(total.client_bytes)}
          detail="File bytes served to your clients"
          icon={<Activity size={16} />}
        />
        <Stat
          label="Packages requested"
          value={number(total.packages)}
          detail="Across all registries and distribution mirrors"
          icon={<Package size={16} />}
        />
      </div>
      <div className="section-heading">
        <h2>Registries</h2>
      </div>
      <div className="registry-grid">
        {registries.map((kind) => {
          const stats = summary.data!.registries[kind]
          return (
            <Card key={kind} className="user-registry-card">
              <CardHeader className="user-registry-header">
                <div className="registry-heading">
                  <span className={`registry-logo ${kind}`}>
                    {registryIcon(kind)}
                  </span>
                  <div>
                    <CardTitle>{labels[kind]}</CardTitle>
                    <CardDescription>
                      {registryDescription(kind)}
                    </CardDescription>
                  </div>
                  <span
                    className={`badge ${config.data?.registries[kind].enabled === false ? 'muted-badge' : 'green'}`}
                  >
                    {config.data?.registries[kind].enabled === false ? 'Disabled' : 'Enabled'}
                  </span>
                </div>
              </CardHeader>
              <CardContent className="user-registry-content">
                <div className="registry-metrics">
                  <div>
                    <strong>{number(stats.requests)}</strong>
                    <span>requests</span>
                  </div>
                  <div>
                    <strong>{number(stats.hits)}</strong>
                    <span>cache hits</span>
                  </div>
                  <div>
                    <strong>{bytes(stats.client_bytes)}</strong>
                    <span>traffic</span>
                  </div>
                </div>
              </CardContent>
            </Card>
          )
        })}
      </div>
      <Card className="traffic-card">
        <CardHeader>
          <CardTitle>Traffic & reliability</CardTitle>
          <CardDescription>Anonymous requests are not attributed to your account.</CardDescription>
        </CardHeader>
        <CardContent>
          <div className="traffic-grid">
            {[
              ['Upstream downloads', number(total.upstream_downloads)],
              ['Upstream traffic', bytes(total.upstream_bytes)],
              ['Cache misses', number(total.misses)],
              ['Failed downloads', number(total.failures)],
              ['Last access', date(total.last_access)],
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
        <h2>Package activity</h2>
      </div>
      <UserPackages revision={revision} />
      <div className="section-heading connect-heading">
        <h2>Connect your projects</h2>
      </div>
      <ClientConfiguration config={config.data} username={username} />
    </div>
  )
}
function UserPackages({ revision }: { revision: number }) {
  const [search, setSearch] = useState('')
  const [registry, setRegistry] = useState('')
  const [page, setPage] = useState(1)
  const data = useData<{ items: PersonalPackage[]; total: number; page_size: number }>(
    `/packages?${new URLSearchParams({ q: search, registry, page: String(page) })}`,
    revision,
    'user',
  )
  return (
    <>
      <ErrorMessage message={data.error} />
      <Card>
        <div className="table-toolbar">
          <div className="search">
            <Search size={16} />
            <Input
              aria-label="Search packages"
              placeholder="Search your packages…"
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
              <TableHead>Requests</TableHead>
              <TableHead>Hits / misses</TableHead>
              <TableHead>Failures</TableHead>
              <TableHead>Client traffic</TableHead>
              <TableHead>Upstream traffic</TableHead>
              <TableHead>Last access</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {data.data?.items.map((item) => (
              <TableRow key={item.id}>
                <TableCell>{item.name}</TableCell>
                <TableCell>
                  <span className={`registry-tag ${item.registry}`}>{item.registry}</span>
                </TableCell>
                <TableCell>{number(item.requests)}</TableCell>
                <TableCell>
                  {number(item.hits)} / {number(item.misses)}
                </TableCell>
                <TableCell>{number(item.failures)}</TableCell>
                <TableCell>{bytes(item.client_bytes)}</TableCell>
                <TableCell>{bytes(item.upstream_bytes)}</TableCell>
                <TableCell className="muted text-xs">{date(item.last_access)}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
        {data.loading && !data.data && <Loading />}
        {data.data?.items.length === 0 && (
          <div className="empty">
            <Package size={30} />
            <h3>No activity yet</h3>
            <p>Use one of your tokens to download a package.</p>
          </div>
        )}
        <div className="table-footer">
          <span>{number(data.data?.total || 0)} packages requested</span>
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
    </>
  )
}
function UserTokens() {
  const [revision, setRevision] = useState(0)
  const data = useData<UserToken[]>(
    '/tokens',
    revision,
    'user',
  )
  const [open, setOpen] = useState(false)
  const [name, setName] = useState('')
  const [issued, setIssued] = useState('')
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)
  const confirm = useConfirm()
  async function create(event: FormEvent) {
    event.preventDefault()
    setBusy(true)
    setError('')
    try {
      const result = await userApi<{ token: string }>('/tokens', post({ name }))
      setIssued(result.token)
      setRevision(revision + 1)
    } catch (error) {
      setError((error as Error).message)
    } finally {
      setBusy(false)
    }
  }
  return (
    <>
      <div className="page-heading">
        <div>
          <h1>Tokens</h1>
          <p>Issue a named token for each client. Tokens work across protected registries.</p>
        </div>
        <Button
          onClick={() => {
            setOpen(true)
            setIssued('')
            setName('')
            setError('')
          }}
        >
          <Plus />
          New token
        </Button>
      </div>
      <ErrorMessage message={data.error} />
      <Card>
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Name</TableHead>
              <TableHead>Prefix</TableHead>
              <TableHead>Created</TableHead>
              <TableHead>Last used</TableHead>
              <TableHead>Actions</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {data.data?.map((token) => (
              <TableRow key={token.id}>
                <TableCell>
                  <span className="flex items-center gap-2">
                    <KeyRound size={16} />
                    {token.name}
                  </span>
                </TableCell>
                <TableCell className="font-mono whitespace-nowrap">{token.prefix}…</TableCell>
                <TableCell className="muted text-xs">{date(token.created_at)}</TableCell>
                <TableCell className="muted text-xs whitespace-nowrap">
                  {token.last_used_at === null ? 'Never' : date(token.last_used_at)}
                </TableCell>
                <TableCell>
                  <Button
                    variant="ghost"
                    size="icon"
                    aria-label={`Revoke ${token.name}`}
                    onClick={() =>
                      confirm({
                        title: 'Revoke token?',
                        description: `${token.name} will immediately stop working.`,
                        action: async () => {
                          await userApi(`/tokens/${token.id}`, { method: 'DELETE' })
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
        {data.loading && !data.data && <Loading />}
        {data.data?.length === 0 && (
          <div className="empty">
            <KeyRound size={30} />
            <h3>No tokens yet</h3>
            <p>Create a token to connect your package managers.</p>
          </div>
        )}
      </Card>
      <Dialog
        open={open}
        onOpenChange={(open) => {
          if (!busy) {
            setOpen(open)
            if (!open) setIssued('')
          }
        }}
        title={issued ? 'Token created' : 'Create token'}
        description={
          issued
            ? 'Copy this token now. It cannot be displayed again.'
            : 'Use the token as the password for Python, APT, or APK clients, or as the registry token for JavaScript and Cargo clients.'
        }
      >
        {issued ? (
          <>
            <Code>{issued}</Code>
            <Button
              onClick={() => {
                setOpen(false)
                setIssued('')
              }}
            >
              Done
            </Button>
          </>
        ) : (
          <form className="form" onSubmit={create}>
            <ErrorMessage message={error} />
            <Field label="Token name">
              <Input
                value={name}
                required
                maxLength={100}
                placeholder="e.g. My laptop"
                onChange={(e) => setName(e.target.value)}
              />
            </Field>
            <Button disabled={busy}>{busy ? 'Creating…' : 'Create token'}</Button>
          </form>
        )}
      </Dialog>
    </>
  )
}
