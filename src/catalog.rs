use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::storage::atomic_write;

const CATALOG_JSON: &str = include_str!("../assets/catalogo.json");
const MANIFEST_JSON: &str = include_str!("../assets/manifesto.json");
const TEMPLATE_XML: &str = include_str!("../assets/patch_template.xml");

pub const OPTION_COUNT: usize = 27;

#[derive(Debug, Clone, Deserialize)]
pub struct OptionDefinition {
    pub id: String,
    pub source: String,
    pub label: String,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub min_dmem: u64,
    #[serde(default)]
    pub vblank: Option<u32>,
    #[serde(default)]
    pub experimental: bool,
    #[serde(default)]
    pub gameplay_validated: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CatalogDocument {
    pub schema_version: u32,
    pub package_version: String,
    pub manifest_sha256: String,
    pub options: Vec<OptionDefinition>,
    #[serde(default)]
    pub application_mode: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PatchLine {
    pub address: String,
    pub value: String,
    pub original: String,
    pub file_offset: usize,
    #[serde(default)]
    pub segment_padding: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PatchDefinition {
    pub name: String,
    pub lines: Vec<PatchLine>,
    #[serde(default)]
    pub required_executable_sha256: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProfileDefinition {
    pub file: String,
    pub sha256: String,
    pub enabled: Vec<String>,
    pub vblank: u32,
    #[serde(default)]
    pub vblank_override: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManifestDocument {
    pub title_id: String,
    pub app_version: String,
    pub self_sha256: String,
    pub self_length: u64,
    pub patches: Vec<PatchDefinition>,
    pub profiles: BTreeMap<String, ProfileDefinition>,
}

#[derive(Debug, Clone)]
pub struct Catalog {
    pub package_version: String,
    pub title_id: String,
    pub app_version: String,
    pub self_sha256: String,
    pub self_length: u64,
    pub options: Vec<OptionDefinition>,
    pub by_id: HashMap<String, OptionDefinition>,
    pub by_source: HashMap<String, String>,
    pub patches: HashMap<String, PatchDefinition>,
    pub profiles: BTreeMap<String, Vec<String>>,
    pub profile_overrides: BTreeMap<String, Option<u32>>,
    pub profile_order: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSelection {
    pub enabled: Vec<String>,
    pub vblank: u32,
    pub min_dmem: u64,
    pub checked_lines: usize,
    pub vblank_override: Option<u32>,
}

fn sha256_hex(data: &[u8]) -> String {
    format!("{:X}", Sha256::digest(data))
}

impl Catalog {
    pub fn load_embedded() -> Result<Self> {
        let document: CatalogDocument =
            serde_json::from_str(CATALOG_JSON).context("catálogo embutido inválido")?;
        if document.schema_version != 1 {
            bail!("versão de catálogo não suportada");
        }
        let raw_manifest_bytes = MANIFEST_JSON.as_bytes();
        if sha256_hex(raw_manifest_bytes) != document.manifest_sha256.to_uppercase() {
            bail!("o manifesto embutido não corresponde ao catálogo");
        }
        let manifest: ManifestDocument =
            serde_json::from_str(MANIFEST_JSON).context("manifesto embutido inválido")?;

        if document.options.len() != OPTION_COUNT {
            bail!("o catálogo deve conter {OPTION_COUNT} opções");
        }
        if manifest.patches.len() != OPTION_COUNT {
            bail!("o manifesto deve conter {OPTION_COUNT} patches");
        }

        let template_sha = sha256_hex(TEMPLATE_XML.as_bytes());
        let base60 = manifest
            .profiles
            .get("Base60")
            .context("perfil Base60 ausente no manifesto")?;
        if template_sha != base60.sha256.to_uppercase() {
            bail!("o XML base embutido não corresponde ao manifesto");
        }

        let mut options = Vec::with_capacity(document.options.len());
        let mut by_id = HashMap::new();
        let mut by_source = HashMap::new();
        for option in document.options {
            if by_id.contains_key(&option.id) {
                bail!("opção duplicada no catálogo: {}", option.id);
            }
            if by_source.contains_key(&option.source) {
                bail!("fonte duplicada no catálogo: {}", option.source);
            }
            by_id.insert(option.id.clone(), option.clone());
            by_source.insert(option.source.clone(), option.id.clone());
            options.push(option);
        }

        let mut patches = HashMap::new();
        for patch in &manifest.patches {
            let id = by_source
                .get(&patch.name)
                .with_context(|| format!("patch sem opção correspondente: {}", patch.name))?;
            if patches.contains_key(id) {
                bail!("patch duplicado: {}", patch.name);
            }
            for line in &patch.lines {
                let address = u64::from_str_radix(line.address.trim_start_matches("0x"), 16)
                    .with_context(|| format!("endereço inválido em {}", patch.name))?;
                if hex::decode_len(&line.value).is_none()
                    || (!line.segment_padding && hex::decode_len(&line.original).is_none())
                    || (line.segment_padding && (!line.original.is_empty() || line.file_offset != 0))
                {
                    bail!("bytes hexadecimais inválidos em {}", patch.name);
                }
                if address == 0 {
                    bail!("endereço zero em {}", patch.name);
                }
            }
            patches.insert(id.clone(), patch.clone());
        }
        for option in &options {
            if !patches.contains_key(&option.id) {
                bail!("opção sem bytes de patch: {}", option.id);
            }
        }

        let mut profiles = BTreeMap::new();
        let mut profile_overrides = BTreeMap::new();
        for (name, profile) in &manifest.profiles {
            for source in &profile.enabled {
                if !by_source.contains_key(source) {
                    bail!("perfil {name} referencia patch desconhecido: {source}");
                }
            }
            let enabled = profile
                .enabled
                .iter()
                .filter_map(|source| by_source.get(source).cloned())
                .collect::<Vec<_>>();
            profiles.insert(name.clone(), enabled.clone());
            profile_overrides.insert(name.clone(), profile.vblank_override);
            let resolved = resolve_with(&options, &patches, &enabled, profile.vblank_override)?;
            if resolved.vblank != profile.vblank {
                bail!("VBlank inconsistente no perfil {name}");
            }
        }

        const PREFERRED: [&str; 4] = [
            "SemPatches",
            "Base60",
            "Originais120",
            "ReferenciaObservada",
        ];
        let mut profile_order = PREFERRED
            .iter()
            .filter(|name| profiles.contains_key(**name))
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>();
        profile_order.extend(
            profiles
                .keys()
                .filter(|name| !PREFERRED.contains(&name.as_str()))
                .cloned(),
        );

        Ok(Self {
            package_version: document.package_version,
            title_id: manifest.title_id,
            app_version: manifest.app_version,
            self_sha256: manifest.self_sha256,
            self_length: manifest.self_length,
            options,
            by_id,
            by_source,
            patches,
            profiles,
            profile_overrides,
            profile_order,
        })
    }

    pub fn profile_names(&self) -> &[String] {
        &self.profile_order
    }

    pub fn profile(&self, name: &str) -> Option<&Vec<String>> {
        self.profiles.get(name)
    }

    pub fn profile_override(&self, name: &str) -> Option<u32> {
        self.profile_overrides.get(name).copied().flatten()
    }

    pub fn resolve(
        &self,
        enabled: &[String],
        vblank_override: Option<u32>,
    ) -> Result<ResolvedSelection> {
        resolve_with(&self.options, &self.patches, enabled, vblank_override)
    }

    pub fn option_label(&self, id: &str) -> String {
        self.by_id
            .get(id)
            .map(|option| option.label.clone())
            .unwrap_or_else(|| id.to_string())
    }

    /// The reference XMLs are not all embedded, but the base template is
    /// verified against the manifest. Each metadata flag is rewritten in place
    /// so notes, authors and bytes stay byte-for-byte identical to the package.
    pub fn render_patch_xml(&self, enabled: &[String]) -> Result<String> {
        let resolved = self.resolve(enabled, None)?;
        let enabled_ids = resolved.enabled.iter().cloned().collect::<HashSet<_>>();
        let mut output = String::with_capacity(TEMPLATE_XML.len() + 64);
        let mut rest = TEMPLATE_XML;
        let mut seen = HashSet::new();

        while let Some(start) = rest.find("<Metadata") {
            output.push_str(&rest[..start]);
            let after_start = &rest[start..];
            let tag_end = after_start
                .find('>')
                .context("tag Metadata sem fechamento no XML base")?;
            let tag = &after_start[..=tag_end];
            let source = xml_attribute(tag, "Name")
                .context("Metadata sem atributo Name no XML base")?
                .to_string();
            let id = self
                .by_source
                .get(&source)
                .with_context(|| format!("Metadata desconhecido no XML base: {source}"))?;
            let desired = enabled_ids.contains(id);
            output.push_str(&rewrite_enabled_attribute(tag, desired)?);
            seen.insert(source.clone());
            rest = &after_start[tag_end + 1..];
        }
        output.push_str(rest);

        if seen.len() != self.options.len() {
            bail!("o XML base não contém todas as opções esperadas");
        }
        for option in &self.options {
            if !seen.contains(&option.source) {
                bail!("opção ausente no XML base: {}", option.id);
            }
        }
        if output.matches("<Metadata").count() != self.options.len() {
            bail!("o XML gerado tem uma contagem de opções inesperada");
        }
        Ok(output)
    }

    pub fn write_patch_xml(&self, enabled: &[String], path: &Path) -> Result<ResolvedSelection> {
        let resolved = self.resolve(enabled, None)?;
        let xml = self.render_patch_xml(enabled)?;
        atomic_write(path, xml.as_bytes())?;
        Ok(resolved)
    }

    pub fn custom_profile_blurb(name: &str) -> String {
        match name {
            "SemPatches" => "Nenhum patch de código. Use como referência limpa do jogo.".into(),
            "Base60" => {
                "Ponto de partida recomendado: 720p, logos e vídeos puláveis, VBlank em 60 Hz.".into()
            }
            "Originais120" => {
                "Os patches originais do cuesta com 720p e 120 FPS. Exige hardware e VBlank compatíveis."
                    .into()
            }
            "ReferenciaObservada" => {
                "Reproduz a referência do estudo: patch de 120 FPS com VBlank deliberadamente em 60 Hz."
                    .into()
            }
            _ if name.starts_with("Teste_") || name.starts_with("Teste") => {
                "Perfil de teste do pacote. Ativa um candidato isolado; confirme visualmente no jogo."
                    .into()
            }
            "Personalizado" => "Sua seleção atual não coincide com nenhum perfil salvo.".into(),
            _ => "Perfil salvo por você com uma combinação própria de patches.".into(),
        }
    }
}

fn resolve_with(
    options: &[OptionDefinition],
    patches: &HashMap<String, PatchDefinition>,
    enabled: &[String],
    vblank_override: Option<u32>,
) -> Result<ResolvedSelection> {
    if enabled.iter().any(|id| id.trim().is_empty()) {
        bail!("seleção inválida: existe um identificador vazio");
    }
    let requested = enabled.iter().cloned().collect::<HashSet<_>>();
    if requested.len() != enabled.len() {
        bail!("opção repetida na seleção");
    }

    let mut groups: HashMap<String, String> = HashMap::new();
    let mut occupied: HashMap<u64, String> = HashMap::new();
    let mut vblank = 60u32;
    let mut min_dmem = 0u64;
    let mut checked_lines = 0usize;
    let mut ordered = Vec::new();

    for option in options {
        if !requested.contains(&option.id) {
            continue;
        }
        if let Some(group) = &option.group {
            if let Some(previous) = groups.get(group) {
                let previous_label = options
                    .iter()
                    .find(|candidate| &candidate.id == previous)
                    .map(|candidate| candidate.label.as_str())
                    .unwrap_or(previous);
                bail!(
                    "opções incompatíveis: {} e {}",
                    previous_label,
                    option.label
                );
            }
            groups.insert(group.clone(), option.id.clone());
        }
        vblank = option.vblank.unwrap_or(vblank);
        min_dmem = min_dmem.max(option.min_dmem);
        if let Some(patch) = patches.get(&option.id) {
            for line in &patch.lines {
                let address = u64::from_str_radix(line.address.trim_start_matches("0x"), 16)
                    .with_context(|| format!("endereço inválido em {}", option.id))?;
                let width = hex::decode_len(&line.value)
                    .with_context(|| format!("valor inválido em {}", option.id))?
                    as u64;
                for position in address..address + width {
                    if let Some(existing) = occupied.insert(position, option.id.clone()) {
                        bail!("escritas sobrepostas: {existing} e {}", option.id);
                    }
                }
                checked_lines += 1;
            }
        }
        ordered.push(option.id.clone());
    }

    if requested.len() != ordered.len() {
        let unknown = requested
            .iter()
            .filter(|id| !options.iter().any(|option| &option.id == *id))
            .cloned()
            .collect::<Vec<_>>();
        bail!("opção desconhecida: {}", unknown.join(", "));
    }

    if let Some(value) = vblank_override {
        if value != 60 {
            bail!("o override observado só aceita VBlank 60");
        }
        if !requested.contains("fps_120") {
            bail!("a referência observada exige o patch original de 120 FPS");
        }
        vblank = 60;
    }

    Ok(ResolvedSelection {
        enabled: ordered,
        vblank,
        min_dmem,
        checked_lines,
        vblank_override,
    })
}

fn xml_attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!("{name}=\"");
    let start = tag.find(&needle)? + needle.len();
    let rest = &tag[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

fn rewrite_enabled_attribute(tag: &str, desired: bool) -> Result<String> {
    let needle = "isEnabled=\"";
    let start = tag
        .find(needle)
        .context("Metadata sem isEnabled no XML base")?;
    let value_start = start + needle.len();
    let rest = &tag[value_start..];
    let end = rest
        .find('"')
        .context("isEnabled sem fechamento no XML base")?;
    let value_end = value_start + end;
    let mut rewritten = String::with_capacity(tag.len() + 1);
    rewritten.push_str(&tag[..value_start]);
    rewritten.push_str(if desired { "true" } else { "false" });
    rewritten.push_str(&tag[value_end..]);
    Ok(rewritten)
}

mod hex {
    pub fn decode_len(value: &str) -> Option<usize> {
        if value.is_empty() || value.len() % 2 != 0 {
            return None;
        }
        if value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            Some(value.len() / 2)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn embedded_catalog_is_integrity_checked_and_complete() {
        let catalog = Catalog::load_embedded().unwrap();
        assert_eq!(catalog.title_id, "CUSA01623");
        assert_eq!(catalog.app_version, "01.02");
        assert_eq!(catalog.options.len(), OPTION_COUNT);
        assert_eq!(catalog.profile_names()[0], "SemPatches");
        assert_eq!(
            catalog.profile("Base60").unwrap(),
            &ids(&["res_720", "skip_intro", "skip_legacy"])
        );
        assert_eq!(catalog.profile_override("ReferenciaObservada"), Some(60));
    }

    #[test]
    fn resolves_groups_vblank_arena_and_reference_clock() {
        let catalog = Catalog::load_embedded().unwrap();
        let base = catalog
            .resolve(&ids(&["res_720", "skip_intro", "skip_legacy"]), None)
            .unwrap();
        assert_eq!(base.vblank, 60);
        assert_eq!(base.min_dmem, 2048);
        assert_eq!(base.checked_lines, 15);

        let native = catalog
            .resolve(
                &ids(&["res_720", "fps_120", "skip_intro", "skip_legacy"]),
                None,
            )
            .unwrap();
        assert_eq!(native.vblank, 120);
        let observed = catalog
            .resolve(
                &ids(&["res_720", "fps_120", "skip_intro", "skip_legacy"]),
                Some(60),
            )
            .unwrap();
        assert_eq!(observed.vblank, 60);

        assert!(catalog.resolve(&ids(&["res_720", "res_4k"]), None).is_err());
        assert!(catalog.resolve(&ids(&["res_720"]), Some(60)).is_err());
        assert!(catalog.resolve(&ids(&["res_720", "nope"]), None).is_err());
        assert!(catalog
            .resolve(&ids(&["res_720", "res_720"]), None)
            .is_err());
    }

    #[test]
    fn generated_xml_toggles_all_metadata_and_matches_base60() {
        let catalog = Catalog::load_embedded().unwrap();
        let selection = catalog.profile("Base60").unwrap();
        let xml = catalog.render_patch_xml(selection).unwrap();
        assert_eq!(xml.matches("<Metadata").count(), OPTION_COUNT);
        assert_eq!(xml.matches("isEnabled=\"true\"").count(), 3);
        let index = xml.find("Name=\"Resolution Patch - 720p\"").unwrap();
        let tag_end = index + xml[index..].find('>').unwrap();
        assert!(xml[index..tag_end].contains("isEnabled=\"true\""));
        let index = xml.find("Name=\"Resolution Patch - 480p\"").unwrap();
        let fragment = &xml[index..index + 400];
        assert!(fragment.contains("isEnabled=\"false\""));
        assert!(xml.contains("Value=\"00050000\""));
    }

    #[test]
    fn all_builtin_profiles_resolve() {
        let catalog = Catalog::load_embedded().unwrap();
        for name in catalog.profile_names() {
            let enabled = catalog.profile(name).unwrap();
            let resolved = catalog
                .resolve(enabled, catalog.profile_override(name))
                .unwrap();
            assert_eq!(
                resolved.vblank,
                if catalog.profile_override(name) == Some(60) {
                    60
                } else {
                    if enabled.iter().any(|id| id == "fps_120") {
                        120
                    } else {
                        60
                    }
                }
            );
            catalog.render_patch_xml(enabled).unwrap();
        }
    }

    #[test]
    fn job_queue_patch_is_separate_from_graphical_changes() {
        let catalog = Catalog::load_embedded().unwrap();
        let only = ids(&["job_queue_pause"]);
        let resolved = catalog.resolve(&only, None).unwrap();
        assert_eq!(resolved.checked_lines, 6);
        assert_eq!(resolved.vblank, 60);
        assert_eq!(resolved.min_dmem, 0);
        assert!(!catalog.profile("Base60").unwrap().contains(&only[0]));
        let profile = catalog.profile("Teste_CPU_Jobs_120").unwrap();
        let mut expected = catalog.profile("Originais120").unwrap().clone();
        expected.push(only[0].clone());
        assert_eq!(profile, &expected);
        let xml = catalog.render_patch_xml(&only).unwrap();
        assert_eq!(xml.matches("isEnabled=\"true\"").count(), 1);
        assert!(xml.contains("Job Queue Contention PAUSE"));
    }

    #[test]
    fn shadow_range_options_require_one_quality_choice() {
        let catalog = Catalog::load_embedded().unwrap();
        for percent in [75, 50] {
            let id = format!("shadow_range_{percent}");
            let option = catalog.by_id.get(&id).unwrap();
            assert_eq!(option.group.as_deref(), Some("shadow_quality"));
            assert!(catalog.resolve(&vec![id.clone(), "dynamic_shadows_native_off".into()], None).is_err());
            assert_eq!(catalog.resolve(&vec![id], None).unwrap().checked_lines, 2);
        }
        assert!(catalog.resolve(&ids(&["shadow_range_75", "shadow_range_50"]), None).is_err());
    }
}
