import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useId,
  useState,
  type ReactNode,
} from 'react'
import { AlertCircle, Check, Copy, Loader2 } from 'lucide-react'
import { roleApi, type DashboardRole } from './api'
import { Button } from './components/ui/button'
import { Card, CardContent } from './components/ui/card'
import { Dialog } from './components/ui/dialog'
export function useData<T>(path: string, revision = 0, role: DashboardRole = 'admin') {
  const [data, setData] = useState<T>()
  const [error, setError] = useState('')
  const [loading, setLoading] = useState(true)
  useEffect(() => {
    let current = true
    setLoading(true)
    setError('')
    roleApi(role)<T>(path)
      .then((result) => {
        if (current) setData(result)
      })
      .catch((error) => {
        if (current) setError(error.message)
      })
      .finally(() => {
        if (current) setLoading(false)
      })
    return () => {
      current = false
    }
  }, [path, revision, role])
  return { data, error, loading }
}
export function ErrorMessage({ message }: { message: string }) {
  return message ? (
    <div role="alert" className="notice error">
      <AlertCircle size={16} />
      {message}
    </div>
  ) : null
}
export function Loading() {
  return (
    <div className="loading">
      <Loader2 size={20} className="animate-spin" />
      <span>Loading…</span>
    </div>
  )
}
export function Field({
  label,
  hint,
  children,
}: {
  label: string
  hint?: string
  children: ReactNode
}) {
  return (
    <label className="field">
      <span className="field-label">{label}</span>
      {children}
      {hint && <span className="field-hint">{hint}</span>}
    </label>
  )
}
export function Toggle({
  checked,
  onChange,
  label,
  hint,
}: {
  checked: boolean
  onChange: (checked: boolean) => void
  label: string
  hint?: string
}) {
  const id = useId()
  return (
    <div className="toggle-row">
      <label htmlFor={id}>
        <span className="field-label">{label}</span>
        {hint && <span className="field-hint">{hint}</span>}
      </label>
      <label className="switch">
        <input
          id={id}
          type="checkbox"
          checked={checked}
          onChange={(e) => onChange(e.target.checked)}
        />
        <span />
      </label>
    </div>
  )
}
export function Stat({
  label,
  value,
  detail,
  icon,
}: {
  label: string
  value: ReactNode
  detail?: string
  icon?: ReactNode
}) {
  return (
    <Card>
      <CardContent className="stat">
        <div className="stat-label">
          {label}
          {icon}
        </div>
        <div className="stat-value">{value}</div>
        {detail && <div className="stat-detail">{detail}</div>}
      </CardContent>
    </Card>
  )
}
export function Code({ children }: { children: string }) {
  const [copied, setCopied] = useState(false)
  const [error, setError] = useState('')
  return (
    <div className="code">
      <pre>{children}</pre>
      <Button
        variant="ghost"
        size="icon"
        aria-label="Copy configuration"
        onClick={async () => {
          try {
            await navigator.clipboard.writeText(children)
            setCopied(true)
            setTimeout(() => setCopied(false), 2000)
          } catch {
            setError('Clipboard unavailable; select the configuration to copy it.')
          }
        }}
      >
        {copied ? <Check /> : <Copy />}
      </Button>
      {error && <span className="field-hint">{error}</span>}
    </div>
  )
}
type Confirmation = { title: string; description: string; action: () => Promise<void> }
const ConfirmContext = createContext<(confirmation: Confirmation) => void>(() => {})
export function useConfirm() {
  return useContext(ConfirmContext)
}
export function ConfirmProvider({ children }: { children: ReactNode }) {
  const [confirmation, setConfirmation] = useState<Confirmation>()
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const confirm = useCallback((value: Confirmation) => {
    setError('')
    setConfirmation(value)
  }, [])
  return (
    <ConfirmContext.Provider value={confirm}>
      {children}
      <Dialog
        open={!!confirmation}
        onOpenChange={(open) => {
          if (!open && !busy) setConfirmation(undefined)
        }}
        title={confirmation?.title || 'Confirm action'}
        description={confirmation?.description}
      >
        <ErrorMessage message={error} />
        <div className="dialog-actions">
          <Button variant="outline" disabled={busy} onClick={() => setConfirmation(undefined)}>
            Cancel
          </Button>
          <Button
            variant="destructive"
            disabled={busy}
            onClick={async () => {
              if (!confirmation) return
              setBusy(true)
              setError('')
              try {
                await confirmation.action()
                setConfirmation(undefined)
              } catch (error) {
                setError((error as Error).message)
              } finally {
                setBusy(false)
              }
            }}
          >
            {busy && <Loader2 className="animate-spin" />}Confirm
          </Button>
        </div>
      </Dialog>
    </ConfirmContext.Provider>
  )
}
