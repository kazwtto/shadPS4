//! Atualizações do repositório do usuário.
//!
//! O launcher nunca consulta um repositório que não seja o escolhido. A fonte
//! padrão é o repositório configurado (detectado do `git` local quando existe um
//! remoto do GitHub) e a consulta olha, nesta ordem:
//!
//! 1. a última *release* publicada, quando houver;
//! 2. o commit mais recente do repositório;
//! 3. a pasta `builds/` do repositório, com executáveis para baixar.
//!
//! O transporte é o WinHTTP do próprio Windows: nenhuma dependência nova.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::builds::Version;

pub const GITHUB_API: &str = "https://api.github.com";

/// Repositório do dono desta instalação. É fixo no código: nenhuma tela pede
/// um link e o upstream nunca é consultado.
pub const DEFAULT_REPOSITORY: &str = "kazwtto/shadPS4";

/// Dados salvos só valem para o repositório a que pertencem: um cache de outro
/// projeto (por exemplo, o upstream) nunca aparece como atualização deste.
pub fn same_repository(left: &str, right: &str) -> bool {
    let left = left.trim();
    !left.is_empty() && left.eq_ignore_ascii_case(right.trim())
}

/// Publicação (release) encontrada no repositório do usuário.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub tag: String,
    pub name: String,
    pub published: String,
    pub asset_name: String,
    pub asset_url: String,
}

impl Release {
    pub fn version(&self) -> Option<Version> {
        Version::parse(&self.tag)
    }

    pub fn version_label(&self) -> String {
        self.version()
            .map(|version| version.label())
            .unwrap_or_else(|| self.tag.clone())
    }
}

/// Commit mais recente do repositório.
#[derive(Debug, Clone, PartialEq)]
pub struct Commit {
    pub sha: String,
    pub date: String,
    pub message: String,
}

impl Commit {
    /// Mensagem em uma linha, para caber no aviso.
    pub fn short_message(&self) -> String {
        self.message
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_string()
    }
}

/// Executável publicado na pasta `builds/` do repositório.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteBuild {
    pub name: String,
    pub size: u64,
    pub url: String,
}

impl RemoteBuild {
    pub fn version(&self) -> Option<Version> {
        crate::builds::version_from_name(&self.name)
    }
}

/// Aceita `usuário/repositório` ou a URL completa do GitHub.
pub fn repository_slug(repository: &str) -> Result<String> {
    let text = repository.trim().trim_end_matches('/');
    let text = text
        .strip_prefix("https://github.com/")
        .or_else(|| text.strip_prefix("http://github.com/"))
        .or_else(|| text.strip_prefix("github.com/"))
        .unwrap_or(text);
    let parts = text.split('/').collect::<Vec<_>>();
    if parts.len() != 2 {
        bail!("use o formato usuário/repositório");
    }
    for part in &parts {
        if part.is_empty()
            || !part
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
        {
            bail!("nome de repositório inválido: {repository}");
        }
    }
    Ok(format!("{}/{}", parts[0], parts[1]))
}

pub fn releases_url(repository: &str) -> Result<String> {
    Ok(format!(
        "{GITHUB_API}/repos/{}/releases/latest",
        repository_slug(repository)?
    ))
}

pub fn commits_url(repository: &str) -> Result<String> {
    Ok(format!(
        "{GITHUB_API}/repos/{}/commits?per_page=1",
        repository_slug(repository)?
    ))
}

pub fn contents_url(repository: &str, path: &str) -> Result<String> {
    Ok(format!(
        "{GITHUB_API}/repos/{}/contents/{}",
        repository_slug(repository)?,
        path.trim_matches('/')
    ))
}

/// Só o que o repositório do usuário publica interessa.
pub fn repository_link(repository: &str) -> String {
    repository_slug(repository)
        .map(|slug| format!("https://github.com/{slug}"))
        .unwrap_or_default()
}

/// Lê a resposta JSON da API de publicações e escolhe o pacote do Windows.
pub fn parse_release(json: &str) -> Result<Release> {
    let value: Value = serde_json::from_str(json).context("resposta do GitHub inválida")?;
    if let Some(message) = value.get("message").and_then(Value::as_str) {
        bail!("GitHub respondeu: {message}");
    }
    let tag = value
        .get("tag_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if tag.is_empty() {
        bail!("a publicação não informa tag_name");
    }
    let text = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let assets = value
        .get("assets")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let asset_name = |asset: &Value| {
        asset
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let is_windows_zip = |asset: &Value| {
        let name = asset_name(asset).to_ascii_lowercase();
        name.ends_with(".zip") && (name.contains("win") || name.contains("windows"))
    };
    let chosen = assets
        .iter()
        .find(|asset| is_windows_zip(asset))
        .or_else(|| {
            assets
                .iter()
                .find(|asset| asset_name(asset).to_ascii_lowercase().ends_with(".zip"))
        });
    let (asset_name, asset_url) = match chosen {
        Some(asset) => (
            asset_name(asset),
            asset
                .get("browser_download_url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        ),
        None => (String::new(), String::new()),
    };
    Ok(Release {
        tag,
        name: text("name"),
        published: text("published_at"),
        asset_name,
        asset_url,
    })
}

/// Lê a lista de commits da API (o parâmetro `per_page=1` traz um só).
pub fn parse_commit(json: &str) -> Result<Commit> {
    let value: Value = serde_json::from_str(json).context("resposta de commits inválida")?;
    if let Some(message) = value.get("message").and_then(Value::as_str) {
        bail!("GitHub respondeu: {message}");
    }
    let entry = if value.is_array() {
        value
            .as_array()
            .and_then(|items| items.first())
            .cloned()
    } else {
        Some(value.clone())
    }
    .context("o repositório não tem commits")?;
    let sha = entry
        .get("sha")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if sha.is_empty() {
        bail!("o commit não informa sha");
    }
    let commit = entry.get("commit").cloned().unwrap_or(Value::Null);
    let date = commit
        .get("committer")
        .and_then(|committer| committer.get("date"))
        .and_then(Value::as_str)
        .or_else(|| {
            commit
                .get("author")
                .and_then(|author| author.get("date"))
                .and_then(Value::as_str)
        })
        .unwrap_or_default()
        .to_string();
    let message = commit
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok(Commit {
        sha,
        date,
        message,
    })
}

/// Lê a pasta `builds/` do repositório (só `.exe` e `.zip`).
pub fn parse_builds(json: &str) -> Result<Vec<RemoteBuild>> {
    let value: Value = serde_json::from_str(json).context("lista de builds inválida")?;
    if let Some(message) = value.get("message").and_then(Value::as_str) {
        bail!("GitHub respondeu: {message}");
    }
    let items = value
        .as_array()
        .context("a pasta builds não devolveu uma lista")?;
    let mut builds = Vec::new();
    for item in items {
        if item.get("type").and_then(Value::as_str) != Some("file") {
            continue;
        }
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let lowered = name.to_ascii_lowercase();
        if !(lowered.ends_with(".exe") || lowered.ends_with(".zip")) {
            continue;
        }
        let url = item
            .get("download_url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if url.is_empty() {
            continue;
        }
        builds.push(RemoteBuild {
            name,
            size: item.get("size").and_then(Value::as_u64).unwrap_or(0),
            url,
        });
    }
    // Versão mais alta primeiro; sem versão no nome, mantém a ordem da API.
    builds.sort_by(|left, right| match (left.version(), right.version()) {
        (Some(a), Some(b)) => b.cmp(&a),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    Ok(builds)
}

/// A publicação é mais nova que a versão instalada?
pub fn is_newer(latest: &str, installed: &str) -> bool {
    match (Version::parse(latest), Version::parse(installed)) {
        (Some(latest), Some(installed)) => {
            latest.cmp(&installed) == std::cmp::Ordering::Greater
        }
        // Sem versão instalada não há como afirmar que a publicação é antiga.
        (Some(_), None) => true,
        _ => false,
    }
}

/// Separa esquema, host, porta e caminho de uma URL HTTP(S).
pub fn split_url(url: &str) -> Result<(bool, String, u16, String)> {
    let (scheme, rest) = url
        .split_once("://")
        .with_context(|| format!("URL sem esquema: {url}"))?;
    let secure = match scheme.to_ascii_lowercase().as_str() {
        "https" => true,
        "http" => false,
        other => bail!("esquema não suportado: {other}"),
    };
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, format!("/{path}")),
        None => (rest, "/".to_string()),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if port.parse::<u16>().is_ok() => {
            (host.to_string(), port.parse::<u16>().unwrap_or(443))
        }
        _ => (authority.to_string(), if secure { 443 } else { 80 }),
    };
    if host.is_empty() {
        bail!("URL sem host: {url}");
    }
    Ok((secure, host, port, path))
}

/// Nome de pasta/arquivo seguro a partir da tag ou do nome publicado.
pub fn sanitize(tag: &str) -> String {
    let cleaned: String = tag
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '.' || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('.').to_string();
    if trimmed.is_empty() {
        "build".to_string()
    } else {
        trimmed
    }
}

/// Última publicação do repositório; `None` quando ele não publica releases.
pub fn latest_release(repository: &str) -> Result<Option<Release>> {
    let (status, body) = http_get_with(&releases_url(repository)?)?;
    match status {
        200 => {
            let text = String::from_utf8(body).context("resposta do GitHub não é UTF-8")?;
            parse_release(&text).map(Some)
        }
        404 => Ok(None),
        other => bail!("o repositório respondeu HTTP {other}"),
    }
}

/// Commit mais recente do repositório padrão.
pub fn latest_commit(repository: &str) -> Result<Commit> {
    let (status, body) = http_get_with(&commits_url(repository)?)?;
    if status != 200 {
        bail!("o repositório respondeu HTTP {status}");
    }
    let text = String::from_utf8(body).context("resposta do GitHub não é UTF-8")?;
    parse_commit(&text)
}

/// Executáveis publicados na pasta `builds/` do repositório.
pub fn remote_builds(repository: &str) -> Result<Vec<RemoteBuild>> {
    let (status, body) = http_get_with(&contents_url(repository, "builds")?)?;
    match status {
        200 => {
            let text = String::from_utf8(body).context("resposta do GitHub não é UTF-8")?;
            parse_builds(&text)
        }
        404 => Ok(Vec::new()),
        other => bail!("o repositório respondeu HTTP {other}"),
    }
}

/// Baixa um executável publicado para a pasta de builds do launcher.
pub fn download_build(build: &RemoteBuild, builds_dir: &Path) -> Result<PathBuf> {
    let bytes = http_get(&build.url)?;
    let target = builds_dir.join(sanitize(&build.name));
    crate::storage::atomic_write(&target, &bytes)?;
    if target
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.eq_ignore_ascii_case("zip"))
        .unwrap_or(false)
    {
        // A pasta de extração não pode terminar em `.zip`: o Expand-Archive trata
        // esse destino como o próprio arquivo e não extrai nada.
        let stem = Path::new(&build.name)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or(&build.name);
        let folder = builds_dir.join(format!("pacote-{}", sanitize(stem)));
        fs::create_dir_all(&folder)
            .with_context(|| format!("não foi possível criar {}", folder.display()))?;
        expand_archive(&target, &folder)?;
        let found = find_executable(&folder)
            .context("o pacote baixado não contém um executável do emulador")?;
        let name = found
            .file_name()
            .map(PathBuf::from)
            .context("executável sem nome")?;
        let installed = builds_dir.join(name);
        if found != installed {
            fs::copy(&found, &installed).with_context(|| {
                format!(
                    "não foi possível copiar {} para {}",
                    found.display(),
                    installed.display()
                )
            })?;
        }
        return Ok(installed);
    }
    Ok(target)
}

/// Baixa o pacote de uma release publicada e o instala na pasta de builds.
#[cfg(windows)]
pub fn download_release(release: &Release, builds_dir: &Path) -> Result<PathBuf> {
    if release.asset_url.is_empty() || release.asset_name.is_empty() {
        bail!(
            "a publicação {} não traz um pacote para Windows",
            release.tag
        );
    }
    download_build(
        &RemoteBuild {
            name: release.asset_name.clone(),
            size: 0,
            url: release.asset_url.clone(),
        },
        builds_dir,
    )
}

#[cfg(not(windows))]
pub fn download_release(_release: &Release, _builds_dir: &Path) -> Result<PathBuf> {
    bail!("o download de builds exige Windows")
}

#[cfg(windows)]
fn expand_archive(archive: &Path, destination: &Path) -> Result<()> {
    fn quote(path: &Path) -> String {
        path.display().to_string().replace('\'', "''")
    }
    let script = format!(
        "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
        quote(archive),
        quote(destination)
    );
    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .status()
        .context("não foi possível chamar o PowerShell para extrair o pacote")?;
    if !status.success() {
        bail!("o PowerShell não conseguiu extrair {}", archive.display());
    }
    Ok(())
}

fn find_executable(dir: &Path) -> Option<PathBuf> {
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, found);
            } else if path
                .extension()
                .and_then(|extension| extension.to_str())
                .map(|extension| extension.eq_ignore_ascii_case("exe"))
                .unwrap_or(false)
            {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(dir, &mut found);
    found
        .iter()
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.to_ascii_lowercase().contains("shadps4"))
                .unwrap_or(false)
        })
        .cloned()
        .or_else(|| {
            found.into_iter().max_by_key(|path| {
                fs::metadata(path).map(|metadata| metadata.len()).unwrap_or(0)
            })
        })
}

/// GET por WinHTTP, seguindo redirecionamentos; devolve o status HTTP.
#[cfg(windows)]
pub fn http_get_with(url: &str) -> Result<(u16, Vec<u8>)> {
    use std::ffi::c_void;

    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Networking::WinHttp::{
        WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders,
        WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetOption,
        WinHttpSetTimeouts, WINHTTP_ACCESS_TYPE_DEFAULT_PROXY, WINHTTP_FLAG_SECURE,
        WINHTTP_OPTION_REDIRECT_POLICY, WINHTTP_OPTION_REDIRECT_POLICY_ALWAYS,
        WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
    };

    struct Handle(*mut c_void);
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { WinHttpCloseHandle(self.0) };
            }
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn error(context: &str) -> anyhow::Error {
        let code = unsafe { GetLastError() };
        anyhow::anyhow!("{context} (erro {code})")
    }

    let (secure, host, port, path) = split_url(url)?;
    let agent = wide("gow3-launcher (builds do repositorio)");
    let session = unsafe {
        WinHttpOpen(
            agent.as_ptr(),
            WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
            std::ptr::null(),
            std::ptr::null(),
            0,
        )
    };
    if session.is_null() {
        return Err(error("WinHttpOpen falhou"));
    }
    let session = Handle(session);
    unsafe { WinHttpSetTimeouts(session.0, 6_000, 6_000, 12_000, 60_000) };

    let host_wide = wide(&host);
    let connection = unsafe { WinHttpConnect(session.0, host_wide.as_ptr(), port, 0) };
    if connection.is_null() {
        return Err(error("WinHttpConnect falhou"));
    }
    let connection = Handle(connection);

    let verb = wide("GET");
    let object = wide(&path);
    let flags = if secure { WINHTTP_FLAG_SECURE } else { 0 };
    let request = unsafe {
        WinHttpOpenRequest(
            connection.0,
            verb.as_ptr(),
            object.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            flags,
        )
    };
    if request.is_null() {
        return Err(error("WinHttpOpenRequest falhou"));
    }
    let request = Handle(request);

    let policy: u32 = WINHTTP_OPTION_REDIRECT_POLICY_ALWAYS;
    unsafe {
        WinHttpSetOption(
            request.0,
            WINHTTP_OPTION_REDIRECT_POLICY,
            &policy as *const u32 as *const c_void,
            std::mem::size_of::<u32>() as u32,
        )
    };

    if unsafe { WinHttpSendRequest(request.0, std::ptr::null(), 0, std::ptr::null(), 0, 0, 0) }
        == 0
    {
        return Err(error("WinHttpSendRequest falhou"));
    }
    if unsafe { WinHttpReceiveResponse(request.0, std::ptr::null_mut()) } == 0 {
        return Err(error("WinHttpReceiveResponse falhou"));
    }

    let mut status: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let queried = unsafe {
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            std::ptr::null(),
            &mut status as *mut u32 as *mut c_void,
            &mut size,
            std::ptr::null_mut(),
        )
    };
    if queried == 0 {
        return Err(error("WinHttpQueryHeaders falhou"));
    }

    let mut body = Vec::new();
    let mut buffer = vec![0u8; 32 * 1024];
    loop {
        let mut read = 0u32;
        let ok = unsafe {
            WinHttpReadData(
                request.0,
                buffer.as_mut_ptr() as *mut c_void,
                buffer.len() as u32,
                &mut read,
            )
        };
        if ok == 0 {
            return Err(error("WinHttpReadData falhou"));
        }
        if read == 0 {
            break;
        }
        body.extend_from_slice(&buffer[..read as usize]);
        if body.len() > 512 * 1024 * 1024 {
            bail!("resposta grande demais para um pacote de build");
        }
    }
    Ok((status as u16, body))
}

#[cfg(not(windows))]
pub fn http_get_with(_url: &str) -> Result<(u16, Vec<u8>)> {
    bail!("consultas ao repositório exigem Windows")
}

/// GET exigindo sucesso.
pub fn http_get(url: &str) -> Result<Vec<u8>> {
    let (status, body) = http_get_with(url)?;
    if status != 200 {
        bail!("o servidor respondeu HTTP {status} para {url}");
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    const RELEASE: &str = r#"{
        "tag_name": "v.0.21.0",
        "name": "shadPS4 v.0.21.0",
        "published_at": "2026-11-01T10:00:00Z",
        "assets": [
            {"name": "shadPS4-win64-qt-v.0.21.0.zip", "browser_download_url": "https://example.invalid/win64.zip"},
            {"name": "shadPS4-linux-qt-0.21.0.AppImage", "browser_download_url": "https://example.invalid/linux.AppImage"}
        ]
    }"#;

    const COMMIT: &str = r#"[{
        "sha": "a019a27610a5a36ecf159e9ee06fd6c0a3a8324e",
        "commit": {
            "message": "README: VRR frame pacing\n\nCorpo do commit.",
            "committer": {"date": "2026-10-04T01:33:01Z"}
        }
    }]"#;

    const BUILDS: &str = r#"[
        {"name": "leituras.txt", "type": "file", "size": 10, "download_url": "https://example.invalid/leituras.txt"},
        {"name": "shadgow3-0.0.1.exe", "type": "file", "size": 64781824, "download_url": "https://example.invalid/a.exe"},
        {"name": "shadgow3-0.0.2.exe", "type": "file", "size": 64800000, "download_url": "https://example.invalid/b.exe"},
        {"name": "builds", "type": "dir", "size": 0, "download_url": null}
    ]"#;

    #[test]
    fn the_repository_comes_from_the_configured_slug_only() {
        assert_eq!(repository_slug("kazwtto/shadPS4").unwrap(), "kazwtto/shadPS4");
        assert_eq!(
            repository_slug("https://github.com/kazwtto/shadPS4").unwrap(),
            "kazwtto/shadPS4"
        );
        assert_eq!(repository_slug("kazwtto/shadPS4/").unwrap(), "kazwtto/shadPS4");
        assert!(repository_slug("shadps4-emu/shadPS4").unwrap() != "kazwtto/shadPS4");
        assert!(repository_slug("só-um-nome").is_err());
        assert!(repository_slug("").is_err());
        assert!(repository_slug("a b/c").is_err());
        assert_eq!(
            releases_url("kazwtto/shadPS4").unwrap(),
            "https://api.github.com/repos/kazwtto/shadPS4/releases/latest"
        );
        assert!(commits_url("kazwtto/shadPS4").unwrap().contains("/repos/kazwtto/shadPS4/commits"));
        assert!(contents_url("kazwtto/shadPS4", "builds")
            .unwrap()
            .ends_with("/contents/builds"));
        assert_eq!(repository_link("kazwtto/shadPS4"), "https://github.com/kazwtto/shadPS4");
    }

    #[test]
    fn saved_data_only_applies_to_the_repository_it_came_from() {
        assert!(same_repository("kazwtto/shadPS4", "kazwtto/shadPS4"));
        assert!(same_repository(" Kazwtto/SHADPS4 ", "kazwtto/shadPS4"));
        assert!(!same_repository("shadps4-emu/shadPS4", "kazwtto/shadPS4"));
        assert!(!same_repository("", "kazwtto/shadPS4"), "cache antigo sem repositório é descartado");
    }

    #[test]
    fn release_parsing_picks_the_windows_package() {
        let release = parse_release(RELEASE).unwrap();
        assert_eq!(release.tag, "v.0.21.0");
        assert_eq!(release.version_label(), "0.21.0");
        assert_eq!(release.asset_name, "shadPS4-win64-qt-v.0.21.0.zip");
        assert_eq!(release.published, "2026-11-01T10:00:00Z");
    }

    #[test]
    fn release_parsing_reports_api_errors_and_missing_tags() {
        assert!(parse_release(r#"{"message":"API rate limit exceeded"}"#)
            .unwrap_err()
            .to_string()
            .contains("rate limit"));
        assert!(parse_release(r#"{"name":"sem tag"}"#).is_err());
        assert!(parse_release("não é json").is_err());
    }

    #[test]
    fn the_latest_commit_is_read_with_its_date_and_first_line() {
        let commit = parse_commit(COMMIT).unwrap();
        assert_eq!(commit.sha, "a019a27610a5a36ecf159e9ee06fd6c0a3a8324e");
        assert_eq!(commit.date, "2026-10-04T01:33:01Z");
        assert_eq!(commit.short_message(), "README: VRR frame pacing");
        assert!(parse_commit("[]").is_err());
        assert!(parse_commit(r#"{"message":"Not Found"}"#).is_err());
    }

    #[test]
    fn builds_published_in_the_repository_are_listed_newest_first() {
        let builds = parse_builds(BUILDS).unwrap();
        assert_eq!(builds.len(), 2, "só .exe/.zip entram");
        assert_eq!(builds[0].name, "shadgow3-0.0.2.exe");
        assert_eq!(builds[1].name, "shadgow3-0.0.1.exe");
        assert_eq!(builds[0].version().unwrap().label(), "0.0.2");
        assert!(parse_builds(r#"{"message":"Not Found"}"#).is_err());
    }

    #[test]
    fn a_newer_release_is_detected_numerically() {
        assert!(is_newer("v.0.21.0", "0.0.1"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(!is_newer("0.0.1", "0.0.1"));
        assert!(is_newer("0.21.0", ""), "sem versão instalada sempre avisa");
        assert!(!is_newer("", "0.0.1"), "publicação ilegível não vira aviso");
    }

    #[test]
    fn urls_split_into_connection_parts() {
        let (secure, host, port, path) =
            split_url("https://api.github.com/repos/a/b/releases/latest").unwrap();
        assert!(secure);
        assert_eq!(host, "api.github.com");
        assert_eq!(port, 443);
        assert_eq!(path, "/repos/a/b/releases/latest");
        let (secure, host, port, path) = split_url("http://exemplo:8080/pacote.zip").unwrap();
        assert!(!secure);
        assert_eq!((host.as_str(), port, path.as_str()), ("exemplo", 8080, "/pacote.zip"));
        assert!(split_url("ftp://exemplo/x").is_err());
        assert!(split_url("sem-esquema").is_err());
    }

    #[test]
    fn tags_become_safe_folder_names() {
        assert_eq!(sanitize("v.0.21.0"), "v.0.21.0");
        assert_eq!(sanitize("v 0.21/beta"), "v_0.21_beta");
        assert_eq!(sanitize("..."), "build");
    }

    /// O pacote publicado é um zip: extrair e achar o `.exe` é o caminho que o
    /// botão de download usa, então o teste usa um zip local de verdade.
    #[cfg(windows)]
    #[test]
    fn a_local_archive_is_expanded_and_its_executable_found() {
        let dir = env::temp_dir().join(format!("gow3-zip-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(dir.join("pacote")).unwrap();
        fs::write(dir.join("pacote").join("shadPS4.exe"), b"emu").unwrap();
        let archive = dir.join("build.zip");
        let status = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!(
                    "Compress-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
                    dir.join("pacote").join("shadPS4.exe").display(),
                    archive.display()
                ),
            ])
            .status()
            .unwrap();
        assert!(status.success(), "o zip de teste não foi criado");
        let target = dir.join("saida");
        fs::create_dir_all(&target).unwrap();
        expand_archive(&archive, &target).unwrap();
        let found = find_executable(&target).unwrap();
        assert_eq!(found.file_name().unwrap(), "shadPS4.exe");
        assert_eq!(fs::read(&found).unwrap(), b"emu");
        fs::remove_dir_all(dir).unwrap();
    }
}
