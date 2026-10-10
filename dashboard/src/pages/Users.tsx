import { useState, type FormEvent } from 'react'
import { KeyRound, Plus, Shield, Trash2, UserRound } from 'lucide-react'
import { api, post, put, type User } from '../api'
import { Button } from '../components/ui/button'
import { Input } from '../components/ui/input'
import { Card } from '../components/ui/card'
import { Dialog } from '../components/ui/dialog'
import { Code, ErrorMessage, Field, Loading, Toggle, useConfirm, useData } from '../common'
import { date } from '../lib/utils'
export function UsersPage() {
  const [revision, setRevision] = useState(0)
  const data = useData<User[]>('/users', revision)
  const confirm = useConfirm()
  const [edit, setEdit] = useState<User | 'new'>()
  const [username, setUsername] = useState('')
  const [enabled, setEnabled] = useState(true)
  const [tokenUser, setTokenUser] = useState<User>()
  const [tokenName, setTokenName] = useState('')
  const [issued, setIssued] = useState('')
  const [temporary, setTemporary] = useState<{ username: string; password: string }>()
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)
  function open(user: User | 'new') {
    setError('')
    setEdit(user)
    setUsername(user === 'new' ? '' : user.username)
    setEnabled(user === 'new' ? true : user.enabled)
  }
  async function save(event: FormEvent) {
    event.preventDefault()
    setBusy(true)
    setError('')
    try {
      const body = { username, enabled }
      const result = await api<{ temporary_password?: string }>(
        edit === 'new' ? '/users' : `/users/${(edit as User).id}`,
        edit === 'new' ? post(body) : put(body),
      )
      if (result?.temporary_password)
        setTemporary({ username, password: result.temporary_password })
      setEdit(undefined)
      setRevision(revision + 1)
    } catch (error) {
      setError((error as Error).message)
    } finally {
      setBusy(false)
    }
  }
  async function issue(event: FormEvent) {
    event.preventDefault()
    if (!tokenUser) return
    setBusy(true)
    setError('')
    try {
      const result = await api<{ token: string }>(
        `/users/${tokenUser.id}/tokens`,
        post({ name: tokenName }),
      )
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
          <div className="eyebrow">CONTROL ACCESS</div>
          <h1>Mirror users</h1>
          <p>Manage user dashboard accounts and package-manager tokens.</p>
        </div>
        <Button onClick={() => open('new')}>
          <Plus />
          Add user
        </Button>
      </div>
      <ErrorMessage message={data.error} />
      <div className="info-box mb-6">
        <Shield size={17} />
        <span>
          Registry authentication is controlled independently in Settings. Disabling a user
          immediately blocks dashboard access and all their tokens. Users can issue tokens after
          changing their temporary dashboard password.
        </span>
      </div>
      <Card>
        <div className="table-scroll">
          <table>
            <thead>
              <tr>
                <th>User</th>
                <th>Status</th>
                <th>Tokens</th>
                <th>Created</th>
                <th>Actions</th>
              </tr>
            </thead>
            <tbody>
              {data.data?.map((user) => (
                <tr key={user.id}>
                  <td>
                    <button className="package-link" onClick={() => open(user)}>
                      <UserRound size={17} />
                      {user.username}
                    </button>
                  </td>
                  <td>
                    <span className={`badge ${user.enabled ? 'green' : 'muted-badge'}`}>
                      {user.enabled ? 'Enabled' : 'Disabled'}
                    </span>
                    {user.must_change_password && (
                      <div className="muted text-xs mt-1">Password change required</div>
                    )}
                  </td>
                  <td>
                    <div className="token-list">
                      {user.tokens.length ? (
                        user.tokens.map((token) => (
                          <span key={token.id} className="token-chip">
                            <KeyRound size={12} />
                            {token.name}
                            <button
                              aria-label={`Revoke ${token.name}`}
                              onClick={() =>
                                confirm({
                                  title: 'Revoke token?',
                                  description: `${token.name} will immediately stop working.`,
                                  action: async () => {
                                    await api(`/tokens/${token.id}`, { method: 'DELETE' })
                                    setRevision(revision + 1)
                                  },
                                })
                              }
                            >
                              ×
                            </button>
                          </span>
                        ))
                      ) : (
                        <span className="muted">None</span>
                      )}
                    </div>
                  </td>
                  <td className="text-xs muted">{date(user.created_at)}</td>
                  <td>
                    <div className="row-actions">
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => {
                          setTokenUser(user)
                          setTokenName('')
                          setIssued('')
                          setError('')
                        }}
                      >
                        <KeyRound />
                        New token
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() =>
                          confirm({
                            title: `Reset password for ${user.username}?`,
                            description:
                              'This signs out their dashboard sessions. Package tokens stay valid.',
                            action: async () => {
                              const result = await api<{ temporary_password: string }>(
                                `/users/${user.id}/reset-password`,
                                post({}),
                              )
                              setTemporary({
                                username: user.username,
                                password: result.temporary_password,
                              })
                              setRevision(revision + 1)
                            },
                          })
                        }
                      >
                        Reset password
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon"
                        aria-label={`Delete ${user.username}`}
                        onClick={() =>
                          confirm({
                            title: `Delete ${user.username}?`,
                            description:
                              'The user and all their tokens will be permanently removed.',
                            action: async () => {
                              await api(`/users/${user.id}`, { method: 'DELETE' })
                              setRevision(revision + 1)
                            },
                          })
                        }
                      >
                        <Trash2 />
                      </Button>
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
        {data.loading && !data.data && <Loading />}
        {data.data?.length === 0 && (
          <div className="empty">
            <UserRound size={30} />
            <h3>No mirror users</h3>
            <p>Add a user to connect clients to authenticated registries.</p>
            <Button className="mt-3" onClick={() => open('new')}>
              <Plus />
              Add your first user
            </Button>
          </div>
        )}
      </Card>
      <Dialog
        open={!!edit}
        onOpenChange={(open) => {
          if (!open && !busy) setEdit(undefined)
        }}
        title={edit === 'new' ? 'Add mirror user' : 'Edit mirror user'}
        description={
          edit === 'new'
            ? 'A temporary dashboard password will be generated. The user must change it before creating tokens.'
            : 'Existing tokens stay valid. Update the username in pip or uv after renaming.'
        }
      >
        <form className="form" onSubmit={save}>
          <ErrorMessage message={error} />
          <Field label="Username">
            <Input
              value={username}
              required
              maxLength={64}
              autoComplete="off"
              onChange={(e) => setUsername(e.target.value)}
            />
          </Field>
          <Toggle label="User enabled" checked={enabled} onChange={setEnabled} />
          <div className="dialog-actions">
            <Button
              type="button"
              variant="outline"
              disabled={busy}
              onClick={() => setEdit(undefined)}
            >
              Cancel
            </Button>
            <Button disabled={busy}>{busy ? 'Saving…' : 'Save user'}</Button>
          </div>
        </form>
      </Dialog>
      <Dialog
        open={!!temporary}
        onOpenChange={(open) => {
          if (!open) setTemporary(undefined)
        }}
        title="Temporary password"
        description="Copy this password now and share it with the user. It cannot be displayed again."
      >
        <p className="text-sm">
          Username: <strong>{temporary?.username}</strong>
        </p>
        <p className="text-sm muted">
          Sign in at {window.location.origin}/account and choose a new password.
        </p>
        <Code>{temporary?.password || ''}</Code>
        <Button onClick={() => setTemporary(undefined)}>Done</Button>
      </Dialog>
      <Dialog
        open={!!tokenUser}
        onOpenChange={(open) => {
          if (!open && !busy) {
            setTokenUser(undefined)
            setIssued('')
          }
        }}
        title={issued ? 'Token created' : `New token for ${tokenUser?.username || ''}`}
        description={
          issued
            ? 'Copy this token now. It cannot be displayed again.'
            : 'Use this token for any protected registry, including as the password for pip or uv.'
        }
      >
        {issued ? (
          <>
            <Code>{issued}</Code>
            <Button
              onClick={() => {
                setTokenUser(undefined)
                setIssued('')
              }}
            >
              Done
            </Button>
          </>
        ) : (
          <form className="form" onSubmit={issue}>
            <ErrorMessage message={error} />
            <Field label="Token name">
              <Input
                required
                maxLength={100}
                value={tokenName}
                placeholder="e.g. CI runner"
                onChange={(e) => setTokenName(e.target.value)}
              />
            </Field>
            <Button disabled={busy}>{busy ? 'Creating…' : 'Create token'}</Button>
          </form>
        )}
      </Dialog>
    </>
  )
}
