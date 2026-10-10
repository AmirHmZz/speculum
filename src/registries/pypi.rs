use super::{artifact_link, encode, join};
use crate::{
    App,
    cache::{self, ArtifactInput},
    error::{Error, Result},
};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, Method},
    response::Response,
};
use serde_json::{Value, json};
use sqlx::Row;
use std::{collections::BTreeMap, sync::Arc};

pub fn normalize(name: &str) -> Result<String> {
    if name.is_empty()
        || name.len() > 200
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        return Err(Error::bad("Invalid Python package name"));
    }
    let mut output = String::new();
    for character in name.chars() {
        if matches!(character, '-' | '_' | '.') {
            if !output.ends_with('-') {
                output.push('-');
            }
        } else {
            output.push(character.to_ascii_lowercase());
        }
    }
    Ok(output)
}
pub fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn version(filename: &str) -> String {
    if filename.ends_with(".whl") {
        return filename.split('-').nth(1).unwrap_or("unknown").to_owned();
    }
    let mut stem = filename;
    for extension in [".tar.gz", ".tar.bz2", ".tar.xz", ".tgz", ".zip", ".tar"] {
        if let Some(value) = filename.strip_suffix(extension) {
            stem = value;
            break;
        }
    }
    stem.rsplit_once('-')
        .map(|(_, version)| version)
        .unwrap_or("unknown")
        .to_owned()
}
fn wants_json(headers: &HeaderMap) -> bool {
    headers
        .get("accept")
        .and_then(|h| h.to_str().ok())
        .is_some_and(|accept| {
            accept.contains("application/vnd.pypi.simple") && accept.contains("json")
                || accept == "application/json"
        })
}
pub async fn root(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
) -> Result<Response> {
    let rows = sqlx::query("SELECT name FROM packages WHERE registry='pypi' ORDER BY name")
        .fetch_all(&app.db)
        .await?;
    let names: Vec<String> = rows.into_iter().map(|r| r.get("name")).collect();
    if wants_json(&headers) {
        return Ok(cache::metadata_response(serde_json::to_vec(&json!({"meta":{"api-version":"1.0"},"projects":names.iter().map(|name| json!({"name":name})).collect::<Vec<_>>()})).unwrap(),"application/vnd.pypi.simple.v1+json",&headers,&method));
    }
    let links: String = names
        .iter()
        .map(|name| format!("<a href=\"{}/\">{}</a>\n", encode(name), escape(name)))
        .collect();
    Ok(cache::metadata_response(format!("<!doctype html><html><head><meta name=\"pypi:repository-version\" content=\"1.0\"><title>Speculum index</title></head><body>{links}</body></html>").into_bytes(),"application/vnd.pypi.simple.v1+html",&headers,&method))
}
pub async fn project(
    State(app): State<Arc<App>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    method: Method,
) -> Result<Response> {
    let _maintenance = app.maintenance.read().await;
    let name = normalize(&name)?;
    let runtime = app.snapshot().await;
    let url = format!(
        "{}/",
        join(
            &runtime.settings.registries["pypi"].upstream_url,
            &encode(&name)
        )
    );
    let (bytes, content_type) = cache::metadata(
        &app,
        &runtime,
        "pypi",
        &url,
        "application/vnd.pypi.simple.v1+json, application/vnd.pypi.simple.v1+html;q=0.5",
    )
    .await?;
    let mut document: Value = if content_type.contains("json") {
        serde_json::from_slice(&bytes).map_err(|_| Error::upstream("Invalid PyPI metadata"))?
    } else {
        html_document(&bytes, &name, &url)?
    };
    document["name"] = json!(name);
    let files = document
        .get_mut("files")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| Error::upstream("Missing PyPI file list"))?;
    for file in files.iter_mut() {
        let filename = file
            .get("filename")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::upstream("Missing Python filename"))?
            .to_owned();
        let upstream_url = reqwest::Url::parse(&url)
            .unwrap()
            .join(
                file.get("url")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::upstream("Missing Python file URL"))?,
            )
            .map_err(|_| Error::upstream("Invalid Python file URL"))?;
        let checksums: BTreeMap<String, String> = file
            .get("hashes")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| Error::upstream("Invalid Python hashes"))?
            .unwrap_or_default();
        let version = version(&filename);
        let item = cache::register(
            &app,
            &runtime,
            ArtifactInput {
                registry: "pypi",
                package: &name,
                version: &version,
                filename: &filename,
                url: upstream_url.as_str(),
                checksums,
                sidecar: false,
            },
        )
        .await?;
        let link = artifact_link(&runtime, &item);
        if let Some(metadata) = file
            .get("core-metadata")
            .or_else(|| file.get("dist-info-metadata"))
            && metadata != &Value::Bool(false)
            && !metadata.is_null()
        {
            let metadata_hashes = if metadata.is_object() {
                serde_json::from_value(metadata.clone())
                    .map_err(|_| Error::upstream("Invalid Python metadata hashes"))?
            } else {
                BTreeMap::new()
            };
            let mut metadata_url = upstream_url.clone();
            metadata_url.set_fragment(None);
            metadata_url.set_path(&format!("{}.metadata", metadata_url.path()));
            // The distribution URL's .metadata suffix resolves this record.
            cache::register(
                &app,
                &runtime,
                ArtifactInput {
                    registry: "pypi",
                    package: &name,
                    version: &version,
                    filename: &format!("{filename}.metadata"),
                    url: metadata_url.as_str(),
                    checksums: metadata_hashes,
                    sidecar: true,
                },
            )
            .await?;
        }
        file["url"] = json!(link);
        // Optional external signatures/attestations are not distribution artifacts.
        if let Some(object) = file.as_object_mut() {
            object.remove("provenance");
            object.remove("gpg-sig");
        }
    }
    app.package("pypi", &name).await?;
    if wants_json(&headers) {
        return Ok(cache::metadata_response(
            serde_json::to_vec(&document).unwrap(),
            "application/vnd.pypi.simple.v1+json",
            &headers,
            &method,
        ));
    }
    let mut links = String::new();
    for file in files.iter() {
        let filename = file["filename"].as_str().unwrap();
        let mut href = file["url"].as_str().unwrap().to_owned();
        if let Some((algorithm, value)) =
            file.get("hashes")
                .and_then(Value::as_object)
                .and_then(|hashes| {
                    hashes
                        .get_key_value("sha256")
                        .or_else(|| hashes.get_key_value("sha512"))
                        .or_else(|| hashes.iter().next())
                })
            && let Some(hash) = value.as_str()
        {
            href.push_str(&format!("#{algorithm}={hash}"));
        }
        let mut attributes = String::new();
        if let Some(requirement) = file.get("requires-python").and_then(Value::as_str) {
            attributes.push_str(&format!(
                " data-requires-python=\"{}\"",
                escape(requirement)
            ));
        }
        if let Some(yanked) = file.get("yanked")
            && (yanked == &Value::Bool(true) || yanked.is_string())
        {
            attributes.push_str(&format!(
                " data-yanked=\"{}\"",
                escape(yanked.as_str().unwrap_or(""))
            ));
        }
        if let Some(metadata) = file
            .get("core-metadata")
            .or_else(|| file.get("dist-info-metadata"))
            && metadata != &Value::Bool(false)
            && !metadata.is_null()
        {
            let value = metadata
                .as_object()
                .and_then(|hashes| hashes.iter().next())
                .map(|(key, value)| format!("{key}={}", value.as_str().unwrap_or("")))
                .unwrap_or_else(|| "true".into());
            attributes.push_str(&format!(
                " data-core-metadata=\"{}\" data-dist-info-metadata=\"{}\"",
                escape(&value),
                escape(&value)
            ));
        }
        links.push_str(&format!(
            "<a href=\"{}\"{attributes}>{}</a>\n",
            escape(&href),
            escape(filename)
        ));
    }
    Ok(cache::metadata_response(format!("<!doctype html><html><head><meta name=\"pypi:repository-version\" content=\"1.0\"><title>{}</title></head><body>{links}</body></html>",escape(&name)).into_bytes(),"application/vnd.pypi.simple.v1+html",&headers,&method))
}
pub async fn file(
    State(app): State<Arc<App>>,
    Path((id, filename)): Path<(String, String)>,
    headers: HeaderMap,
    method: Method,
    identity: Option<axum::Extension<crate::auth::MirrorIdentity>>,
) -> Result<Response> {
    let distribution = cache::artifact(&app, &id).await?;
    let id = if filename == format!("{}.metadata", distribution.filename) {
        sqlx::query_scalar::<_,String>("SELECT id FROM artifacts WHERE package_id=? AND upstream_identity=? AND filename=? AND is_sidecar=1")
            .bind(&distribution.package_id).bind(&distribution.upstream_identity).bind(&filename).fetch_optional(&app.db).await?.ok_or_else(Error::not_found)?
    } else {
        id
    };
    cache::serve(
        app,
        "pypi",
        &id,
        Some(&filename),
        headers,
        method,
        identity.map(|identity| identity.0.0),
    )
    .await
}
// A small tokenizer for the Simple API's anchor attributes. It recognizes quoted
// delimiters, entities, comments and raw script/style sections; no HTML is forwarded.
fn html_document(bytes: &[u8], name: &str, base: &str) -> Result<Value> {
    let html = std::str::from_utf8(bytes).map_err(|_| Error::upstream("Invalid HTML encoding"))?;
    let lower = html.to_ascii_lowercase();
    let mut offset = 0;
    let mut files = Vec::new();
    while let Some(relative) = lower[offset..].find('<') {
        let start = offset + relative;
        if lower[start..].starts_with("<!--") {
            offset = lower[start + 4..]
                .find("-->")
                .map(|end| start + 4 + end + 3)
                .unwrap_or(lower.len());
            continue;
        }
        if lower[start..].starts_with("<script") || lower[start..].starts_with("<style") {
            let tag = if lower[start..].starts_with("<script") {
                "</script>"
            } else {
                "</style>"
            };
            offset = lower[start..]
                .find(tag)
                .map(|end| start + end + tag.len())
                .unwrap_or(lower.len());
            continue;
        }
        let mut quote = None;
        let mut end = None;
        for (i, c) in html[start + 1..].char_indices() {
            if let Some(q) = quote {
                if c == q {
                    quote = None;
                }
            } else if c == '\'' || c == '"' {
                quote = Some(c);
            } else if c == '>' {
                end = Some(start + 1 + i);
                break;
            }
        }
        let Some(end) = end else {
            break;
        };
        offset = end + 1;
        let tag = &html[start + 1..end];
        if !tag.starts_with(['a', 'A']) || !tag[1..].starts_with(char::is_whitespace) {
            continue;
        }
        let mut attrs = BTreeMap::<String, String>::new();
        let mut rest = tag[1..].trim();
        while !rest.is_empty() {
            let n = rest
                .find(|c: char| c.is_whitespace() || c == '=')
                .unwrap_or(rest.len());
            let key = rest[..n].to_ascii_lowercase();
            rest = rest[n..].trim_start();
            if n == 0 {
                rest = &rest[1..];
                continue;
            }
            let value = if let Some(tail) = rest.strip_prefix('=') {
                rest = tail.trim_start();
                if rest.starts_with(['\'', '"']) {
                    let q = rest.chars().next().unwrap();
                    rest = &rest[1..];
                    let end = rest.find(q).unwrap_or(rest.len());
                    let value = &rest[..end];
                    rest = rest.get(end + 1..).unwrap_or("");
                    value.to_owned()
                } else {
                    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                    let value = &rest[..end];
                    rest = &rest[end..];
                    value.to_owned()
                }
            } else {
                String::new()
            };
            attrs.insert(key, decode_entities(&value));
            rest = rest.trim_start();
        }
        let Some(href) = attrs.get("href") else {
            continue;
        };
        let url = reqwest::Url::parse(base)
            .unwrap()
            .join(href)
            .map_err(|_| Error::upstream("Invalid file link"))?;
        let filename =
            percent_encoding::percent_decode_str(url.path().rsplit('/').next().unwrap_or(""))
                .decode_utf8_lossy()
                .to_string();
        let mut hashes = serde_json::Map::new();
        if let Some((key, value)) = url.fragment().and_then(|fragment| fragment.split_once('=')) {
            hashes.insert(key.into(), json!(value));
        }
        let mut file = json!({"filename":filename,"url":url.as_str(),"hashes":hashes});
        if let Some(value) = attrs.get("data-requires-python") {
            file["requires-python"] = json!(value);
        }
        if let Some(value) = attrs.get("data-yanked") {
            file["yanked"] = if value.is_empty() {
                json!(true)
            } else {
                json!(value)
            };
        }
        if let Some(value) = attrs
            .get("data-core-metadata")
            .or_else(|| attrs.get("data-dist-info-metadata"))
        {
            file["core-metadata"] = if let Some((key, hash)) = value.split_once('=') {
                json!({key:hash})
            } else {
                json!(value != "false")
            };
        }
        files.push(file);
    }
    Ok(json!({"meta":{"api-version":"1.0"},"name":name,"files":files}))
}
fn decode_entities(value: &str) -> String {
    let mut result = String::new();
    let mut rest = value;
    while let Some(start) = rest.find('&') {
        result.push_str(&rest[..start]);
        rest = &rest[start..];
        let Some(end) = rest.find(';') else {
            break;
        };
        let entity = &rest[1..end];
        let character = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .and_then(|n| u32::from_str_radix(n, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|n| n.parse().ok()))
                .and_then(char::from_u32),
        };
        if let Some(character) = character {
            result.push(character);
        } else {
            result.push_str(&rest[..=end]);
        }
        rest = &rest[end + 1..];
    }
    result.push_str(rest);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn html_metadata_is_parsed_and_escaped_without_forwarding_markup() {
        let html = br#"<html><!-- <a href='evil'> --><script><a href='evil'></a></script><a data-requires-python='&gt;=3.9' data-yanked='review &amp; test' data-core-metadata='sha256=abc' href='/files/demo-1.0.0.tar.gz#sha256=def'>demo</a></html>"#;
        let parsed = html_document(html, "demo", "https://pypi.org/simple/demo/").unwrap();
        assert_eq!(parsed["files"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["files"][0]["requires-python"], ">=3.9");
        assert_eq!(parsed["files"][0]["yanked"], "review & test");
        assert_eq!(parsed["files"][0]["hashes"]["sha256"], "def");
        assert_eq!(parsed["files"][0]["core-metadata"]["sha256"], "abc");
        assert_eq!(version("some_package-1.2.3-py3-none-any.whl"), "1.2.3");
        assert_eq!(version("some-package-1.2.3.tar.gz"), "1.2.3");
        assert!(escape("<script>\"").contains("&lt;script&gt;&quot;"));
    }
    #[test]
    fn truncated_unicode_html_does_not_panic() {
        let parsed = html_document(
            "<a href='café".as_bytes(),
            "demo",
            "https://pypi.org/simple/demo/",
        )
        .unwrap();
        assert_eq!(parsed["files"].as_array().unwrap().len(), 0);
        let yanked = html_document(
            b"<a href='/files/demo-1.0.tar.gz' data-yanked>demo</a>",
            "demo",
            "https://pypi.org/simple/demo/",
        )
        .unwrap();
        assert_eq!(yanked["files"][0]["yanked"], true);
    }
}
