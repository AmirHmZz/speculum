use crate::error::{Error, Result};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, time::Duration};

pub const DISTROS: [&str; 6] = [
    "debian",
    "debian-security",
    "ubuntu",
    "ubuntu-security",
    "ubuntu-ports",
    "alpine",
];
pub const REGISTRIES: [&str; 9] = [
    "pypi",
    "npm",
    "cargo",
    "debian",
    "debian-security",
    "ubuntu",
    "ubuntu-security",
    "ubuntu-ports",
    "alpine",
];
pub fn is_distro(kind: &str) -> bool {
    DISTROS.contains(&kind)
}
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct ProxySettings {
    pub url: String,
    pub username: Option<String>,
    pub password: Option<String>,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct RegistrySettings {
    pub enabled: bool,
    pub auth_required: bool,
    pub upstream_url: String,
    pub allowed_artifact_hosts: Vec<String>,
    pub proxy: Option<ProxySettings>,
    pub cache_lifetime_seconds: Option<u64>,
    pub metadata_freshness_seconds: u64,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub public_base_url: String,
    pub registries: BTreeMap<String, RegistrySettings>,
    pub connect_timeout_seconds: u64,
    pub download_timeout_seconds: u64,
    pub download_concurrency: usize,
    pub cleanup_interval_seconds: u64,
    pub admin_session_lifetime_seconds: u64,
    pub user_session_lifetime_seconds: u64,
}
impl Default for Settings {
    fn default() -> Self {
        let registries = [
            ("pypi", "https://pypi.org/simple/", "files.pythonhosted.org"),
            ("npm", "https://registry.npmjs.org/", "registry.npmjs.org"),
            ("cargo", "https://index.crates.io/", "static.crates.io"),
            ("debian", "https://deb.debian.org/debian/", "deb.debian.org"),
            (
                "debian-security",
                "https://deb.debian.org/debian-security/",
                "deb.debian.org",
            ),
            (
                "ubuntu",
                "https://archive.ubuntu.com/ubuntu/",
                "archive.ubuntu.com",
            ),
            (
                "ubuntu-security",
                "https://security.ubuntu.com/ubuntu/",
                "security.ubuntu.com",
            ),
            (
                "ubuntu-ports",
                "https://ports.ubuntu.com/ubuntu-ports/",
                "ports.ubuntu.com",
            ),
            (
                "alpine",
                "https://dl-cdn.alpinelinux.org/alpine/",
                "dl-cdn.alpinelinux.org",
            ),
        ]
        .into_iter()
        .map(|(kind, url, host)| {
            (
                kind.into(),
                RegistrySettings {
                    enabled: true,
                    auth_required: true,
                    upstream_url: url.into(),
                    allowed_artifact_hosts: vec![host.into()],
                    proxy: None,
                    cache_lifetime_seconds: None,
                    metadata_freshness_seconds: 300,
                },
            )
        })
        .collect();
        Self {
            public_base_url: "http://localhost:8080".into(),
            registries,
            connect_timeout_seconds: 10,
            download_timeout_seconds: 600,
            download_concurrency: 8,
            cleanup_interval_seconds: 60,
            admin_session_lifetime_seconds: 43200,
            user_session_lifetime_seconds: 43200,
        }
    }
}
pub fn http_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).map_err(|_| Error::bad("Invalid URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.query().is_some()
    {
        return Err(Error::bad(
            "Use an HTTP(S) URL without credentials, query, or fragment",
        ));
    }
    Ok(url)
}
impl Settings {
    /// Add new mirrors to saved configurations without replacing existing settings.
    pub fn upgrade(&mut self) {
        let defaults = Self::default();
        for kind in DISTROS {
            self.registries
                .entry(kind.into())
                .or_insert_with(|| defaults.registries[kind].clone());
        }
    }

    pub fn validate(&self) -> Result<()> {
        let base = http_url(&self.public_base_url)?;
        if base.path() != "/" {
            return Err(Error::bad(
                "Public base URL must be an origin without a path",
            ));
        }
        if self.registries.len() != REGISTRIES.len()
            || REGISTRIES.iter().any(|k| !self.registries.contains_key(*k))
        {
            return Err(Error::bad(
                "Configure all supported registries and distribution mirrors",
            ));
        }
        if !(1..=u32::MAX as u64).contains(&self.connect_timeout_seconds)
            || !(1..=u32::MAX as u64).contains(&self.download_timeout_seconds)
            || !(1..=256).contains(&self.download_concurrency)
            || !(1..=u32::MAX as u64).contains(&self.cleanup_interval_seconds)
            || !(60..=2592000).contains(&self.admin_session_lifetime_seconds)
            || !(60..=2592000).contains(&self.user_session_lifetime_seconds)
        {
            return Err(Error::bad(
                "Timeouts/cleanup must be 1–4294967295 seconds, concurrency 1–256, session lifetime 60–2592000 seconds",
            ));
        }
        for config in self.registries.values() {
            http_url(&config.upstream_url)?;
            if config.cache_lifetime_seconds == Some(0)
                || config
                    .cache_lifetime_seconds
                    .is_some_and(|n| n > i64::MAX as u64)
                || config.metadata_freshness_seconds > i64::MAX as u64
            {
                return Err(Error::bad("Invalid cache duration; use null for forever"));
            }
            for host in &config.allowed_artifact_hosts {
                if host.is_empty() || host.contains(['/', ':', '@', '*', ' ']) {
                    return Err(Error::bad(
                        "Artifact hosts must be exact hostnames (no ports or wildcards)",
                    ));
                }
            }
            if let Some(proxy) = &config.proxy {
                let url = Url::parse(&proxy.url).map_err(|_| Error::bad("Invalid proxy URL"))?;
                if !matches!(url.scheme(), "socks5" | "socks5h" | "http" | "https")
                    || url.host_str().is_none()
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.query().is_some()
                    || url.fragment().is_some()
                    || !url.path().is_empty() && url.path() != "/"
                {
                    return Err(Error::bad(
                        "Use a proxy URL with credentials in the separate fields",
                    ));
                }
            }
        }
        Ok(())
    }
    pub fn redacted(&self) -> Self {
        let mut settings = self.clone();
        for config in settings.registries.values_mut() {
            if let Some(proxy) = &mut config.proxy
                && proxy.password.is_some()
            {
                proxy.password = Some("********".into());
            }
        }
        settings
    }
}
pub fn clients(settings: &Settings) -> Result<BTreeMap<String, Client>> {
    settings.validate()?;
    settings
        .registries
        .iter()
        .map(|(kind, config)| {
            let mut builder = Client::builder()
                .no_proxy()
                .connect_timeout(Duration::from_secs(settings.connect_timeout_seconds))
                .timeout(Duration::from_secs(settings.download_timeout_seconds))
                .user_agent(concat!("Speculum/", env!("CARGO_PKG_VERSION")))
                .redirect(reqwest::redirect::Policy::none());
            if let Some(proxy) = &config.proxy {
                let mut url =
                    Url::parse(&proxy.url).map_err(|_| Error::bad("Invalid proxy URL"))?;
                if let Some(username) = &proxy.username {
                    url.set_username(username)
                        .map_err(|_| Error::bad("Invalid proxy username"))?;
                }
                if let Some(password) = &proxy.password {
                    url.set_password(Some(password))
                        .map_err(|_| Error::bad("Invalid proxy password"))?;
                }
                builder = builder.proxy(
                    reqwest::Proxy::all(url)
                        .map_err(|_| Error::bad("Invalid proxy configuration"))?,
                );
            }
            Ok((
                kind.clone(),
                builder
                    .build()
                    .map_err(|_| Error::bad("Unable to construct upstream client"))?,
            ))
        })
        .collect()
}
