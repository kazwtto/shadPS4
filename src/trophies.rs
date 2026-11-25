//! Leitura somente leitura dos troféus do jogo e do ambiente privado.
//!
//! Duas fontes, nesta ordem:
//!
//! 1. O que o núcleo já extraiu em `ambiente/user/trophy/<NPCommID>/{Xml,Icons}`,
//!    com o progresso em `ambiente/user/home/<usuário>/trophy/<NPCommID>.xml`.
//! 2. O pacote original do jogo (`sce_sys/npbind.dat` + `sce_sys/trophy/*.trp`),
//!    lido aqui mesmo — assim a página abre preenchida sem esperar uma sessão do
//!    núcleo. As definições são descriptografadas em memória com a chave de
//!    troféus de `ambiente/user/keys.json`, e apenas os ícones são guardados em
//!    cache dentro dos dados do launcher.
//!
//! Nada aqui escreve no ambiente do núcleo, altera progresso ou cria um segundo
//! formato de armazenamento. Tudo é tratado como dado não confiável: tamanho
//! limitado, campos ausentes tolerados e erros recuperáveis em vez de pânico.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use anyhow::{bail, Context, Result};

/// Usuário do ambiente privado, o mesmo usado por `PrivateEnvironment::save_dir`.
const USER_ID: &str = "1000";
/// Um TROPCONF real tem poucos KiB; acima disso o arquivo não é aceito.
const MAX_XML_BYTES: u64 = 4 * 1024 * 1024;
/// O pacote de troféus chega a alguns MiB.
const MAX_PACKAGE_BYTES: u64 = 64 * 1024 * 1024;
/// Limite de sanidade para uma coleção; o maior conjunto do PS4 tem 128 troféus.
const MAX_TROPHIES: usize = 512;
/// Textos vindos do XML nunca ocupam mais que isto na interface.
const MAX_TEXT_CHARS: usize = 400;

const TRP_MAGIC: u32 = 0xDCA2_4D00;
const TRP_HEADER_SIZE: usize = 96;
const ENTRY_FLAG_PNG: u32 = 0;
const ENTRY_FLAG_ENCRYPTED_XML: u32 = 3;
const NPBIND_MAGIC: u32 = 0xD294_A018;
const NPBIND_HEADER_SIZE: usize = 128;
const NPBIND_BODY_PADDING: usize = 0x98;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grade {
    Platinum,
    Gold,
    Silver,
    Bronze,
    Unknown,
}

impl Grade {
    /// O núcleo usa a primeira letra de `ttype` (p/g/s/b).
    fn from_code(code: &str) -> Self {
        match code.chars().next().map(|c| c.to_ascii_lowercase()) {
            Some('p') => Grade::Platinum,
            Some('g') => Grade::Gold,
            Some('s') => Grade::Silver,
            Some('b') => Grade::Bronze,
            _ => Grade::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Grade::Platinum => "Platina",
            Grade::Gold => "Ouro",
            Grade::Silver => "Prata",
            Grade::Bronze => "Bronze",
            Grade::Unknown => "Tipo desconhecido",
        }
    }

    /// Ordem usada pela interface: 0 platina, 1 ouro, 2 prata, 3 bronze, 4 outro.
    pub fn kind(self) -> i32 {
        match self {
            Grade::Platinum => 0,
            Grade::Gold => 1,
            Grade::Silver => 2,
            Grade::Bronze => 3,
            Grade::Unknown => 4,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Trophy {
    pub id: u32,
    pub name: String,
    pub detail: String,
    pub grade: Grade,
    pub hidden: bool,
    pub unlocked: bool,
    /// Caminho do ícone em disco; pode não existir.
    pub icon: PathBuf,
}

impl Trophy {
    pub fn has_icon(&self) -> bool {
        self.icon.is_file()
    }
}

#[derive(Debug, Clone)]
pub struct Collection {
    pub np_comm_id: String,
    /// `<title-name>` do TROPCONF, quando presente.
    pub title: String,
    pub trophies: Vec<Trophy>,
    /// Nome do arquivo de definição usado, para diagnóstico.
    pub xml_name: String,
    /// `None` quando o progresso do usuário ainda não foi gravado.
    pub progress_path: Option<PathBuf>,
    /// Preenchido quando o XML de progresso existe mas não pôde ser lido.
    pub progress_error: Option<String>,
    /// Verdadeiro quando as definições vieram do pacote do jogo, não extraídas.
    pub from_package: bool,
}

impl Collection {
    pub fn total(&self) -> usize {
        self.trophies.len()
    }

    pub fn unlocked(&self) -> usize {
        self.trophies.iter().filter(|trophy| trophy.unlocked).count()
    }
}

/// Resultado de uma leitura, com o motivo quando nada foi carregado.
#[derive(Debug, Clone)]
pub enum State {
    Loaded(Collection),
    /// O pacote foi encontrado, mas falta a chave que descriptografa as definições.
    NoKey,
    /// Nada encontrado e nenhum pacote para ler.
    Empty { message: String },
    /// Arquivos presentes, porém ilegíveis.
    Invalid { message: String },
}

/// Origens usadas pela leitura.
pub struct Sources<'a> {
    pub user_dir: &'a Path,
    /// Pasta de cache do launcher para os ícones lidos do pacote.
    pub cache_dir: &'a Path,
    /// Pasta do jogo (a mesma de Configurações), quando configurada.
    pub game_base: Option<PathBuf>,
    /// Código do idioma do console (`General.console_language`).
    pub language: i64,
}

/// Assinatura barata dos arquivos que alimentam a página: datas e tamanhos do
/// ambiente, do progresso, da chave e do pacote, além do idioma escolhido.
/// Serve para não reler nem descriptografar o pacote quando nada mudou.
pub fn signature(sources: &Sources<'_>) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    sources.language.hash(&mut hasher);

    let metadata_root = sources.user_dir.join("trophy");
    let progress_root = sources.user_dir.join("home").join(USER_ID).join("trophy");
    stamp_path(&metadata_root, &mut hasher);
    stamp_children(&metadata_root, &mut hasher);
    stamp_path(&progress_root, &mut hasher);
    stamp_children(&progress_root, &mut hasher);
    stamp_path(&sources.user_dir.join("keys.json"), &mut hasher);

    if let Some(base) = sources.game_base.as_ref() {
        let sce_sys = base.join("sce_sys");
        stamp_path(&sce_sys.join("npbind.dat"), &mut hasher);
        let trophy_dir = sce_sys.join("trophy");
        stamp_path(&trophy_dir, &mut hasher);
        stamp_children(&trophy_dir, &mut hasher);
    }

    hasher.finish()
}

fn stamp_path(path: &Path, hasher: &mut DefaultHasher) {
    use std::hash::Hash;

    match fs::metadata(path) {
        Ok(meta) => {
            let modified = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|span| span.as_secs())
                .unwrap_or(0);
            modified.hash(hasher);
            meta.len().hash(hasher);
        }
        Err(_) => 0u8.hash(hasher),
    }
}

/// Arquivos dentro da pasta, em ordem estável: criar ou apagar um progresso
/// muda a assinatura mesmo quando a data da pasta não muda.
fn stamp_children(dir: &Path, hasher: &mut DefaultHasher) {
    use std::hash::Hash;

    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for name in names {
        name.hash(hasher);
        stamp_path(&dir.join(name), hasher);
    }
}

pub fn load(sources: &Sources<'_>) -> State {
    let metadata_root = sources.user_dir.join("trophy");
    let progress_root = sources.user_dir.join("home").join(USER_ID).join("trophy");

    if let Some(state) = load_from_environment(sources, &metadata_root, &progress_root) {
        return state;
    }
    load_from_package(sources, &progress_root)
}

/// Fonte 1: arquivos que o núcleo já extraiu. `None` significa "tente o pacote".
fn load_from_environment(
    sources: &Sources<'_>,
    metadata_root: &Path,
    progress_root: &Path,
) -> Option<State> {
    let candidates = candidates(metadata_root, progress_root);
    if candidates.is_empty() {
        return None;
    }
    let np_comm_id = candidates
        .iter()
        .find(|id| progress_root.join(format!("{id}.xml")).is_file())
        .cloned()
        .unwrap_or_else(|| candidates[0].clone());
    let dir = metadata_root.join(&np_comm_id);
    let xml_path = pick_definition(&dir.join("Xml"), sources.language)?;
    let text = match read_bounded(&xml_path) {
        Ok(text) => text,
        Err(error) => {
            return Some(State::Invalid {
                message: format!("{} não pôde ser lido: {error}", file_label(&xml_path)),
            })
        }
    };
    let parsed = match parse(&text) {
        Ok(parsed) => parsed,
        Err(error) => {
            return Some(State::Invalid {
                message: format!("{} não pôde ser interpretado: {error}", file_label(&xml_path)),
            })
        }
    };
    let icons = icon_index(&dir.join("Icons"));
    Some(State::Loaded(build_collection(
        parsed,
        np_comm_id,
        file_label(&xml_path),
        progress_root,
        &icons,
        false,
    )))
}

/// Fonte 2: o pacote original do jogo, lido sem depender de uma sessão.
fn load_from_package(sources: &Sources<'_>, progress_root: &Path) -> State {
    let Some(base) = sources.game_base.as_ref() else {
        return State::Empty {
            message: "Nenhum troféu disponível.".into(),
        };
    };
    let sce_sys = base.join("sce_sys");
    let trophy_dir = sce_sys.join("trophy");
    let packages = match select_packages(&trophy_dir) {
        Ok(packages) => packages,
        Err(message) => return State::Empty { message },
    };
    let (index, package) = &packages[0];

    let np_comm_ids = match read_np_comm_ids(&sce_sys.join("npbind.dat")) {
        Ok(ids) => ids,
        Err(error) => {
            return State::Empty {
                message: format!("npbind.dat ilegível: {error}"),
            }
        }
    };
    let Some(np_comm_id) = np_comm_ids.get(*index as usize).cloned() else {
        return State::Empty {
            message: "O pacote não tem NPCommID correspondente em npbind.dat.".into(),
        };
    };
    let Some(key) = trophy_key(sources.user_dir) else {
        return State::NoKey;
    };
    match read_package(package, &np_comm_id, &key, sources.language) {
        Ok((parsed, icons, xml_name)) => {
            let cached = cache_icons(sources.cache_dir, &np_comm_id, &icons);
            return State::Loaded(build_collection(
                parsed,
                np_comm_id,
                xml_name,
                progress_root,
                &cached,
                true,
            ));
        }
        Err(error) => State::Invalid {
            message: format!("{} não pôde ser lido: {error}", file_label(package)),
        },
    }
}

/// Uma coleção a partir das definições e do progresso do usuário.
fn build_collection(
    parsed: Parsed,
    np_comm_id: String,
    xml_name: String,
    progress_root: &Path,
    icons: &HashMap<String, PathBuf>,
    from_package: bool,
) -> Collection {
    let progress_path = progress_root.join(format!("{np_comm_id}.xml"));
    let mut progress_error = None;
    let mut progress: HashMap<u32, bool> = HashMap::new();
    let mut has_progress = false;
    if progress_path.is_file() {
        match read_bounded(&progress_path).and_then(|text| parse(&text)) {
            Ok(loaded) => {
                progress = loaded
                    .trophies
                    .into_iter()
                    .map(|trophy| (trophy.id, trophy.unlocked))
                    .collect();
                has_progress = true;
            }
            Err(error) => {
                progress_error = Some(format!(
                    "{} não pôde ser lido: {error}",
                    file_label(&progress_path)
                ));
            }
        }
    }

    let mut trophies: Vec<Trophy> = parsed
        .trophies
        .into_iter()
        .take(MAX_TROPHIES)
        .map(|raw| {
            let unlocked = progress.get(&raw.id).copied().unwrap_or(false);
            Trophy {
                icon: icons
                    .get(&format!("trop{:03}", raw.id))
                    .cloned()
                    .unwrap_or_else(|| progress_root.join(format!("TROP{:03}.PNG", raw.id))),
                id: raw.id,
                name: raw.name,
                detail: raw.detail,
                grade: raw.grade,
                hidden: raw.hidden,
                unlocked,
            }
        })
        .collect();
    trophies.sort_by_key(|trophy| trophy.id);

    Collection {
        np_comm_id,
        title: parsed.title,
        trophies,
        xml_name,
        progress_path: if has_progress { Some(progress_path) } else { None },
        progress_error,
        from_package,
    }
}

/// Conjuntos encontrados no ambiente: pastas com `Xml/` e arquivos de progresso.
fn candidates(metadata_root: &Path, progress_root: &Path) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    if let Ok(entries) = fs::read_dir(metadata_root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.join("Xml").is_dir() {
                if let Some(name) = path.file_name().and_then(|value| value.to_str()) {
                    found.push(name.to_string());
                }
            }
        }
    }
    if let Ok(entries) = fs::read_dir(progress_root) {
        for entry in entries.flatten() {
            let path = entry.path();
            let is_xml = path
                .extension()
                .and_then(|value| value.to_str())
                .map(|value| value.eq_ignore_ascii_case("xml"))
                .unwrap_or(false);
            if !is_xml {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|value| value.to_str()) {
                if !found.iter().any(|known| known == stem) {
                    found.push(stem.to_string());
                }
            }
        }
    }
    found.sort();
    found
}

/// Mesma ordem do núcleo: `TROP_XX.XML` do idioma, depois `TROP.XML`, depois
/// qualquer arquivo de troféus presente.
fn pick_definition(xml_dir: &Path, language: i64) -> Option<PathBuf> {
    let preferred = xml_dir.join(format!("TROP_{:02}.XML", language.clamp(0, 30)));
    if preferred.is_file() {
        return Some(preferred);
    }
    let master = xml_dir.join("TROP.XML");
    if master.is_file() {
        return Some(master);
    }
    let mut files: Vec<PathBuf> = fs::read_dir(xml_dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|value| value.to_str())
                    .map(|value| value.eq_ignore_ascii_case("xml"))
                    .unwrap_or(false)
        })
        .collect();
    files.sort_by(|left, right| left.file_name().cmp(&right.file_name()));
    files.into_iter().next()
}

/// Índice dos ícones por nome em minúsculas; ausência de ícone não é erro.
fn icon_index(dir: &Path) -> HashMap<String, PathBuf> {
    let mut index = HashMap::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(stem) = path.file_stem().and_then(|value| value.to_str()) {
                index.entry(stem.to_ascii_lowercase()).or_insert(path);
            }
        }
    }
    index
}

/// Lista os pacotes `trophyNN.trp` do jogo, do menor índice para o maior.
fn select_packages(trophy_dir: &Path) -> std::result::Result<Vec<(u32, PathBuf)>, String> {
    let mut found = Vec::new();
    if let Ok(entries) = fs::read_dir(trophy_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = match path.file_name().and_then(|value| value.to_str()) {
                Some(name) => name.to_ascii_lowercase(),
                None => continue,
            };
            if !name.ends_with(".trp") || !name.starts_with("trophy") {
                continue;
            }
            let stem = &name["trophy".len()..name.len() - 4];
            if let Ok(index) = stem.parse::<u32>() {
                found.push((index, path));
            }
        }
    }
    if found.is_empty() {
        return Err("O jogo não tem pacote de troféus em sce_sys/trophy.".into());
    }
    found.sort_by_key(|(index, _)| *index);
    Ok(found)
}

/// NPCommIDs na ordem do `npbind.dat`, que indexa os pacotes `trophyNN.trp`.
fn read_np_comm_ids(path: &Path) -> Result<Vec<String>> {
    let bytes = fs::read(path)?;
    if bytes.len() < NPBIND_HEADER_SIZE {
        bail!("npbind.dat truncado");
    }
    if be_u32(&bytes, 0) != NPBIND_MAGIC {
        bail!("npbind.dat com assinatura inválida");
    }
    let entries = be_u64(&bytes, 24) as usize;
    let mut offset = NPBIND_HEADER_SIZE;
    let mut ids = Vec::new();
    for _ in 0..entries.min(64) {
        // Quatro entradas TLV: npcommid, trophy, desconhecida, desconhecida.
        let mut np_comm_id = String::new();
        for position in 0..4 {
            let Some((kind, size)) = read_tlv_header(&bytes, &mut offset) else {
                return Ok(ids);
            };
            let end = offset + size as usize;
            if end > bytes.len() {
                return Ok(ids);
            }
            if position == 0 && kind == 0x0010 {
                np_comm_id = String::from_utf8_lossy(&bytes[offset..end])
                    .trim_end_matches('\0')
                    .to_string();
            }
            offset = end;
        }
        offset += NPBIND_BODY_PADDING;
        if !np_comm_id.is_empty() {
            ids.push(np_comm_id);
        }
    }
    Ok(ids)
}

fn read_tlv_header(bytes: &[u8], offset: &mut usize) -> Option<(u16, u16)> {
    if *offset + 4 > bytes.len() {
        return None;
    }
    let kind = ((bytes[*offset] as u16) << 8) | bytes[*offset + 1] as u16;
    let size = ((bytes[*offset + 2] as u16) << 8) | bytes[*offset + 3] as u16;
    *offset += 4;
    Some((kind, size))
}

fn be_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn be_u64(bytes: &[u8], offset: usize) -> u64 {
    let mut value = [0u8; 8];
    value.copy_from_slice(&bytes[offset..offset + 8]);
    u64::from_be_bytes(value)
}

/// Chave de troféus do usuário, em `keys.json` (`TrophyKeySet.ReleaseTrophyKey`).
fn trophy_key(user_dir: &Path) -> Option<[u8; 16]> {
    let text = fs::read_to_string(user_dir.join("keys.json")).ok()?;
    let document: serde_json::Value = serde_json::from_str(&text).ok()?;
    let value = document.pointer("/TrophyKeySet/ReleaseTrophyKey")?;
    let bytes = key_bytes(value)?;
    if bytes.len() != 16 {
        return None;
    }
    let mut key = [0u8; 16];
    key.copy_from_slice(&bytes);
    Some(key)
}

fn key_bytes(value: &serde_json::Value) -> Option<Vec<u8>> {
    if let Some(text) = value.as_str() {
        let trimmed: String = text.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        if trimmed.len() != 32 {
            return None;
        }
        let mut bytes = Vec::with_capacity(16);
        for chunk in trimmed.as_bytes().chunks(2) {
            let pair = std::str::from_utf8(chunk).ok()?;
            bytes.push(u8::from_str_radix(pair, 16).ok()?);
        }
        return Some(bytes);
    }
    value
        .as_array()?
        .iter()
        .map(|item| item.as_u64().map(|number| number as u8))
        .collect()
}

/// Lê o pacote: definições descriptografadas e ícones (PNG, em claro).
#[allow(clippy::type_complexity)]
fn read_package(
    path: &Path,
    np_comm_id: &str,
    key: &[u8; 16],
    language: i64,
) -> Result<(Parsed, HashMap<String, Vec<u8>>, String)> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_PACKAGE_BYTES {
        bail!("pacote maior que {} MiB", MAX_PACKAGE_BYTES / 1024 / 1024);
    }
    let bytes = fs::read(path)?;
    if bytes.len() < TRP_HEADER_SIZE || be_u32(&bytes, 0) != TRP_MAGIC {
        bail!("assinatura de troféus inválida");
    }
    let entries = be_u32(&bytes, 16) as usize;
    let entry_size = be_u32(&bytes, 20) as usize;
    if entry_size < 64 || entries == 0 || entries > 1024 {
        bail!("índice de troféus inválido");
    }

    let mut icons: HashMap<String, Vec<u8>> = HashMap::new();
    let mut definitions: HashMap<String, Parsed> = HashMap::new();
    let mut offset = TRP_HEADER_SIZE;
    for _ in 0..entries {
        if offset + 52 > bytes.len() {
            break;
        }
        let name = String::from_utf8_lossy(&bytes[offset..offset + 32])
            .trim_end_matches('\0')
            .to_string();
        let position = be_u64(&bytes, offset + 32) as usize;
        let length = be_u64(&bytes, offset + 40) as usize;
        let flag = be_u32(&bytes, offset + 48);
        offset += entry_size;
        let end = position.saturating_add(length);
        if length == 0 || end > bytes.len() {
            continue;
        }
        let body = &bytes[position..end];
        match flag {
            ENTRY_FLAG_PNG => {
                let stem = name
                    .rsplit_once('.')
                    .map(|(stem, _)| stem.to_string())
                    .unwrap_or_else(|| name.clone());
                icons.insert(stem.to_ascii_uppercase(), body.to_vec());
            }
            ENTRY_FLAG_ENCRYPTED_XML => {
                if body.len() <= 16 {
                    continue;
                }
                let mut iv = [0u8; 16];
                iv.copy_from_slice(&body[..16]);
                let plain = decrypt_efsm(key, np_comm_id, &iv, &body[16..]);
                let text = trim_to_xml(&plain);
                if let Ok(parsed) = parse(&text) {
                    definitions.insert(name.to_ascii_uppercase(), parsed);
                }
            }
            _ => {}
        }
    }

    let preferred = format!("TROP_{:02}.ESFM", language.clamp(0, 30));
    let chosen = [preferred.as_str(), "TROP.ESFM", "TROPCONF.ESFM"]
        .iter()
        .find(|name| definitions.contains_key(**name))
        .map(|name| name.to_string())
        .or_else(|| definitions.keys().min().cloned());
    let Some(chosen) = chosen else {
        bail!("nenhuma definição de troféu pôde ser descriptografada");
    };
    let xml_name = chosen.replace(".ESFM", ".XML");
    let parsed = definitions.remove(&chosen).context("definição ausente")?;
    Ok((parsed, icons, xml_name))
}

/// Deriva a chave secundária (NPCommID cifrado com a chave do usuário) e abre o EFSM.
fn decrypt_efsm(key: &[u8; 16], np_comm_id: &str, iv: &[u8; 16], ciphertext: &[u8]) -> Vec<u8> {
    let mut id = [0u8; 16];
    let raw = np_comm_id.as_bytes();
    let length = raw.len().min(16);
    id[..length].copy_from_slice(&raw[..length]);
    let derived = cbc_encrypt(key, &[0u8; 16], &id);
    let mut trp_key = [0u8; 16];
    trp_key.copy_from_slice(&derived[..16]);
    cbc_decrypt(&trp_key, iv, ciphertext)
}

fn cbc_encrypt(key: &[u8; 16], iv: &[u8; 16], data: &[u8]) -> Vec<u8> {
    let cipher = aes::Aes128::new(key.into());
    let mut previous = *iv;
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.chunks(16) {
        if chunk.len() < 16 {
            break;
        }
        let mut block = [0u8; 16];
        for index in 0..16 {
            block[index] = chunk[index] ^ previous[index];
        }
        cipher.encrypt_block((&mut block).into());
        out.extend_from_slice(&block);
        previous = block;
    }
    out
}

fn cbc_decrypt(key: &[u8; 16], iv: &[u8; 16], data: &[u8]) -> Vec<u8> {
    let cipher = aes::Aes128::new(key.into());
    let mut previous = *iv;
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.chunks(16) {
        if chunk.len() < 16 {
            break;
        }
        let mut block = [0u8; 16];
        block.copy_from_slice(chunk);
        let cipher_block = block;
        cipher.decrypt_block((&mut block).into());
        for index in 0..16 {
            block[index] ^= previous[index];
        }
        out.extend_from_slice(&block);
        previous = cipher_block;
    }
    out
}

/// O núcleo corta o preenchimento no último `>`; aqui o texto é decodificado como
/// está e o parser ignora o que vem depois do fechamento.
fn trim_to_xml(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    match text.rfind('>') {
        Some(end) => text[..=end].to_string(),
        None => text.to_string(),
    }
}

/// Grava os ícones do pacote no cache do launcher e devolve o índice por id.
fn cache_icons(
    cache_dir: &Path,
    np_comm_id: &str,
    icons: &HashMap<String, Vec<u8>>,
) -> HashMap<String, PathBuf> {
    let directory = cache_dir.join(np_comm_id);
    let mut index = HashMap::new();
    if icons.is_empty() {
        return index;
    }
    if fs::create_dir_all(&directory).is_err() {
        return index;
    }
    for (name, bytes) in icons {
        let path = directory.join(format!("{name}.PNG"));
        if !path.is_file() && fs::write(&path, bytes).is_err() {
            continue;
        }
        index.insert(name.to_ascii_lowercase(), path);
    }
    index
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .map(|value| value.to_string())
        .unwrap_or_else(|| path.display().to_string())
}

fn read_bounded(path: &Path) -> Result<String> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_XML_BYTES {
        bail!("arquivo maior que 4 MiB");
    }
    let bytes = fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[derive(Debug, Default)]
struct Parsed {
    title: String,
    trophies: Vec<RawTrophy>,
}

#[derive(Debug)]
struct RawTrophy {
    id: u32,
    grade: Grade,
    hidden: bool,
    unlocked: bool,
    name: String,
    detail: String,
}

/// Interpreta um TROPCONF. O formato é fixo e simples (`<trophy>` com atributos e
/// os filhos `<name>`/`<detail>`), então a leitura é feita por varredura tolerante
/// em vez de exigir uma dependência de XML.
fn parse(text: &str) -> Result<Parsed> {
    let lower = text.to_ascii_lowercase();
    if !lower.contains("<trophyconf") {
        bail!("falta o elemento <trophyconf>");
    }
    let title = element_text(text, &lower, "title-name").unwrap_or_default();
    let mut trophies = Vec::new();
    let mut cursor = 0usize;
    while let Some(offset) = lower[cursor..].find("<trophy") {
        let start = cursor + offset;
        // `<trophyconf>` também começa com `<trophy`; só um limite real abre troféu.
        let opens = matches!(
            lower[start + "<trophy".len()..].chars().next(),
            Some(value) if value == '>' || value == '/' || value.is_whitespace()
        );
        if !opens {
            cursor = start + "<trophy".len();
            continue;
        }
        let Some(tag_offset) = lower[start..].find('>') else {
            break;
        };
        let tag_end = start + tag_offset;
        let tag = &text[start..=tag_end];
        let (body, next_cursor) = if tag.trim_end().ends_with("/>") {
            ("", tag_end + 1)
        } else {
            match lower[tag_end..].find("</trophy>") {
                Some(offset) => {
                    let end = tag_end + offset;
                    (&text[tag_end + 1..end], end + "</trophy>".len())
                }
                None => (&text[tag_end + 1..], text.len()),
            }
        };
        cursor = next_cursor.max(start + "<trophy".len());

        let Some(id) = attribute(tag, "id").and_then(|value| value.parse::<u32>().ok()) else {
            continue;
        };
        if trophies.len() >= MAX_TROPHIES {
            break;
        }
        let body_lower = body.to_ascii_lowercase();
        trophies.push(RawTrophy {
            id,
            grade: Grade::from_code(&attribute(tag, "ttype").unwrap_or_default()),
            hidden: flag(tag, "hidden"),
            unlocked: flag(tag, "unlockstate"),
            name: element_text(body, &body_lower, "name").unwrap_or_default(),
            detail: element_text(body, &body_lower, "detail").unwrap_or_default(),
        });
    }
    if trophies.is_empty() {
        bail!("nenhum troféu encontrado no arquivo");
    }
    Ok(Parsed { title, trophies })
}

fn flag(tag: &str, key: &str) -> bool {
    matches!(
        attribute(tag, key).as_deref().map(str::trim),
        Some(value) if value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes") || value == "1"
    )
}

/// Lê `key="valor"` do texto de uma tag de abertura.
fn attribute(tag: &str, key: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let pattern = format!("{key}=");
    let mut cursor = 0usize;
    while let Some(offset) = lower[cursor..].find(&pattern) {
        let at = cursor + offset;
        // Evita casar `pid` com `id` ou `unlockstate` com `state`.
        let boundary = at == 0
            || lower[..at]
                .chars()
                .next_back()
                .map(|value| value.is_whitespace())
                .unwrap_or(false);
        if boundary {
            let rest = &tag[at + pattern.len()..];
            let quote = rest.chars().next()?;
            if quote == '"' || quote == '\'' {
                let value = &rest[quote.len_utf8()..];
                let end = value.find(quote)?;
                return Some(decode_entities(&value[..end]));
            }
        }
        cursor = at + pattern.len();
    }
    None
}

/// Texto de um elemento simples (`<name>…</name>`).
fn element_text(text: &str, lower: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = lower.find(&open)? + open.len();
    let end = lower[start..].find(&close)? + start;
    let value = clean(&text[start..end]);
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn clean(value: &str) -> String {
    let decoded = decode_entities(value);
    let mut text = String::with_capacity(decoded.len());
    let mut space = false;
    let mut count = 0usize;
    for character in decoded.chars() {
        if character.is_whitespace() {
            space = true;
        } else {
            if space && !text.is_empty() {
                text.push(' ');
            }
            space = false;
            text.push(character);
            count += 1;
            if count >= MAX_TEXT_CHARS {
                break;
            }
        }
    }
    text.trim().to_string()
}

fn decode_entities(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(offset) = rest.find('&') {
        out.push_str(&rest[..offset]);
        rest = &rest[offset..];
        let Some(end) = rest.find(';') else {
            break;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            other => numeric_entity(other),
        };
        match decoded {
            Some(character) => {
                out.push(character);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn numeric_entity(value: &str) -> Option<char> {
    let digits = value.strip_prefix('#')?;
    let code = if let Some(hex) = digits.strip_prefix('x').or_else(|| digits.strip_prefix('X')) {
        u32::from_str_radix(hex, 16).ok()?
    } else {
        digits.parse::<u32>().ok()?
    };
    char::from_u32(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TROPCONF: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<trophyconf version="1_00" platform="PS4">
  <title-name>God of War III</title-name>
  <title-detail>Remastered</title-detail>
  <trophy id="0" hidden="yes" ttype="p" pid="-1" gid="0">
    <name>O melhor do melhor</name>
    <detail>Desbloqueie &amp; todos os troféus.</detail>
  </trophy>
  <trophy id="1" hidden="no" ttype="b" pid="-1" gid="1">
    <name>Primeiro passo</name>
    <detail>Complete o prólogo.</detail>
  </trophy>
</trophyconf>
"#;

    fn sources<'a>(user_dir: &'a Path, cache: &'a Path) -> Sources<'a> {
        Sources {
            user_dir,
            cache_dir: cache,
            game_base: None,
            language: 1,
        }
    }

    #[test]
    fn reads_definitions_and_flags() {
        let parsed = parse(TROPCONF).expect("valid trophyconf");
        assert_eq!(parsed.title, "God of War III");
        assert_eq!(parsed.trophies.len(), 2);
        assert_eq!(parsed.trophies[0].id, 0);
        assert_eq!(parsed.trophies[0].grade, Grade::Platinum);
        assert!(parsed.trophies[0].hidden);
        assert!(!parsed.trophies[0].unlocked);
        assert_eq!(parsed.trophies[1].name, "Primeiro passo");
        assert_eq!(parsed.trophies[1].grade, Grade::Bronze);
        assert_eq!(parsed.trophies[0].detail, "Desbloqueie & todos os troféus.");
    }

    #[test]
    fn progress_attributes_override_definitions() {
        let progress = r#"<trophyconf version="1_00" platform="PS4">
  <trophy id="1" hidden="no" ttype="b" unlockstate="true" timestamp="1600000000">
    <name>Primeiro passo</name>
    <detail>Complete o prólogo.</detail>
  </trophy>
</trophyconf>"#;
        let parsed = parse(progress).expect("valid progress");
        assert!(parsed.trophies[0].unlocked);
    }

    #[test]
    fn attributes_do_not_match_similar_names() {
        let tag = r#"<trophy id="7" hidden="no" ttype="g" pid="-1" gid="0" unlockstate="false">"#;
        assert_eq!(attribute(tag, "id").as_deref(), Some("7"));
        assert!(!flag(tag, "unlockstate"));
        assert!(!flag(tag, "hidden"));
    }

    #[test]
    fn rejects_files_without_trophies() {
        assert!(parse("<config/>").is_err());
        assert!(parse("<trophyconf><title-name>Vazio</title-name></trophyconf>").is_err());
    }

    #[test]
    fn grades_map_to_interface_kinds() {
        assert_eq!(Grade::from_code("p").kind(), 0);
        assert_eq!(Grade::from_code("G").kind(), 1);
        assert_eq!(Grade::from_code("s").kind(), 2);
        assert_eq!(Grade::from_code("bronze").kind(), 3);
        assert_eq!(Grade::from_code("").kind(), 4);
    }

    #[test]
    fn missing_environment_is_reported_without_panicking() {
        let root = std::env::temp_dir().join("gow3-trophy-missing");
        match load(&sources(&root, &root)) {
            State::Empty { .. } => {}
            other => panic!("expected an empty state, got {other:?}"),
        }
    }

    #[test]
    fn reads_collection_from_private_layout() {
        let root = std::env::temp_dir().join(format!("gow3-trophy-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let xml_dir = root.join("trophy").join("NPWR00867_00").join("Xml");
        let icons_dir = root.join("trophy").join("NPWR00867_00").join("Icons");
        let progress_dir = root.join("home").join(USER_ID).join("trophy");
        fs::create_dir_all(&xml_dir).unwrap();
        fs::create_dir_all(&icons_dir).unwrap();
        fs::create_dir_all(&progress_dir).unwrap();
        fs::write(xml_dir.join("TROP_01.XML"), TROPCONF).unwrap();
        fs::write(
            progress_dir.join("NPWR00867_00.xml"),
            r#"<trophyconf><trophy id="1" ttype="b" unlockstate="true"/></trophyconf>"#,
        )
        .unwrap();

        let state = load(&sources(&root, &root.join("cache")));
        let State::Loaded(collection) = state else {
            panic!("expected a loaded collection, got {state:?}");
        };
        assert_eq!(collection.np_comm_id, "NPWR00867_00");
        assert_eq!(collection.xml_name, "TROP_01.XML");
        assert_eq!(collection.total(), 2);
        assert_eq!(collection.unlocked(), 1);
        assert!(!collection.from_package);
        assert!(!collection.trophies[1].has_icon());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn aes_matches_the_fips_197_vector() {
        // FIPS-197, apêndice C.1: chave 000102…0f, bloco 00112233…ff.
        let key = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ];
        let block = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ];
        let encrypted = cbc_encrypt(&key, &[0u8; 16], &block);
        assert_eq!(
            encrypted,
            vec![
                0x69, 0xc4, 0xe0, 0xd8, 0x6a, 0x7b, 0x04, 0x30, 0xd8, 0xcd, 0xb7, 0x80, 0x70, 0xb4,
                0xc5, 0x5a
            ]
        );
        assert_eq!(cbc_decrypt(&key, &[0u8; 16], &encrypted), block.to_vec());
    }

    fn build_trp(np_comm_id: &str, key: &[u8; 16], xml: &str) -> Vec<u8> {
        let mut id = [0u8; 16];
        id[..np_comm_id.len()].copy_from_slice(np_comm_id.as_bytes());
        let derived = cbc_encrypt(key, &[0u8; 16], &id);
        let mut trp_key = [0u8; 16];
        trp_key.copy_from_slice(&derived[..16]);
        let iv = [7u8; 16];
        let mut plain = xml.as_bytes().to_vec();
        while plain.len() % 16 != 0 {
            plain.push(b' ');
        }
        let mut payload = iv.to_vec();
        payload.extend(cbc_encrypt(&trp_key, &iv, &plain));

        let png = vec![0x89, b'P', b'N', b'G'];
        let header = TRP_HEADER_SIZE;
        let entries = 2usize;
        let xml_pos = header + entries * 64;
        let png_pos = xml_pos + payload.len();

        let mut file = vec![0u8; header];
        file[0..4].copy_from_slice(&TRP_MAGIC.to_be_bytes());
        file[16..20].copy_from_slice(&(entries as u32).to_be_bytes());
        file[20..24].copy_from_slice(&64u32.to_be_bytes());

        let mut entry = vec![0u8; 64];
        entry[..13].copy_from_slice(b"TROP_01.ESFM ");
        entry[32..40].copy_from_slice(&(xml_pos as u64).to_be_bytes());
        entry[40..48].copy_from_slice(&(payload.len() as u64).to_be_bytes());
        entry[48..52].copy_from_slice(&ENTRY_FLAG_ENCRYPTED_XML.to_be_bytes());
        file.extend_from_slice(&entry);

        entry = vec![0u8; 64];
        entry[..11].copy_from_slice(b"TROP001.PNG");
        entry[32..40].copy_from_slice(&(png_pos as u64).to_be_bytes());
        entry[40..48].copy_from_slice(&(png.len() as u64).to_be_bytes());
        entry[48..52].copy_from_slice(&ENTRY_FLAG_PNG.to_be_bytes());
        file.extend_from_slice(&entry);

        file.extend_from_slice(&payload);
        file.extend_from_slice(&png);
        file
    }

    #[test]
    fn reads_package_without_the_emulator() {
        let root = std::env::temp_dir().join(format!("gow3-trophy-pkg-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let game = root.join("game");
        let sce_sys = game.join("sce_sys");
        fs::create_dir_all(sce_sys.join("trophy")).unwrap();
        let key = [3u8; 16];
        let np_comm_id = "NPWR08268_00";

        let mut npbind = vec![0u8; NPBIND_HEADER_SIZE];
        npbind[0..4].copy_from_slice(&NPBIND_MAGIC.to_be_bytes());
        npbind[24..32].copy_from_slice(&1u64.to_be_bytes());
        npbind.extend_from_slice(&0x0010u16.to_be_bytes());
        npbind.extend_from_slice(&12u16.to_be_bytes());
        npbind.extend_from_slice(np_comm_id.as_bytes());
        for _ in 0..3 {
            npbind.extend_from_slice(&0u16.to_be_bytes());
            npbind.extend_from_slice(&0u16.to_be_bytes());
        }
        npbind.extend(std::iter::repeat(0u8).take(NPBIND_BODY_PADDING));
        fs::write(sce_sys.join("npbind.dat"), &npbind).unwrap();
        fs::write(
            sce_sys.join("trophy").join("trophy00.trp"),
            build_trp(np_comm_id, &key, TROPCONF),
        )
        .unwrap();

        let user = root.join("user");
        fs::create_dir_all(&user).unwrap();
        let hex: String = key.iter().map(|byte| format!("{byte:02x}")).collect();
        fs::write(
            user.join("keys.json"),
            format!(r#"{{"TrophyKeySet":{{"ReleaseTrophyKey":"{hex}"}}}}"#),
        )
        .unwrap();

        let cache = root.join("cache");
        let sources = Sources {
            user_dir: &user,
            cache_dir: &cache,
            game_base: Some(game),
            language: 1,
        };
        match load(&sources) {
            State::Loaded(collection) => {
                assert_eq!(collection.np_comm_id, np_comm_id);
                assert!(collection.from_package);
                assert_eq!(collection.total(), 2);
                assert_eq!(collection.unlocked(), 0);
                assert_eq!(collection.xml_name, "TROP_01.XML");
                assert!(cache.join(np_comm_id).join("TROP001.PNG").is_file());
            }
            other => panic!("esperava uma coleção, veio {other:?}"),
        }

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    #[ignore = "sonda local com caminho fixo da máquina de desenvolvimento"]
    fn real_machine_probe() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let user = root.join("data").join("ambiente").join("user");
        let cache = root.join("data").join("cache").join("trophies");
        let game = std::path::PathBuf::from(
            "C:/Users/kazwtto/Documents/shadps4/games/CUSA01623",
        );
        let sources = Sources {
            user_dir: &user,
            cache_dir: &cache,
            game_base: Some(game),
            language: 1,
        };
        let started = std::time::Instant::now();
        let state = load(&sources);
        let load_ms = started.elapsed().as_secs_f64() * 1000.;
        if let State::Loaded(collection) = &state {
            let started = std::time::Instant::now();
            let mut decoded = 0;
            for trophy in &collection.trophies {
                if trophy.has_icon() {
                    if let Ok(bytes) = fs::read(&trophy.icon) {
                        if let Ok(picture) = image::load_from_memory(&bytes) {
                            let _ = picture.to_rgba8();
                            decoded += 1;
                        }
                    }
                }
            }
            println!(
                "PROBE load_ms={load_ms:.1} decode_ms={:.1} icones={decoded}",
                started.elapsed().as_secs_f64() * 1000.
            );
        }
        match state {
            State::Loaded(collection) => println!(
                "PROBE total={} unlocked={} id={} xml={} titulo={}",
                collection.total(),
                collection.unlocked(),
                collection.np_comm_id,
                collection.xml_name,
                collection.title
            ),
            other => println!("PROBE estado={other:?}"),
        }
    }

    #[test]
    fn key_accepts_hex_and_array() {
        let hex = serde_json::json!("000102030405060708090a0b0c0d0e0f");
        assert_eq!(key_bytes(&hex).map(|bytes| bytes.len()), Some(16));
        let array = serde_json::json!([0, 1, 2]);
        assert_eq!(key_bytes(&array), Some(vec![0, 1, 2]));
        let empty = serde_json::json!("");
        assert!(key_bytes(&empty).is_none());
    }
}
