import type { ClientConfig, Registry } from '../api'

export function aptSources(base: string, distro: 'debian' | 'ubuntu', ports = false) {
  const root = ports ? 'ubuntu-ports' : distro
  const components = distro === 'debian' ? 'main contrib non-free non-free-firmware' : 'main restricted universe multiverse'
  const keyring = distro === 'debian' ? 'debian-archive-keyring' : 'ubuntu-archive-keyring'
  const stanza = (mirror: string, suites: string) => `Types: deb\nURIs: ${base}/${mirror}/\nSuites: ${suites}\nComponents: ${components}\nSigned-By: /usr/share/keyrings/${keyring}.gpg`
  const normal = '<release> <release>-updates <release>-backports'
  return ports ? stanza(root, `${normal} <release>-security`) : `${stanza(root, normal)}\n\n${stanza(`${distro}-security`, '<release>-security')}`
}

export function aptCredentials(base: string, kinds: Registry[], config?: ClientConfig, username?: string) {
  return kinds.filter((kind) => config?.registries[kind].auth_required !== false)
    .map((kind) => `machine ${base}/${kind}/\nlogin ${username || '<username>'}\npassword <token>`).join('\n\n')
}

export function apkCredentials(base: string, username?: string) {
  return `machine ${new URL(base).hostname}\nlogin ${username || '<username>'}\npassword <token>`
}
