export const registries = ['pypi', 'npm', 'cargo', 'debian', 'debian-security', 'ubuntu', 'ubuntu-security', 'ubuntu-ports', 'alpine'] as const
export type Registry = (typeof registries)[number]
export const labels: Record<Registry, string> = {
  pypi: 'Python', npm: 'JavaScript', cargo: 'Rust', debian: 'Debian',
  'debian-security': 'Debian security', ubuntu: 'Ubuntu', 'ubuntu-security': 'Ubuntu security',
  'ubuntu-ports': 'Ubuntu ports', alpine: 'Alpine',
}
export const isDistro = (kind: Registry) => kind !== 'pypi' && kind !== 'npm' && kind !== 'cargo'
const languageRegistries: Partial<Record<Registry, { icon: string; description: string }>> = {
  pypi: { icon: 'Py', description: 'PyPI' }, npm: { icon: 'JS', description: 'npm registry' },
  cargo: { icon: 'Rs', description: 'crates.io' },
}
export const registryIcon = (kind: Registry) => languageRegistries[kind]?.icon ?? (kind === 'alpine' ? 'Apk' : 'Apt')
export const registryDescription = (kind: Registry) => languageRegistries[kind]?.description ?? (kind === 'alpine' ? 'APK repository' : 'APT repository')
export interface ProxyConfig {
  url: string
  username: string | null
  password: string | null
}
export interface RegistryConfig {
  enabled: boolean
  auth_required: boolean
  upstream_url: string
  allowed_artifact_hosts: string[]
  proxy: ProxyConfig | null
  cache_lifetime_seconds: number | null
  metadata_freshness_seconds: number
}
export interface Settings {
  public_base_url: string
  registries: Record<Registry, RegistryConfig>
  connect_timeout_seconds: number
  download_timeout_seconds: number
  download_concurrency: number
  cleanup_interval_seconds: number
  admin_session_lifetime_seconds: number
  user_session_lifetime_seconds: number
}
export interface Totals {
  packages: number
  versions: number
  files: number
  stored_bytes: number
  requests: number
  hits: number
  misses: number
  upstream_downloads: number
  failures: number
  upstream_bytes: number
  client_bytes: number
  last_access: number | null
}
export interface Package extends Totals {
  id: string
  name: string
  registry: Registry
}
export interface Artifact {
  id: string
  version: string
  filename: string
  size: number
  downloaded_at: number | null
  expires_at: number | null
  is_sidecar: boolean
  upstream: string
}
export interface User {
  id: string
  username: string
  enabled: boolean
  created_at: number
  must_change_password: boolean
  tokens: { id: string; name: string; created_at: number }[]
}
export interface Summary {
  total: Totals
  registries: Record<Registry, Totals>
  known_packages: number
}
export type DashboardRole = 'admin' | 'user'
export interface DashboardSession {
  role: DashboardRole
  username: string
  csrf_token: string
  must_change_password: boolean
}
export interface UserToken {
  id: string
  name: string
  prefix: string
  created_at: number
  last_used_at: number | null
}
export interface ClientConfig {
  public_base_url: string
  registries: Record<Registry, { enabled: boolean; auth_required: boolean }>
}
export type PersonalTotals = Omit<Totals, 'versions' | 'files' | 'stored_bytes'>
export interface PersonalSummary {
  total: PersonalTotals
  registries: Record<Registry, PersonalTotals>
}
export interface PersonalPackage extends Omit<PersonalTotals, 'packages'> {
  id: string
  name: string
  registry: Registry
}
const csrf = { admin: '', user: '' }
export function setCsrf(value: string, role: DashboardRole = 'admin') {
  csrf[role] = value
}
export async function api<T>(path: string, options: RequestInit = {}): Promise<T> {
  return request<T>('admin', path, options)
}
export async function userApi<T>(path: string, options: RequestInit = {}): Promise<T> {
  return request<T>('user', path, options)
}
export function roleApi(role: DashboardRole) {
  return role === 'admin' ? api : userApi
}
export async function signIn(username: string, password: string): Promise<DashboardSession> {
  const response = await fetch('/api/login', {
    ...post({ username, password }),
    headers: { 'Content-Type': 'application/json' },
  })
  const body = await response.json()
  if (!response.ok) throw new Error(body.error || 'Sign in failed')
  return body
}
async function request<T>(role: DashboardRole, path: string, options: RequestInit): Promise<T> {
  const response = await fetch(`/api/${role}${path}`, {
    ...options,
    headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': csrf[role], ...options.headers },
  })
  if (response.status === 401 && path !== '/login' && path !== '/session')
    window.dispatchEvent(new Event(`${role}-session-expired`))
  if (!response.ok) {
    const body = await response.json().catch(() => ({ error: response.statusText }))
    throw new Error(body.error || 'Request failed')
  }
  if (response.status === 204) return undefined as T
  return response.json() as Promise<T>
}
export const put = (body: unknown) => ({ method: 'PUT', body: JSON.stringify(body) })
export const post = (body: unknown) => ({ method: 'POST', body: JSON.stringify(body) })
