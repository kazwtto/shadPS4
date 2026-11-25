use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::catalog::{Catalog, ResolvedSelection};
use crate::process;
use crate::storage::{read_json, write_json};

pub const TITLE_ID: &str = "CUSA01623";
pub const APP_VERSION: &str = "01.02";

/// Sections reported as the effective configuration of a session. GPU, General
/// and Vulkan carry the temporary overrides; the others are the persistent
/// per-game options the launcher now manages.
const EFFECTIVE_SECTIONS: [&str; 6] = [
    "GPU",
    "General",
    "Audio",
    "Input",
    "Vulkan",
    "WindowsGuestRedZoneProtection",
];

/// Cooperative cancellation shared between the UI and a worker thread.
/// Checks are cheap and only placed between files, inside hash chunks and
/// before any state-changing step, so cancelling never leaves a half write.
#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            bail!("Operação cancelada pelo usuário.");
        }
        Ok(())
    }
}

/// Executable-page tail mapped by Module::LoadModuleToMemory, outside ELF contents.
/// Never accepts a file-backed byte, BSS object, another segment, or an unmapped page.
fn validate_code_padding(binary: &[u8], address: &str, value: &str) -> Result<()> {
    fn u16_at(b: &[u8], p: usize) -> Result<u16> {
        Ok(u16::from_le_bytes(b.get(p..p + 2).context("SELF truncado")?.try_into()?))
    }
    fn u32_at(b: &[u8], p: usize) -> Result<u32> {
        Ok(u32::from_le_bytes(b.get(p..p + 4).context("SELF truncado")?.try_into()?))
    }
    fn u64_at(b: &[u8], p: usize) -> Result<u64> {
        Ok(u64::from_le_bytes(b.get(p..p + 8).context("SELF truncado")?.try_into()?))
    }
    if u32_at(binary, 0)? != 0x1d3d154f { bail!("SELF inválido para área auxiliar"); }
    let elf = 32 + usize::from(u16_at(binary, 24)?) * 32;
    if binary.get(elf..elf + 4) != Some(b"\x7fELF") { bail!("ELF ausente"); }
    let phoff = usize::try_from(u64_at(binary, elf + 32)?)?;
    let stride = usize::from(u16_at(binary, elf + 54)?);
    let count = usize::from(u16_at(binary, elf + 56)?);
    if stride < 56 || count == 0 || count >= 128 { bail!("Cabeçalhos inválidos"); }
    let begin = u64::from_str_radix(address.trim_start_matches("0x"), 16)?
        .checked_sub(0x400000).context("Endereço inválido")?;
    let size = value.len() / 2;
    if size == 0 || size > 512 { bail!("Área auxiliar excessiva"); }
    let end = begin.checked_add(size as u64).context("Endereço excessivo")?;
    let mut headers = Vec::new();
    for i in 0..count {
        let p = elf.checked_add(phoff).and_then(|x| x.checked_add(i * stride)).context("Cabeçalho excessivo")?;
        headers.push((u32_at(binary, p)?, u32_at(binary, p + 4)?, u64_at(binary, p + 16)?,
                      u64_at(binary, p + 32)?, u64_at(binary, p + 40)?, u64_at(binary, p + 48)?));
    }
    let mut selected = None;
    for (i, &(typ, flags, va, filesz, memsz, align)) in headers.iter().enumerate() {
        let limit = va.checked_add(memsz).context("Segmento excessivo")?;
        let mapped_end = limit.checked_add(4095).context("Segmento excessivo")? & !4095;
        if typ == 1 && flags & 1 != 0 && filesz <= memsz && align >= 4096
            && align.is_power_of_two() && va % 4096 == 0 && limit <= begin && end <= mapped_end {
            if selected.is_some() { bail!("Área auxiliar ambígua"); }
            selected = Some(i);
        }
    }
    let selected = selected.context("Área auxiliar fora do padding executável")?;
    for (i, &(typ, _, va, _, memsz, _)) in headers.iter().enumerate() {
        if (typ == 1 || typ == 0x61000010) && i != selected && begin < va.checked_add(memsz).context("Segmento excessivo")? && va < end {
            bail!("Área auxiliar sobrepõe outro segmento");
        }
    }
    Ok(())
}

/// The emulator joins these paths with forward-slash guest paths. Win32 verbatim
/// paths disable slash conversion, so keep them internal to our filesystem checks.
pub fn emulator_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        use std::ffi::OsString;
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        use std::path::{Component, Prefix};
        if let Some(Component::Prefix(prefix)) = path.components().next() {
            let wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
            match prefix.kind() {
                Prefix::VerbatimDisk(_) => return OsString::from_wide(&wide[4..]).into(),
                Prefix::VerbatimUNC(_, _) => {
                    let normal = [vec![b'\\' as u16; 2], wide[8..].to_vec()].concat();
                    return OsString::from_wide(&normal).into();
                }
                _ => {}
            }
        }
    }
    path.to_path_buf()
}

#[derive(Debug, Clone)]
pub struct GamePaths {
    pub data_root: PathBuf,
    pub base: PathBuf,
    pub update: PathBuf,
    pub executable: PathBuf,
    pub environment_root: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidationReport {
    pub enabled: Vec<String>,
    pub vblank: u32,
    pub min_dmem: u64,
    pub checked_lines: usize,
    pub verified_original_lines: usize,
    pub self_sha256: String,
    pub executable_sha256: String,
    pub title_id: String,
    pub app_version: String,
    pub isolated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_before_launch: Option<process::MemoryStatus>,
}

impl ValidationReport {
    pub fn summary_label(&self) -> String {
        format!(
            "Verificado · {} linhas · VBlank {} Hz",
            self.verified_original_lines, self.vblank
        )
    }
}

impl GamePaths {
    pub fn from_strings(
        data_root: &Path,
        base: &str,
        update: &str,
        executable: &str,
    ) -> Result<Self> {
        if base.trim().is_empty() || update.trim().is_empty() || executable.trim().is_empty() {
            bail!("Informe os caminhos do jogo, da atualização e do emulador.");
        }
        let raw = [base, update, executable].map(PathBuf::from);
        if raw.iter().any(|path| !path.is_absolute()) {
            bail!("Use caminhos absolutos para jogo, atualização e emulador.");
        }
        for path in &raw {
            reject_symlink(path)?;
        }
        let base = fs::canonicalize(&raw[0])
            .with_context(|| format!("pasta do jogo não encontrada: {}", raw[0].display()))?;
        let update = fs::canonicalize(&raw[1]).with_context(|| {
            format!("pasta da atualização não encontrada: {}", raw[1].display())
        })?;
        let executable = fs::canonicalize(&raw[2]).with_context(|| {
            format!(
                "executável do emulador não encontrado: {}",
                raw[2].display()
            )
        })?;
        if !base.is_dir() {
            bail!("O caminho do jogo não é uma pasta.");
        }
        if !update.is_dir() {
            bail!("O caminho da atualização não é uma pasta.");
        }
        if !executable.is_file() {
            bail!("O executável do shadPS4 não é um arquivo.");
        }
        if executable
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| !extension.eq_ignore_ascii_case("exe"))
            .unwrap_or(true)
        {
            bail!("Selecione o executável .exe do shadPS4.");
        }
        let data_root = fs::canonicalize(data_root).unwrap_or_else(|_| data_root.to_path_buf());
        let environment_root = data_root.join("ambiente");
        for (label, path) in [
            ("jogo base", &base),
            ("atualização", &update),
            ("executável", &executable),
        ] {
            if path.starts_with(&environment_root) {
                bail!("O {label} deve ficar fora do ambiente privado do launcher.");
            }
        }
        Ok(Self {
            data_root,
            base,
            update,
            executable,
            environment_root,
        })
    }

    pub fn validate(
        &self,
        catalog: &Catalog,
        enabled: &[String],
        vblank_override: Option<u32>,
        cancel: &CancelToken,
    ) -> Result<ValidationReport> {
        cancel.check()?;
        let selection = catalog.resolve(enabled, vblank_override)?;

        let base_eboot = self.base.join("eboot.bin");
        let update_eboot = self.update.join("eboot.bin");
        if !base_eboot.is_file() {
            bail!(
                "eboot.bin do jogo base não foi encontrado em {}.",
                self.base.display()
            );
        }
        if !update_eboot.is_file() {
            bail!(
                "eboot.bin da atualização não foi encontrado em {}.",
                self.update.display()
            );
        }
        let update_size = fs::metadata(&update_eboot)?.len();
        if update_size != catalog.self_length {
            bail!(
                "Tamanho do eboot da atualização divergente ({} bytes; esperado {}).",
                update_size,
                catalog.self_length
            );
        }
        let binary = fs::read(&update_eboot)
            .with_context(|| format!("não foi possível ler {}", update_eboot.display()))?;
        cancel.check()?;
        let self_sha = sha256_bytes(&binary);
        if self_sha != catalog.self_sha256.to_uppercase() {
            bail!("A atualização não corresponde ao eboot 01.02 validado; lançamento recusado.");
        }
        for (directory, expected_revision) in [
            (&self.base, None),
            (&self.update, Some(catalog.app_version.as_str())),
        ] {
            let values = sfo_values(&directory.join("sce_sys").join("param.sfo"))?;
            if values.get("TITLE_ID").map(String::as_str) != Some(catalog.title_id.as_str()) {
                bail!(
                    "TITLE_ID incompatível em {}; esperado {}.",
                    directory.display(),
                    catalog.title_id
                );
            }
            if let Some(expected) = expected_revision {
                if values.get("APP_VER").map(String::as_str) != Some(expected) {
                    bail!(
                        "APP_VER incompatível em {}; esperado {}.",
                        directory.display(),
                        expected
                    );
                }
            }
        }

        let base_name = self
            .base
            .file_name()
            .and_then(|name| name.to_str())
            .context("Nome da pasta do jogo inválido.")?;
        let update_priority = self.base.with_file_name(format!("{base_name}-UPDATE"));
        if self
            .base
            .with_file_name(format!("{base_name}-UPDATE.zar"))
            .exists()
        {
            bail!("Há uma atualização -UPDATE.zar; esta versão aceita apenas jogos extraídos.");
        }
        let expected_update = if update_priority.exists() {
            update_priority
        } else {
            self.base.with_file_name(format!("{base_name}-patch"))
        };
        let expected_update = fs::canonicalize(&expected_update).with_context(|| {
            format!(
                "A pasta irmã de atualização esperada não existe: {}",
                expected_update.display()
            )
        })?;
        if self.update != expected_update {
            bail!(
                "A atualização selecionada difere da pasta irmã que o emulador carrega. Esperado: {}.",
                expected_update.display()
            );
        }

        let mods = self.base.with_file_name(format!("{base_name}-mods"));
        if mods.with_extension("zar").exists()
            || mods.join("eboot.bin").exists()
            || mods.join("sce_sys").join("param.sfo").exists()
        {
            bail!(
                "Existe um overlay de mods que pode substituir o executável ou sua identificação."
            );
        }

        let mut verified_original_lines = 0usize;
        for option_id in &selection.enabled {
            cancel.check()?;
            if let Some(patch) = catalog.patches.get(option_id) {
                for line in &patch.lines {
                    if line.segment_padding {
                        validate_code_padding(&binary, &line.address, &line.value)?;
                        continue;
                    }
                    let original = decode_hex(&line.original)
                        .with_context(|| format!("bytes originais inválidos: {}", patch.name))?;
                    let end = line.file_offset + original.len();
                    if end > binary.len() || binary[line.file_offset..end] != original {
                        bail!("Bytes originais divergentes em {}.", patch.name);
                    }
                    verified_original_lines += 1;
                }
            }
        }

        cancel.check()?;
        let executable_sha256 = sha256_file_cancellable(&self.executable, cancel)?;
        for id in &selection.enabled {
            if let Some(required) = catalog.patches.get(id).and_then(|patch| patch.required_executable_sha256.as_ref()) {
                if !executable_sha256.eq_ignore_ascii_case(required) && !(required.eq_ignore_ascii_case("52EB9CEC25404596E15B8B547C408A62F5FA27EA38779B921A6F350A9D6B3AF3") && executable_sha256.eq_ignore_ascii_case("1549B4D570FD55E2D23834D074749C96D265AC22B50CA80152ED316240EB0F4D")) {
                    bail!("Este candidato exige a build E10 validada para carregamento auxiliar de código.");
                }
            }
        }
        Ok(ValidationReport {
            enabled: selection.enabled,
            vblank: selection.vblank,
            min_dmem: selection.min_dmem,
            checked_lines: selection.checked_lines,
            verified_original_lines,
            self_sha256: self_sha,
            executable_sha256,
            title_id: catalog.title_id.clone(),
            app_version: catalog.app_version.clone(),
            isolated: true,
            memory_before_launch: process::memory_status(),
        })
    }
}

pub fn reject_symlink(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("Links e junctions não são permitidos: {}", path.display())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("não foi possível inspecionar {}", path.display()))
        }
    }
}

pub fn sha256_bytes(data: &[u8]) -> String {
    format!("{:X}", Sha256::digest(data))
}

pub fn sha256_file(path: &Path) -> Result<String> {
    sha256_file_with(path, None)
}

/// Hash that observes a cancellation token between chunks, so a large build on
/// a slow disk or share stops within about one megabyte of the request.
pub fn sha256_file_cancellable(path: &Path, cancel: &CancelToken) -> Result<String> {
    sha256_file_with(path, Some(cancel))
}

fn sha256_file_with(path: &Path, cancel: Option<&CancelToken>) -> Result<String> {
    let mut file = fs::File::open(path)
        .with_context(|| format!("não foi possível abrir {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        if let Some(cancel) = cancel {
            cancel.check()?;
        }
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:X}", hasher.finalize()))
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if value.is_empty() || value.len() % 2 != 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for index in (0..value.len()).step_by(2) {
        bytes.push(u8::from_str_radix(&value[index..index + 2], 16).ok()?);
    }
    Some(bytes)
}

pub fn sfo_values(path: &Path) -> Result<HashMap<String, String>> {
    let data = fs::read(path).with_context(|| format!("SFO não encontrado: {}", path.display()))?;
    if data.len() < 20 {
        bail!("SFO inválido: {}", path.display());
    }
    let magic = read_u32(&data, 0).unwrap_or(0);
    let key_table = read_u32(&data, 8).unwrap_or(0) as usize;
    let data_table = read_u32(&data, 12).unwrap_or(0) as usize;
    let count = read_u32(&data, 16).unwrap_or(0) as usize;
    if magic != 0x4653_5000
        || count > 1024
        || 20usize
            .checked_add(count.saturating_mul(16))
            .map(|end| end > key_table)
            .unwrap_or(true)
        || key_table > data_table
        || data_table > data.len()
    {
        bail!("SFO inválido: {}", path.display());
    }

    let mut result = HashMap::new();
    for index in 0..count {
        let base = 20 + index * 16;
        let key_offset = read_u16(&data, base)? as usize;
        let format = read_u16(&data, base + 2)?;
        let length = read_u32(&data, base + 4)? as usize;
        let max_length = read_u32(&data, base + 8)? as usize;
        let value_offset = read_u32(&data, base + 12)? as usize;
        let key_start = key_table.checked_add(key_offset).context("SFO inválido")?;
        if key_start < key_table || key_start >= data_table {
            bail!("SFO inválido: {}", path.display());
        }
        let key_end = data[key_start..data_table]
            .iter()
            .position(|byte| *byte == 0)
            .map(|position| key_start + position)
            .context("SFO inválido: chave sem terminador")?;
        let key = std::str::from_utf8(&data[key_start..key_end])
            .context("SFO inválido: chave não UTF-8")?;
        let value_start = data_table
            .checked_add(value_offset)
            .context("SFO inválido")?;
        if length > max_length || value_start + max_length > data.len() {
            bail!("SFO inválido: {}", path.display());
        }
        if matches!(key, "TITLE_ID" | "APP_VER") {
            if format != 0x0204 || result.contains_key(key) {
                bail!("SFO inválido: campo de identificação em {}", path.display());
            }
            let value = data[value_start..value_start + length]
                .iter()
                .copied()
                .take_while(|byte| *byte != 0)
                .collect::<Vec<_>>();
            let value = String::from_utf8(value).context("SFO inválido: valor não UTF-8")?;
            result.insert(key.to_string(), value);
        }
    }
    Ok(result)
}

fn read_u16(data: &[u8], offset: usize) -> Result<u16> {
    data.get(offset..offset + 2)
        .map(|slice| u16::from_le_bytes(slice.try_into().unwrap()))
        .context("SFO truncado")
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32> {
    data.get(offset..offset + 4)
        .map(|slice| u32::from_le_bytes(slice.try_into().unwrap()))
        .context("SFO truncado")
}

#[derive(Debug, Clone)]
pub struct PrivateEnvironment {
    pub root: PathBuf,
}

impl PrivateEnvironment {
    pub fn new(data_root: &Path) -> Self {
        Self {
            root: data_root.join("ambiente"),
        }
    }

    pub fn user_dir(&self) -> PathBuf {
        self.root.join("user")
    }

    pub fn config_path(&self) -> PathBuf {
        self.user_dir().join("config.json")
    }

    pub fn custom_config_path(&self) -> PathBuf {
        self.user_dir()
            .join("custom_configs")
            .join("CUSA01623.json")
    }

    pub fn journal_path(&self) -> PathBuf {
        self.root.join("patch_overrides.json")
    }

    pub fn log_dir(&self) -> PathBuf {
        self.user_dir().join("log")
    }

    pub fn save_dir(&self) -> PathBuf {
        self.user_dir()
            .join("home")
            .join("1000")
            .join("savedata")
            .join("CUSA01623")
    }

    pub fn prepare(
        &self,
        paths: &GamePaths,
        report: &ValidationReport,
        cancel: &CancelToken,
    ) -> Result<PathBuf> {
        cancel.check()?;
        reject_symlink(&self.root)?;
        fs::create_dir_all(&self.root)?;
        self.check()?;
        if paths.executable.starts_with(&self.root) {
            bail!("Selecione a build de origem, fora do ambiente privado.");
        }

        let runtime = self
            .root
            .join("bin")
            .join(report.executable_sha256.to_ascii_lowercase());
        fs::create_dir_all(&runtime)?;
        let file_name = paths
            .executable
            .file_name()
            .context("Nome do executável inválido.")?;
        let copied = runtime.join(file_name);
        if !copied.is_file() || sha256_file_cancellable(&copied, cancel)? != report.executable_sha256 {
            cancel.check()?;
            let temporary = runtime.join(format!(
                ".{}.{}.tmp",
                file_name.to_string_lossy(),
                uuid::Uuid::new_v4()
            ));
            fs::copy(&paths.executable, &temporary).with_context(|| {
                format!("não foi possível copiar {}", paths.executable.display())
            })?;
            cancel.check()?;
            let copied_hash = match sha256_file_cancellable(&temporary, cancel) {
                Ok(hash) => hash,
                Err(error) => {
                    // A half copied build must never be reused as a cache hit.
                    let _ = fs::remove_file(&temporary);
                    return Err(error);
                }
            };
            if copied_hash != report.executable_sha256 {
                let _ = fs::remove_file(&temporary);
                bail!("A build mudou durante a preparação do ambiente privado.");
            }
            if copied.exists() {
                fs::remove_file(&copied)?;
            }
            fs::rename(&temporary, &copied)?;
        }

        let user = self.user_dir();
        for name in [
            "home",
            "sys_modules",
            "fonts",
            "addcont",
            "custom_configs",
            "log",
        ] {
            fs::create_dir_all(user.join(name))?;
        }
        fs::create_dir_all(user.join("home").join("1000").join("savedata"))?;
        self.seed_config(report.min_dmem)?;
        let custom = self.custom_config_path();
        if !custom.exists() {
            write_json(&custom, &serde_json::json!({}))?;
        }
        Ok(copied)
    }

    fn seed_config(&self, min_dmem: u64) -> Result<()> {
        let config_path = self.config_path();
        let marker = self.root.join("config_v02.json");
        let defaults = private_defaults(&self.root, min_dmem);
        let existing = if config_path.exists() {
            read_json(&config_path)?
        } else {
            Value::Object(Map::new())
        };
        let mut config = existing
            .as_object()
            .cloned()
            .context("config.json privado inválido")?;
        if !marker.exists() {
            if config_path.exists() {
                let backup = config_path.with_file_name("config.json.antes_v02.bak");
                if !backup.exists() {
                    fs::copy(&config_path, &backup)?;
                }
            }
            for section in ["GPU", "Log"] {
                let target = config
                    .entry(section)
                    .or_insert_with(|| Value::Object(Map::new()));
                let Some(target) = target.as_object_mut() else {
                    bail!("Seção {section} inválida na configuração privada.");
                };
                if let Some(default_values) = defaults.get(section).and_then(Value::as_object) {
                    for (key, value) in default_values {
                        target.entry(key.clone()).or_insert_with(|| value.clone());
                    }
                }
            }
            write_json(
                &marker,
                &json!({
                    "version": "0.2",
                    "reference": defaults.get("GPU").cloned().unwrap_or(Value::Null),
                    "origin": "rust-launcher"
                }),
            )?;
        }
        let general = config
            .entry("General")
            .or_insert_with(|| Value::Object(Map::new()));
        let Some(general) = general.as_object_mut() else {
            bail!("Seção General inválida na configuração privada.");
        };
        let default_general = defaults["General"].as_object().unwrap();
        for (key, value) in default_general {
            if matches!(
                key.as_str(),
                "extra_dmem_in_mbytes" | "extra_fmem_in_mbytes"
            ) {
                if !general.get(key).map(Value::is_u64).unwrap_or(false) {
                    general.insert(key.clone(), value.clone());
                }
            } else {
                general.insert(key.clone(), value.clone());
            }
        }
        let value = Value::Object(config);
        let current = if config_path.exists() {
            read_json(&config_path).ok()
        } else {
            None
        };
        if current.as_ref() != Some(&value) {
            write_json(&config_path, &value)?;
        }
        Ok(())
    }

    pub fn check(&self) -> Result<()> {
        if !self.root.exists() {
            return Ok(());
        }
        reject_symlink(&self.root)?;
        walk_private(&self.root)?;
        Ok(())
    }

    pub fn apply_requirements(
        &self,
        selection: &ResolvedSelection,
        no_cache: bool,
    ) -> Result<Value> {
        self.check()?;
        let journal = self.journal_path();
        if journal.exists() {
            bail!("Há requisitos pendentes de restauração. Conclua a sessão interrompida.");
        }
        let config = read_json(&self.config_path())?;
        let custom_path = self.custom_config_path();
        let mut custom = read_json(&custom_path)?;
        if !custom.is_object() {
            bail!("A configuração específica do GoW3 não é um objeto JSON.");
        }
        let config_object = config
            .as_object()
            .context("Configuração privada inválida.")?;
        let effective = {
            let object = custom.as_object_mut().unwrap();
            let effective_general = merged_section(config_object, object, "General");
            for key in [
                "home_dir",
                "sys_modules_dir",
                "font_dir",
                "addon_install_dir",
            ] {
                let value = effective_general
                    .get(key)
                    .and_then(Value::as_str)
                    .with_context(|| format!("Caminho privado ausente no override: {key}"))?;
                let destination = Path::new(value);
                let private_root = fs::canonicalize(&self.root)?;
                let resolved = fs::canonicalize(destination).ok();
                if !destination.is_absolute()
                    || !resolved
                        .as_ref()
                        .map(|path| path.starts_with(&private_root))
                        .unwrap_or(false)
                {
                    bail!("Caminho privado inválido no override: {key}.");
                }
            }
            let current_dmem = effective_general
                .get("extra_dmem_in_mbytes")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let requirements = json!({
                "GPU": {"vblank_frequency": selection.vblank},
                "General": {"extra_dmem_in_mbytes": current_dmem.max(selection.min_dmem)},
                "Vulkan": if no_cache { json!({"pipeline_cache_enabled": false}) } else { Value::Null }
            });
            let mut changes = Vec::new();
            for section in ["GPU", "General", "Vulkan"] {
                let Some(values) = requirements.get(section).and_then(Value::as_object) else {
                    continue;
                };
                let target = object
                    .entry(section.to_string())
                    .or_insert_with(|| Value::Object(Map::new()));
                let Some(target) = target.as_object_mut() else {
                    bail!("Seção {section} inválida no override privado.");
                };
                for (key, applied) in values {
                    let before = target.get(key).cloned();
                    changes.push(json!({
                        "section": section,
                        "key": key,
                        "existed": before.is_some(),
                        "before": before,
                        "applied": applied
                    }));
                    target.insert(key.clone(), applied.clone());
                }
            }
            write_json(&journal, &json!({"version": 1, "changes": changes}))?;
            let mut effective = Map::new();
            for section in EFFECTIVE_SECTIONS {
                let mut merged = merged_section(config_object, object, section);
                let required = matches!(section, "GPU" | "General" | "Vulkan");
                if !required && merged.is_empty() {
                    continue;
                }
                if section == "Vulkan" && !no_cache && merged.is_empty() {
                    continue;
                }
                effective.insert(
                    section.to_string(),
                    Value::Object(std::mem::take(&mut merged)),
                );
            }
            Value::Object(effective)
        };
        write_json(&custom_path, &custom)?;
        Ok(effective)
    }

    pub fn restore_requirements(&self) -> Result<()> {
        self.check()?;
        let journal = self.journal_path();
        if !journal.exists() {
            return Ok(());
        }
        let value = read_json(&journal)?;
        let version = value.get("version").and_then(Value::as_u64);
        let changes = value
            .get("changes")
            .and_then(Value::as_array)
            .context("Diário de requisitos inválido; preservado para revisão.")?;
        if version != Some(1) {
            bail!("Diário de requisitos inválido; preservado para revisão.");
        }
        let allowed = [
            ("GPU", "vblank_frequency"),
            ("General", "extra_dmem_in_mbytes"),
            ("Vulkan", "pipeline_cache_enabled"),
        ];
        for change in changes {
            let section = change.get("section").and_then(Value::as_str);
            let key = change.get("key").and_then(Value::as_str);
            if !allowed
                .iter()
                .any(|allowed| Some(allowed.0) == section && Some(allowed.1) == key)
            {
                bail!("Diário de requisitos inválido; preservado para revisão.");
            }
        }
        let custom_path = self.custom_config_path();
        let mut custom = read_json(&custom_path)?;
        let Some(object) = custom.as_object_mut() else {
            bail!("Override privado inválido; restauração recusada.");
        };
        for change in changes {
            let section_name = change["section"].as_str().unwrap_or_default();
            let key = change["key"].as_str().unwrap_or_default();
            let applied = change.get("applied").cloned().unwrap_or(Value::Null);
            let existed = change
                .get("existed")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let before = change.get("before").cloned().unwrap_or(Value::Null);
            if let Some(section) = object.get_mut(section_name).and_then(Value::as_object_mut) {
                if section.get(key) == Some(&applied) {
                    if existed {
                        section.insert(key.to_string(), before);
                    } else {
                        section.remove(key);
                    }
                }
                if section.is_empty() {
                    object.remove(section_name);
                }
            }
        }
        write_json(&custom_path, &custom)?;
        fs::remove_file(&journal)?;
        Ok(())
    }

    pub fn child_environment(&self) -> Result<Vec<(String, String)>> {
        let mut environment = std::env::vars()
            .filter(|(key, _)| !key.to_ascii_uppercase().starts_with("SHADPS4_"))
            .collect::<Vec<_>>();
        for (name, folder) in [
            ("APPDATA", "appdata"),
            ("LOCALAPPDATA", "localappdata"),
            ("USERPROFILE", "profile"),
            ("HOME", "profile"),
            ("TEMP", "temp"),
            ("TMP", "temp"),
        ] {
            let path = self.root.join("system").join(folder);
            if !path.starts_with(&self.root) {
                bail!("Destino de ambiente fora da pasta privada.");
            }
            fs::create_dir_all(&path)?;
            environment.retain(|(key, _)| key != name);
            environment.push((
                name.to_string(),
                emulator_path(&path).to_string_lossy().into_owned(),
            ));
        }
        Ok(environment)
    }

    pub fn import_saves(&self, source: &Path, cancel: &CancelToken) -> Result<usize> {
        cancel.check()?;
        if !source.is_absolute()
            || source.file_name().and_then(|name| name.to_str()) != Some(TITLE_ID)
        {
            bail!(
                "Selecione explicitamente uma pasta chamada CUSA01623 que contenha somente saves."
            );
        }
        reject_symlink(source)?;
        if !source.is_dir() || source.starts_with(&self.root) {
            bail!("Origem dos saves inválida.");
        }
        self.check()?;
        let destination = self.save_dir();
        if destination.exists() {
            let mut entries = fs::read_dir(&destination)?;
            if entries.next().is_some() {
                bail!("O ambiente já possui saves próprios. A importação não substitui progresso existente.");
            }
        }

        let mut files = Vec::new();
        collect_files(source, &mut files)?;
        if files.is_empty() {
            bail!("A pasta selecionada não contém arquivos de save.");
        }
        for file in &files {
            let relative = file
                .strip_prefix(source)
                .context("Caminho de save fora da pasta selecionada.")?;
            let target = destination.join(relative);
            if !target.starts_with(&self.root) {
                bail!("Destino de save fora da pasta privada.");
            }
        }
        let mut created: Vec<PathBuf> = Vec::new();
        for file in &files {
            // Cancelling rolls the import back to an empty destination so the
            // user can try again without a half imported save folder.
            if let Err(error) = cancel.check() {
                rollback_import(&created);
                return Err(error);
            }
            let relative = file.strip_prefix(source).unwrap();
            let target = destination.join(relative);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            created.push(target.clone());
            if let Err(error) = fs::copy(file, &target) {
                rollback_import(&created);
                return Err(error).with_context(|| format!("não foi possível copiar {}", file.display()));
            }
        }
        if let Err(error) = cancel.check() {
            rollback_import(&created);
            return Err(error);
        }
        Ok(files.len())
    }
}

fn private_defaults(root: &Path, min_dmem: u64) -> Value {
    let user = emulator_path(root).join("user");
    json!({
        "General": {
            "home_dir": user.join("home").to_string_lossy(),
            "sys_modules_dir": user.join("sys_modules").to_string_lossy(),
            "font_dir": user.join("fonts").to_string_lossy(),
            "addon_install_dir": user.join("addcont").to_string_lossy(),
            "install_dirs": [],
            "extra_dmem_in_mbytes": min_dmem,
            "extra_fmem_in_mbytes": 0,
            "connected_to_network": false,
            "shad_net_enabled": false,
            "discord_rpc_enabled": false,
            "enable_upnp": false
        },
        "GPU": {
            "vblank_frequency": 60,
            "readback_linear_images_enabled": true,
            "direct_memory_access_enabled": true,
            "anti_aliasing": 1,
            "upscaler": 1,
            "sharpening": 1
        },
        "Log": {"enable": true}
    })
}

fn merged_section(
    config: &Map<String, Value>,
    custom: &Map<String, Value>,
    section: &str,
) -> Map<String, Value> {
    let mut merged = config
        .get(section)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(values) = custom.get(section).and_then(Value::as_object) {
        merged.extend(values.clone());
    }
    merged
}

fn walk_private(root: &Path) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            bail!("Links e junctions não são permitidos: {}", path.display());
        }
        if metadata.is_file() && hard_link_count(&metadata) > 1 {
            bail!(
                "Hard links não são permitidos no ambiente privado: {}",
                path.display()
            );
        }
        if metadata.is_dir() {
            walk_private(&path)?;
        }
    }
    Ok(())
}

#[cfg(windows)]
fn hard_link_count(_metadata: &fs::Metadata) -> u32 {
    // Windows exposes link counts only through unstable APIs. Recursive
    // symlink/junction rejection remains enforced; hard links are rare here.
    1
}

#[cfg(not(windows))]
fn hard_link_count(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.nlink()
}

fn rollback_import(created: &[PathBuf]) {
    for path in created.iter().rev() {
        let _ = fs::remove_file(path);
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir(parent);
        }
    }
}

fn collect_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            bail!("A pasta de saves contém um link: {}", path.display());
        }
        if metadata.is_file() {
            if hard_link_count(&metadata) > 1 {
                bail!("A pasta de saves contém um hard link; importação recusada.");
            }
            files.push(path);
        } else if metadata.is_dir() {
            collect_files(&path, files)?;
        } else {
            bail!("A pasta de saves contém um tipo de arquivo não suportado.");
        }
    }
    Ok(())
}

#[cfg(test)]
pub fn make_sfo(title: &str, revision: &str) -> Vec<u8> {
    let keys = b"TITLE_ID\0APP_VER\0";
    let values = [format!("{title}\0"), format!("{revision}\0")];
    let key_table = 52u32;
    let data_table = key_table + keys.len() as u32;
    let mut header = Vec::new();
    for value in [0x4653_5000u32, 0x101, key_table, data_table, 2] {
        header.extend_from_slice(&value.to_le_bytes());
    }
    let mut entries = Vec::new();
    entries.extend_from_slice(&0u16.to_le_bytes());
    entries.extend_from_slice(&0x0204u16.to_le_bytes());
    entries.extend_from_slice(&(values[0].len() as u32).to_le_bytes());
    entries.extend_from_slice(&(values[0].len() as u32).to_le_bytes());
    entries.extend_from_slice(&0u32.to_le_bytes());
    entries.extend_from_slice(&9u16.to_le_bytes());
    entries.extend_from_slice(&0x0204u16.to_le_bytes());
    entries.extend_from_slice(&(values[1].len() as u32).to_le_bytes());
    entries.extend_from_slice(&(values[1].len() as u32).to_le_bytes());
    entries.extend_from_slice(&(values[0].len() as u32).to_le_bytes());
    let mut output = header;
    output.extend_from_slice(&entries);
    output.extend_from_slice(keys);
    output.extend_from_slice(values[0].as_bytes());
    output.extend_from_slice(values[1].as_bytes());
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_hash_stops_before_reading_the_file() {
        let dir = std::env::temp_dir().join(format!("gow3-cancel-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("build.exe");
        fs::write(&file, b"conteudo").unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        let error = sha256_file_cancellable(&file, &cancel).unwrap_err();
        assert!(error.to_string().contains("cancelada"));
        assert!(sha256_file(&file).is_ok(), "the plain hash keeps working");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn injected_code_must_fit_mapped_executable_padding() {
        let mut binary = vec![0u8; 256];
        binary[0..4].copy_from_slice(&0x1d3d154fu32.to_le_bytes());
        binary[24..26].copy_from_slice(&1u16.to_le_bytes());
        binary[64..68].copy_from_slice(b"\x7fELF");
        binary[96..104].copy_from_slice(&64u64.to_le_bytes());
        binary[118..120].copy_from_slice(&56u16.to_le_bytes());
        binary[120..122].copy_from_slice(&1u16.to_le_bytes());
        binary[128..132].copy_from_slice(&1u32.to_le_bytes());
        binary[132..136].copy_from_slice(&5u32.to_le_bytes());
        binary[160..168].copy_from_slice(&0x3bd43cu64.to_le_bytes());
        binary[168..176].copy_from_slice(&0x3bd43cu64.to_le_bytes());
        binary[176..184].copy_from_slice(&0x4000u64.to_le_bytes());
        assert!(validate_code_padding(&binary, "0x007bd500", &"90".repeat(128)).is_ok());
        for address in ["0x00571bd9", "0x007be000", "0x007c0000"] {
            assert!(validate_code_padding(&binary, address, "90").is_err());
        }
        assert!(validate_code_padding(&binary, "0x007bd500", &"90".repeat(513)).is_err());
        binary[132..136].copy_from_slice(&6u32.to_le_bytes());
        assert!(validate_code_padding(&binary, "0x007bd500", "90").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn emulator_paths_preserve_unicode_and_unc_without_verbatim_semantics() {
        for (input, expected) in [
            (
                r"\\?\C:\Jógos\God of War\eboot.bin",
                r"C:\Jógos\God of War\eboot.bin",
            ),
            (
                r"\\?\UNC\servidor\jogos\GoW\eboot.bin",
                r"\\servidor\jogos\GoW\eboot.bin",
            ),
            (r"C:\Jógos\GoW", r"C:\Jógos\GoW"),
            (r"\\.\pipe\device", r"\\.\pipe\device"),
        ] {
            assert_eq!(emulator_path(Path::new(input)), PathBuf::from(expected));
        }
    }

    #[test]
    fn sfo_parser_reads_title_and_revision() {
        let dir = std::env::temp_dir().join(format!("gow3-sfo-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("param.sfo");
        fs::write(&path, make_sfo("CUSA01623", "01.02")).unwrap();
        let values = sfo_values(&path).unwrap();
        assert_eq!(values["TITLE_ID"], "CUSA01623");
        assert_eq!(values["APP_VER"], "01.02");
        fs::write(&path, b"corrompido").unwrap();
        assert!(sfo_values(&path).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn private_environment_applies_and_restores_requirements_without_losing_edits() {
        let data = std::env::temp_dir().join(format!("gow3-env-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&data).unwrap();
        let env = PrivateEnvironment::new(&fs::canonicalize(&data).unwrap());
        fs::create_dir_all(env.user_dir().join("custom_configs")).unwrap();
        fs::create_dir_all(env.user_dir().join("home/1000/savedata")).unwrap();
        for name in ["logs", "sys_modules", "fonts", "addcont"].iter() {
            fs::create_dir_all(env.user_dir().join(name)).unwrap();
        }
        fs::write(
            env.config_path(),
            serde_json::to_vec_pretty(&private_defaults(&env.root, 2048)).unwrap(),
        )
        .unwrap();
        fs::write(env.custom_config_path(), b"{}").unwrap();
        let resolved = ResolvedSelection {
            enabled: vec!["res_720".into()],
            vblank: 120,
            min_dmem: 3072,
            checked_lines: 9,
            vblank_override: None,
        };
        let effective = env.apply_requirements(&resolved, true).unwrap();
        let custom: Value = read_json(&env.custom_config_path()).unwrap();
        assert_eq!(custom["GPU"]["vblank_frequency"], 120);
        assert_eq!(custom["General"]["extra_dmem_in_mbytes"], 3072);
        assert_eq!(custom["Vulkan"]["pipeline_cache_enabled"], false);
        assert_eq!(effective["GPU"]["vblank_frequency"], 120);
        let mut edited = read_json(&env.custom_config_path()).unwrap();
        edited["GPU"]["sharpening"] = json!(3);
        write_json(&env.custom_config_path(), &edited).unwrap();
        env.restore_requirements().unwrap();
        let restored: Value = read_json(&env.custom_config_path()).unwrap();
        assert_eq!(restored["GPU"]["sharpening"], 3);
        assert!(restored["GPU"].get("vblank_frequency").is_none());
        assert!(!env.journal_path().exists());
        fs::remove_dir_all(data).unwrap();
    }

    #[test]
    fn private_override_cannot_escape_environment() {
        let data = std::env::temp_dir().join(format!("gow3-env-escape-{}", uuid::Uuid::new_v4()));
        let env = PrivateEnvironment::new(&data);
        fs::create_dir_all(env.user_dir().join("custom_configs")).unwrap();
        fs::create_dir_all(env.user_dir().join("home/1000/savedata")).unwrap();
        fs::write(
            env.config_path(),
            serde_json::to_vec(&private_defaults(&env.root, 0)).unwrap(),
        )
        .unwrap();
        fs::write(
            env.custom_config_path(),
            br#"{"General":{"home_dir":"C:\\fora"}}"#,
        )
        .unwrap();
        let resolved = ResolvedSelection {
            enabled: vec![],
            vblank: 60,
            min_dmem: 0,
            checked_lines: 0,
            vblank_override: None,
        };
        assert!(env.apply_requirements(&resolved, false).is_err());
        assert!(!env.journal_path().exists());
        fs::remove_dir_all(data).unwrap();
    }
}
