import { useState, type FormEvent } from 'react'
import { ChevronRight, LockKeyhole } from 'lucide-react'
import { roleApi, post, put, type DashboardRole, type DashboardSession } from './api'
import { BrandIcon } from './components/brand-icon'
import { Button } from './components/ui/button'
import { Input } from './components/ui/input'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from './components/ui/card'
import { ErrorMessage, Field } from './common'
export function Login({
  role,
  onLogin,
}: {
  role: DashboardRole
  onLogin: (session: DashboardSession) => void
}) {
  const [username, setUsername] = useState(role === 'admin' ? 'admin' : '')
  const [password, setPassword] = useState('')
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)
  async function submit(event: FormEvent) {
    event.preventDefault()
    setBusy(true)
    setError('')
    try {
      onLogin(await roleApi(role)<DashboardSession>('/login', post({ username, password })))
    } catch (error) {
      setError((error as Error).message)
    } finally {
      setBusy(false)
    }
  }
  return (
    <div className="login">
      <div className="login-heading">
        <span className="brand-icon">
          <BrandIcon />
        </span>
        <h1>speculum</h1>
        <p>{role === 'admin' ? 'Your packages. Closer to home.' : 'Your access. Your activity.'}</p>
      </div>
      <Card className="login-card">
        <CardHeader>
          <CardTitle>Welcome back</CardTitle>
          <CardDescription>
            {role === 'admin'
              ? 'Sign in to manage your package mirror.'
              : 'Sign in to manage your tokens and view your usage.'}
          </CardDescription>
        </CardHeader>
        <CardContent>
          <form onSubmit={submit} className="form">
            <ErrorMessage message={error} />
            <Field label="Username">
              <Input
                autoComplete="username"
                required
                readOnly={role === 'admin'}
                value={username}
                onChange={(e) => setUsername(e.target.value)}
              />
            </Field>
            <Field label="Password">
              <Input
                type="password"
                autoComplete="current-password"
                required
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              />
            </Field>
            <Button disabled={busy}>
              {busy ? 'Signing in…' : 'Sign in'}
              <ChevronRight />
            </Button>
          </form>
        </CardContent>
      </Card>
      <a className="text-sm muted" href={role === 'admin' ? '/account' : '/admin'}>
        {role === 'admin' ? 'Mirror user sign in' : 'Administrator sign in'}
      </a>
    </div>
  )
}
export function PasswordForm({
  role,
  onChanged,
}: {
  role: DashboardRole
  onChanged: (session: DashboardSession) => void
}) {
  const [current, setCurrent] = useState('')
  const [password, setPassword] = useState('')
  const [confirm, setConfirm] = useState('')
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)
  const [saved, setSaved] = useState(false)
  async function submit(event: FormEvent) {
    event.preventDefault()
    setError('')
    setSaved(false)
    if (password !== confirm) {
      setError('Passwords do not match')
      return
    }
    setBusy(true)
    try {
      const session = await roleApi(role)<DashboardSession>(
        role === 'admin' ? '/credentials' : '/password',
        put({ current_password: current, password }),
      )
      setCurrent('')
      setPassword('')
      setConfirm('')
      setSaved(true)
      onChanged(session)
    } catch (error) {
      setError((error as Error).message)
    } finally {
      setBusy(false)
    }
  }
  return (
    <form className="form" onSubmit={submit}>
      <ErrorMessage message={error} />
      {saved && (
        <p className="text-sm muted" role="status">
          Password changed. Your other dashboard sessions have been signed out.
        </p>
      )}
      <Field label="Current password">
        <Input
          type="password"
          required
          autoComplete="current-password"
          value={current}
          onChange={(e) => setCurrent(e.target.value)}
        />
      </Field>
      <Field label="New password" hint="Use at least 10 characters.">
        <Input
          type="password"
          required
          minLength={10}
          maxLength={1024}
          autoComplete="new-password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
        />
      </Field>
      <Field label="Confirm new password">
        <Input
          type="password"
          required
          autoComplete="new-password"
          value={confirm}
          onChange={(e) => setConfirm(e.target.value)}
        />
      </Field>
      <Button disabled={busy}>
        {busy ? 'Changing…' : 'Change password'}
        <LockKeyhole />
      </Button>
    </form>
  )
}
export function PasswordGate({
  role,
  onChanged,
  onLogout,
}: {
  role: DashboardRole
  onChanged: (session: DashboardSession) => void
  onLogout: () => Promise<void>
}) {
  const [error, setError] = useState('')
  return (
    <div className="login">
      <div className="login-heading">
        <span className="brand-icon">
          <BrandIcon />
        </span>
        <h1>Choose a new password</h1>
        <p>Replace your temporary password to continue.</p>
      </div>
      <Card className="login-card">
        <CardHeader>
          <CardTitle>Make this account yours</CardTitle>
          <CardDescription>
            Your temporary password must be changed before accessing the dashboard.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <PasswordForm role={role} onChanged={onChanged} />
          <ErrorMessage message={error} />
        </CardContent>
      </Card>
      <Button
        variant="ghost"
        onClick={() => onLogout().catch((error: Error) => setError(error.message))}
      >
        Sign out
      </Button>
    </div>
  )
}
