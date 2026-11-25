//! Per-game ShadPS4 settings for the GoW3 launcher.
//!
//! Everything here targets `ambiente/user/custom_configs/CUSA01623.json`, the
//! game-specific file the core reads on top of `config.json`. The launcher's own
//! preferences stay in `settings.json`; this module never touches them.
//!
//! The UI only sends a field identifier plus the chosen value. Loading, range and
//! enum validation, unknown-key preservation and atomic saving all happen here.
//! Defaults mirror the supported core build (`emulator_settings.h`); a default is
//! never written back unless the user actually changes that field.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};

use crate::storage::{read_json, write_json};

/// Console languages accepted by the core, in the order the original dialog shows
/// them. The second value is the code stored in `General.console_language`.
pub const LANGUAGES: [(&str, i64); 31] = [
    ("Árabe", 21),
    ("Tcheco", 23),
    ("Dinamarquês", 14),
    ("Holandês", 6),
    ("Inglês (Reino Unido)", 18),
    ("Inglês (Estados Unidos)", 1),
    ("Finlandês", 12),
    ("Francês (Canadá)", 22),
    ("Francês (França)", 2),
    ("Alemão", 4),
    ("Grego", 25),
    ("Húngaro", 24),
    ("Indonésio", 29),
    ("Italiano", 5),
    ("Japonês", 0),
    ("Coreano", 9),
    ("Norueguês (Bokmål)", 15),
    ("Polonês", 16),
    ("Português (Brasil)", 17),
    ("Português (Portugal)", 7),
    ("Romeno", 26),
    ("Russo", 8),
    ("Chinês simplificado", 11),
    ("Espanhol (América Latina)", 20),
    ("Espanhol (Espanha)", 3),
    ("Sueco", 13),
    ("Tailandês", 27),
    ("Chinês tradicional", 10),
    ("Turco", 19),
    ("Ucraniano", 30),
    ("Vietnamita", 28),
];

pub const AUDIO_BACKENDS: [&str; 2] = ["SDL", "OpenAL"];
pub const DISPLAY_MODES: [&str; 3] = ["Janela", "Tela cheia", "Tela cheia sem bordas"];
pub const PRESENT_MODES: [&str; 3] = ["Mailbox", "Fifo", "Immediate"];
/// Display label and the value the core expects. PSMAA and CMAA2 are 3 and 4.
pub const ANTI_ALIASING: [(&str, i64); 4] = [
    ("Nenhum", 0),
    ("FSR1", 1),
    ("PSMAA", 3),
    ("CMAA2", 4),
];
pub const UPSCALERS: [&str; 2] = ["Nenhum", "FSR1"];
pub const SHARPENING: [&str; 2] = ["Nenhum", "RCAS"];
pub const CURSOR_STATES: [&str; 3] = ["Nunca", "Ocioso", "Sempre"];
pub const READBACKS: [&str; 3] = ["Desativado", "Relaxado", "Preciso"];
pub const RED_ZONE: [&str; 2] = ["Desativado", "Static Patching"];

pub const CONSOLE_LANGUAGE: &str = "General.console_language";
pub const VOLUME: &str = "General.volume_slider";
pub const SHOW_SPLASH: &str = "General.show_splash";
pub const AUDIO_BACKEND: &str = "Audio.audio_backend";
pub const DISPLAY_MODE: &str = "GPU.display_mode";
pub const PRESENT_MODE: &str = "GPU.present_mode";
pub const WINDOW_WIDTH: &str = "GPU.window_width";
pub const WINDOW_HEIGHT: &str = "GPU.window_height";
pub const HDR: &str = "GPU.hdr_allowed";
pub const ANTI_ALIASING_FIELD: &str = "GPU.anti_aliasing";
pub const UPSCALER: &str = "GPU.upscaler";
pub const SHARPENING_FIELD: &str = "GPU.sharpening";
pub const RCAS: &str = "GPU.rcas_attenuation";
pub const MOTION_CONTROLS: &str = "Input.motion_controls_enabled";
pub const BACKGROUND_CONTROLLER: &str = "Input.background_controller_input";
pub const CURSOR_STATE: &str = "Input.cursor_state";
pub const CURSOR_TIMEOUT: &str = "Input.cursor_hide_timeout";
pub const EXTRA_DMEM: &str = "General.extra_dmem_in_mbytes";
pub const APP0_BANDWIDTH: &str = "General.app0_read_bandwidth_mibps";
pub const APP0_TIME_STRETCHING: &str = "General.app0_read_disable_time_stretching";
pub const READBACKS_FIELD: &str = "GPU.readbacks_mode";
pub const READBACK_LINEAR: &str = "GPU.readback_linear_images_enabled";
pub const DIRECT_MEMORY: &str = "GPU.direct_memory_access_enabled";
pub const NV_RAW_ACCESS_CHAINS: &str = "Vulkan.use_nv_raw_access_chains";
pub const FORCE_UNIFORM_BUFFERS: &str = "Vulkan.force_uniform_buffers";
pub const RED_ZONE_FIELD: &str = "WindowsGuestRedZoneProtection.windows_guest_red_zone_protection_mode";
pub const DEVKIT: &str = "General.dev_kit_mode";
pub const NEO: &str = "General.neo_mode";
pub const SHADNET: &str = "General.shad_net_enabled";
pub const NETWORK_CONNECTED: &str = "General.connected_to_network";
pub const PIPELINE_CACHE: &str = "Vulkan.pipeline_cache_enabled";
pub const PIPELINE_CACHE_ARCHIVED: &str = "Vulkan.pipeline_cache_archived";

/// One user edit, as sent by the UI. Numeric fields arrive as text so the
/// launcher can parse, clamp and explain an invalid entry.
#[derive(Debug, Clone, Copy)]
pub enum Change<'a> {
    Toggle(bool),
    Index(i64),
    Text(&'a str),
}

/// Resolved values for every managed field. Missing keys fall back to the
/// supported build's defaults without being written back.
#[derive(Debug, Clone, PartialEq)]
pub struct Values {
    pub console_language: i64,
    pub volume: i64,
    pub show_splash: bool,
    pub audio_backend: i64,
    /// 0 windowed, 1 full screen, 2 borderless. Reflects `full_screen` plus
    /// `full_screen_mode` together, which is what the core reads.
    pub display_mode: i64,
    pub present_mode: i64,
    pub window_width: i64,
    pub window_height: i64,
    pub hdr: bool,
    pub anti_aliasing: i64,
    pub upscaler: i64,
    pub sharpening: i64,
    /// Stored attenuation divided by 1000, as the original dialog shows it.
    pub rcas_attenuation: f64,
    pub motion_controls: bool,
    pub background_controller: bool,
    pub cursor_state: i64,
    pub cursor_hide_timeout: i64,
    pub extra_dmem: i64,
    pub app0_bandwidth: i64,
    pub app0_disable_time_stretching: bool,
    pub readbacks_mode: i64,
    pub readback_linear_images: bool,
    pub direct_memory_access: bool,
    pub nv_raw_access_chains: bool,
    pub force_uniform_buffers: bool,
    pub red_zone_protection: i64,
    pub dev_kit_mode: bool,
    pub neo_mode: bool,
    pub shad_net: bool,
    pub connected_to_network: bool,
    pub pipeline_cache_enabled: bool,
    pub pipeline_cache_archived: bool,
}

impl Default for Values {
    fn default() -> Self {
        Self {
            console_language: 1,
            volume: 100,
            show_splash: false,
            audio_backend: 0,
            display_mode: 0,
            present_mode: 0,
            window_width: 1280,
            window_height: 720,
            hdr: false,
            anti_aliasing: 0,
            upscaler: 0,
            sharpening: 0,
            rcas_attenuation: 0.25,
            motion_controls: true,
            background_controller: false,
            cursor_state: 1,
            cursor_hide_timeout: 5,
            extra_dmem: 0,
            app0_bandwidth: 0,
            app0_disable_time_stretching: false,
            readbacks_mode: 0,
            readback_linear_images: false,
            direct_memory_access: false,
            nv_raw_access_chains: false,
            force_uniform_buffers: false,
            red_zone_protection: 0,
            dev_kit_mode: false,
            neo_mode: false,
            shad_net: false,
            connected_to_network: false,
            pipeline_cache_enabled: false,
            pipeline_cache_archived: false,
        }
    }
}

impl Values {
    fn from_document(document: &Value) -> Self {
        let defaults = Self::default();
        let language = int(document, "General", "console_language", defaults.console_language);
        let full_screen = boolean(document, "GPU", "full_screen", false);
        let full_screen_mode = text(document, "GPU", "full_screen_mode", "Windowed");
        let display_mode = if !full_screen {
            0
        } else if full_screen_mode.eq_ignore_ascii_case("Windowed") {
            0
        } else if full_screen_mode.to_ascii_lowercase().contains("borderless") {
            2
        } else {
            1
        };
        Self {
            console_language: language,
            volume: int(document, "General", "volume_slider", defaults.volume),
            show_splash: boolean(document, "General", "show_splash", defaults.show_splash),
            audio_backend: int(document, "Audio", "audio_backend", defaults.audio_backend),
            display_mode,
            present_mode: present_index(text(
                document,
                "GPU",
                "present_mode",
                PRESENT_MODES[0],
            )),
            window_width: int(
                document,
                "GPU",
                "window_width",
                defaults.window_width,
            ),
            window_height: int(
                document,
                "GPU",
                "window_height",
                defaults.window_height,
            ),
            hdr: boolean(document, "GPU", "hdr_allowed", defaults.hdr),
            anti_aliasing: anti_aliasing_index(int(document, "GPU", "anti_aliasing", 0)),
            upscaler: int(document, "GPU", "upscaler", defaults.upscaler),
            sharpening: int(document, "GPU", "sharpening", defaults.sharpening),
            rcas_attenuation: int(document, "GPU", "rcas_attenuation", 250) as f64 / 1000.0,
            motion_controls: boolean(
                document,
                "Input",
                "motion_controls_enabled",
                defaults.motion_controls,
            ),
            background_controller: boolean(
                document,
                "Input",
                "background_controller_input",
                defaults.background_controller,
            ),
            cursor_state: int(document, "Input", "cursor_state", defaults.cursor_state)
                .clamp(0, 2),
            cursor_hide_timeout: int(
                document,
                "Input",
                "cursor_hide_timeout",
                defaults.cursor_hide_timeout,
            ),
            extra_dmem: int(
                document,
                "General",
                "extra_dmem_in_mbytes",
                defaults.extra_dmem,
            ),
            app0_bandwidth: int(
                document,
                "General",
                "app0_read_bandwidth_mibps",
                defaults.app0_bandwidth,
            ),
            app0_disable_time_stretching: boolean(
                document,
                "General",
                "app0_read_disable_time_stretching",
                defaults.app0_disable_time_stretching,
            ),
            readbacks_mode: int(
                document,
                "GPU",
                "readbacks_mode",
                defaults.readbacks_mode,
            )
            .clamp(0, 2),
            readback_linear_images: boolean(
                document,
                "GPU",
                "readback_linear_images_enabled",
                defaults.readback_linear_images,
            ),
            direct_memory_access: boolean(
                document,
                "GPU",
                "direct_memory_access_enabled",
                defaults.direct_memory_access,
            ),
            nv_raw_access_chains: boolean(
                document,
                "Vulkan",
                "use_nv_raw_access_chains",
                defaults.nv_raw_access_chains,
            ),
            force_uniform_buffers: boolean(
                document,
                "Vulkan",
                "force_uniform_buffers",
                defaults.force_uniform_buffers,
            ),
            red_zone_protection: int(
                document,
                "WindowsGuestRedZoneProtection",
                "windows_guest_red_zone_protection_mode",
                defaults.red_zone_protection,
            )
            .clamp(0, 1),
            dev_kit_mode: boolean(document, "General", "dev_kit_mode", defaults.dev_kit_mode),
            neo_mode: boolean(document, "General", "neo_mode", defaults.neo_mode),
            shad_net: boolean(document, "General", "shad_net_enabled", defaults.shad_net),
            connected_to_network: boolean(
                document,
                "General",
                "connected_to_network",
                defaults.connected_to_network,
            ),
            pipeline_cache_enabled: boolean(
                document,
                "Vulkan",
                "pipeline_cache_enabled",
                defaults.pipeline_cache_enabled,
            ),
            pipeline_cache_archived: boolean(
                document,
                "Vulkan",
                "pipeline_cache_archived",
                defaults.pipeline_cache_archived,
            ),
        }
    }
}

/// The game-specific configuration file plus its resolved values.
pub struct EmulatorConfig {
    path: PathBuf,
    document: Value,
    pub values: Values,
    /// Set when the file exists but cannot be used. Saving is refused so the
    /// original is never replaced by defaults.
    pub error: Option<String>,
}

impl EmulatorConfig {
    pub fn load(path: PathBuf) -> Self {
        let mut config = Self {
            path,
            document: Value::Object(Map::new()),
            values: Values::default(),
            error: None,
        };
        if !config.path.exists() {
            return config;
        }
        match read_json(&config.path) {
            Ok(value) if value.is_object() => {
                config.values = Values::from_document(&value);
                config.document = value;
            }
            Ok(_) => {
                config.error = Some(
                    "A configuração específica não é um objeto JSON. O arquivo foi preservado."
                        .into(),
                );
            }
            Err(error) => {
                config.error = Some(format!(
                    "Configuração específica inválida. O arquivo foi preservado. {error}"
                ));
            }
        }
        config
    }

    /// O arquivo existe no disco? Sem ele o idioma do console é desconhecido.
    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    /// Re-read the file after the session journal restored its temporary values.
    pub fn reload(&mut self) {
        let path = self.path.clone();
        *self = Self::load(path);
    }

    pub fn apply(&mut self, field: &str, change: Change<'_>) -> Result<()> {
        if let Some(error) = &self.error {
            bail!("{error}");
        }
        let mut values = self.values.clone();
        let mut entries: Vec<(&'static str, &'static str, Value)> = Vec::new();
        match (field, change) {
            (CONSOLE_LANGUAGE, Change::Index(index)) => {
                let (_, code) = LANGUAGES
                    .get(index.max(0) as usize)
                    .context("Idioma do console inválido.")?;
                values.console_language = *code;
                entries.push(("General", "console_language", Value::from(*code)));
            }
            (VOLUME, Change::Text(text)) => {
                let value = parse_int(text, "Volume", 0, 500)?;
                values.volume = value;
                entries.push(("General", "volume_slider", Value::from(value)));
            }
            (SHOW_SPLASH, Change::Toggle(enabled)) => {
                values.show_splash = enabled;
                entries.push(("General", "show_splash", Value::from(enabled)));
            }
            (AUDIO_BACKEND, Change::Index(index)) => {
                let index = index.clamp(0, AUDIO_BACKENDS.len() as i64 - 1);
                values.audio_backend = index;
                entries.push(("Audio", "audio_backend", Value::from(index)));
            }
            (DISPLAY_MODE, Change::Index(index)) => {
                let index = index.clamp(0, 2);
                let (full_screen, mode) = match index {
                    0 => (false, "Windowed"),
                    1 => (true, "Fullscreen"),
                    _ => (true, "Fullscreen (Borderless)"),
                };
                values.display_mode = index;
                entries.push(("GPU", "full_screen", Value::from(full_screen)));
                entries.push(("GPU", "full_screen_mode", Value::from(mode)));
            }
            (PRESENT_MODE, Change::Index(index)) => {
                let index = index.clamp(0, PRESENT_MODES.len() as i64 - 1);
                values.present_mode = index;
                entries.push((
                    "GPU",
                    "present_mode",
                    Value::from(PRESENT_MODES[index as usize]),
                ));
            }
            (WINDOW_WIDTH, Change::Text(text)) => {
                let value = parse_int(text, "Largura da janela", 0, 8000)?;
                values.window_width = value;
                entries.push(("GPU", "window_width", Value::from(value)));
            }
            (WINDOW_HEIGHT, Change::Text(text)) => {
                let value = parse_int(text, "Altura da janela", 0, 7000)?;
                values.window_height = value;
                entries.push(("GPU", "window_height", Value::from(value)));
            }
            (HDR, Change::Toggle(enabled)) => {
                values.hdr = enabled;
                entries.push(("GPU", "hdr_allowed", Value::from(enabled)));
            }
            (ANTI_ALIASING_FIELD, Change::Index(index)) => {
                let index = index.clamp(0, ANTI_ALIASING.len() as i64 - 1);
                let (_, raw) = ANTI_ALIASING[index as usize];
                values.anti_aliasing = index;
                entries.push(("GPU", "anti_aliasing", Value::from(raw)));
            }
            (UPSCALER, Change::Index(index)) => {
                let index = index.clamp(0, 1);
                values.upscaler = index;
                entries.push(("GPU", "upscaler", Value::from(index)));
            }
            (SHARPENING_FIELD, Change::Index(index)) => {
                let index = index.clamp(0, 1);
                values.sharpening = index;
                entries.push(("GPU", "sharpening", Value::from(index)));
            }
            (RCAS, Change::Text(text)) => {
                let value = parse_float(text, "Intensidade RCAS", 0.0, 3.0)?;
                values.rcas_attenuation = value;
                entries.push((
                    "GPU",
                    "rcas_attenuation",
                    Value::from((value * 1000.0).round() as i64),
                ));
            }
            (MOTION_CONTROLS, Change::Toggle(enabled)) => {
                values.motion_controls = enabled;
                entries.push(("Input", "motion_controls_enabled", Value::from(enabled)));
            }
            (BACKGROUND_CONTROLLER, Change::Toggle(enabled)) => {
                values.background_controller = enabled;
                entries.push(("Input", "background_controller_input", Value::from(enabled)));
            }
            (CURSOR_STATE, Change::Index(index)) => {
                let index = index.clamp(0, 2);
                values.cursor_state = index;
                entries.push(("Input", "cursor_state", Value::from(index)));
            }
            (CURSOR_TIMEOUT, Change::Text(text)) => {
                let value = parse_int(text, "Tempo para ocultar o cursor", 1, 10)?;
                values.cursor_hide_timeout = value;
                entries.push(("Input", "cursor_hide_timeout", Value::from(value)));
            }
            (EXTRA_DMEM, Change::Text(text)) => {
                let value = parse_int(text, "DMEM adicional", 0, 20000)?;
                values.extra_dmem = value;
                entries.push(("General", "extra_dmem_in_mbytes", Value::from(value)));
            }
            (APP0_BANDWIDTH, Change::Text(text)) => {
                let requested = parse_int(text, "Banda de leitura do HDD", 0, 1000)?;
                let normalized = normalize_bandwidth(requested);
                values.app0_bandwidth = normalized;
                entries.push((
                    "General",
                    "app0_read_bandwidth_mibps",
                    Value::from(normalized),
                ));
            }
            (APP0_TIME_STRETCHING, Change::Toggle(enabled)) => {
                if enabled && normalize_bandwidth(values.app0_bandwidth) == 0 {
                    bail!("O time stretching só pode ser desativado com uma banda de leitura limitada.");
                }
                values.app0_disable_time_stretching = enabled;
                entries.push((
                    "General",
                    "app0_read_disable_time_stretching",
                    Value::from(enabled),
                ));
            }
            (READBACKS_FIELD, Change::Index(index)) => {
                let index = index.clamp(0, 2);
                values.readbacks_mode = index;
                entries.push(("GPU", "readbacks_mode", Value::from(index)));
            }
            (READBACK_LINEAR, Change::Toggle(enabled)) => {
                values.readback_linear_images = enabled;
                entries.push((
                    "GPU",
                    "readback_linear_images_enabled",
                    Value::from(enabled),
                ));
            }
            (DIRECT_MEMORY, Change::Toggle(enabled)) => {
                values.direct_memory_access = enabled;
                entries.push((
                    "GPU",
                    "direct_memory_access_enabled",
                    Value::from(enabled),
                ));
            }
            (NV_RAW_ACCESS_CHAINS, Change::Toggle(enabled)) => {
                values.nv_raw_access_chains = enabled;
                entries.push(("Vulkan", "use_nv_raw_access_chains", Value::from(enabled)));
            }
            (FORCE_UNIFORM_BUFFERS, Change::Toggle(enabled)) => {
                values.force_uniform_buffers = enabled;
                entries.push(("Vulkan", "force_uniform_buffers", Value::from(enabled)));
            }
            (RED_ZONE_FIELD, Change::Index(index)) => {
                let index = index.clamp(0, 1);
                values.red_zone_protection = index;
                entries.push((
                    "WindowsGuestRedZoneProtection",
                    "windows_guest_red_zone_protection_mode",
                    Value::from(index),
                ));
            }
            (DEVKIT, Change::Toggle(enabled)) => {
                values.dev_kit_mode = enabled;
                entries.push(("General", "dev_kit_mode", Value::from(enabled)));
            }
            (NEO, Change::Toggle(enabled)) => {
                values.neo_mode = enabled;
                entries.push(("General", "neo_mode", Value::from(enabled)));
            }
            (SHADNET, Change::Toggle(enabled)) => {
                values.shad_net = enabled;
                entries.push(("General", "shad_net_enabled", Value::from(enabled)));
            }
            (NETWORK_CONNECTED, Change::Toggle(enabled)) => {
                values.connected_to_network = enabled;
                entries.push(("General", "connected_to_network", Value::from(enabled)));
            }
            (PIPELINE_CACHE, Change::Toggle(enabled)) => {
                values.pipeline_cache_enabled = enabled;
                entries.push(("Vulkan", "pipeline_cache_enabled", Value::from(enabled)));
            }
            (PIPELINE_CACHE_ARCHIVED, Change::Toggle(enabled)) => {
                if enabled && !values.pipeline_cache_enabled {
                    bail!("Habilite o cache de shaders antes de compactá-lo.");
                }
                values.pipeline_cache_archived = enabled;
                entries.push(("Vulkan", "pipeline_cache_archived", Value::from(enabled)));
            }
            _ => bail!("Opção de configuração desconhecida: {field}."),
        }
        self.write_entries(&entries)?;
        self.values = values;
        Ok(())
    }

    /// Writes only the touched keys, so unknown sections and keys survive and no
    /// default is recorded as an explicit choice.
    fn write_entries(&mut self, entries: &[(&'static str, &'static str, Value)]) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        for (section, _, _) in entries {
            match self.document.get(*section) {
                Some(Value::Object(_)) | None => {}
                Some(_) => bail!(
                    "A seção {section} da configuração não é um objeto JSON. O arquivo foi preservado."
                ),
            }
        }
        let document = self
            .document
            .as_object_mut()
            .context("Configuração inválida. O arquivo foi preservado.")?;
        for (section, key, value) in entries {
            let target = document
                .entry((*section).to_string())
                .or_insert_with(|| Value::Object(Map::new()));
            target
                .as_object_mut()
                .expect("seções são objetos validados acima")
                .insert((*key).to_string(), value.clone());
        }
        write_json(&self.path, &self.document)
    }
}

/// Mirrors `Core::FileSys::NormalizeReadBandwidth`: zero or above 200 means
/// unlimited, and anything between 1 and 49 becomes the 50 MiB/s minimum.
pub fn normalize_bandwidth(requested: i64) -> i64 {
    if requested <= 0 || requested > 200 {
        0
    } else {
        requested.max(50)
    }
}

/// Display index of a stored console language code.
pub fn language_index(code: i64) -> i64 {
    LANGUAGES
        .iter()
        .position(|(_, value)| *value == code)
        .map(|index| index as i64)
        .unwrap_or(0)
}

fn present_index(mode: &str) -> i64 {
    PRESENT_MODES
        .iter()
        .position(|label| label.eq_ignore_ascii_case(mode))
        .map(|index| index as i64)
        .unwrap_or(0)
}

fn anti_aliasing_index(value: i64) -> i64 {
    ANTI_ALIASING
        .iter()
        .position(|(_, raw)| *raw == value)
        .map(|index| index as i64)
        .unwrap_or(0)
}

fn parse_int(text: &str, label: &str, min: i64, max: i64) -> Result<i64> {
    let value: i64 = text
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("{label}: use um número inteiro."))?;
    if !(min..=max).contains(&value) {
        bail!("{label} deve ficar entre {min} e {max}.");
    }
    Ok(value)
}

fn parse_float(text: &str, label: &str, min: f64, max: f64) -> Result<f64> {
    let normalized = text.trim().replace(',', ".");
    let value: f64 = normalized
        .parse()
        .map_err(|_| anyhow::anyhow!("{label}: use um número entre {min} e {max}."))?;
    if !value.is_finite() || !(min..=max).contains(&value) {
        bail!("{label} deve ficar entre {min} e {max}.");
    }
    Ok(value)
}

fn section<'a>(document: &'a Value, name: &str) -> Option<&'a Map<String, Value>> {
    document.get(name).and_then(Value::as_object)
}

fn int(document: &Value, section_name: &str, key: &str, default: i64) -> i64 {
    section(document, section_name)
        .and_then(|values| values.get(key))
        .and_then(Value::as_i64)
        .unwrap_or(default)
}

fn boolean(document: &Value, section_name: &str, key: &str, default: bool) -> bool {
    section(document, section_name)
        .and_then(|values| values.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(default)
}

fn text<'a>(document: &'a Value, section_name: &str, key: &str, default: &'a str) -> &'a str {
    section(document, section_name)
        .and_then(|values| values.get(key))
        .and_then(Value::as_str)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_path(label: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "gow3-emulator-config-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&directory).unwrap();
        directory.join("CUSA01623.json")
    }

    #[test]
    fn only_changed_fields_are_written() {
        let path = temp_path("write");
        let mut config = EmulatorConfig::load(path.clone());
        assert_eq!(config.values.volume, 100, "the build default is shown");
        config.apply(VOLUME, Change::Text("250")).unwrap();
        let stored: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(stored["General"]["volume_slider"], Value::from(250));
        assert!(
            stored["General"].get("show_splash").is_none(),
            "untouched defaults must not be recorded as explicit choices"
        );
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn unknown_keys_and_sections_survive_an_edit() {
        let path = temp_path("preserve");
        fs::write(
            &path,
            r#"{"General":{"volume_slider":10,"jogo_especifico":"x"},"Outra":{"k":1}}"#,
        )
        .unwrap();
        let mut config = EmulatorConfig::load(path.clone());
        config.apply(VOLUME, Change::Text("300")).unwrap();
        let stored: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(stored["General"]["volume_slider"], Value::from(300));
        assert_eq!(stored["General"]["jogo_especifico"], Value::from("x"));
        assert_eq!(stored["Outra"]["k"], Value::from(1));
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn invalid_configuration_is_preserved_and_never_overwritten() {
        let path = temp_path("invalid");
        fs::write(&path, b"{ nao e json").unwrap();
        let mut config = EmulatorConfig::load(path.clone());
        assert!(config.error.is_some());
        assert!(config.apply(VOLUME, Change::Text("120")).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"{ nao e json");
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn display_mode_keeps_full_screen_and_mode_in_step() {
        let path = temp_path("display");
        let mut config = EmulatorConfig::load(path.clone());
        config.apply(DISPLAY_MODE, Change::Index(2)).unwrap();
        let stored: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(stored["GPU"]["full_screen"], Value::Bool(true));
        assert_eq!(stored["GPU"]["full_screen_mode"], Value::from("Fullscreen (Borderless)"));
        let reloaded = EmulatorConfig::load(path.clone());
        assert_eq!(reloaded.values.display_mode, 2);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn rcas_is_stored_multiplied_by_a_thousand() {
        let path = temp_path("rcas");
        let mut config = EmulatorConfig::load(path.clone());
        config.apply(RCAS, Change::Text("1,5")).unwrap();
        let stored: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(stored["GPU"]["rcas_attenuation"], Value::from(1500));
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn out_of_range_values_and_known_enums_are_validated() {
        let path = temp_path("ranges");
        let mut config = EmulatorConfig::load(path.clone());
        assert!(config.apply(VOLUME, Change::Text("900")).is_err());
        assert!(config.apply(WINDOW_WIDTH, Change::Text("9000")).is_err());
        assert!(config.apply(CURSOR_TIMEOUT, Change::Text("0")).is_err());
        config.apply(APP0_BANDWIDTH, Change::Text("10")).unwrap();
        assert_eq!(config.values.app0_bandwidth, 50, "1-49 normalises to 50");
        config.apply(APP0_BANDWIDTH, Change::Text("0")).unwrap();
        assert_eq!(config.values.app0_bandwidth, 0);
        assert!(config
            .apply(APP0_TIME_STRETCHING, Change::Toggle(true))
            .is_err());
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
