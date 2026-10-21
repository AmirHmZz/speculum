import { describe, expect, test } from 'bun:test'
import { aptCredentials, aptSources, apkCredentials } from './distro-config'
import { registries } from '../api'

describe('distribution client configuration', () => {
  const base = 'https://mirror.example.com:8443'
  test('APT preserves keyrings and separates archive and security origins', () => {
    const debian = aptSources(base, 'debian')
    expect(debian).toContain(`URIs: ${base}/debian/`)
    expect(debian).toContain(`URIs: ${base}/debian-security/`)
    expect(debian).toContain('Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg')
    const ports = aptSources(base, 'ubuntu', true)
    expect(ports).toContain(`URIs: ${base}/ubuntu-ports/`)
    expect(ports).not.toContain('/ubuntu-security/')
    expect(ports).toContain('<release>-security')
  })
  test('only protected APT mirrors receive path-specific credential examples', () => {
    const config = { public_base_url: base, registries: Object.fromEntries(registries.map((kind) => [kind, { enabled: true, auth_required: false }])) }
    expect(aptCredentials(base, ['debian', 'debian-security'], config)).toBe('')
    config.registries['debian-security'].auth_required = true
    expect(aptCredentials(base, ['debian', 'debian-security'], config, 'alice')).toBe(`machine ${base}/debian-security/\nlogin alice\npassword <token>`)
  })
  test('APK netrc matches the host rather than the repository path or port', () => {
    expect(apkCredentials(base, 'alice')).toBe('machine mirror.example.com\nlogin alice\npassword <token>')
  })
})
