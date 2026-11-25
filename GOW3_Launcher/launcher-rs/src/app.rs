use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::builds::{self, BuildEntry};
use crate::catalog::Catalog;
use crate::emulator_config::{self, Change, EmulatorConfig};
use crate::game::{CancelToken, GamePaths, PrivateEnvironment, ValidationReport};
use crate::i18n::{self, Lang};
use crate::presence::{self, Presence, PresenceService};
use crate::process::{self, MemoryStatus};
use crate::save_watch::SaveWatch;
use crate::session::{
    LaunchOptions, LaunchResult, LogTail, RunningInfo, SessionManager, SessionSummary,
};
use crate::storage::{acquire_instance_lock, application_root, data_directory, Settings, Store};
use crate::trophies::{self, Trophy};
use crate::updates::{self, Commit, Release, RemoteBuild};
use crate::{AppWindow, BuildItem, StepItem, ToggleItem, TrophyItem};

const PAGE_TITLES: [&str; 5] = [
    "Início",
    "Ajustes",
    "Atividade e logs",
    "Troféus",
    "Configurações",
];
const INITIAL_VERIFICATION: &str = "Ainda não verificado";
const MAX_LOG_CHARS: usize = 64_000;
const TRIM_LOG_TO: usize = 48_000;

#[derive(Debug)]
enum BgEvent {
    GpuChecked {
        path: String,
        size: u64,
        modified: Option<std::time::SystemTime>,
        supported: bool,
    },
    InitialSessions {
        summary: Option<SessionSummary>,
        can_recover: bool,
    },
    SessionSummary {
        generation: u64,
        summary: Option<SessionSummary>,
    },
    Stage {
        label: String,
        message: String,
        detail: String,
    },
    DownloadProgress {
        name: String,
        downloaded: u64,
        total: u64,
    },
    Verified {
        report: ValidationReport,
        signature: String,
    },
    Started {
        info: RunningInfo,
        signature: String,
    },
    Finished(LaunchResult),
    Recovered(usize),
    SavesImported(usize),
    Failed {
        message: String,
    },
    Cancelled,
    Trophies(TrophySnapshot),
    /// Resultado da consulta ao repositório configurado.
    UpdateChecked {
        repo: String,
        release: Option<Release>,
        commit: Option<Commit>,
        remote: Vec<RemoteBuild>,
        error: Option<String>,
    },
    UpdateInstalled {
        result: std::result::Result<String, String>,
    },
}

/// Leitura de troféus pronta, feita fora da interface. Os ícones vêm como
/// pixels RGBA porque `slint::Image` não pode atravessar threads.
#[derive(Debug)]
struct TrophySnapshot {
    stamp: u64,
    state: trophies::State,
    icons: Vec<(PathBuf, u32, u32, Vec<u8>)>,
}

struct RuntimeState {
    session_refresh_generation: u64,
    log_text: String,
    log_dirty: bool,
    selection: BTreeSet<String>,
    profile: String,
    vblank_override: Option<u32>,
    capture: bool,
    launch_mode: u8,
    busy: bool,
    running: bool,
    verified_signature: Option<String>,
    verified_label: Option<String>,
    latest_session: Option<SessionSummary>,
    current_session: Option<PathBuf>,
    log_tails: Vec<LogTail>,
    stop_tails_after_read: bool,
    can_recover: bool,
}

impl RuntimeState {
    fn new(selection: BTreeSet<String>, profile: String) -> Self {
        Self {
            session_refresh_generation: 0,
            log_text: String::new(),
            log_dirty: false,
            selection,
            profile,
            vblank_override: None,
            capture: false,
            launch_mode: 0,
            busy: false,
            running: false,
            verified_signature: None,
            verified_label: None,
            latest_session: None,
            current_session: None,
            log_tails: Vec::new(),
            stop_tails_after_read: false,
            can_recover: false,
        }
    }
}

#[derive(Default)]
struct GpuCheck {
    /// path, size, modified time and result of the last completed hash.
    cached: Option<(String, u64, Option<std::time::SystemTime>, bool)>,
    /// path and instant of the newest edit waiting for the debounce window.
    pending: Option<(String, Instant)>,
}

#[derive(Default)]
struct UpdateState {
    repo: String,
    release: Option<Release>,
    commit: Option<Commit>,
    remote: Vec<RemoteBuild>,
    /// Commit já conhecido: só um sha diferente vira aviso.
    seen_sha: String,
    checking: bool,
    installing: bool,
    skipped: String,
    checked_at: String,
    error: Option<String>,
}

/// Preferências da consulta de atualizações guardadas em `data/updates.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct StoredUpdate {
    #[serde(default)]
    repository: String,
    #[serde(default)]
    skipped: String,
    #[serde(default)]
    latest_sha: String,
    #[serde(default)]
    latest_tag: String,
    #[serde(default)]
    latest_name: String,
    #[serde(default)]
    latest_published: String,
    #[serde(default)]
    checked_at: String,
}

pub struct AppController {
    ui: AppWindow,
    catalog: Rc<Catalog>,
    store: Rc<RefCell<Store>>,
    data_root: PathBuf,
    sessions: SessionManager,
    runtime: RefCell<RuntimeState>,
    // Last persisted path trio; the UI is ``dirty`` when it differs from this.
    saved_paths: RefCell<(String, String, String)>,
    // Debounced GPU compatibility check and its per-file cache.
    gpu_check: RefCell<GpuCheck>,
    // Game-specific ShadPS4 options, kept apart from the launcher preferences.
    emulator: RefCell<EmulatorConfig>,
    // Última leitura dos troféus do ambiente privado e o filtro exibido.
    trophies: RefCell<trophies::State>,
    trophy_filter: Cell<i32>,
    /// Assinatura dos arquivos de troféus da última leitura: reabrir a página
    /// sem nenhuma mudança não relê nem descriptografa o pacote de novo.
    trophy_stamp: RefCell<Option<u64>>,
    /// Ícones já decodificados por caminho, para que filtrar não decodifique
    /// a mesma imagem outra vez.
    trophy_icons: RefCell<HashMap<PathBuf, slint::Image>>,
    /// Assinatura da leitura em andamento, para não pedir a mesma duas vezes.
    trophy_loading: Cell<Option<u64>>,
    // Token of the running operation; cancelled by the button or by closing.
    cancel: RefCell<Option<CancelToken>>,
    // A close request waits for the cancelled worker to settle before hiding.
    pending_close: Cell<bool>,
    event_sender: Sender<BgEvent>,
    event_receiver: Receiver<BgEvent>,
    // Atividade do Discord: a thread só conecta quando há sessão.
    presence: PresenceService,
    presence_started: Cell<Option<i64>>,
    /// A janela foi fechada pelo usuário: o processo segue vivo só enquanto
    /// uma sessão precisa ser restaurada.
    window_hidden: Cell<bool>,
    /// Pede o encerramento do laço assim que der (ver `poll`).
    should_quit: Cell<bool>,
    /// Permite publicar atividade no Discord. Fica ligado só entre o início de
    /// uma sessão e a morte do processo do jogo (ou o fechamento da janela).
    presence_live: Cell<bool>,
    /// Área gravada no último checkpoint — a fonte confiável da fase exibida.
    zone: RefCell<Option<String>>,
    /// Vigia do SAVEDATA: cada checkpoint regrava o arquivo e a área acompanha.
    save_watch: RefCell<SaveWatch>,
    /// PID do jogo em execução: se ele morrer, a atividade sai do Discord
    /// mesmo que o encerramento demore a ser percebido.
    game_pid: Cell<Option<u32>>,
    last_alive_check: Cell<Option<std::time::Instant>>,
    /// Builds lidas da pasta do repositório, na ordem mostrada.
    builds: RefCell<Vec<BuildEntry>>,
    /// Builds remotas disponíveis no GitHub e ainda não baixadas, exibidas após as locais.
    remote_builds: RefCell<Vec<updates::RemoteBuild>>,
    /// Build atualmente em download: `(nome, bytes_baixados, total_bytes)`.
    downloading_build: RefCell<Option<(String, u64, u64)>>,
    /// Pasta criada pelo próprio launcher onde as builds são instaladas.
    builds_dir: RefCell<PathBuf>,
    /// Última publicação conhecida e o estado das consultas.
    update_state: RefCell<UpdateState>,
    last_update_check: Cell<Option<Instant>>,
    /// Idioma efetivo do launcher (escolhido ou herdado do console).
    language: Cell<Lang>,
}

impl AppController {
    pub fn new(ui: AppWindow, catalog: Catalog, store: Store, data_root: PathBuf) -> Rc<Self> {
        // `\\?\` nunca aparece na interface nem em `settings.json`.
        let data_root = strip_verbatim(data_root);
        let catalog = Rc::new(catalog);
        let (event_sender, event_receiver) = channel();
        let sessions = SessionManager::new(data_root.clone());
        let runtime = RuntimeState::new(BTreeSet::new(), "Base60".into());
        let emulator = EmulatorConfig::load(PrivateEnvironment::new(&data_root).custom_config_path());
        Rc::new(Self {
            ui,
            catalog,
            store: Rc::new(RefCell::new(store)),
            data_root,
            sessions,
            emulator: RefCell::new(emulator),
            trophies: RefCell::new(trophies::State::Empty {
                message: String::new(),
            }),
            trophy_filter: Cell::new(0),
            trophy_stamp: RefCell::new(None),
            trophy_icons: RefCell::new(HashMap::new()),
            trophy_loading: Cell::new(None),
            runtime: RefCell::new(runtime),
            saved_paths: RefCell::new((String::new(), String::new(), String::new())),
            gpu_check: RefCell::new(GpuCheck::default()),
            cancel: RefCell::new(None),
            pending_close: Cell::new(false),
            event_sender,
            event_receiver,
            presence: PresenceService::start(),
            presence_started: Cell::new(None),
            window_hidden: Cell::new(false),
            should_quit: Cell::new(false),
            presence_live: Cell::new(false),
            zone: RefCell::new(None),
            save_watch: RefCell::new(SaveWatch::new()),
            game_pid: Cell::new(None),
            last_alive_check: Cell::new(None),
            builds: RefCell::new(Vec::new()),
            remote_builds: RefCell::new(Vec::new()),
            downloading_build: RefCell::new(None),
            builds_dir: RefCell::new(PathBuf::new()),
            update_state: RefCell::new(UpdateState::default()),
            last_update_check: Cell::new(None),
            language: Cell::new(Lang::Pt),
        })
    }

    pub fn initialize(self: &Rc<Self>) {
        self.ui.set_app_version(env!("CARGO_PKG_VERSION").into());
        self.ui
            .set_catalog_version(self.catalog.package_version.clone().into());
        self.ui
            .set_data_path(self.data_root.display().to_string().into());
        self.ui.set_page_title(PAGE_TITLES[0].into());

        let mut notices = self.store.borrow().notices.clone();
        let settings = match self.store.borrow().settings() {
            Ok(settings) => settings,
            Err(error) => {
                notices.push(format!(
                    "Preferências não puderam ser lidas; foram preservadas. {error}"
                ));
                self.backup_bad_settings();
                Settings::default()
            }
        };

        // Arquivo antigo (schema < 3): depois de montar a interface, a migração
        // é gravada uma vez — a pasta de builds passa a ser a do launcher.
        let needs_migration = crate::storage::read_json(&self.store.borrow().settings_path)
            .ok()
            .and_then(|value| {
                value
                    .get("schema_version")
                    .and_then(|version| version.as_u64())
            })
            .is_some_and(|version| version < 3);

        let mut selection = settings.desired_enabled.clone().unwrap_or_else(|| {
            self.store
                .borrow()
                .profile(&settings.profile, &self.catalog)
                .unwrap_or_else(|| self.catalog.profile("Base60").cloned().unwrap_or_default())
        });
        let mut override_clock = settings.vblank_override;
        if override_clock.is_some() && !selection.iter().any(|id| id == "fps_120") {
            override_clock = None;
            notices.push(
                "VBlank 60 observado foi ignorado porque o patch 120 FPS não está ativo.".into(),
            );
        }
        selection = self
            .catalog
            .options
            .iter()
            .filter(|option| selection.contains(&option.id))
            .map(|option| option.id.clone())
            .collect();
        if self.catalog.resolve(&selection, override_clock).is_err() {
            notices.push("Seleção salva inválida; Base60 foi restaurada.".into());
            selection = self.catalog.profile("Base60").cloned().unwrap_or_default();
            override_clock = None;
        }
        if override_clock.is_none() {
            if settings.desired_enabled.is_none() {
                override_clock = self
                    .profile_override(&settings.profile)
                    .filter(|_| selection.iter().any(|id| id == "fps_120"));
            }
        }
        let selection_set = selection.into_iter().collect::<BTreeSet<_>>();
        let profile = self.initial_profile_name(&settings, &selection_set, override_clock);

        {
            let mut runtime = self.runtime.borrow_mut();
            runtime.selection = selection_set;
            runtime.profile = profile;
            runtime.vblank_override = override_clock;
            runtime.capture = settings.capture_enabled;
            runtime.launch_mode = settings.launch_mode.unwrap_or(0).min(2);
            // Discover sessions once in the worker; navigation can render immediately.
            runtime.busy = true;
        }

        *self.saved_paths.borrow_mut() = (
            settings.base.trim().to_string(),
            settings.update.trim().to_string(),
            settings.executable.trim().to_string(),
        );
        self.ui.set_gpu_mode(settings.gpu_mode as i32);
        self.ui.set_lazy_cache(settings.lazy_cache);
        self.ui.set_disable_animations(settings.disable_animations);
        self.ui.set_fluid_enabled(settings.fluid_background);
        self.ui.set_discord_presence(settings.discord_presence);
        self.setup_language(&settings);
        self.setup_builds();
        self.setup_updates(&settings);
        self.ui.set_base_path(settings.base.into());
        self.ui.set_update_path(settings.update.into());
        self.ui.set_executable_path(settings.executable.into());
        self.ui.set_capture_enabled(settings.capture_enabled);
        self.refresh_path_states();
        self.refresh_paths_dirty();
        self.ui
            .set_launch_mode(self.runtime.borrow().launch_mode as i32);
        if needs_migration {
            let _ = self.persist_settings();
        }
        self.ui.set_observed_clock(override_clock == Some(60));
        self.ui.set_busy(true);
        self.refresh_all();
        self.check_gpu_build();
        self.setup_emulator_models();
        if let Some(error) = self.emulator.borrow().error.clone() {
            notices.push(error);
        }
        self.push_emulator_config();
        // A lista é lida em segundo plano: o launcher abre sem esperar o pacote.
        self.request_trophies(false);

        self.set_memory(process::memory_status());

        let mut initial_log = format!(
            "GoW III Launcher {} · Rust + Slint\nCatálogo {} · {} patches disponíveis\nDados privados: {}\n",
            env!("CARGO_PKG_VERSION"),
            self.catalog.package_version,
            self.catalog.options.len(),
            self.data_root.display()
        );
        if !notices.is_empty() {
            initial_log.push_str("\nAvisos:\n");
            for notice in &notices {
                initial_log.push_str("· ");
                initial_log.push_str(notice);
                initial_log.push('\n');
            }
        }
        initial_log
            .push_str("\nAguardando uma sessão. O painel mostrará os logs nativos do emulador.\n");
        self.set_log_view(&initial_log);
        self.runtime.borrow_mut().log_text = initial_log;

        self.set_status(
            1,
            "Carregando",
            "Carregando sessões…",
            "A interface já está disponível.",
        );
        let sessions = self.sessions.clone();
        spawn_task(self.event_sender.clone(), move |sender| {
            let can_recover = sessions.recovery_needed().unwrap_or(true);
            let summary = sessions.latest_summary();
            let _ = sender.send(BgEvent::InitialSessions {
                summary,
                can_recover,
            });
        });
    }

    pub fn bind(self: &Rc<Self>) {
        {
            let weak = Rc::downgrade(self);
            self.ui.on_preferences_changed(move || {
                if let Some(this) = weak.upgrade() {
                    let _ = this.persist_settings();
                    this.refresh_presence();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_group_picked(move |group, index| {
                if let Some(this) = weak.upgrade() { this.pick_group(group.as_str(), index); }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_trophy_refresh(move || {
                if let Some(this) = weak.upgrade() {
                    this.refresh_trophies();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_trophy_filter_set(move |filter| {
                if let Some(this) = weak.upgrade() {
                    this.trophy_filter.set(filter.clamp(0, 2));
                    this.push_trophies();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_navigate(move |page| {
                if let Some(this) = weak.upgrade() {
                    this.navigate(page);
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_browse(move |kind| {
                if let Some(this) = weak.upgrade() {
                    this.browse(kind);
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_path_changed(move || {
                if let Some(this) = weak.upgrade() {
                    this.on_path_changed();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_builds_refresh(move || {
                if let Some(this) = weak.upgrade() {
                    this.refresh_builds();
                    this.check_updates();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_build_picked(move |index| {
                if let Some(this) = weak.upgrade() {
                    this.pick_build(index);
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_build_download(move |index| {
                if let Some(this) = weak.upgrade() {
                    this.download_build_at(index);
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_update_check_now(move || {
                if let Some(this) = weak.upgrade() {
                    this.check_updates();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_update_download(move || {
                if let Some(this) = weak.upgrade() {
                    this.download_update();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_update_dismiss(move || {
                if let Some(this) = weak.upgrade() {
                    this.dismiss_update();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_launcher_language_set(move |index| {
                if let Some(this) = weak.upgrade() {
                    this.set_launcher_language(index);
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_resolution_picked(move |name| {
                if let Some(this) = weak.upgrade() {
                    this.pick_resolution(name.as_str());
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_feature_toggled(move |index| {
                if let Some(this) = weak.upgrade() {
                    this.toggle_feature(index);
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_experiment_toggled(move |index| {
                if let Some(this) = weak.upgrade() {
                    this.toggle_experiment(index);
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_profile_picked(move |name| {
                if let Some(this) = weak.upgrade() {
                    let mut names = vec!["Personalizado".to_string()];
                    names.extend(this.store.borrow().all_profile_names(&this.catalog));
                    if let Some(id) = names.iter().find(|id| profile_label(id) == name.as_str()) {
                        this.pick_profile(id);
                    }
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_open_save_profile(move || {
                if let Some(this) = weak.upgrade() {
                    this.open_save_profile();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_close_save_profile(move || {
                if let Some(this) = weak.upgrade() {
                    this.close_save_profile();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_confirm_save_profile(move || {
                if let Some(this) = weak.upgrade() {
                    this.confirm_save_profile();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_save_settings(move || {
                if let Some(this) = weak.upgrade() {
                    this.save_settings_user();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_discard_paths(move || {
                if let Some(this) = weak.upgrade() {
                    this.discard_paths();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_launch_requested(move || {
                if let Some(this) = weak.upgrade() {
                    this.launch();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_verify_requested(move || {
                if let Some(this) = weak.upgrade() {
                    this.verify();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_cancel_requested(move || {
                if let Some(this) = weak.upgrade() {
                    this.cancel_operation();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_recovery_requested(move || {
                if let Some(this) = weak.upgrade() {
                    this.recover();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_discard_recovery(move || {
                if let Some(this) = weak.upgrade() {
                    this.discard_recovery();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_import_saves_requested(move || {
                if let Some(this) = weak.upgrade() {
                    this.import_saves();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_open_data(move || {
                if let Some(this) = weak.upgrade() {
                    this.open_data();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_open_logs(move || {
                if let Some(this) = weak.upgrade() {
                    this.open_logs();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_open_session(move || {
                if let Some(this) = weak.upgrade() {
                    this.open_session();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_set_launch_mode(move |mode| {
                if let Some(this) = weak.upgrade() {
                    this.set_launch_mode(mode);
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_capture_set(move |enabled| {
                if let Some(this) = weak.upgrade() {
                    this.set_capture(enabled);
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_observed_set(move |enabled| {
                if let Some(this) = weak.upgrade() {
                    this.set_observed(enabled);
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_emulator_toggle(move |field, enabled| {
                if let Some(this) = weak.upgrade() {
                    this.apply_emulator_change(field.as_str(), Change::Toggle(enabled));
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_emulator_index(move |field, index| {
                if let Some(this) = weak.upgrade() {
                    this.apply_emulator_change(field.as_str(), Change::Index(index as i64));
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_emulator_number(move |field, text| {
                if let Some(this) = weak.upgrade() {
                    this.apply_emulator_change(field.as_str(), Change::Text(text.as_str()));
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_volume_set(move |value| {
                if let Some(this) = weak.upgrade() {
                    this.apply_volume(value);
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.on_clear_log(move || {
                if let Some(this) = weak.upgrade() {
                    let mut runtime = this.runtime.borrow_mut();
                    runtime.log_text = "Log limpo. Novas linhas aparecerão aqui.\n".into();
                    runtime.log_dirty = false;
                    this.set_log_view(&runtime.log_text);
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.ui.window().on_close_requested(move || {
                if let Some(this) = weak.upgrade() {
                    if this.runtime.borrow().busy {
                        let confirm = rfd::MessageDialog::new()
                            .set_title("Operação em andamento")
                            .set_description(
                                "Uma verificação, preparação ou cópia ainda está em andamento.\n\nCancelar a operação e fechar? Se o emulador já tiver sido aberto, ele continua em execução; a pendência será restaurada na próxima abertura.",
                            )
                            .set_buttons(rfd::MessageButtons::YesNo)
                            .set_level(rfd::MessageLevel::Warning)
                            .show();
                        if !matches!(confirm, rfd::MessageDialogResult::Yes) {
                            return slint::CloseRequestResponse::KeepWindowShown;
                        }
                        this.pending_close.set(true);
                        this.cancel_operation();
                        return slint::CloseRequestResponse::KeepWindowShown;
                    }
                    if this.runtime.borrow().running {
                        let confirm = rfd::MessageDialog::new()
                            .set_title("Jogo em execução")
                            .set_description(
                                "O jogo ainda está aberto. Fechar o launcher agora deixa a configuração temporária pendente; ela será restaurada na próxima abertura do launcher.\n\nFechar mesmo assim?",
                            )
                            .set_buttons(rfd::MessageButtons::YesNo)
                            .set_level(rfd::MessageLevel::Warning)
                            .show();
                        if !matches!(confirm, rfd::MessageDialogResult::Yes) {
                            return slint::CloseRequestResponse::KeepWindowShown;
                        }
                    }
                    let _ = this.persist_settings();
                    // Fechar a janela não pode deixar a atividade no Discord nem
                    // o processo pendurado: só continua vivo se houver sessão.
                    this.window_hidden.set(true);
                    this.presence_live.set(false);
                    this.presence.clear();
                    if !this.runtime.borrow().running {
                        this.should_quit.set(true);
                    }
                }
                slint::CloseRequestResponse::HideWindow
            });
        }
    }

    pub fn poll(&self) {
        let mut events = Vec::new();
        while let Ok(event) = self.event_receiver.try_recv() {
            events.push(event);
            if events.len() >= 64 {
                break;
            }
        }
        for event in events {
            self.handle_event(event);
        }
        self.flush_pending_gpu_check();
        self.poll_logs();
        self.poll_updates();
        self.flush_visible_log();
        self.close_if_settled();
        if let Some(pid) = self.game_pid.get() {
            let due = self
                .last_alive_check
                .get()
                .is_none_or(|at| at.elapsed() >= std::time::Duration::from_secs(1));
            if due {
                self.last_alive_check.set(Some(std::time::Instant::now()));
                if !crate::process::pid_alive(pid) {
                    // O jogo fechou: a atividade sair do perfil na mesma hora,
                    // sem esperar o laço da sessão perceber o encerramento.
                    self.game_pid.set(None);
                    self.presence_started.set(None);
                    self.presence_live.set(false);
                    self.presence.clear();
                    self.zone.borrow_mut().take();
                } else {
                    self.poll_saved_zone();
                }
            }
        }
        if self.should_quit.get() {
            self.should_quit.set(false);
            let _ = slint::quit_event_loop();
        }
    }

    fn handle_event(&self, event: BgEvent) {
        match event {
            BgEvent::UpdateChecked {
                repo,
                release,
                commit,
                remote,
                error,
            } => {
                {
                    let mut state = self.update_state.borrow_mut();
                    state.checking = false;
                    state.repo = repo;
                    state.release = release;
                    state.commit = commit;
                    state.remote = remote;
                    state.error = error;
                    state.checked_at = Local::now().format("%d/%m %H:%M").to_string();
                    if let Some(commit) = state.commit.as_ref() {
                        if state.seen_sha.is_empty() {
                            // Primeira leitura: registra a referência sem avisar.
                            state.seen_sha = commit.sha.clone();
                        }
                    }
                }
                self.ui.set_builds_checking(false);
                self.persist_update_state();
                self.publish_update();
                self.refresh_builds();
            }
            BgEvent::DownloadProgress {
                name,
                downloaded,
                total,
            } => {
                *self.downloading_build.borrow_mut() = Some((name, downloaded, total));
                self.refresh_builds();
            }
            BgEvent::UpdateInstalled { result } => {
                self.update_state.borrow_mut().installing = false;
                *self.downloading_build.borrow_mut() = None;
                self.ui.set_any_build_downloading(false);
                match result {
                    Ok(path) => {
                        self.ui.set_executable_path(path.clone().into());
                        if let Some(parent) = Path::new(&path).parent() {
                            *self.builds_dir.borrow_mut() = parent.to_path_buf();
                        }
                        self.refresh_builds();
                        self.on_path_changed();
                        self.set_status(
                            0,
                            "Pronto",
                            "Build baixada e selecionada.",
                            &path,
                        );
                        self.log_launcher(&format!("Build instalada: {path}"));
                    }
                    Err(message) => self.set_status(
                        3,
                        "Erro",
                        "Não foi possível baixar a build.",
                        &message,
                    ),
                }
                self.refresh_builds();
                self.publish_update();
            }
            BgEvent::Trophies(snapshot) => {
                *self.trophies.borrow_mut() = snapshot.state;
                self.trophy_stamp.replace(Some(snapshot.stamp));
                self.trophy_loading.set(None);
                {
                    let mut cache = self.trophy_icons.borrow_mut();
                    cache.clear();
                    for (path, width, height, bytes) in snapshot.icons {
                        if width == 0 || height == 0 {
                            continue;
                        }
                        let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                            &bytes, width, height,
                        );
                        cache.insert(path, slint::Image::from_rgba8(buffer));
                    }
                }
                self.ui.set_trophy_refresh_enabled(true);
                if self.ui.get_page() == 3 {
                    self.push_trophies();
                }
                // O progresso do cartão acompanha a lista.
                self.refresh_presence();
            }
            BgEvent::GpuChecked {
                path,
                size,
                modified,
                supported,
            } => {
                self.gpu_check.borrow_mut().cached = Some((path.clone(), size, modified, supported));
                if path == self.ui.get_executable_path().trim() {
                    self.set_gpu_result(supported);
                }
            }
            BgEvent::InitialSessions {
                summary,
                can_recover,
            } => {
                {
                    let mut runtime = self.runtime.borrow_mut();
                    runtime.latest_session = summary;
                    runtime.can_recover = can_recover;
                }
                self.ui.set_can_recover(can_recover);
                self.set_busy(false);
                self.update_session_labels();
                let can_recover = self.runtime.borrow().can_recover;
                if can_recover {
                    self.set_status(
                2,
                "Atenção",
                "Há uma sessão ou configuração temporária pendente.",
                "Use Recuperar sessão para restaurar os requisitos privados antes de iniciar.",
            );
                } else if self.paths_ready() {
                    self.set_status(
                        0,
                        "Pronto",
                        "Caminhos configurados.",
                        "Verifique os arquivos antes do primeiro início.",
                    );
                } else {
                    self.set_status(
                        0,
                        "Pronto",
                        "Bem-vindo ao launcher.",
                        "Configure o jogo, a atualização e o executável do shadPS4 para começar.",
                    );
                }
                self.refresh_steps();
            }
            BgEvent::SessionSummary {
                generation,
                summary,
            } => {
                if generation == self.runtime.borrow().session_refresh_generation {
                    self.runtime.borrow_mut().latest_session = summary;
                    self.update_session_labels();
                    self.refresh_steps();
                }
            }
            BgEvent::Stage {
                label,
                message,
                detail,
            } => {
                self.set_busy(true);
                self.set_status(1, &label, &message, &detail);
                self.refresh_presence();
            }
            BgEvent::Verified { report, signature } => {
                self.set_busy(false);
                let current = self.verification_signature();
                if current == signature {
                    let label = report.summary_label();
                    self.runtime.borrow_mut().verified_signature = Some(signature);
                    self.runtime.borrow_mut().verified_label = Some(label.clone());
                    self.set_status(
                        0,
                        "Pronto",
                        "Verificação concluída.",
                        &format!(
                            "{} · eboot {} · build {}",
                            label,
                            short_hash(&report.self_sha256),
                            short_hash(&report.executable_sha256)
                        ),
                    );
                    self.log_launcher(&format!(
                        "Verificação OK: {} linhas originais, {} patches, VBlank {} Hz.",
                        report.verified_original_lines,
                        report.enabled.len(),
                        report.vblank
                    ));
                } else {
                    self.set_status(
                        2,
                        "Atenção",
                        "A seleção ou os caminhos mudaram durante a verificação.",
                        "Execute a verificação novamente para confirmar a configuração atual.",
                    );
                }
                self.refresh_verification();
                self.refresh_steps();
            }
            BgEvent::Started { info, signature } => {
                {
                    let mut runtime = self.runtime.borrow_mut();
                    runtime.busy = false;
                    runtime.running = true;
                    runtime.current_session = Some(info.session_dir.clone());
                    runtime.verified_signature = Some(signature);
                    runtime.verified_label =
                        Some(format!("Arquivos validados · VBlank {} Hz", info.vblank));
                    runtime.log_tails = self.log_tails_for(&info);
                    runtime.stop_tails_after_read = false;
                }
                self.ui.set_busy(false);
                self.ui.set_running(true);
                let mode = if info.no_cache {
                    "sem cache em disco"
                } else if info.wait_for_shaders {
                    "cache + espera de shaders"
                } else {
                    "cache normal"
                };
                self.set_status(
                    2,
                    "Em execução",
                    &format!("Sessão ativa · PID {}", info.pid),
                    &format!(
                        "{} · VBlank {} Hz · {} patches · {}",
                        info.profile,
                        info.vblank,
                        info.enabled.len(),
                        mode
                    ),
                );
                self.set_memory(info.memory);
                self.ui.set_verification_label(
                    format!("Arquivos validados · VBlank {} Hz", info.vblank).into(),
                );
                self.ui.set_verification_short("Validado".into());
                self.log_launcher(&format!(
                    "Sessão iniciada · PID {} · perfil {} · VBlank {} Hz · {}",
                    info.pid, info.profile, info.vblank, mode
                ));
                self.log_launcher(&format!("Pasta da sessão: {}", info.session_dir.display()));
                self.log_launcher(&format!(
                    "Patches enviados: {}",
                    if info.enabled.is_empty() {
                        "nenhum".to_string()
                    } else {
                        info.enabled.join(", ")
                    }
                ));
                if !info.native_log_enabled {
                    self.log_launcher(
                        "Logs nativos desativados na configuração privada; o painel pode ficar vazio.",
                    );
                }
                self.refresh_latest_session();
                self.ui
                    .set_session_meta(format!("PID {} · {}", info.pid, mode).into());
                self.refresh_steps();
                // Enquanto o jogo escreve o progresso, a página mantém o último estado.
                self.ui.set_trophy_refresh_enabled(false);
                self.presence_started.set(Some(presence::now_ms()));
                self.presence_live.set(true);
                self.game_pid.set(Some(info.pid));
                // A área do último checkpoint entra no cartão antes de o jogo
                // escrever qualquer coisa; os próximos saves atualizam sozinhos.
                self.save_watch.borrow_mut().reset();
                *self.zone.borrow_mut() = None;
                self.poll_saved_zone();
                self.refresh_presence();
            }
            BgEvent::Finished(result) => {
                {
                    let mut runtime = self.runtime.borrow_mut();
                    runtime.busy = false;
                    runtime.running = false;
                    runtime.stop_tails_after_read = true;
                    runtime.current_session = Some(result.session_dir.clone());
                }
                self.ui.set_busy(false);
                self.ui.set_running(false);
                // The session journal restored the temporary overrides on disk.
                self.emulator.borrow_mut().reload();
                self.push_emulator_config();
                if let Some(error) = &result.error {
                    self.set_status(
                        3,
                        "Erro",
                        &format!("A sessão terminou com código {}.", result.exit_code),
                        error,
                    );
                    self.log_launcher(error);
                } else {
                    self.set_status(
                        0,
                        "Pronto",
                        "Sessão encerrada.",
                        &format!(
                            "Código {} · requisitos temporários restaurados{}.",
                            result.exit_code,
                            if result.capture {
                                " · captura gravada"
                            } else {
                                ""
                            }
                        ),
                    );
                    self.log_launcher(&format!(
                        "Sessão encerrada com código {}; configuração temporária restaurada.",
                        result.exit_code
                    ));
                }
                self.log_launcher(&format!(
                    "Pasta da sessão: {}",
                    result.session_dir.display()
                ));
                self.refresh_latest_session();
                self.refresh_can_recover();
                self.refresh_steps();
                // O núcleo pode ter gravado desbloqueios durante a sessão.
                self.request_trophies(true);
                self.presence_started.set(None);
                self.game_pid.set(None);
                self.presence_live.set(false);
                self.zone.borrow_mut().take();
                self.refresh_presence();
                if self.window_hidden.get() {
                    // Nada mais a restaurar: o launcher pode sair de vez.
                    self.presence.clear();
                    self.should_quit.set(true);
                }
            }
            BgEvent::Recovered(count) => {
                self.set_busy(false);
                self.emulator.borrow_mut().reload();
                self.push_emulator_config();
                self.set_status(
                    0,
                    "Pronto",
                    "Recuperação concluída.",
                    &format!(
                        "{} sessão(ões) finalizada(s). Os requisitos privados foram restaurados.",
                        count
                    ),
                );
                self.log_launcher(&format!(
                    "Recuperação concluída: {count} sessão(ões) pendente(s)."
                ));
                self.refresh_latest_session();
                self.refresh_can_recover();
                self.refresh_steps();
            }
            BgEvent::SavesImported(count) => {
                self.set_busy(false);
                self.set_status(
                    0,
                    "Pronto",
                    "Saves importados.",
                    &format!(
                        "{count} arquivo(s) copiado(s) para o ambiente privado. A origem não foi alterada."
                    ),
                );
                self.log_launcher(&format!(
                    "{count} arquivo(s) de save importado(s) para o ambiente privado."
                ));
            }
            BgEvent::Failed { message } => {
                self.set_busy(false);
                self.set_status(3, "Erro", "A operação não foi concluída.", &message);
                self.log_launcher(&message);
                self.refresh_can_recover();
                self.refresh_latest_session();
            }
            BgEvent::Cancelled => {
                self.set_busy(false);
                self.set_status(
                    2,
                    "Cancelado",
                    "Operação cancelada.",
                    "Nenhum arquivo original foi alterado; se a preparação já tinha aplicado requisitos temporários, eles foram restaurados.",
                );
                self.log_launcher("Operação cancelada pelo usuário.");
                self.refresh_can_recover();
                self.refresh_latest_session();
                self.refresh_verification();
                self.refresh_steps();
            }
        }
    }

    fn navigate(&self, page: i32) {
        let page = page.clamp(0, 4) as usize;
        self.ui.set_page(page as i32);
        self.ui.set_page_title(PAGE_TITLES[page].into());
        self.flush_visible_log();
        if page == 3 {
            // A leitura acontece fora da interface; a página abre na hora e
            // recebe a lista assim que ela fica pronta.
            self.refresh_trophies_if_stale();
            if self.trophy_loading.get().is_none() {
                self.push_trophies();
            }
        }
    }

    fn browse(&self, kind: i32) {
        let label = match kind {
            0 => "Selecionar pasta do jogo base",
            1 => "Selecionar pasta de atualização",
            _ => "Selecionar executável do shadPS4",
        };
        let mut dialog = rfd::FileDialog::new().set_title(label);
        match kind {
            0 | 1 => {
                let current = if kind == 0 {
                    self.ui.get_base_path().to_string()
                } else {
                    self.ui.get_update_path().to_string()
                };
                if !current.trim().is_empty() {
                    let candidate = Path::new(current.trim());
                    let directory = if candidate.is_dir() {
                        candidate
                    } else {
                        candidate.parent().unwrap_or(candidate)
                    };
                    if directory.exists() {
                        dialog = dialog.set_directory(directory);
                    }
                }
                let Some(path) = dialog.pick_folder() else {
                    return;
                };
                if kind == 0 {
                    self.ui.set_base_path(path.display().to_string().into());
                    match derive_update_path(&path) {
                        Some(update) => {
                            self.ui.set_update_path(update.display().to_string().into());
                        }
                        None => {
                            self.ui.set_update_path("".into());
                        }
                    }
                } else {
                    self.ui.set_update_path(path.display().to_string().into());
                }
            }
            _ => {
                let current = self.ui.get_executable_path().to_string();
                if !current.trim().is_empty() {
                    if let Some(parent) = Path::new(current.trim()).parent() {
                        if parent.exists() {
                            dialog = dialog.set_directory(parent);
                        }
                    }
                }
                let Some(path) = dialog.add_filter("Executável", &["exe"]).pick_file() else {
                    return;
                };
                self.ui
                    .set_executable_path(path.display().to_string().into());
            }
        }
        self.on_path_changed();
    }

    /// Debounced: typing a path updates the message immediately, but the
    /// executable is hashed only once the edits settle and only if the file
    /// signature is not already cached.
    fn check_gpu_build(&self) {
        let path = self.ui.get_executable_path().trim().to_string();
        if path.is_empty() {
            self.gpu_check.borrow_mut().pending = None;
            self.ui.set_gpu_supported(false);
            self.ui
                .set_gpu_compatibility("Selecione o executável para conferir compatibilidade.".into());
            return;
        }
        let Ok(metadata) = fs::metadata(&path) else {
            self.gpu_check.borrow_mut().pending = None;
            self.ui.set_gpu_supported(false);
            self.ui
                .set_gpu_compatibility("Executável não encontrado no caminho informado.".into());
            return;
        };
        if !metadata.is_file() {
            self.gpu_check.borrow_mut().pending = None;
            self.ui.set_gpu_supported(false);
            self.ui
                .set_gpu_compatibility("O caminho do executável não é um arquivo.".into());
            return;
        }
        let size = metadata.len();
        let modified = metadata.modified().ok();
        let cached = {
            let state = self.gpu_check.borrow();
            state
                .cached
                .as_ref()
                .filter(|(cached_path, cached_size, cached_modified, _)| {
                    cached_path == &path && *cached_size == size && *cached_modified == modified
                })
                .map(|(_, _, _, supported)| *supported)
        };
        if let Some(supported) = cached {
            self.gpu_check.borrow_mut().pending = None;
            self.set_gpu_result(supported);
            return;
        }
        self.gpu_check.borrow_mut().pending = Some((path, Instant::now()));
        self.ui.set_gpu_supported(false);
        self.ui
            .set_gpu_compatibility("Conferindo compatibilidade do executável…".into());
    }

    fn set_gpu_result(&self, supported: bool) {
        self.ui.set_gpu_supported(supported);
        self.ui.set_gpu_compatibility(
            if supported {
                "E11 reconhecido por SHA256. Preferências serão enviadas no próximo início."
            } else {
                "Sem suporte confirmado. E10 ignora estes recursos. Para usá-los, selecione o E11 reconhecido."
            }
            .into(),
        );
    }

    fn flush_pending_gpu_check(&self) {
        let ready = {
            let state = self.gpu_check.borrow();
            state
                .pending
                .as_ref()
                .filter(|(_, since)| since.elapsed() >= Duration::from_millis(300))
                .map(|(path, _)| path.clone())
        };
        let Some(path) = ready else { return };
        self.gpu_check.borrow_mut().pending = None;
        let Ok(metadata) = fs::metadata(&path) else { return };
        let size = metadata.len();
        let modified = metadata.modified().ok();
        let sender = self.event_sender.clone();
        spawn_task(sender, move |sender| {
            let supported = crate::game::sha256_file(Path::new(&path))
                .map(|hash| crate::session::supports_gpu_modes(&hash))
                .unwrap_or(false);
            let _ = sender.send(BgEvent::GpuChecked {
                path,
                size,
                modified,
                supported,
            });
        });
    }

    fn pick_group(&self, group: &str, index: i32) {
        if self.reject_if_busy() { return; }
        let options = self.catalog.options.iter().filter(|o| o.group.as_deref() == Some(group)).collect::<Vec<_>>();
        if index < 0 || index as usize > options.len() { return; }
        let mut candidate = self.runtime.borrow().selection.clone();
        for option in &options { candidate.remove(&option.id); }
        if index > 0 { candidate.insert(options[index as usize - 1].id.clone()); }
        if let Err(error) = self.catalog.resolve(&self.selection_vec(&candidate), self.runtime.borrow().vblank_override) {
            self.set_status(2, "Incompatível", "Combinação não aplicada.", &error.to_string()); return;
        }
        self.runtime.borrow_mut().selection = candidate;
        self.after_selection_changed();
    }

    fn on_path_changed(&self) {
        self.refresh_path_states();
        self.refresh_paths_dirty();
        self.check_gpu_build();
        self.refresh_paths_and_steps();
        self.refresh_verification();
    }

    /// Per-field existence check so a typo is visible in Configurações without
    /// running the full verification.
    fn refresh_path_states(&self) {
        fn state(value: &str, is_dir: bool) -> i32 {
            let value = value.trim();
            if value.is_empty() {
                return 0;
            }
            let path = Path::new(value);
            if if is_dir { path.is_dir() } else { path.is_file() } {
                1
            } else {
                2
            }
        }
        let base = self.ui.get_base_path();
        let update = self.ui.get_update_path();
        let executable = self.ui.get_executable_path();
        self.ui.set_base_state(state(base.as_str(), true));
        self.ui.set_update_state(state(update.as_str(), true));
        self.ui
            .set_executable_state(state(executable.as_str(), false));
    }

    fn refresh_paths_dirty(&self) {
        let saved = self.saved_paths.borrow();
        let dirty = saved.0 != self.ui.get_base_path().trim()
            || saved.1 != self.ui.get_update_path().trim()
            || saved.2 != self.ui.get_executable_path().trim();
        self.ui.set_paths_dirty(dirty);
    }

    /// Restores the last persisted paths, discarding unsaved edits.
    fn discard_paths(&self) {
        if self.runtime.borrow().busy || self.runtime.borrow().running {
            return;
        }
        let (base, update, executable) = self.saved_paths.borrow().clone();
        self.ui.set_base_path(base.into());
        self.ui.set_update_path(update.into());
        self.ui.set_executable_path(executable.into());
        self.on_path_changed();
    }

    fn pick_resolution(&self, name: &str) {
        if self.reject_if_busy() {
            return;
        }
        let Some((choice, _, _, _)) = self
            .resolution_entries()
            .into_iter()
            .find(|(_, title, _, _)| title == name)
        else {
            return;
        };
        let index = choice.and_then(|id| self.catalog.options.iter()
            .filter(|o| o.group.as_deref() == Some("resolution"))
            .position(|o| o.id == id).map(|i| i + 1)).unwrap_or(0);
        self.pick_group("resolution", index as i32);
    }

    fn toggle_feature(&self, index: i32) {
        if self.reject_if_busy() {
            return;
        }
        let ids = self.feature_ids();
        let Some(id) = ids.get(index.max(0) as usize).cloned() else {
            return;
        };
        {
            let mut runtime = self.runtime.borrow_mut();
            if runtime.selection.contains(&id) {
                runtime.selection.remove(&id);
            } else {
                runtime.selection.insert(id.clone());
            }
            if id == "fps_120" && !runtime.selection.contains("fps_120") {
                runtime.vblank_override = None;
            }
        }
        self.after_selection_changed();
    }

    fn toggle_experiment(&self, index: i32) {
        if self.reject_if_busy() {
            return;
        }
        let ids = self.experiment_ids();
        let Some(id) = ids.get(index.max(0) as usize).cloned() else {
            return;
        };
        {
            let mut runtime = self.runtime.borrow_mut();
            if runtime.selection.contains(&id) {
                runtime.selection.remove(&id);
            } else {
                let mut candidate = runtime.selection.clone();
                candidate.insert(id.clone());
                if let Err(error) = self.catalog.resolve(&self.selection_vec(&candidate), runtime.vblank_override) {
                    drop(runtime);
                    self.set_status(2, "Incompatível", "Combinação não aplicada.", &error.to_string());
                    return;
                }
                runtime.selection = candidate;
            }
        }
        self.after_selection_changed();
    }

    fn after_selection_changed(&self) {
        let selection = self.ordered_selection();
        let override_clock = self.runtime.borrow().vblank_override;
        let current = self.runtime.borrow().profile.clone();
        let profile = if current != "Personalizado"
            && self.profile_matches(&current, &selection, override_clock)
        {
            current
        } else {
            self.find_matching_profile(&selection, override_clock)
                .unwrap_or_else(|| "Personalizado".into())
        };
        self.runtime.borrow_mut().profile = profile;
        self.refresh_all();
        if self.runtime.borrow().running {
            self.set_status(
                2,
                "Em execução",
                "Sessão ativa · alterações pendentes",
                "A seleção atual será aplicada somente no próximo início.",
            );
        } else {
            self.set_status(
                0,
                "Pronto",
                "Seleção atualizada.",
                "As mudanças nos patches serão aplicadas no próximo início.",
            );
        }
    }

    fn pick_profile(&self, name: &str) {
        if self.reject_if_busy() {
            self.refresh_profiles();
            return;
        }
        if name == "Personalizado" {
            self.runtime.borrow_mut().profile = "Personalizado".into();
            self.refresh_profiles();
            self.refresh_selection();
            return;
        }
        let Some(profile) = self.store.borrow().profile(name, &self.catalog) else {
            self.set_status(
                3,
                "Erro",
                "Perfil não encontrado.",
                "O perfil selecionado não existe mais; escolha outro.",
            );
            self.refresh_profiles();
            return;
        };
        if let Err(error) = self.catalog.resolve(&profile, self.profile_override(name)) {
            self.set_status(3, "Erro", "Perfil inválido.", &error.to_string());
            self.refresh_profiles();
            return;
        }
        {
            let mut runtime = self.runtime.borrow_mut();
            runtime.selection = profile.into_iter().collect();
            runtime.vblank_override = self.profile_override(name);
            runtime.profile = name.to_string();
        }
        self.refresh_all();
        if self.runtime.borrow().running {
            self.set_status(
                2,
                "Em execução",
                "Sessão ativa · perfil alterado",
                "O novo perfil será aplicado no próximo início.",
            );
        } else {
            self.set_status(
                0,
                "Pronto",
                &format!("Perfil {name} selecionado."),
                "Revise a seleção e inicie a próxima sessão.",
            );
        }
    }

    fn open_save_profile(&self) {
        self.ui.set_profile_save_error("".into());
        self.ui.set_new_profile_name("".into());
        self.ui.set_save_profile_open(true);
    }

    fn close_save_profile(&self) {
        self.ui.set_save_profile_open(false);
        self.ui.set_profile_save_error("".into());
    }

    fn confirm_save_profile(&self) {
        let selected = self.runtime.borrow().selection.clone();
        if self.runtime.borrow().vblank_override.is_some() {
            self.ui.set_profile_save_error(
                "A referência observada (120 FPS com VBlank 60) já existe como perfil do pacote e não pode ser salva como perfil personalizado."
                    .into(),
            );
            return;
        }
        let name = self.ui.get_new_profile_name().to_string();
        let enabled = self.ordered_selection();
        if let Err(error) = self.catalog.resolve(&enabled, None) {
            self.ui.set_profile_save_error(error.to_string().into());
            return;
        }
        let result = self.store.borrow_mut().save_profile(
            &name,
            &selected.iter().cloned().collect::<Vec<_>>(),
            &self.catalog,
        );
        match result {
            Ok(()) => {
                let name = name.trim().to_string();
                self.runtime.borrow_mut().profile = name.clone();
                self.ui.set_save_profile_open(false);
                self.ui.set_profile_save_error("".into());
                self.refresh_all();
                self.set_status(
                    0,
                    "Pronto",
                    &format!("Perfil {name} salvo."),
                    "O perfil personalizado já está disponível no seletor.",
                );
                self.log_launcher(&format!("Perfil personalizado salvo: {name}."));
                let _ = self.persist_settings();
            }
            Err(error) => {
                self.ui.set_profile_save_error(error.to_string().into());
            }
        }
    }

    fn save_settings_user(&self) {
        match self.persist_settings() {
            Ok(()) => {
                self.set_status(
                    0,
                    "Pronto",
                    "Preferências salvas.",
                    "Caminhos, perfil e opções de inicialização foram persistidos.",
                );
                self.log_launcher("Preferências salvas.");
            }
            Err(error) => {
                self.set_status(3, "Erro", "Não foi possível salvar.", &error.to_string());
                self.log_launcher(&format!("Falha ao salvar preferências: {error:#}"));
            }
        }
    }

    fn verify(&self) {
        if self.runtime.borrow().busy || self.runtime.borrow().running {
            self.set_status(
                1,
                "Ocupado",
                "Aguarde a operação atual.",
                "Não é possível verificar durante uma sessão ou preparação.",
            );
            return;
        }
        if !self.paths_ready() {
            self.navigate(4);
            self.set_status(
                2,
                "Configuração",
                "Complete os três caminhos primeiro.",
                "Informe o jogo base, a atualização e o executável do shadPS4.",
            );
            return;
        }
        let base = self.ui.get_base_path().to_string();
        let update = self.ui.get_update_path().to_string();
        let executable = self.ui.get_executable_path().to_string();
        let selection = self.ordered_selection();
        let override_clock = self.runtime.borrow().vblank_override;
        let signature = self.verification_signature();
        let catalog = (*self.catalog).clone();
        let data_root = self.data_root.clone();
        let sender = self.event_sender.clone();
        self.set_busy(true);
        let cancel = self.begin_operation();
        self.set_status(
            1,
            "Verificando",
            "Conferindo arquivos e patches…",
            "Validando eboot, SFO, bytes originais e build do emulador.",
        );
        spawn_task(sender, move |tx| {
            let result = (|| -> Result<ValidationReport> {
                let paths = GamePaths::from_strings(&data_root, &base, &update, &executable)?;
                paths.validate(&catalog, &selection, override_clock, &cancel)
            })();
            match result {
                Ok(report) if !cancel.is_cancelled() => {
                    let _ = tx.send(BgEvent::Verified { report, signature });
                }
                Ok(_) => {
                    let _ = tx.send(BgEvent::Cancelled);
                }
                Err(error) => {
                    let _ = report_worker_error(&tx, &cancel, error);
                }
            }
        });
    }

    fn launch(&self) {
        if self.runtime.borrow().busy || self.runtime.borrow().running {
            self.set_status(
                1,
                "Ocupado",
                "Já existe uma operação em andamento.",
                "Aguarde a sessão atual ou a preparação em curso.",
            );
            return;
        }
        if !self.paths_ready() {
            self.navigate(4);
            self.set_status(
                2,
                "Configuração",
                "Configure os caminhos antes de iniciar.",
                "O jogo base, a atualização e o executável do shadPS4 são obrigatórios.",
            );
            return;
        }
        if self.sessions.recovery_needed().unwrap_or(true) {
            self.navigate(2);
            self.set_status(
                2,
                "Atenção",
                "Conclua a recuperação antes de iniciar.",
                "Há uma sessão ou configuração temporária pendente.",
            );
            return;
        }
        if self.ui.get_gpu_mode() != 0 && !self.ui.get_gpu_supported() {
            self.navigate(4);
            self.set_status(
                2,
                "Modo GPU",
                "O executável atual não foi reconhecido para os modos de GPU.",
                "Volte o modo GPU para a configuração do emulador ou selecione um build E11 reconhecido.",
            );
            return;
        }
        if let Err(error) = self.persist_settings() {
            self.set_status(
                3,
                "Erro",
                "Não foi possível salvar as preferências.",
                &error.to_string(),
            );
            return;
        }
        if let Err(error) = self.catalog.resolve(
            &self.ordered_selection(),
            self.runtime.borrow().vblank_override,
        ) {
            self.set_status(3, "Erro", "Seleção inválida.", &error.to_string());
            return;
        }
        let base = self.ui.get_base_path().to_string();
        let update = self.ui.get_update_path().to_string();
        let executable = self.ui.get_executable_path().to_string();
        let selection = self.ordered_selection();
        let profile = self.runtime.borrow().profile.clone();
        let override_clock = self.runtime.borrow().vblank_override;
        let mode = self.runtime.borrow().launch_mode;
        let options = LaunchOptions {
            capture: self.runtime.borrow().capture || mode == 2,
            no_cache: mode == 1,
            wait_for_shaders: mode == 2,
            gpu_mode: self.ui.get_gpu_mode() as u8,
            lazy_cache: self.ui.get_lazy_cache(),
        };
        let signature = self.verification_signature();
        let catalog = (*self.catalog).clone();
        let data_root = self.data_root.clone();
        let sender = self.event_sender.clone();
        self.set_busy(true);
        let cancel = self.begin_operation();
        self.set_status(
            1,
            "Preparando",
            "Validando a seleção antes de iniciar…",
            "O jogo será aberto depois da validação do eboot, dos patches e do ambiente privado.",
        );
        spawn_task(sender, move |tx| {
            let stage_tx = tx.clone();
            let _ = stage_tx.send(BgEvent::Stage {
                label: "Preparando".into(),
                message: "Preparando a sessão privada…".into(),
                detail: "Copiando a build, aplicando requisitos temporários e iniciando o shadPS4."
                    .into(),
            });
            let result = (|| -> Result<LaunchResult> {
                let paths = GamePaths::from_strings(&data_root, &base, &update, &executable)?;
                let manager = SessionManager::new(data_root.clone());
                let running_tx = tx.clone();
                let running_signature = signature.clone();
                manager.launch(
                    &catalog,
                    &paths,
                    &selection,
                    &profile,
                    override_clock,
                    options,
                    &cancel,
                    move |info| {
                        let _ = running_tx.send(BgEvent::Started {
                            info: info.clone(),
                            signature: running_signature.clone(),
                        });
                    },
                )
            })();
            match result {
                Ok(result) => {
                    let _ = tx.send(BgEvent::Finished(result));
                }
                Err(error) => {
                    let _ = report_worker_error(&tx, &cancel, error);
                }
            }
        });
    }

    /// Last-resort exit when a journal or session state is corrupt: abandon
    /// the pending state (keeping .bak copies) instead of restoring it.
    fn discard_recovery(&self) {
        if self.runtime.borrow().busy || self.runtime.borrow().running {
            self.set_status(
                1,
                "Ocupado",
                "Aguarde a operação atual.",
                "Não é possível descartar pendências durante uma sessão.",
            );
            return;
        }
        if let Err(error) = process::ensure_emulator_stopped("") {
            self.set_status(
                2,
                "Atenção",
                "Feche o emulador antes de descartar pendências.",
                &error.to_string(),
            );
            return;
        }
        let confirm = rfd::MessageDialog::new()
            .set_title("Descartar pendências")
            .set_description(
                "Isto encerra as sessões pendentes e move o diário de requisitos para .bak sem restaurar a configuração privada. Se o diário estiver corrompido, ajustes temporários podem permanecer no ambiente privado.\n\nOs arquivos das sessões são preservados. Continuar?",
            )
            .set_buttons(rfd::MessageButtons::YesNo)
            .set_level(rfd::MessageLevel::Warning)
            .show();
        if !matches!(confirm, rfd::MessageDialogResult::Yes) {
            return;
        }
        match self.sessions.discard_pending() {
            Ok(count) => {
                let remaining = self.sessions.recovery_needed().unwrap_or(true);
                self.ui.set_can_recover(remaining);
                self.emulator.borrow_mut().reload();
                self.push_emulator_config();
                self.update_session_labels();
                self.set_status(
                    0,
                    "Pronto",
                    &format!("{count} pendência(s) descartada(s)."),
                    "Sessões e diário foram preservados como .bak no ambiente privado.",
                );
                self.log_launcher(&format!("Pendências descartadas: {count}."));
            }
            Err(error) => {
                self.set_status(
                    3,
                    "Erro",
                    "Não foi possível descartar as pendências.",
                    &error.to_string(),
                );
            }
        }
    }

    fn recover(&self) {
        if self.runtime.borrow().running {
            self.set_status(
                2,
                "Em execução",
                "Não é possível recuperar uma sessão ativa.",
                "Feche o jogo e tente novamente.",
            );
            return;
        }
        if self.runtime.borrow().busy {
            return;
        }
        let manager = self.sessions.clone();
        let sender = self.event_sender.clone();
        self.set_busy(true);
        let cancel = self.begin_operation();
        self.set_status(
            1,
            "Recuperando",
            "Concluindo sessões interrompidas…",
            "Restaurando os requisitos temporários do ambiente privado.",
        );
        spawn_task(sender, move |tx| match manager.recover_all(&cancel) {
            Ok(count) => {
                let _ = tx.send(BgEvent::Recovered(count));
            }
            Err(error) => {
                let _ = report_worker_error(&tx, &cancel, error);
            }
        });
    }

    fn import_saves(&self) {
        if self.runtime.borrow().busy || self.runtime.borrow().running {
            self.set_status(
                1,
                "Ocupado",
                "Aguarde a operação atual.",
                "A importação de saves exige o launcher e o jogo parados.",
            );
            return;
        }
        if self.sessions.recovery_needed().unwrap_or(true) {
            self.set_status(
                2,
                "Atenção",
                "Conclua a recuperação antes de importar saves.",
                "Finalize a sessão interrompida e tente novamente.",
            );
            return;
        }
        let Some(source) = rfd::FileDialog::new()
            .set_title("Selecione a pasta CUSA01623 com somente saves")
            .pick_folder()
        else {
            return;
        };
        if source.file_name().and_then(|name| name.to_str()) != Some("CUSA01623") {
            self.set_status(
                3,
                "Erro",
                "A pasta selecionada não se chama CUSA01623.",
                "Escolha explicitamente a pasta de saves, não a instalação inteira.",
            );
            return;
        }
        let environment = self.sessions.environment.clone();
        let sender = self.event_sender.clone();
        self.set_busy(true);
        let cancel = self.begin_operation();
        self.set_status(
            1,
            "Importando",
            "Copiando saves para o ambiente privado…",
            "A origem permanece intacta; nenhum save é sincronizado de volta.",
        );
        spawn_task(sender, move |tx| match environment.import_saves(&source, &cancel) {
            Ok(count) => {
                let _ = tx.send(BgEvent::SavesImported(count));
            }
            Err(error) => {
                let _ = report_worker_error(&tx, &cancel, error);
            }
        });
    }

    fn open_data(&self) {
        self.open_folder(&self.data_root);
    }

    fn open_logs(&self) {
        let path = self.sessions.environment.log_dir();
        self.open_folder(&path);
    }

    fn open_session(&self) {
        let path = self
            .runtime
            .borrow()
            .current_session
            .clone()
            .or_else(|| {
                self.runtime
                    .borrow()
                    .latest_session
                    .as_ref()
                    .map(|summary| summary.path.clone())
            })
            .unwrap_or_else(|| self.sessions.sessions_dir.clone());
        self.open_folder(&path);
    }

    fn open_folder(&self, path: &Path) {
        let target = if path.exists() {
            path.to_path_buf()
        } else if path.file_name().and_then(|name| name.to_str()) == Some("log") {
            let _ = fs::create_dir_all(path);
            path.to_path_buf()
        } else {
            self.data_root.clone()
        };
        if let Err(error) = open::that(&target) {
            self.set_status(
                3,
                "Erro",
                "Não foi possível abrir a pasta.",
                &format!("{}: {error}", target.display()),
            );
        }
    }

    fn set_launch_mode(&self, mode: i32) {
        let mode = mode.clamp(0, 2) as u8;
        self.runtime.borrow_mut().launch_mode = mode;
        self.ui.set_launch_mode(mode as i32);
        let (label, detail) = match mode {
            0 => (
                "Modo normal selecionado.",
                "Usa cache e ajustes privados já preparados.",
            ),
            1 => (
                "Modo sem cache selecionado.",
                "O cache em disco será ignorado apenas na próxima sessão.",
            ),
            _ => (
                "Modo cache + espera selecionado.",
                "A captura será ativada somente nesta próxima sessão.",
            ),
        };
        self.set_status(0, "Pronto", label, detail);
    }

    fn set_capture(&self, enabled: bool) {
        self.runtime.borrow_mut().capture = enabled;
        self.ui.set_capture_enabled(enabled);
        self.set_status(
            0,
            "Pronto",
            if enabled {
                "Captura de diagnóstico ativada."
            } else {
                "Captura de diagnóstico desativada."
            },
            "A alteração vale para a próxima sessão.",
        );
    }

    fn set_observed(&self, enabled: bool) {
        if enabled && !self.runtime.borrow().selection.contains("fps_120") {
            self.set_status(
                2,
                "Atenção",
                "Ative o patch de 120 FPS primeiro.",
                "A referência observada exige o patch original de 120 FPS.",
            );
            return;
        }
        {
            let mut runtime = self.runtime.borrow_mut();
            runtime.vblank_override = if enabled { Some(60) } else { None };
        }
        self.after_selection_changed();
    }

    fn reject_if_busy(&self) -> bool {
        if self.runtime.borrow().busy {
            self.set_status(
                1,
                "Ocupado",
                "Aguarde a verificação ou preparação atual.",
                "Alterações de seleção são bloqueadas durante operações de arquivo.",
            );
            true
        } else {
            false
        }
    }

    fn refresh_all(&self) {
        self.refresh_selection();
        self.refresh_verification();
        self.update_session_labels();
    }

    fn refresh_selection(&self) {
        let selection = self.ordered_selection();
        let override_clock = self.runtime.borrow().vblank_override;
        let resolved = match self.catalog.resolve(&selection, override_clock) {
            Ok(resolved) => resolved,
            Err(error) => {
                self.set_status(3, "Erro", "Seleção inválida.", &error.to_string());
                return;
            }
        };
        let resolution_entries = self.resolution_entries();
        let has_resolution = selection.iter().any(|selected| {
            self.catalog.options.iter().any(|option| {
                &option.id == selected && option.group.as_deref() == Some("resolution")
            })
        });
        let selected_resolution = resolution_entries
            .iter()
            .position(|(id, _, _, _)| match id {
                Some(id) => selection.contains(id),
                None => !has_resolution,
            })
            .unwrap_or(0);
        self.ui.set_resolution_names(model(
            resolution_entries
                .iter()
                .map(|(_, title, _, _)| SharedString::from(title.as_str()))
                .collect::<Vec<_>>(),
        ));
        self.ui.set_resolution_index(selected_resolution as i32);
        self.ui.set_resolution_detail(
            resolution_entries
                .get(selected_resolution)
                .map(|(_, _, detail, _)| detail.clone())
                .unwrap_or_default()
                .into(),
        );

        for group in ["shadow_quality", "particle_rate"] {
            let options = self.catalog.options.iter().filter(|o| o.group.as_deref() == Some(group)).collect::<Vec<_>>();
            let index = options.iter().position(|o| selection.contains(&o.id)).map(|i| i + 1).unwrap_or(0) as i32;
            if group == "shadow_quality" { self.ui.set_shadow_index(index); } else { self.ui.set_emission_index(index); }
        }
        let features = self
            .feature_ids()
            .into_iter()
            .map(|id| {
                let selected = selection.contains(&id);
                let (title, subtitle, kind) = describe_feature(&id);
                ToggleItem {
                    title: title.into(),
                    subtitle: subtitle.into(),
                    badge: if id == "fps_120" && selected {
                        "120 Hz".into()
                    } else {
                        SharedString::new()
                    },
                    selected,
                    kind,
                    section: 0,
                    available: true,
                }
            })
            .collect::<Vec<_>>();
        self.ui.set_features(model(features));

        let experiments = self
            .experiment_ids()
            .into_iter()
            .map(|id| {
                let selected = selection.contains(&id);
                let (title, mut subtitle) = describe_experiment(&id);
                let mut candidate = selection.clone();
                if !selected { candidate.push(id.clone()); }
                let available = selected || self.catalog.resolve(&candidate, override_clock).is_ok();
                if !available {
                    if let Err(error) = self.catalog.resolve(&candidate, override_clock) {
                        subtitle.push_str(&format!(" Indisponível: {error}"));
                    }
                }
                let section = match id.as_str() {
                    "particles_soft_off" | "particles_distortion_off" | "particle_halfres" => 1,
                    "job_queue_pause" | "geometry_weights_simd" | "native_lod_75" => 2,
                    _ => 0,
                };
                ToggleItem {
                    title: title.into(),
                    subtitle: subtitle.into(),
                    badge: "Experimental".into(),
                    selected,
                    kind: 2,
                    section,
                    available,
                }
            })
            .collect::<Vec<_>>();
        self.ui.set_experiments(model(experiments));

        self.ui
            .set_summary_vblank(format!("{} Hz", resolved.vblank).into());
        self.ui
            .set_summary_dmem(format!("{} MB", resolved.min_dmem).into());
        self.ui
            .set_summary_patches(resolved.enabled.len().to_string().into());
        self.ui
            .set_summary_profile(self.runtime.borrow().profile.clone().into());
        self.ui
            .set_fps_120_selected(selection.iter().any(|id| id == "fps_120"));
        self.ui.set_observed_clock(
            override_clock == Some(60) && selection.iter().any(|id| id == "fps_120"),
        );
        self.refresh_profiles();
        self.refresh_paths_and_steps();
    }

    fn refresh_profiles(&self) {
        let mut names = vec!["Personalizado".to_string()];
        names.extend(self.store.borrow().all_profile_names(&self.catalog));
        let profile = self.runtime.borrow().profile.clone();
        let index = names.iter().position(|name| name == &profile).unwrap_or(0) as i32;
        self.ui.set_profile_names(model(
            names
                .iter()
                .map(|name| SharedString::from(profile_label(name)))
                .collect::<Vec<_>>(),
        ));
        self.ui.set_profile_index(index);
        self.ui.set_summary_profile(profile.clone().into());
        let blurb = if profile == "Personalizado" {
            "Sua seleção atual não coincide com nenhum perfil salvo. Salve-a para reutilizar."
                .to_string()
        } else if self.catalog.profiles.contains_key(&profile) {
            Catalog::custom_profile_blurb(&profile)
        } else {
            "Perfil personalizado salvo por você. As alterações atuais ainda não foram salvas."
                .to_string()
        };
        self.ui.set_profile_blurb(blurb.into());
    }

    fn refresh_paths_and_steps(&self) {
        let ready = self.paths_ready();
        self.ui.set_setup_complete(ready);
        self.ui.set_launch_label(
            if ready {
                "Iniciar jogo"
            } else {
                "Configurar caminhos"
            }
            .into(),
        );
        self.refresh_steps();
    }

    fn refresh_steps(&self) {
        let paths_done = self.paths_ready();
        let verified_done = self
            .runtime
            .borrow()
            .verified_signature
            .as_ref()
            .map(|signature| signature == &self.verification_signature())
            .unwrap_or(false);
        let session_done = self.runtime.borrow().latest_session.is_some();
        let completed = [paths_done, verified_done, session_done]
            .iter()
            .filter(|done| **done)
            .count();
        self.ui.set_readiness(completed as i32);
        self.ui
            .set_readiness_label(format!("{completed} de 3 etapas").into());
        let steps = vec![
            StepItem {
                title: "Informar os caminhos".into(),
                detail: if paths_done {
                    "Jogo, atualização e emulador configurados".into()
                } else {
                    "Jogo base, atualização e executável do shadPS4".into()
                },
                done: paths_done,
            },
            StepItem {
                title: "Verificar os arquivos".into(),
                detail: if verified_done {
                    "Eboot, SFO, patches e build conferidos".into()
                } else {
                    "Confirme a revisão 01.02 e os bytes originais".into()
                },
                done: verified_done,
            },
            StepItem {
                title: "Iniciar a primeira sessão".into(),
                detail: if session_done {
                    self.runtime
                        .borrow()
                        .latest_session
                        .as_ref()
                        .map(session_status_short)
                        .unwrap_or_else(|| "Sessão privada preparada".to_string())
                        .into()
                } else {
                    "O ambiente privado cuida dos saves e caches".into()
                },
                done: session_done,
            },
        ];
        self.ui.set_steps(model(steps));
    }

    fn refresh_verification(&self) {
        let runtime = self.runtime.borrow();
        let (label, short) = match (&runtime.verified_signature, &runtime.verified_label) {
            (Some(signature), Some(label)) if signature == &self.verification_signature() => {
                (label.clone(), "Verificado".to_string())
            }
            (Some(_), Some(_)) => (
                "Verificação desatualizada".to_string(),
                "Desatualizado".to_string(),
            ),
            _ => (INITIAL_VERIFICATION.to_string(), "Pendente".to_string()),
        };
        self.ui.set_verification_label(label.into());
        self.ui.set_verification_short(short.into());
    }

    fn set_memory(&self, memory: Option<MemoryStatus>) {
        match memory {
            Some(memory) => {
                let gib = 1024f64 * 1024f64 * 1024f64;
                self.ui.set_memory_label(memory.compact_label().into());
                self.ui.set_memory_short(
                    format!("{:.1} GiB", memory.physical_available as f64 / gib).into(),
                );
                self.ui.set_memory_detail(
                    format!(
                        "RAM livre · commit {:.1} GiB",
                        memory.commit_available as f64 / gib
                    )
                    .into(),
                );
            }
            None => {
                self.ui.set_memory_label("Memória não disponível".into());
                self.ui.set_memory_short("—".into());
                self.ui
                    .set_memory_detail("Consulta indisponível neste sistema".into());
            }
        }
    }

    fn refresh_latest_session(&self) {
        let generation = {
            let mut runtime = self.runtime.borrow_mut();
            runtime.session_refresh_generation += 1;
            runtime.session_refresh_generation
        };
        let sessions = self.sessions.clone();
        spawn_task(self.event_sender.clone(), move |sender| {
            let _ = sender.send(BgEvent::SessionSummary {
                generation,
                summary: sessions.latest_summary(),
            });
        });
    }

    fn update_session_labels(&self) {
        let summary = self.runtime.borrow().latest_session.clone();
        let running = self.runtime.borrow().running;
        if let Some(summary) = summary {
            self.ui
                .set_last_session_label(format_session_label(&summary).into());
            if !running {
                self.ui.set_session_meta(
                    format!("{} · {}", summary.profile, session_status(&summary)).into(),
                );
            }
        } else {
            self.ui
                .set_last_session_label("Nenhuma sessão nesta instalação.".into());
            if !running {
                self.ui.set_session_meta("Nenhuma execução".into());
            }
        }
    }

    fn refresh_can_recover(&self) {
        let can = self.sessions.recovery_needed().unwrap_or(true);
        self.runtime.borrow_mut().can_recover = can;
        self.ui.set_can_recover(can);
    }

    fn initial_profile_name(
        &self,
        settings: &Settings,
        selection: &BTreeSet<String>,
        override_clock: Option<u32>,
    ) -> String {
        let requested = settings.profile.trim();
        if requested == "Personalizado" {
            return "Personalizado".into();
        }
        if !requested.is_empty()
            && self
                .store
                .borrow()
                .profile(requested, &self.catalog)
                .is_some()
            && self.profile_matches(requested, &self.selection_vec(selection), override_clock)
        {
            return requested.to_string();
        }
        self.find_matching_profile(&self.selection_vec(selection), override_clock)
            .unwrap_or_else(|| "Personalizado".into())
    }

    fn profile_matches(
        &self,
        name: &str,
        selection: &[String],
        override_clock: Option<u32>,
    ) -> bool {
        let Some(profile) = self.store.borrow().profile(name, &self.catalog) else {
            return false;
        };
        let current = selection.iter().cloned().collect::<BTreeSet<_>>();
        let expected = profile.into_iter().collect::<BTreeSet<_>>();
        current == expected && self.profile_override(name) == override_clock
    }

    fn find_matching_profile(
        &self,
        selection: &[String],
        override_clock: Option<u32>,
    ) -> Option<String> {
        let current = selection.iter().cloned().collect::<BTreeSet<_>>();
        let names = self.store.borrow().all_profile_names(&self.catalog);
        for name in names {
            if self.profile_matches(&name, &self.selection_vec(&current), override_clock) {
                return Some(name);
            }
        }
        None
    }

    fn selection_vec(&self, selection: &BTreeSet<String>) -> Vec<String> {
        self.catalog
            .options
            .iter()
            .filter(|option| selection.contains(&option.id))
            .map(|option| option.id.clone())
            .collect()
    }

    fn ordered_selection(&self) -> Vec<String> {
        self.selection_vec(&self.runtime.borrow().selection)
    }

    fn profile_override(&self, name: &str) -> Option<u32> {
        if self.catalog.profiles.contains_key(name) {
            self.catalog.profile_override(name)
        } else {
            None
        }
    }

    fn resolution_ids(&self) -> Vec<Option<String>> {
        vec![
            None,
            Some("texture_fix".into()),
            Some("res_480".into()),
            Some("res_720".into()),
            Some("res_1440".into()),
            Some("res_1800".into()),
            Some("res_4k".into()),
        ]
    }

    fn resolution_entries(&self) -> Vec<(Option<String>, String, String, bool)> {
        self.resolution_ids()
            .into_iter()
            .filter_map(|id| {
                let (title, detail, recommended) = match &id {
                    None => (
                        "Original 1080p",
                        "Sem patch de resolução; usa a arena padrão do jogo.",
                        false,
                    ),
                    Some(id) if id == "texture_fix" => (
                        "Correção de texturas",
                        "Arena ampliada em +2048 MB. Substitui os patches de resolução.",
                        false,
                    ),
                    Some(id) if id == "res_480" => ("480p", "DMEM extra ≥ 2048 MB.", false),
                    Some(id) if id == "res_720" => (
                        "720p",
                        "DMEM extra ≥ 2048 MB. Interações com efeitos ainda em investigação.",
                        true,
                    ),
                    Some(id) if id == "res_1440" => ("1440p", "DMEM extra ≥ 3072 MB.", false),
                    Some(id) if id == "res_1800" => ("1800p", "DMEM extra ≥ 4608 MB.", false),
                    Some(id) if id == "res_4k" => ("4K", "DMEM extra ≥ 6144 MB.", false),
                    _ => return None,
                };
                if let Some(id) = &id {
                    if !self.catalog.by_id.contains_key(id) {
                        return None;
                    }
                }
                Some((id, title.to_string(), detail.to_string(), recommended))
            })
            .collect()
    }

    fn feature_ids(&self) -> Vec<String> {
        ["fps_120", "skip_intro", "skip_legacy"]
            .into_iter()
            .filter(|id| self.catalog.by_id.contains_key(*id))
            .map(str::to_string)
            .collect()
    }

    fn experiment_ids(&self) -> Vec<String> {
        self.catalog
            .options
            .iter()
            .filter(|option| option.experimental && option.group.is_none())
            .map(|option| option.id.clone())
            .collect()
    }

    fn paths_ready(&self) -> bool {
        !self.ui.get_base_path().trim().is_empty()
            && !self.ui.get_update_path().trim().is_empty()
            && !self.ui.get_executable_path().trim().is_empty()
    }

    fn verification_signature(&self) -> String {
        let selection = self.ordered_selection().join(",");
        format!(
            "{}\n{}\n{}\n{}\n{:?}",
            self.ui.get_base_path().trim(),
            self.ui.get_update_path().trim(),
            self.ui.get_executable_path().trim(),
            selection,
            self.runtime.borrow().vblank_override
        )
    }

    /// Enum models come from the Rust tables so the labels shown never drift
    /// from the values actually written to the game config.
    fn setup_emulator_models(&self) {
        // Windows-only options follow the build target.
        self.ui.set_cfg_windows(cfg!(windows));
        self.ui.set_cfg_languages(model(
            emulator_config::LANGUAGES
                .iter()
                .map(|(name, _)| SharedString::from(*name))
                .collect(),
        ));
        self.ui
            .set_cfg_audio_backends(model(labels(&emulator_config::AUDIO_BACKENDS)));
        self.ui
            .set_cfg_display_modes(model(labels(&emulator_config::DISPLAY_MODES)));
        self.ui
            .set_cfg_present_modes(model(labels(&emulator_config::PRESENT_MODES)));
        self.ui.set_cfg_anti_aliasing_modes(model(
            emulator_config::ANTI_ALIASING
                .iter()
                .map(|(name, _)| SharedString::from(*name))
                .collect(),
        ));
        self.ui
            .set_cfg_upscalers(model(labels(&emulator_config::UPSCALERS)));
        self.ui
            .set_cfg_sharpenings(model(labels(&emulator_config::SHARPENING)));
        self.ui
            .set_cfg_cursor_states(model(labels(&emulator_config::CURSOR_STATES)));
        self.ui
            .set_cfg_readbacks_modes(model(labels(&emulator_config::READBACKS)));
        self.ui
            .set_cfg_red_zones(model(labels(&emulator_config::RED_ZONE)));
    }

    fn push_emulator_config(&self) {
        let values = self.emulator.borrow().values.clone();
        let ui = &self.ui;
        ui.set_cfg_console_language(emulator_config::language_index(values.console_language) as i32);
        ui.set_cfg_volume_value(values.volume as f32);
        ui.set_cfg_show_splash(values.show_splash);
        ui.set_cfg_audio_backend(values.audio_backend as i32);
        ui.set_cfg_display_mode(values.display_mode as i32);
        ui.set_cfg_present_mode(values.present_mode as i32);
        ui.set_cfg_window_width(values.window_width.to_string().into());
        ui.set_cfg_window_height(values.window_height.to_string().into());
        ui.set_cfg_hdr(values.hdr);
        ui.set_cfg_anti_aliasing(values.anti_aliasing as i32);
        ui.set_cfg_upscaler(values.upscaler as i32);
        ui.set_cfg_sharpening(values.sharpening as i32);
        ui.set_cfg_rcas(format!("{:.2}", values.rcas_attenuation).into());
        ui.set_cfg_motion_controls(values.motion_controls);
        ui.set_cfg_background_controller(values.background_controller);
        ui.set_cfg_cursor_state(values.cursor_state as i32);
        ui.set_cfg_cursor_timeout(values.cursor_hide_timeout.to_string().into());
        ui.set_cfg_extra_dmem(values.extra_dmem.to_string().into());
        ui.set_cfg_app0_bandwidth(values.app0_bandwidth.to_string().into());
        ui.set_cfg_app0_time_stretching(values.app0_disable_time_stretching);
        ui.set_cfg_readbacks(values.readbacks_mode as i32);
        ui.set_cfg_readback_linear(values.readback_linear_images);
        ui.set_cfg_direct_memory(values.direct_memory_access);
        ui.set_cfg_nv_raw_access(values.nv_raw_access_chains);
        ui.set_cfg_force_ubo(values.force_uniform_buffers);
        ui.set_cfg_red_zone(values.red_zone_protection as i32);
        ui.set_cfg_devkit(values.dev_kit_mode);
        ui.set_cfg_neo(values.neo_mode);
        ui.set_cfg_shadnet(values.shad_net);
        ui.set_cfg_network_connected(values.connected_to_network);
        ui.set_cfg_pipeline_cache(values.pipeline_cache_enabled);
        ui.set_cfg_pipeline_cache_archived(values.pipeline_cache_archived);

        let bandwidth = if values.app0_bandwidth == 0 {
            "Ilimitado".to_string()
        } else {
            format!("{} MiB/s", values.app0_bandwidth)
        };
        ui.set_cfg_app0_detail(bandwidth.into());

        // The patch profile can raise the DMEM minimum above the chosen value.
        let min_dmem = self
            .catalog
            .resolve(
                &self.ordered_selection(),
                self.runtime.borrow().vblank_override,
            )
            .map(|resolved| resolved.min_dmem)
            .unwrap_or(0);
        let chosen = values.extra_dmem.max(0) as u64;
        let effective = chosen.max(min_dmem);
        let detail = if effective > chosen {
            format!("Efetivo: {effective} MB — o perfil exige ao menos {min_dmem} MB.")
        } else {
            format!("DMEM efetivo: {effective} MB.")
        };
        ui.set_cfg_dmem_detail(detail.into());
    }

    /// O slider entrega um float; o valor é arredondado antes de ser gravado.

    /// Cada checkpoint regrava o save; a área do cartão acompanha esse carimbo.
    fn poll_saved_zone(&self) {
        let root = self.sessions.environment.save_dir();
        // Uma leitura parcial não consome o carimbo: a área não congela.
        let Some(code) = self.save_watch.borrow_mut().poll(&root) else {
            return;
        };
        let label = presence::zone_label(&code);
        if self.zone.borrow().as_deref() == Some(label.as_str()) {
            return;
        }
        *self.zone.borrow_mut() = Some(label);
        self.refresh_presence();
    }

    /// Publica ou limpa a atividade do Discord. Só existe presença enquanto o
    /// processo do jogo está vivo: nem o launcher aberto, nem a janela fechada,
    /// nem o jogo encerrado deixam cartão no perfil.
    fn refresh_presence(&self) {
        let allowed = presence_allowed(
            self.ui.get_discord_presence(),
            self.presence_live.get(),
            self.runtime.borrow().running,
            self.game_pid.get(),
            self.window_hidden.get(),
        );
        if !allowed {
            self.presence.clear();
            return;
        }
        // Sem save legível o cartão fica sem área; nunca um nome inventado.
        let phase = self.zone.borrow().clone().unwrap_or_default();
        self.presence.publish(Presence::new(
            phase,
            self.trophy_progress(),
            self.presence_started.get(),
        ));
    }

    /// "Troféus 12 de 35 · 34%": a linha de progresso do cartão.
    fn trophy_progress(&self) -> String {
        let trophies = self.trophies.borrow();
        let trophies::State::Loaded(collection) = &*trophies else {
            return String::new();
        };
        let total = collection.total();
        if total == 0 {
            return String::new();
        }
        presence::trophy_progress_text(collection.unlocked(), total)
    }

    /// Relê os troféus por pedido explícito (botão de recarregar).
    fn refresh_trophies(&self) {
        self.reload_trophies(true);
    }

    /// Ao abrir a página: relê apenas quando algum arquivo mudou de fato.
    fn refresh_trophies_if_stale(&self) {
        self.reload_trophies(false);
    }

    /// Entrada histórica de recarga (nome preservado do snapshot): aplica a
    /// mesma regra de antes — nunca durante uma sessão, só relê quando a
    /// assinatura mudou, a menos que `force` — e entrega a leitura ao mesmo
    /// caminho de carga usado pelo resto do app.
    fn reload_trophies(&self, force: bool) {
        self.request_trophies(force);
    }

    /// Lê primeiro o que o núcleo extraiu e depois o pacote do jogo, sempre fora
    /// da interface: nem a abertura do launcher nem a da página esperam a
    /// descriptografia nem a decodificação dos ícones. Nunca lê durante uma
    /// sessão, porque o núcleo pode estar gravando o progresso.
    fn request_trophies(&self, force: bool) {
        if self.runtime.borrow().running {
            self.ui.set_trophy_refresh_enabled(false);
            return;
        }
        let environment = PrivateEnvironment::new(&self.data_root);
        let user_dir = environment.user_dir();
        let cache_dir = self.data_root.join("cache").join("trophies");
        let base = self.ui.get_base_path().trim().to_string();
        let language = self.emulator.borrow().values.console_language;
        let game_base = (!base.is_empty()).then(|| PathBuf::from(base));
        let stamp = {
            let sources = trophies::Sources {
                user_dir: &user_dir,
                cache_dir: &cache_dir,
                game_base: game_base.clone(),
                language,
            };
            trophies::signature(&sources)
        };
        if !force && self.trophy_stamp.borrow().as_ref() == Some(&stamp) {
            return;
        }
        if self.trophy_loading.get() == Some(stamp) {
            return;
        }
        self.trophy_loading.set(Some(stamp));
        spawn_task(self.event_sender.clone(), move |sender| {
            let sources = trophies::Sources {
                user_dir: &user_dir,
                cache_dir: &cache_dir,
                game_base,
                language,
            };
            let state = trophies::load(&sources);
            let icons = decode_trophy_icons(&state);
            let _ = sender.send(BgEvent::Trophies(TrophySnapshot {
                stamp,
                state,
                icons,
            }));
        });
    }

    /// Publica contagens, legenda e a lista filtrada para a interface.
    fn push_trophies(&self) {
        let filter = self.trophy_filter.get();
        let mut items: Vec<TrophyItem> = Vec::new();
        let mut subtitle = String::new();
        let mut labels = (
            String::from("—"),
            String::from("—"),
            String::from("—"),
            String::from("—"),
        );
        let empty;

        match &*self.trophies.borrow() {
            trophies::State::Loaded(collection) => {
                let total = collection.total();
                let unlocked = collection.unlocked();
                labels = (
                    total.to_string(),
                    unlocked.to_string(),
                    (total - unlocked).to_string(),
                    if total == 0 {
                        String::from("—")
                    } else {
                        format!("{}%", unlocked * 100 / total)
                    },
                );
                subtitle = format!("{} · {}", collection.np_comm_id, collection.xml_name);
                empty = match filter {
                    1 => "Nenhum troféu desbloqueado ainda.".to_string(),
                    2 => "Nenhum troféu bloqueado: a coleção está completa.".to_string(),
                    _ => "Nenhum troféu para mostrar.".to_string(),
                };
                let mut icons = self.trophy_icons.borrow_mut();
                for trophy in &collection.trophies {
                    if (filter == 1 && !trophy.unlocked) || (filter == 2 && trophy.unlocked) {
                        continue;
                    }
                    push_trophy_item(&mut items, trophy, &mut icons);
                }
            }
            trophies::State::NoKey => {
                subtitle = String::from("Pacote do jogo encontrado");
                empty = String::from(
                    "Falta a chave de troféus em ambiente/user/keys.json para ler as definições do pacote.",
                );
            }
            trophies::State::Empty { message } => empty = message.clone(),
            trophies::State::Invalid { message } => empty = message.clone(),
        }

        self.ui.set_trophy_filter(filter);
        self.ui.set_trophy_subtitle(subtitle.into());
        self.ui.set_trophy_empty(empty.into());
        self.ui.set_trophy_total_label(labels.0.into());
        self.ui.set_trophy_unlocked_label(labels.1.into());
        self.ui.set_trophy_locked_label(labels.2.into());
        self.ui.set_trophy_percent_label(labels.3.into());
        self.ui.set_trophy_items(ModelRc::new(VecModel::from(items)));
    }

    fn apply_volume(&self, value: f32) {
        let level = value.round().clamp(0.0, 500.0) as i64;
        let text = level.to_string();
        self.apply_emulator_change(emulator_config::VOLUME, Change::Text(&text));
    }

    fn apply_emulator_change(&self, field: &str, change: Change<'_>) {
        if self.runtime.borrow().busy || self.runtime.borrow().running {
            self.push_emulator_config();
            self.set_status(
                1,
                "Ocupado",
                "Aguarde a operação atual.",
                "A configuração não pode ser alterada durante uma sessão ou preparação.",
            );
            return;
        }
        if self.sessions.recovery_needed().unwrap_or(true) {
            self.push_emulator_config();
            self.set_status(
                2,
                "Atenção",
                "Conclua a recuperação antes de alterar.",
                "Há uma sessão ou configuração temporária pendente no ambiente privado.",
            );
            return;
        }
        let result = self.emulator.borrow_mut().apply(field, change);
        self.push_emulator_config();
        match result {
            Ok(()) => {
                if field == emulator_config::CONSOLE_LANGUAGE {
                    // O idioma do console também é o idioma do launcher.
                    self.apply_language();
                    self.request_trophies(false);
                }
                self.set_status(
                    0,
                    "Pronto",
                    "Configuração atualizada.",
                    "As opções valem no próximo lançamento.",
                )
            }
            Err(error) => self.set_status(
                3,
                "Erro",
                "Não foi possível aplicar a opção.",
                &error.to_string(),
            ),
        }
    }

    /// Idioma efetivo: escolha manual ou o idioma do console do emulador.
    fn setup_language(&self, settings: &Settings) {
        let names = {
            let mut names = vec![SharedString::from("Automático (segue o console)")];
            names.extend(
                i18n::LANGUAGES
                    .iter()
                    .map(|(label, _)| SharedString::from(*label)),
            );
            names
        };
        self.ui.set_launcher_language_names(model(names));
        self.ui
            .set_launcher_language(settings.launcher_language.min(5) as i32);
        self.apply_language();
    }

    fn apply_language(&self) {
        // Sem a configuração do emulador não há "idioma do console": o modo
        // automático abre no idioma base do launcher, português, em vez de
        // herdar o padrão interno do emulador (inglês).
        let console = {
            let emulator = self.emulator.borrow();
            if emulator.exists() {
                emulator.values.console_language
            } else {
                -1
            }
        };
        let language = i18n::resolve(self.ui.get_launcher_language(), console);
        self.language.set(language);
        i18n::apply(&self.ui, language);
    }

    fn set_launcher_language(&self, index: i32) {
        self.ui.set_launcher_language(index.clamp(0, 5));
        self.apply_language();
        if let Err(error) = self.persist_settings() {
            self.set_status(
                3,
                "Erro",
                "Não foi possível salvar o idioma.",
                &error.to_string(),
            );
        }
    }

    /// Pasta de builds do próprio launcher (`data/builds`), criada por ele.
    /// Não existe campo para escolher: a pasta é sempre criada e usada pelo app.
    fn setup_builds(&self) {
        let directory = self.data_root.join("builds");
        // A pasta pode vir com o prefixo verbatim do Windows; o app guarda
        // o caminho limpo e nunca o repassa à interface.
        let directory = strip_verbatim(directory);
        if let Err(error) = builds::ensure_dir(&directory) {
            self.ui.set_builds_status(format!("{error:#}").into());
        }
        *self.builds_dir.borrow_mut() = directory;
        self.refresh_builds();
    }

    fn refresh_builds(&self) {
        let directory = self.builds_dir.borrow().clone();
        let (local_error, entries) = match builds::list(&directory) {
            Ok(listed) => (None, listed),
            Err(error) => (Some(format!("{error:#}")), Vec::new()),
        };
        let (remote_all, checking) = {
            let state = self.update_state.borrow();
            (state.remote.clone(), state.checking)
        };
        let remote_only = remote_all
            .into_iter()
            .filter(|remote| {
                let sanitized = updates::sanitize(&remote.name);
                !entries.iter().any(|local| {
                    local.name.eq_ignore_ascii_case(&remote.name)
                        || local.name.eq_ignore_ascii_case(&sanitized)
                })
            })
            .collect::<Vec<_>>();
        let downloading_info = self.downloading_build.borrow().clone();
        let any_downloading = downloading_info.is_some();
        let status = if let Some((name, downloaded, total)) = &downloading_info {
            format!("Baixando {name} ({})…", download_progress_text(*downloaded, *total))
        } else if let Some(error) = local_error {
            error
        } else {
            match (entries.len(), remote_only.len()) {
                (0, 0) if checking => "Consultando builds no GitHub…".to_string(),
                (0, 0) => "Nenhuma build encontrada.".to_string(),
                (0, 1) => "1 build disponível no GitHub.".to_string(),
                (0, r) => format!("{r} builds disponíveis no GitHub."),
                (1, 0) => "1 build instalada.".to_string(),
                (l, 0) => format!("{l} builds instaladas."),
                (1, 1) => "1 build instalada · 1 disponível no GitHub.".to_string(),
                (l, r) => format!("{l} builds instaladas · {r} disponíveis no GitHub."),
            }
        };
        let selected = self.ui.get_executable_path().trim().to_string();
        let mut items = entries
            .iter()
            .map(|entry| BuildItem {
                name: entry.name.clone().into(),
                detail: format!(
                    "{} · {}",
                    entry.size_label(),
                    modified_label(entry.modified)
                )
                .into(),
                version: entry.version_label().into(),
                installed: true,
                selected: !selected.is_empty() && entry.path == Path::new(&selected),
                downloading: false,
                progress: 0.0,
                progress_label: SharedString::default(),
            })
            .collect::<Vec<_>>();
        for remote in &remote_only {
            let active_dl = downloading_info
                .as_ref()
                .filter(|(name, _, _)| name.eq_ignore_ascii_case(&remote.name));
            let (downloading, progress, progress_label) = match active_dl {
                Some((_, downloaded, total)) => {
                    let ratio = if *total > 0 {
                        (*downloaded as f64 / *total as f64).clamp(0.0, 1.0) as f32
                    } else {
                        0.0
                    };
                    (true, ratio, download_progress_text(*downloaded, *total).into())
                }
                None => (false, 0.0, SharedString::default()),
            };
            items.push(BuildItem {
                name: remote.name.clone().into(),
                detail: format!("{} · Disponível no GitHub", remote.size_label()).into(),
                version: remote.version_label().into(),
                installed: false,
                selected: false,
                downloading,
                progress,
                progress_label,
            });
        }
        let label = Path::new(&selected)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        self.ui.set_selected_build(label.into());
        self.ui.set_builds_status(status.into());
        self.ui.set_any_build_downloading(any_downloading);
        self.ui.set_builds(ModelRc::new(VecModel::from(items)));
        *self.builds.borrow_mut() = entries;
        *self.remote_builds.borrow_mut() = remote_only;
    }

    fn pick_build(&self, index: i32) {
        if self.runtime.borrow().busy || self.runtime.borrow().running {
            self.set_status(
                1,
                "Ocupado",
                "Aguarde a operação atual.",
                "A build não pode ser trocada durante uma sessão ou preparação.",
            );
            return;
        }
        let idx = index.max(0) as usize;
        let Some(entry) = self.builds.borrow().get(idx).cloned() else {
            return;
        };
        self.ui
            .set_executable_path(entry.path.display().to_string().into());
        self.refresh_builds();
        self.on_path_changed();
        self.set_status(
            0,
            "Pronto",
            "Build selecionada.",
            &format!("{} · {}", entry.name, entry.size_label()),
        );
        self.log_launcher(&format!("Build selecionada: {}", entry.path.display()));
    }

    fn download_build_at(&self, index: i32) {
        if self.runtime.borrow().busy || self.runtime.borrow().running {
            self.set_status(
                1,
                "Ocupado",
                "Aguarde a operação atual.",
                "O download espera a sessão terminar.",
            );
            return;
        }
        let idx = index.max(0) as usize;
        let local_len = self.builds.borrow().len();
        if idx < local_len {
            return;
        }
        let Some(remote) = self.remote_builds.borrow().get(idx - local_len).cloned() else {
            return;
        };
        self.download_remote_build(remote);
    }

    fn download_remote_build(&self, build: updates::RemoteBuild) {
        if self.update_state.borrow().installing {
            return;
        }
        let directory = self.builds_dir.borrow().clone();
        if let Err(error) = builds::ensure_dir(&directory) {
            self.set_status(
                3,
                "Erro",
                "Não foi possível criar a pasta de builds.",
                &format!("{error:#}"),
            );
            return;
        }
        self.update_state.borrow_mut().installing = true;
        *self.downloading_build.borrow_mut() = Some((build.name.clone(), 0, build.size));
        self.ui.set_any_build_downloading(true);
        self.ui
            .set_update_status(format!("Baixando {}…", build.name).into());
        self.ui.set_update_downloadable(false);
        self.refresh_builds();
        self.set_status(1, "Baixando", &format!("Baixando {}…", build.name), &build.url);
        let build_name = build.name.clone();
        let sender = self.event_sender.clone();
        spawn_task(sender, move |sender| {
            let mut last_emit = Instant::now() - Duration::from_millis(200);
            let result = updates::download_build_with_progress(&build, &directory, |downloaded, total| {
                let now = Instant::now();
                if downloaded == 0
                    || (total > 0 && downloaded >= total)
                    || now.duration_since(last_emit) >= Duration::from_millis(80)
                {
                    last_emit = now;
                    let _ = sender.send(BgEvent::DownloadProgress {
                        name: build_name.clone(),
                        downloaded,
                        total,
                    });
                }
            });
            let _ = sender.send(BgEvent::UpdateInstalled {
                result: result
                    .map(|path| path.display().to_string())
                    .map_err(|error| format!("{error:#}")),
            });
        });
    }

    fn update_store_path(&self) -> PathBuf {
        self.data_root.join("updates.json")
    }

    fn setup_updates(&self, settings: &Settings) {
        // Repositório fixo do launcher: nenhum campo na interface.
        let repository = updates::DEFAULT_REPOSITORY.to_string();
        self.ui.set_auto_update_check(settings.auto_update_check);
        {
            let mut state = self.update_state.borrow_mut();
            state.repo = repository;
            if let Ok(value) = crate::storage::read_json(&self.update_store_path()) {
                let stored: StoredUpdate = serde_json::from_value(value).unwrap_or_default();
                // Cache de outro repositório (o upstream, por exemplo) é ignorado:
                // só valem dados gravados para o repositório configurado agora.
                if updates::same_repository(&stored.repository, &state.repo) {
                    state.skipped = stored.skipped;
                    state.checked_at = stored.checked_at;
                    state.seen_sha = stored.latest_sha;
                    if !stored.latest_tag.is_empty() {
                        state.release = Some(Release {
                            tag: stored.latest_tag,
                            name: stored.latest_name,
                            published: stored.latest_published,
                            asset_name: String::new(),
                            asset_url: String::new(),
                        });
                    }
                }
            }
        }
        self.last_update_check.set(None);
        self.publish_update();
    }

    fn persist_update_state(&self) {
        let stored = {
            let state = self.update_state.borrow();
            StoredUpdate {
                repository: state.repo.clone(),
                skipped: state.skipped.clone(),
                latest_sha: state.seen_sha.clone(),
                latest_tag: state
                    .release
                    .as_ref()
                    .map(|release| release.tag.clone())
                    .unwrap_or_default(),
                latest_name: state
                    .release
                    .as_ref()
                    .map(|release| release.name.clone())
                    .unwrap_or_default(),
                latest_published: state
                    .release
                    .as_ref()
                    .map(|release| release.published.clone())
                    .unwrap_or_default(),
                checked_at: state.checked_at.clone(),
            }
        };
        let _ = crate::storage::write_json(&self.update_store_path(), &stored);
    }

    /// Consulta no máximo a cada seis horas, e só quando a preferência está ligada.
    fn poll_updates(&self) {
        if !self.ui.get_auto_update_check() {
            return;
        }
        {
            let state = self.update_state.borrow();
            if state.checking || state.installing {
                return;
            }
        }
        let due = self
            .last_update_check
            .get()
            .is_none_or(|checked| checked.elapsed() >= Duration::from_secs(6 * 60 * 60));
        if due {
            self.check_updates();
        }
    }

    fn check_updates(&self) {
        if self.update_state.borrow().checking {
            return;
        }
        let repo = updates::DEFAULT_REPOSITORY.to_string();
        if updates::repository_slug(&repo).is_err() {
            self.ui
                .set_update_status("Informe o repositório no formato usuário/repositório.".into());
            return;
        }
        self.update_state.borrow_mut().checking = true;
        self.last_update_check.set(Some(Instant::now()));
        self.ui.set_builds_checking(true);
        self.ui
            .set_update_status(format!("Consultando {repo}…").into());
        self.refresh_builds();
        let sender = self.event_sender.clone();
        spawn_task(sender, move |sender| {
            let release = updates::latest_release(&repo);
            let commit = updates::latest_commit(&repo);
            let remote = updates::remote_builds(&repo);
            let error = release
                .as_ref()
                .err()
                .or_else(|| commit.as_ref().err())
                .map(|error| format!("{error:#}"));
            let _ = sender.send(BgEvent::UpdateChecked {
                repo: repo.clone(),
                release: release.ok().flatten(),
                commit: commit.ok(),
                remote: remote.unwrap_or_default(),
                error,
            });
        });
    }

    fn installed_version_label(&self) -> String {
        if let Some(version) = builds::highest_version(&self.builds.borrow()) {
            return version.label();
        }
        let selected = self.ui.get_executable_path().trim().to_string();
        Path::new(&selected)
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(builds::version_from_name)
            .map(|version| version.label())
            .unwrap_or_default()
    }

    fn publish_update(&self) {
        let (available, title, detail, status, downloadable) = {
            let state = self.update_state.borrow();
            let installed = self.installed_version_label();
            let installed_text = if installed.is_empty() {
                "nenhuma build baixada".to_string()
            } else {
                format!("v{installed}")
            };
            let mut available = false;
            let mut title = String::new();
            let detail;
            let mut downloadable = false;
            let mut status;
            if let Some(release) = state.release.as_ref() {
                let newer = updates::is_newer(&release.tag, &installed);
                available = newer && !state.skipped.eq_ignore_ascii_case(&release.tag);
                title = format!(
                    "Publicação nova em {}: v{}",
                    state.repo,
                    release.version_label()
                );
                detail = format!(
                    "Publicada {} · instalada {} · {}",
                    release.published,
                    installed_text,
                    published_builds(state.remote.len())
                );
                downloadable = !release.asset_url.is_empty() || !state.remote.is_empty();
                status = if available {
                    format!("Atualização disponível: v{}.", release.version_label())
                } else {
                    format!("Publicação mais recente: v{}.", release.version_label())
                };
            } else if let Some(commit) = state.commit.as_ref() {
                let newer =
                    !state.seen_sha.is_empty() && !commit.sha.eq_ignore_ascii_case(&state.seen_sha);
                available = newer && !state.skipped.eq_ignore_ascii_case(&commit.sha);
                title = format!(
                    "Atualização nova em {}: {}",
                    state.repo,
                    commit.short_message()
                );
                detail = format!(
                    "Commit {} · {} · {}",
                    short_sha(&commit.sha),
                    commit_date(&commit.date),
                    published_builds(state.remote.len())
                );
                downloadable = !state.remote.is_empty();
                status = if newer {
                    format!(
                        "O repositório tem um commit novo: {}.",
                        short_sha(&commit.sha)
                    )
                } else {
                    format!(
                        "Seu repositório está na versão já conhecida ({}).",
                        short_sha(&commit.sha)
                    )
                };
            } else {
                detail = format!("Repositório {}.", state.repo);
                status = "Nenhuma consulta concluída ainda.".to_string();
            }
            if let Some(error) = state.error.as_ref() {
                status = format!("{status} Falha: {error}");
            } else if !state.checked_at.is_empty() {
                status = format!("{status} Verificado em {}.", state.checked_at);
            }
            (available, title, detail, status, downloadable)
        };
        self.ui.set_update_available(available);
        self.ui.set_update_title(title.into());
        self.ui.set_update_detail(detail.into());
        self.ui.set_update_status(status.into());
        self.ui.set_update_downloadable(downloadable);
    }

    fn download_update(&self) {
        if self.runtime.borrow().busy || self.runtime.borrow().running {
            self.set_status(
                1,
                "Ocupado",
                "Aguarde a operação atual.",
                "O download espera a sessão terminar.",
            );
            return;
        }
        if self.update_state.borrow().installing {
            return;
        }
        let (release, remote) = {
            let state = self.update_state.borrow();
            (
                state
                    .release
                    .clone()
                    .filter(|release| !release.asset_url.is_empty()),
                state.remote.first().cloned(),
            )
        };
        let directory = self.builds_dir.borrow().clone();
        if let Err(error) = builds::ensure_dir(&directory) {
            self.set_status(3, "Erro", "Não foi possível criar a pasta de builds.", &format!("{error:#}"));
            return;
        }
        self.update_state.borrow_mut().installing = true;
        self.ui
            .set_builds_status("Baixando última versão do GitHub…".into());
        self.ui
            .set_update_status("Baixando do repositório…".into());
        self.ui.set_update_downloadable(false);
        self.set_status(1, "Baixando", "Consultando e baixando última build…", updates::DEFAULT_REPOSITORY);
        let repo = updates::DEFAULT_REPOSITORY.to_string();
        let sender = self.event_sender.clone();
        spawn_task(sender, move |sender| {
            let remote = match remote {
                Some(found) => Some(found),
                None => updates::remote_builds(&repo).ok().and_then(|list| list.into_iter().next()),
            };
            let release = match release {
                Some(found) => Some(found),
                None => updates::latest_release(&repo)
                    .ok()
                    .flatten()
                    .filter(|r| !r.asset_url.is_empty()),
            };
            let result = match (remote, release) {
                (Some(build), _) => updates::download_build(&build, &directory),
                (None, Some(release)) => updates::download_release(&release, &directory),
                (None, None) => Err(anyhow::anyhow!(
                    "O repositório não tem arquivos em builds/ nem release com pacote do emulador."
                )),
            };
            let _ = sender.send(BgEvent::UpdateInstalled {
                result: result
                    .map(|path| path.display().to_string())
                    .map_err(|error| format!("{error:#}")),
            });
        });
    }

    fn dismiss_update(&self) {
        let key = {
            let state = self.update_state.borrow();
            state
                .release
                .as_ref()
                .map(|release| release.tag.clone())
                .or_else(|| state.commit.as_ref().map(|commit| commit.sha.clone()))
        };
        if let Some(key) = key {
            self.update_state.borrow_mut().skipped = key;
            self.persist_update_state();
        }
        self.publish_update();
    }

    fn persist_settings(&self) -> Result<()> {
        let settings = Settings {
            schema_version: 2,
            base: self.ui.get_base_path().trim().to_string(),
            update: self.ui.get_update_path().trim().to_string(),
            executable: self.ui.get_executable_path().trim().to_string(),
            profile: self.runtime.borrow().profile.clone(),
            desired_enabled: Some(self.ordered_selection()),
            capture_enabled: self.runtime.borrow().capture,
            vblank_override: self.runtime.borrow().vblank_override,
            launch_mode: Some(self.runtime.borrow().launch_mode),
            gpu_mode: self.ui.get_gpu_mode() as u8,
            lazy_cache: self.ui.get_lazy_cache(),
            disable_animations: self.ui.get_disable_animations(),
            fluid_background: self.ui.get_fluid_enabled(),
            discord_presence: self.ui.get_discord_presence(),
            launcher_language: self.ui.get_launcher_language().clamp(0, 5) as u8,
            auto_update_check: self.ui.get_auto_update_check(),
        };
        let result = self.store.borrow_mut().save_settings(&settings);
        if result.is_ok() {
            *self.saved_paths.borrow_mut() =
                (settings.base.clone(), settings.update.clone(), settings.executable.clone());
            self.ui.set_paths_dirty(false);
        }
        result
    }

    fn set_busy(&self, busy: bool) {
        self.runtime.borrow_mut().busy = busy;
        if !busy {
            // Every operation owns a token while it is busy; a terminal event
            // (including cancellation) releases it here.
            self.cancel.borrow_mut().take();
        }
        self.ui.set_busy(busy);
    }

    /// Starts a cancellable operation. Only one operation runs at a time.
    fn begin_operation(&self) -> CancelToken {
        let token = CancelToken::new();
        *self.cancel.borrow_mut() = Some(token.clone());
        token
    }

    fn cancel_operation(&self) {
        if !self.runtime.borrow().busy {
            return;
        }
        let token = self.cancel.borrow().clone();
        let Some(token) = token else { return };
        if token.is_cancelled() {
            return;
        }
        token.cancel();
        self.set_status(
            1,
            "Cancelando",
            "Cancelando a operação atual…",
            "Aguarde a confirmação; nenhum arquivo original é alterado.",
        );
        self.log_launcher("Cancelamento solicitado.");
    }

    /// Hides the window once the cancelled worker has settled, so a close
    /// request never abandons a thread in the middle of a write.
    fn close_if_settled(&self) {
        if self.pending_close.get() && !self.runtime.borrow().busy {
            self.pending_close.set(false);
            let _ = self.persist_settings();
            self.window_hidden.set(true);
            self.presence_live.set(false);
            self.presence.clear();
            if !self.runtime.borrow().running {
                self.should_quit.set(true);
            }
            let _ = self.ui.window().hide();
        }
    }

    fn set_status(&self, kind: i32, label: &str, message: &str, detail: &str) {
        self.ui.set_state_kind(kind);
        self.ui.set_state_label(label.into());
        self.ui.set_status_message(message.into());
        self.ui.set_status_detail(detail.into());
    }

    fn log_launcher(&self, message: &str) {
        let stamp = Local::now().format("%H:%M:%S");
        let line = format!("[{stamp}] {message}\n");
        self.append_log(&line);
    }

    fn append_log(&self, chunk: &str) {
        if chunk.is_empty() {
            return;
        }
        let mut runtime = self.runtime.borrow_mut();
        let combined = &mut runtime.log_text;
        if !combined.is_empty()
            && !combined.ends_with('\n')
            && !chunk.starts_with('\n')
            && !chunk.contains("[launcher]")
        {
            combined.push('\n');
        }
        combined.push_str(chunk);
        // UTF-8 byte length is a cheap lower bound; count characters only near the cap.
        if combined.len() > MAX_LOG_CHARS {
            let count = combined.chars().count();
            if count > MAX_LOG_CHARS {
                let cut = combined
                    .char_indices()
                    .nth(count - TRIM_LOG_TO)
                    .map(|(index, _)| index)
                    .unwrap_or(0);
                combined.drain(..cut);
            }
        }
        runtime.log_dirty = true;
    }

    fn set_log_view(&self, text: &str) {
        self.ui.set_log_text(text.into());
        self.ui.set_log_lines(model(text.lines().map(SharedString::from).collect()));
    }

    fn flush_visible_log(&self) {
        if self.ui.get_page() != 2 {
            return;
        }
        let mut runtime = self.runtime.borrow_mut();
        if runtime.log_dirty {
            self.set_log_view(&runtime.log_text);
            runtime.log_dirty = false;
        }
    }

    fn log_tails_for(&self, info: &RunningInfo) -> Vec<LogTail> {
        ["shad_log.txt", "shadps4.log"]
            .into_iter()
            .map(|name| {
                LogTail::new(
                    self.sessions.environment.log_dir().join(name),
                    info.log_baselines.get(name).cloned().flatten(),
                    info.log_append,
                )
            })
            .collect()
    }

    fn poll_logs(&self) {
        let mut tails = std::mem::take(&mut self.runtime.borrow_mut().log_tails);
        if tails.is_empty() {
            return;
        }
        let mut output = String::new();
        for tail in &mut tails {
            output.push_str(&tail.read(16 * 1024));
        }
        if !output.is_empty() {
            self.append_log(&output);
        }
        let clear = {
            let runtime = self.runtime.borrow();
            runtime.stop_tails_after_read && !runtime.running
        };
        if clear {
            self.runtime.borrow_mut().stop_tails_after_read = false;
            self.runtime.borrow_mut().log_tails.clear();
        } else {
            self.runtime.borrow_mut().log_tails = tails;
        }
    }

    fn backup_bad_settings(&self) {
        let path = self.store.borrow().settings_path.clone();
        if path.exists() {
            let backup = path.with_file_name("settings.json.antes_rust.bak");
            if !backup.exists() {
                let _ = fs::copy(path, backup);
            }
        }
    }
}

/// A cancelled worker reports `Cancelled` even when the underlying step also
/// failed (for example, a hash read interrupted by the cancellation flag).
/// Código de cenário do jogo: 3 a 5 letras maiúsculas seguidas de 3 dígitos
/// (`Pos100`, `Oly350`, `Ica125`). Recursos como `R_W_Explosive5` não passam.
/// Decodifica os PNGs fora da interface e devolve os pixels prontos: a página
/// só precisa montar a imagem, sem esperar o decodificador.
/// Regra única do cartão: só existe atividade do Discord com a preferência
/// ligada, uma sessão viva (PID do jogo existindo) e a janela do launcher
/// aberta. Sem qualquer uma delas, a atividade é removida.
fn presence_allowed(
    preference: bool,
    live: bool,
    running: bool,
    game_pid: Option<u32>,
    window_hidden: bool,
) -> bool {
    preference && live && running && game_pid.is_some() && !window_hidden
}

#[cfg(test)]
mod presence_gate_tests {
    use super::presence_allowed;

    #[test]
    fn only_a_live_session_keeps_the_card() {
        assert!(presence_allowed(true, true, true, Some(4242), false));
    }

    #[test]
    fn closed_game_removes_the_card() {
        // O jogo fechou: PID some, ou o laço já marcou a sessão como encerrada.
        assert!(!presence_allowed(true, true, true, None, false));
        assert!(!presence_allowed(true, false, true, Some(4242), false));
        assert!(!presence_allowed(true, true, false, Some(4242), false));
    }

    #[test]
    fn closed_launcher_and_disabled_preference_remove_the_card() {
        assert!(!presence_allowed(true, true, true, Some(4242), true));
        assert!(!presence_allowed(false, true, true, Some(4242), false));
    }
}

fn decode_trophy_icons(state: &trophies::State) -> Vec<(PathBuf, u32, u32, Vec<u8>)> {
    let trophies::State::Loaded(collection) = state else {
        return Vec::new();
    };
    collection
        .trophies
        .iter()
        .filter(|trophy| trophy.has_icon())
        .filter_map(|trophy| {
            let bytes = fs::read(&trophy.icon).ok()?;
            let decoded = image::load_from_memory(&bytes).ok()?.to_rgba8();
            let (width, height) = decoded.dimensions();
            Some((trophy.icon.clone(), width, height, decoded.into_raw()))
        })
        .collect()
}

/// Monta uma linha da lista; o ícone ausente não invalida a coleção.
fn push_trophy_item(
    items: &mut Vec<TrophyItem>,
    trophy: &Trophy,
    icons: &mut HashMap<PathBuf, slint::Image>,
) {
    let has_icon = trophy.has_icon();
    let icon = if has_icon {
        icons
            .entry(trophy.icon.clone())
            .or_insert_with(|| slint::Image::load_from_path(&trophy.icon).unwrap_or_default())
            .clone()
    } else {
        slint::Image::default()
    };
    items.push(TrophyItem {
        title: trophy.name.clone().into(),
        detail: trophy.detail.clone().into(),
        grade: trophy.grade.label().into(),
        grade_kind: trophy.grade.kind(),
        unlocked: trophy.unlocked,
        hidden_state: trophy.hidden,
        icon,
        has_icon,
    });
}

fn report_worker_error(tx: &Sender<BgEvent>, cancel: &CancelToken, error: anyhow::Error) {
    if cancel.is_cancelled() {
        let _ = tx.send(BgEvent::Cancelled);
    } else {
        let _ = tx.send(BgEvent::Failed {
            message: format!("{error:#}"),
        });
    }
}

fn spawn_task<F>(sender: Sender<BgEvent>, task: F)
where
    F: FnOnce(Sender<BgEvent>) + Send + 'static,
{
    let panic_sender = sender.clone();
    thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            task(sender);
        }));
        if result.is_err() {
            let _ = panic_sender.send(BgEvent::Failed {
                message: "Falha interna inesperada no launcher. Nenhum dado original foi alterado."
                    .into(),
            });
        }
    });
}

fn model<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}

fn labels(items: &[&'static str]) -> Vec<SharedString> {
    items.iter().map(|item| SharedString::from(*item)).collect()
}

fn short_hash(value: &str) -> String {
    value.chars().take(12).collect()
}

fn profile_label(id: &str) -> &str {
    match id {
        "SemPatches" => "Sem patches",
        "Base60" => "Base · 60 FPS",
        "Originais120" => "Originais · 120 FPS",
        "ReferenciaObservada" => "Referência · VBlank 60",
        _ => id,
    }
}

fn derive_update_path(base: &Path) -> Option<PathBuf> {
    let parent = base.parent()?;
    let name = base.file_name()?.to_str()?;
    for suffix in ["-UPDATE", "-patch"] {
        let candidate = parent.join(format!("{name}{suffix}"));
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

fn describe_feature(id: &str) -> (String, String, i32) {
    match id {
        "fps_120" => (
            "Patch 120 FPS".into(),
            "Desbloqueia o patch original de 120 FPS. Exige VBlank 120 Hz e hardware compatível."
                .into(),
            0,
        ),
        "skip_intro" => (
            "Pular logos iniciais".into(),
            "Avança os logotipos de abertura para chegar mais rápido ao menu.".into(),
            1,
        ),
        "skip_legacy" => (
            "Pular vídeos com X".into(),
            "Patch original do cuesta para saltar vídeos com o botão X.".into(),
            1,
        ),
        _ => (id.to_string(), "Opção do pacote de patches.".into(), 2),
    }
}

fn describe_experiment(id: &str) -> (String, String) {
    let (title, subtitle) = match id {
        "color_grading_off" => (
            "Desativar correção de cor",
            "Desativa a correção de cor. Sem validação de gameplay; compare com o perfil de origem.",
        ),
        "layered_fog_off" => (
            "Remover névoa em camadas",
            "Remove a névoa em camadas. Pode alterar profundidade e atmosfera.",
        ),
        "dof_native_off" => (
            "Desativar profundidade de campo",
            "Desativa o desfoque de profundidade pelo caminho inativo nativo.",
        ),
        "bloom_native_off" => (
            "Desativar bloom",
            "Defeito conhecido: distorce textos do menu. Overglow ainda em investigação.",
        ),
        "motion_blur_native_off" => (
            "Desativar desfoque de movimento",
            "Desliga o desfoque de movimento. Possível interação com overglow/resolução ainda não confirmada.",
        ),
        "camera_sway_off" => (
            "Desativar oscilação de câmera",
            "Força o resultado inativo da oscilação de câmera, preservando o transform neutro.",
        ),
        "dynamic_shadows_native_off" => (
            "Sombras dinâmicas OFF (parcial)",
            "Desliga apenas parte das sombras dinâmicas. Não há ganho de FPS garantido.",
        ),
        "job_queue_pause" => (
            "Fila de jobs com PAUSE",
            "Altera o código do jogo para espaçar tentativas de trava que falharam. Mantém os efeitos gráficos; ganho de FPS pendente de teste.",
        ),
        "shadow_range_75" => (
            "Sombras: alcance 75%",
            "Candidato de qualidade: reduz o alcance publicado após a avaliação da cena. Pode retirar sombras distantes; ganho de FPS não medido.",
        ),
        "shadow_range_50" => (
            "Sombras: alcance 50%",
            "Redução mais forte de alcance. Não combinar com sombras OFF. Validação visual e de desempenho pendentes.",
        ),
        "particles_soft_off" => ("Desativar suavização", "Remove a suavização das bordas das partículas."),
        "particles_distortion_off" => ("Desativar distorção", "Remove a distorção produzida por partículas."),
        "particle_halfres" => ("Buffers em meia resolução", "Reduz a resolução dos buffers de partículas; confira bordas e composição."),
        "ssao_off" => ("Desativar oclusão ambiente", "Remove o SSAO; pode alterar sombras de contato."),
        "geometry_weights_simd" => ("Pesos de geometria SIMD", "Caminho SIMD experimental para os pesos de geometria. Ganho não medido."),
        "native_lod_75" => ("Limite de LOD em 75%", "Antecipa o LOD distante vazio; pode remover geometria distante."),
        _ => (
            "Candidato experimental",
            "Opção sem validação de gameplay; teste isoladamente e confira os logs.",
        ),
    };
    (title.into(), subtitle.into())
}

fn format_session_label(summary: &SessionSummary) -> String {
    let time = DateTime::parse_from_rfc3339(&summary.created_at)
        .map(|value| {
            value
                .with_timezone(&Local)
                .format("%d/%m %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| "data desconhecida".into());
    format!("{time} · {} · {}", summary.profile, session_status(summary))
}

/// Quantas builds o repositório publica em `builds/`, sem "build(s)".
fn published_builds(count: usize) -> String {
    match count {
        0 => "nenhuma build publicada em builds/".to_string(),
        1 => "1 build publicada em builds/".to_string(),
        count => format!("{count} builds publicadas em builds/"),
    }
}

/// Primeiros sete caracteres do sha, como o GitHub mostra.
fn download_progress_text(downloaded: u64, total: u64) -> String {
    let dl_mb = downloaded as f64 / (1024.0 * 1024.0);
    if total > 0 {
        let total_mb = total as f64 / (1024.0 * 1024.0);
        let pct = ((downloaded as f64 / total as f64) * 100.0).clamp(0.0, 100.0);
        format!("{pct:.0}% · {dl_mb:.1} / {total_mb:.1} MB")
    } else if downloaded > 0 {
        format!("{dl_mb:.1} MB baixados…")
    } else {
        "Conectando ao GitHub…".to_string()
    }
}

fn short_sha(sha: &str) -> String {
    sha.chars().take(7).collect()
}

/// `2026-10-04T01:33:01Z` vira `04/10/2026`; sem data, fica o texto original.
fn commit_date(date: &str) -> String {
    let date = date.trim();
    let bytes = date.as_bytes();
    if bytes.len() >= 10
        && bytes[..10].iter().all(|byte| byte.is_ascii_digit() || *byte == b'-')
        && bytes[4] == b'-'
        && bytes[7] == b'-'
    {
        return format!("{}/{}/{}", &date[8..10], &date[5..7], &date[0..4]);
    }
    if date.is_empty() {
        "data desconhecida".to_string()
    } else {
        date.to_string()
    }
}

/// Repositório do GitHub a partir do `git config` do projeto no disco.
/// Remove o prefixo `\\?\` que `fs::canonicalize` acrescenta no Windows, para a
/// interface mostrar e salvar o caminho como o usuário o conhece.
fn strip_verbatim(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\?") {
        let bytes = rest.as_bytes();
        if bytes.len() > 2 && bytes[1] == b':' {
            return PathBuf::from(rest);
        }
    }
    path
}

/// Data de modificação da build, no formato curto usado na lista.
fn modified_label(modified: Option<std::time::SystemTime>) -> String {
    modified
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|duration| {
            use chrono::TimeZone;
            Local
                .timestamp_opt(duration.as_secs() as i64, 0)
                .single()
        })
        .map(|stamp| stamp.format("%d/%m/%Y").to_string())
        .unwrap_or_else(|| "data desconhecida".to_string())
}

fn session_status(summary: &SessionSummary) -> String {
    if !summary.finalized {
        "em andamento".into()
    } else if summary.exit_code.unwrap_or(0) == 0 {
        "finalizada".into()
    } else {
        "terminou com erro".into()
    }
}

fn session_status_short(summary: &SessionSummary) -> String {
    format!(
        "{} · {}",
        summary.profile,
        if summary.finalized {
            "sessão concluída"
        } else {
            "sessão pendente"
        }
    )
}

fn push_fluid_frame(ui: &AppWindow, fluid: &mut crate::fluid::Fluid) {
    let mut buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(
        crate::fluid::OUT_W as u32,
        crate::fluid::OUT_H as u32,
    );
    for (pixel, rgba) in buffer
        .make_mut_slice()
        .iter_mut()
        .zip(fluid.render().chunks_exact(4))
    {
        *pixel = slint::Rgba8Pixel {
            r: rgba[0],
            g: rgba[1],
            b: rgba[2],
            a: rgba[3],
        };
    }
    ui.set_fluid_image(slint::Image::from_rgba8_premultiplied(buffer));
}

pub fn run() -> Result<()> {
    let root = application_root();
    let data_root = data_directory(&root);
    fs::create_dir_all(&data_root)
        .with_context(|| format!("não foi possível criar {}", data_root.display()))?;
    let data_root = strip_verbatim(fs::canonicalize(&data_root).unwrap_or(data_root));
    let _instance_lock = acquire_instance_lock(&data_root)?;
    let catalog = Catalog::load_embedded()?;
    let store = Store::load(data_root.clone(), &catalog)?;
    let ui = AppWindow::new()
        .map_err(|error| anyhow::anyhow!("não foi possível abrir a janela: {error}"))?;
    let controller = AppController::new(ui, catalog, store, data_root);
    controller.initialize();
    controller.bind();

    // The fluid runs on its own cheap timer and is paused as soon as the
    // preference is off, the window is hidden or motion is disabled.
    let mut fluid = crate::fluid::Fluid::new();
    push_fluid_frame(&controller.ui, &mut fluid);
    let fluid_ui = controller.ui.clone_strong();
    let fluid_timer = slint::Timer::default();
    fluid_timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(33),
        move || {
            if !fluid_ui.get_fluid_enabled()
                || fluid_ui.get_disable_animations()
                || !fluid_ui.window().is_visible()
            {
                return;
            }
            fluid.step(0.033);
            push_fluid_frame(&fluid_ui, &mut fluid);
        },
    );

    let weak = Rc::downgrade(&controller);
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(120),
        move || {
            if let Some(controller) = weak.upgrade() {
                controller.poll();
            }
        },
    );

    // O renderer por software às vezes entrega o primeiro quadro incompleto na
    // abertura; alguns pedidos de repintura iniciais evitam a janela quebrada.
    let repaint_ui = controller.ui.clone_strong();
    let repaint_ticks = std::cell::Cell::new(0u32);
    let repaint = slint::Timer::default();
    repaint.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(160),
        move || {
            if repaint_ticks.get() >= 4 {
                return;
            }
            repaint_ui.window().request_redraw();
            repaint_ticks.set(repaint_ticks.get() + 1);
        },
    );

    controller
        .ui
        .run()
        .map_err(|error| anyhow::anyhow!("falha no laço da interface: {error}"))?;
    drop(repaint);
    drop(timer);
    drop(fluid_timer);
    Ok(())
}
