//! Builds do shadPS4 na pasta de builds do próprio launcher (`data/builds`).
//!
//! O launcher não pede mais um `.exe` solto: ele cria a pasta, lista as
//! versões encontradas, deixa o usuário escolher uma e sabe comparar a versão
//! instalada com a publicada. Nada aqui altera arquivos: só lê e ordena.

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};

/// Versão numérica lida do nome do arquivo (`shadgow3-0.0.1.exe`, `v.0.21.0`).
#[derive(Debug, Clone, Copy)]
pub struct Version {
    parts: [u64; 4],
    len: u8,
}

impl Version {
    pub fn parse(text: &str) -> Option<Self> {
        let trimmed = text.trim();
        let cleaned = match trimmed.as_bytes().first().copied() {
            Some(b'v' | b'V' | b'a' | b'A' | b'b' | b'B')
                if trimmed
                    .as_bytes()
                    .get(1)
                    .map(|next| next.is_ascii_digit() || matches!(next, b'.' | b'-' | b'_'))
                    .unwrap_or(false) =>
            {
                &trimmed[1..]
            }
            _ => trimmed,
        }
        .trim_start_matches(|c| c == '.' || c == '-' || c == '_');
        if cleaned.is_empty() {
            return None;
        }
        let mut parts = [0u64; 4];
        let mut len = 0u8;
        for piece in cleaned.split(|c| c == '.' || c == '-' || c == '_') {
            if len as usize >= parts.len() {
                break;
            }
            let digits: String = piece.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                return None;
            }
            parts[len as usize] = digits.parse().ok()?;
            len += 1;
        }
        (len > 0).then_some(Self { parts, len })
    }

    /// `0.21.0` para exibição, sem prefixos.
    pub fn label(&self) -> String {
        self.parts[..self.len as usize]
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(".")
    }
}

impl PartialEq for Version {
    /// `v.0.21.0` e `0.21` são a mesma versão: só os números contam.
    fn eq(&self, other: &Self) -> bool {
        self.parts == other.parts
    }
}

impl Eq for Version {}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.parts.cmp(&other.parts)
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Uma build escolhível na pasta do repositório.
#[derive(Debug, Clone, PartialEq)]
pub struct BuildEntry {
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub version: Option<Version>,
}

impl BuildEntry {
    /// Versão para exibição; sem número no nome, mostra o tamanho em MB.
    pub fn version_label(&self) -> String {
        match &self.version {
            Some(version) => format!("v{}", version.label()),
            None => String::new(),
        }
    }

    pub fn size_label(&self) -> String {
        format!("{:.1} MB", self.size as f64 / (1024.0 * 1024.0))
    }
}

/// Versão embutida no nome do arquivo, quando existe.
pub fn version_from_name(name: &str) -> Option<Version> {
    let stem = name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(name);
    stem.split(|c: char| c == '-' || c == '_' || c.is_whitespace())
        .filter_map(Version::parse)
        .next_back()
}

fn is_executable(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.eq_ignore_ascii_case("exe"))
        .unwrap_or(false)
}

/// Garante a pasta de builds do próprio launcher.
pub fn ensure_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)
        .with_context(|| format!("não foi possível criar {}", dir.display()))
}

/// Executáveis da pasta, do mais novo ao mais antigo.
pub fn list(dir: &Path) -> Result<Vec<BuildEntry>> {
    let mut entries = Vec::new();
    let reader = fs::read_dir(dir)
        .with_context(|| format!("não foi possível ler {}", dir.display()))?;
    for entry in reader.flatten() {
        let path = entry.path();
        if !path.is_file() || !is_executable(&path) {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        let version = version_from_name(&name);
        entries.push(BuildEntry {
            name,
            path,
            size: metadata.len(),
            modified: metadata.modified().ok(),
            version,
        });
    }
    entries.sort_by(|left, right| match (left.version, right.version) {
        (Some(a), Some(b)) => b.cmp(&a),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => right.modified.cmp(&left.modified),
    });
    Ok(entries)
}

/// Versão mais nova encontrada na pasta, se alguma tiver número no nome.
pub fn highest_version(entries: &[BuildEntry]) -> Option<Version> {
    entries.iter().filter_map(|entry| entry.version).max()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn temp_dir(label: &str) -> PathBuf {
        let path = env::temp_dir().join(format!(
            "gow3-builds-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn versions_come_from_the_file_name() {
        assert_eq!(version_from_name("a0.0.1.exe").unwrap().label(), "0.0.1");
        assert_eq!(version_from_name("shadgow3-0.0.1.exe").unwrap().label(), "0.0.1");
        assert_eq!(version_from_name("shadPS4-win64-qt-v.0.21.0.zip").unwrap().label(), "0.21.0");
        assert_eq!(version_from_name("shadps4_1.2.exe").unwrap().label(), "1.2");
        assert!(version_from_name("shadps4_astra_E11_lazy.exe").is_none());
        assert!(Version::parse("") .is_none());
    }

    #[test]
    fn newer_versions_compare_numerically_not_alphabetically() {
        let ten = Version::parse("0.10.0").unwrap();
        let nine = Version::parse("0.9.9").unwrap();
        assert!(ten > nine);
        assert_eq!(ten.label(), "0.10.0");
        assert_eq!(Version::parse("v.0.21.0"), Version::parse("0.21"));
    }

    #[test]
    fn listing_keeps_only_executables_newest_first() {
        let dir = temp_dir("list");
        fs::write(dir.join("shadgow3-0.0.1.exe"), b"a").unwrap();
        fs::write(dir.join("shadgow3-0.0.2.exe"), b"bb").unwrap();
        fs::write(dir.join("notas.txt"), b"c").unwrap();
        fs::write(dir.join("sem_versao.exe"), b"dddd").unwrap();
        let entries = list(&dir).unwrap();
        assert_eq!(entries.len(), 3, "arquivos .txt são ignorados");
        assert_eq!(entries[0].name, "shadgow3-0.0.2.exe");
        assert_eq!(entries[1].name, "shadgow3-0.0.1.exe");
        assert_eq!(entries[2].name, "sem_versao.exe");
        assert_eq!(highest_version(&entries).unwrap().label(), "0.0.2");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_folder_is_an_error_and_the_launcher_creates_its_own() {
        let dir = temp_dir("missing");
        assert!(list(&dir.join("nao_existe")).is_err());
        let own = dir.join("data").join("builds");
        assert!(!own.exists());
        ensure_dir(&own).unwrap();
        assert!(own.is_dir());
        assert!(list(&own).unwrap().is_empty());
        fs::remove_dir_all(dir).unwrap();
    }
}
