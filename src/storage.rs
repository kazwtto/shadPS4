use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::catalog::Catalog;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub base: String,
    #[serde(default)]
    pub update: String,
    #[serde(default)]
    pub executable: String,
    #[serde(default = "default_profile")]
    pub profile: String,
    #[serde(default)]
    pub desired_enabled: Option<Vec<String>>,
    #[serde(default)]
    pub capture_enabled: bool,
    #[serde(default)]
    pub vblank_override: Option<u32>,
    #[serde(default)]
    pub launch_mode: Option<u8>,
    #[serde(default)]
    pub gpu_mode: u8,
    #[serde(default)]
    pub lazy_cache: bool,
    #[serde(default)]
    pub disable_animations: bool,
    #[serde(default = "default_fluid_background")]
    pub fluid_background: bool,
    /// Atividade do Discord; desligada até o usuário pedir.
    #[serde(default)]
    pub discord_presence: bool,
    /// 0 segue o idioma do console; os demais índices escolhem o idioma.
    #[serde(default)]
    pub launcher_language: u8,
    /// Consulta de atualizações no repositório; ligada por padrão.
    #[serde(default = "default_auto_update")]
    pub auto_update_check: bool,
}

fn schema_version() -> u32 {
    1
}

fn default_profile() -> String {
    "Base60".to_string()
}

fn default_fluid_background() -> bool {
    false
}

fn default_auto_update() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: 3,
            base: String::new(),
            update: String::new(),
            executable: String::new(),
            profile: default_profile(),
            desired_enabled: None,
            capture_enabled: false,
            vblank_override: None,
            launch_mode: Some(0),
            gpu_mode: 0,
            lazy_cache: false,
            disable_animations: false,
            fluid_background: false,
            discord_presence: false,
            launcher_language: 0,
            auto_update_check: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredProfile {
    enabled: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProfilesFile {
    schema_version: u32,
    catalog_version: String,
    profiles: BTreeMap<String, StoredProfile>,
}

#[derive(Debug)]
pub struct Store {
    pub directory: PathBuf,
    pub settings_path: PathBuf,
    pub profiles_path: PathBuf,
    pub custom_profiles: BTreeMap<String, Vec<String>>,
    pub notices: Vec<String>,
    profiles_backup_needed: bool,
}

impl Store {
    pub fn load(directory: PathBuf, catalog: &Catalog) -> Result<Self> {
        fs::create_dir_all(&directory)
            .with_context(|| format!("não foi possível criar {}", directory.display()))?;
        let settings_path = directory.join("settings.json");
        let profiles_path = directory.join("profiles.json");
        let mut store = Self {
            directory,
            settings_path,
            profiles_path,
            custom_profiles: BTreeMap::new(),
            notices: Vec::new(),
            profiles_backup_needed: false,
        };
        store.load_profiles(catalog);
        Ok(store)
    }

    fn load_profiles(&mut self, catalog: &Catalog) {
        if !self.profiles_path.exists() {
            return;
        }
        let bytes = match fs::read(&self.profiles_path) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.notices.push(format!(
                    "Não foi possível ler profiles.json ({}). O arquivo foi preservado.",
                    error
                ));
                self.profiles_backup_needed = true;
                return;
            }
        };
        let parsed: ProfilesFile = match serde_json::from_slice(&bytes) {
            Ok(parsed) => parsed,
            Err(error) => {
                self.notices.push(format!(
                    "profiles.json inválido ({}). O arquivo foi preservado antes de novas gravações.",
                    error
                ));
                self.profiles_backup_needed = true;
                return;
            }
        };
        if parsed.schema_version != 1 {
            self.notices.push(
                "Formato de perfis não suportado. O arquivo foi preservado; apenas perfis do pacote serão usados."
                    .into(),
            );
            self.profiles_backup_needed = true;
            return;
        }
        for (name, profile) in parsed.profiles {
            if catalog.profiles.contains_key(&name) {
                self.notices.push(format!(
                    "O perfil de referência {name} foi preservado e ignorado."
                ));
                self.profiles_backup_needed = true;
                continue;
            }
            if profile.enabled.iter().any(|id| id.trim().is_empty()) {
                self.notices
                    .push(format!("Perfil inválido ignorado: {name}."));
                self.profiles_backup_needed = true;
                continue;
            }
            self.custom_profiles.insert(name, profile.enabled);
        }
    }

    pub fn settings(&self) -> Result<Settings> {
        if !self.settings_path.exists() {
            return Ok(Settings::default());
        }
        let bytes = fs::read(&self.settings_path)
            .with_context(|| format!("não foi possível ler {}", self.settings_path.display()))?;
        let mut settings: Settings = serde_json::from_slice(&bytes).with_context(|| {
            format!(
                "formato de preferências inválido: {}",
                self.settings_path.display()
            )
        })?;
        if !matches!(settings.schema_version, 1..=3) {
            bail!("formato de preferências não suportado");
        }
        if settings.schema_version < 2 {
            // Version 1 predates the fluid choice, which now ships off.
            settings.fluid_background = false;
            settings.schema_version = 2;
        }
        if settings.schema_version < 3 {
            // Até a versão 2 a pasta de builds vinha da descoberta ao lado do
            // repositório; a partir da 3 ela é criada em `data/builds` pelo
            // próprio launcher, sem preferência salva.
            settings.schema_version = 3;
        }
        if settings.profile.trim().is_empty() {
            settings.profile = default_profile();
        }
        if let Some(mode) = settings.launch_mode {
            if mode > 2 {
                settings.launch_mode = Some(0);
            }
        }
        settings.gpu_mode = settings.gpu_mode.min(3);
        Ok(settings)
    }

    pub fn save_settings(&mut self, settings: &Settings) -> Result<()> {
        let mut value = settings.clone();
        value.schema_version = 3;
        write_json(&self.settings_path, &value)
    }

    pub fn all_profile_names(&self, catalog: &Catalog) -> Vec<String> {
        let mut names = catalog.profile_names().to_vec();
        let extras = self
            .custom_profiles
            .keys()
            .filter(|name| !names.contains(name))
            .cloned()
            .collect::<Vec<_>>();
        names.extend(extras);
        names
    }

    pub fn profile(&self, name: &str, catalog: &Catalog) -> Option<Vec<String>> {
        catalog
            .profile(name)
            .cloned()
            .or_else(|| self.custom_profiles.get(name).cloned())
    }

    pub fn is_builtin(name: &str, catalog: &Catalog) -> bool {
        catalog.profiles.contains_key(name)
    }

    pub fn save_profile(
        &mut self,
        name: &str,
        enabled: &[String],
        catalog: &Catalog,
    ) -> Result<()> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 80 {
            bail!("Use um nome de perfil de 1 a 80 caracteres.");
        }
        if name.chars().any(|character| character.is_control()) {
            bail!("O nome do perfil contém caracteres de controle.");
        }
        if Self::is_builtin(name, catalog) {
            bail!("Os perfis do pacote são referências e não podem ser substituídos.");
        }
        catalog.resolve(enabled, None)?;
        if self.profiles_backup_needed {
            self.backup_profiles()?;
        }
        self.custom_profiles
            .insert(name.to_string(), enabled.to_vec());
        let value = ProfilesFile {
            schema_version: 1,
            catalog_version: catalog.package_version.clone(),
            profiles: self
                .custom_profiles
                .iter()
                .map(|(name, enabled)| {
                    (
                        name.clone(),
                        StoredProfile {
                            enabled: enabled.clone(),
                        },
                    )
                })
                .collect(),
        };
        write_json(&self.profiles_path, &value)?;
        self.profiles_backup_needed = false;
        Ok(())
    }

    fn backup_profiles(&self) -> Result<()> {
        if !self.profiles_path.exists() {
            return Ok(());
        }
        let target = self
            .profiles_path
            .with_file_name("profiles.json.antes_rust.bak");
        if !target.exists() {
            fs::copy(&self.profiles_path, &target).with_context(|| {
                format!(
                    "não foi possível preservar o backup de {}",
                    self.profiles_path.display()
                )
            })?;
        }
        Ok(())
    }

    pub fn profile_count(&self) -> usize {
        self.custom_profiles.len()
    }
}

pub fn application_root() -> PathBuf {
    if let Ok(value) = env::var("GOW3_LAUNCHER_HOME") {
        if !value.trim().is_empty() {
            return PathBuf::from(value);
        }
    }
    if cfg!(debug_assertions) {
        return PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    }
    env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// One launcher per data folder. A named Windows mutex is released by the OS
/// even when the process is killed, so it cannot leave a stale lock file.
pub struct InstanceLock {
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        #[cfg(windows)]
        if !self.handle.is_null() {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(windows)]
pub fn acquire_instance_lock(data_root: &Path) -> Result<InstanceLock> {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    let key = data_root.to_string_lossy().to_ascii_lowercase();
    let mut hash = 0xcbf29ce484222325u64;
    for byte in key.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let name = format!("Local\\gow3-launcher-{hash:016X}");
    let wide = name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<u16>>();
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, wide.as_ptr()) };
    if handle.is_null() {
        bail!("não foi possível criar o bloqueio de instância do launcher");
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(handle);
        }
        bail!("Já existe outro launcher aberto nesta pasta de dados.");
    }
    Ok(InstanceLock { handle })
}

#[cfg(not(windows))]
pub fn acquire_instance_lock(_data_root: &Path) -> Result<InstanceLock> {
    Ok(InstanceLock {})
}

pub fn data_directory(root: &Path) -> PathBuf {
    if let Ok(value) = env::var("GOW3_LAUNCHER_DATA") {
        if !value.trim().is_empty() {
            return PathBuf::from(value);
        }
    }
    root.join("data")
}

pub fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("destino sem pasta pai: {}", path.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("não foi possível criar {}", parent.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("dados");
    let temporary = parent.join(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = fs::File::create(&temporary)
            .with_context(|| format!("não foi possível criar {}", temporary.display()))?;
        file.write_all(data)
            .with_context(|| format!("não foi possível gravar {}", temporary.display()))?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
            .with_context(|| format!("não foi possível substituir {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() && temporary.exists() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub fn read_json(path: &Path) -> Result<serde_json::Value> {
    let bytes =
        fs::read(path).with_context(|| format!("não foi possível ler {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("JSON inválido em {}", path.display()))
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    atomic_write(path, &bytes)
}

pub fn ensure_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("não foi possível criar {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let path = env::temp_dir().join(format!(
            "gow3-launcher-test-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn settings_round_trip_keeps_launcher_extensions() {
        let dir = temp_dir("settings");
        let catalog = Catalog::load_embedded().unwrap();
        let mut store = Store::load(dir.clone(), &catalog).unwrap();
        let settings = Settings {
            base: "C:\\jogo".into(),
            update: "C:\\jogo-patch".into(),
            executable: "C:\\shadps4.exe".into(),
            profile: "Meu perfil".into(),
            desired_enabled: Some(vec!["res_720".into(), "fps_120".into()]),
            capture_enabled: true,
            vblank_override: Some(60),
            launch_mode: Some(2),
            gpu_mode: 3,
            lazy_cache: true,
            disable_animations: true,
            fluid_background: false,
            ..Settings::default()
        };
        store.save_settings(&settings).unwrap();
        let loaded = store.settings().unwrap();
        assert_eq!(loaded.base, settings.base);
        assert_eq!(loaded.desired_enabled, settings.desired_enabled);
        assert_eq!(loaded.vblank_override, Some(60));
        assert_eq!(loaded.launch_mode, Some(2));
        assert_eq!(loaded.gpu_mode, 3);
        assert!(loaded.lazy_cache && loaded.disable_animations);
        assert!(!loaded.fluid_background);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn old_preferences_keep_selections_and_default_new_features_off() {
        let settings: Settings = serde_json::from_str(r#"{"profile":"Meu perfil","desired_enabled":["bloom_native_off"],"launch_mode":1}"#).unwrap();
        assert_eq!(settings.desired_enabled, Some(vec!["bloom_native_off".into()]));
        assert_eq!(settings.gpu_mode, 0);
        assert!(!settings.lazy_cache && !settings.disable_animations);
        assert!(!settings.fluid_background, "the fluid ships off by default");
    }

    #[test]
    fn version_one_settings_turn_the_fluid_off() {
        let dir = temp_dir("fluid-migration");
        let catalog = Catalog::load_embedded().unwrap();
        let store = Store::load(dir.clone(), &catalog).unwrap();
        fs::write(
            &store.settings_path,
            r#"{"schema_version":1,"fluid_background":true}"#,
        )
        .unwrap();
        let settings = store.settings().unwrap();
        assert!(!settings.fluid_background, "v1 predates the preference and ships off");
        assert_eq!(settings.schema_version, 3);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn version_two_settings_stop_using_a_discovered_builds_folder() {
        let dir = temp_dir("builds-migration");
        let catalog = Catalog::load_embedded().unwrap();
        let store = Store::load(dir.clone(), &catalog).unwrap();
        fs::write(
            &store.settings_path,
            r#"{"schema_version":2,"builds_dir":"C:\\shadPS4_clean\\builds","base":"C:\\Jogo"}"#,
        )
        .unwrap();
        let settings = store.settings().unwrap();
        assert_eq!(settings.schema_version, 3);
        assert_eq!(settings.base, r"C:\Jogo", "os caminhos do jogo continuam");
        let mut store = store;
        store.save_settings(&settings).unwrap();
        let written = fs::read_to_string(&store.settings_path).unwrap();
        assert!(
            !written.contains("builds_dir"),
            "a pasta é criada pelo launcher e não é mais salva"
        );
        assert!(
            !written.contains("repository"),
            "o repositório é fixo no código, não vai para as preferências"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn custom_profiles_are_saved_and_builtins_cannot_be_overwritten() {
        let dir = temp_dir("profiles");
        let catalog = Catalog::load_embedded().unwrap();
        let mut store = Store::load(dir.clone(), &catalog).unwrap();
        let selection = vec!["res_720".to_string(), "skip_intro".to_string()];
        store
            .save_profile("Meu teste", &selection, &catalog)
            .unwrap();
        assert_eq!(store.profile("Meu teste", &catalog).unwrap(), selection);
        let reloaded = Store::load(dir.clone(), &catalog).unwrap();
        assert_eq!(reloaded.profile("Meu teste", &catalog).unwrap(), selection);
        assert!(store.save_profile("Base60", &selection, &catalog).is_err());
        assert!(store.save_profile("  ", &selection, &catalog).is_err());
        assert!(store
            .save_profile(
                "Sobreposição",
                &["res_720".into(), "res_4k".into()],
                &catalog
            )
            .is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn corrupt_profiles_are_preserved_before_new_writes() {
        let dir = temp_dir("corrupt");
        fs::write(dir.join("profiles.json"), b"{ nao e json").unwrap();
        let catalog = Catalog::load_embedded().unwrap();
        let mut store = Store::load(dir.clone(), &catalog).unwrap();
        assert!(store.profiles_backup_needed);
        assert!(store
            .notices
            .iter()
            .any(|notice| notice.contains("preservado")));
        store
            .save_profile("Novo", &["res_720".into()], &catalog)
            .unwrap();
        assert!(dir.join("profiles.json.antes_rust.bak").exists());
        assert_eq!(
            fs::read(dir.join("profiles.json.antes_rust.bak")).unwrap(),
            b"{ nao e json"
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
