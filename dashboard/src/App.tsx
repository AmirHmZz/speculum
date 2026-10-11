import { useEffect, useState } from 'react'
import {
  Braces,
  ChevronRight,
  Database,
  LayoutDashboard,
  LogOut,
  Moon,
  Package,
  Settings2,
  Sun,
  Users,
} from 'lucide-react'
import { api, setCsrf, type DashboardSession } from './api'
import { BrandIcon } from './components/brand-icon'
import { Button } from './components/ui/button'
import { ConfirmProvider, ErrorMessage, Loading } from './common'
import { Overview } from './pages/Overview'
import { Packages } from './pages/Packages'
import { UsersPage } from './pages/Users'
import { SettingsPage } from './pages/Settings'
import { Login, PasswordGate } from './auth-ui'
import { UserApp } from './UserApp'

type View = 'overview' | 'packages' | 'users' | 'settings'
const pages = {
  overview: { label: 'Overview', icon: LayoutDashboard },
  packages: { label: 'Packages', icon: Package },
  users: { label: 'Mirror users', icon: Users },
  settings: { label: 'Settings', icon: Settings2 },
}
export default function App() {
  if (window.location.pathname.startsWith('/account')) return <UserApp />
  if (window.location.pathname.startsWith('/admin')) return <AdminApp />
  return <Login />
}
function AdminApp() {
  const [username, setUsername] = useState<string | null>(null)
  const [checking, setChecking] = useState(true)
  const [mustChange, setMustChange] = useState(false)
  function acceptSession(session: DashboardSession) {
    setUsername(session.username)
    setCsrf(session.csrf_token)
    setMustChange(session.must_change_password)
  }
  const [view, setView] = useState<View>('overview')
  const [dark, setDark] = useState(() => localStorage.getItem('theme') === 'dark')
  const [error, setError] = useState('')
  useEffect(() => {
    document.documentElement.classList.toggle('dark', dark)
    localStorage.setItem('theme', dark ? 'dark' : 'light')
  }, [dark])
  useEffect(() => {
    api<DashboardSession>('/session')
      .then(acceptSession)
      .catch(() => {})
      .finally(() => setChecking(false))
    const expire = () => {
      setUsername(null)
      setCsrf('')
    }
    window.addEventListener('admin-session-expired', expire)
    return () => window.removeEventListener('admin-session-expired', expire)
  }, [])
  if (checking) return <Loading />
  if (!username) return <Login />
  if (mustChange)
    return (
      <PasswordGate
        role="admin"
        onChanged={acceptSession}
        onLogout={async () => {
          await api('/session', { method: 'DELETE' })
          setUsername(null)
          setCsrf('')
        }}
      />
    )
  return (
    <ConfirmProvider>
      <div className="shell">
        <aside className="sidebar">
          <a className="brand" href="/admin">
            <span className="brand-icon">
              <BrandIcon size={21} />
            </span>
            <span>
              speculum
            </span>
          </a>
          <div className="nav-label">WORKSPACE</div>
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
            <div className="service-status">
              <span className="status-dot" />
              <span>Mirror service</span>
              <span className="status-label">Connected</span>
            </div>
            <div className="admin-profile">
              <span className="avatar">{username.slice(0, 1).toUpperCase()}</span>
              <span>
                <strong>{username}</strong>
                <small>Administrator</small>
              </span>
              <Button
                variant="ghost"
                size="icon"
                aria-label="Sign out"
                onClick={async () => {
                  try {
                    await api('/session', { method: 'DELETE' })
                    setUsername(null)
                    setCsrf('')
                  } catch (error) {
                    setError((error as Error).message)
                  }
                }}
              >
                <LogOut />
              </Button>
            </div>
          </div>
        </aside>
        <main className="main">
          <header className="topbar">
            <span>
              Workspace <ChevronRight size={13} /> <strong>{pages[view].label}</strong>
            </span>
            <div className="topbar-actions">
              <span className="topbar-caption">
                <Braces size={14} /> PyPI · npm · Cargo
              </span>
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
            {view === 'overview' && <Overview onPackages={() => setView('packages')} />}
            {view === 'packages' && <Packages />}
            {view === 'users' && <UsersPage />}
            {view === 'settings' && <SettingsPage onCredentialsChanged={acceptSession} />}
          </div>
          <footer>
            <span>
              <Database size={12} /> Speculum <span className="footer-divider">/</span> On-demand
              package caching
            </span>
          </footer>
        </main>
      </div>
    </ConfirmProvider>
  )
}
