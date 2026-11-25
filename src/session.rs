use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::UNIX_EPOCH;

use anyhow::{bail, Context, Result};
use chrono::{Local, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::catalog::Catalog;
use crate::game::{emulator_path, sha256_file, CancelToken, GamePaths, PrivateEnvironment};
use crate::process;
use crate::storage::{read_json, write_json};

#[derive(Debug, Clone, Copy, Default)]
pub struct LaunchOptions {
    pub capture: bool,
    pub no_cache: bool,
    pub wait_for_shaders: bool,
    pub gpu_mode: u8,
    pub lazy_cache: bool,
}

pub const E11_HASH: &str = "1549B4D570FD55E2D23834D074749C96D265AC22B50CA80152ED316240EB0F4D";

pub fn supports_gpu_modes(hash: &str) -> bool { hash.eq_ignore_ascii_case(E11_HASH) }

impl LaunchOptions {
    pub fn effective(mut self, hash: &str) -> Result<Self> {
        if self.gpu_mode > 3 { bail!("Modo GPU inválido."); }
        if (self.gpu_mode != 0 || self.lazy_cache) && !supports_gpu_modes(hash) {
            bail!("Este executável não tem suporte confirmado aos modos GPU/cache preguiçoso. Selecione o E11 reconhecido ou desative esses recursos; E10 os ignora.");
        }
        if self.gpu_mode != 0 { self.lazy_cache = true; self.wait_for_shaders = true; }
        if self.gpu_mode == 1 { self.no_cache = true; }
        Ok(self)
    }

    pub fn gpu_environment(self) -> Vec<(String, String)> {
        let mut values = Vec::new();
        if let Some(mode) = [None, Some("precise"), Some("balanced"), Some("fast")].get(self.gpu_mode as usize).copied().flatten() {
            values.push(("SHADPS4_GPU_MODE".into(), mode.into()));
        }
        if self.lazy_cache || self.gpu_mode != 0 { values.push(("SHADPS4_LAZY_CACHE".into(), "1".into())); }
        if self.wait_for_shaders || self.no_cache || self.gpu_mode != 0 { values.push(("SHADPS4_WAIT_FOR_SHADERS".into(), "1".into())); }
        values
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogBaseline {
    pub size: u64,
    pub modified_ns: u64,
}

#[derive(Debug, Clone)]
pub struct RunningInfo {
    pub session_dir: PathBuf,
    pub pid: u32,
    pub profile: String,
    pub enabled: Vec<String>,
    pub vblank: u32,
    pub min_dmem: u64,
    pub capture: bool,
    pub no_cache: bool,
    pub wait_for_shaders: bool,
    pub log_baselines: BTreeMap<String, Option<LogBaseline>>,
    pub log_append: bool,
    pub native_log_enabled: bool,
    pub memory: Option<process::MemoryStatus>,
}

#[derive(Debug, Clone)]
pub struct LaunchResult {
    pub session_dir: PathBuf,
    pub exit_code: i32,
    pub finalized: bool,
    pub error: Option<String>,
    pub capture: bool,
    pub profile: String,
}

#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub path: PathBuf,
    pub profile: String,
    pub created_at: String,
    pub status: String,
    pub finalized: bool,
    pub exit_code: Option<i32>,
    pub capture: bool,
}

#[derive(Debug, Clone)]
pub struct SessionManager {
    pub data_root: PathBuf,
    pub sessions_dir: PathBuf,
    pub environment: PrivateEnvironment,
}

impl SessionManager {
    pub fn new(data_root: PathBuf) -> Self {
        let root = data_root.clone();
        Self {
            data_root,
            sessions_dir: root.join("sessoes"),
            environment: PrivateEnvironment::new(&root),
        }
    }

    pub fn latest_summary(&self) -> Option<SessionSummary> {
        let mut paths = fs::read_dir(&self.sessions_dir)
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        paths.sort();
        paths.reverse();
        for path in paths {
            let state_path = path.join("session.json");
            let Ok(value) = read_json(&state_path) else {
                continue;
            };
            if value.get("isolated").and_then(Value::as_bool) != Some(true) {
                continue;
            }
            return Some(SessionSummary {
                profile: value
                    .get("profile")
                    .and_then(Value::as_str)
                    .unwrap_or("—")
                    .to_string(),
                created_at: value
                    .get("created_at")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                status: value
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                finalized: value
                    .get("finalized")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                exit_code: value
                    .get("exit_code")
                    .and_then(Value::as_i64)
                    .map(|value| value as i32),
                capture: value
                    .get("capture_enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                path,
            });
        }
        None
    }

    pub fn pending_sessions(&self) -> Result<Vec<PathBuf>> {
        if !self.sessions_dir.exists() {
            return Ok(Vec::new());
        }
        let mut states = Vec::new();
        for entry in fs::read_dir(&self.sessions_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let state_path = path.join("session.json");
            if !state_path.is_file() {
                continue;
            }
            let value = read_json(&state_path)?;
            if value.get("schema_version").and_then(Value::as_u64) != Some(2)
                || value.get("isolated").and_then(Value::as_bool) != Some(true)
            {
                continue;
            }
            if value.get("finalized").and_then(Value::as_bool) != Some(true) {
                states.push(state_path);
            }
        }
        Ok(states)
    }

    pub fn recovery_needed(&self) -> Result<bool> {
        Ok(self.environment.journal_path().exists() || !self.pending_sessions()?.is_empty())
    }

    pub fn recover_all(&self, cancel: &CancelToken) -> Result<usize> {
        cancel.check()?;
        process::ensure_emulator_stopped("")?;
        let states = self.pending_sessions()?;
        for state_path in &states {
            cancel.check()?;
            let value = read_json(state_path)?;
            if value.get("schema_version").and_then(Value::as_u64) != Some(2)
                || value.get("isolated").and_then(Value::as_bool) != Some(true)
            {
                bail!("Sessão incompatível; recuperação recusada para preservar os arquivos.");
            }
            let root = value
                .get("root")
                .and_then(Value::as_str)
                .context("Sessão sem raiz privada; recuperação recusada.")?;
            let recorded = fs::canonicalize(root).unwrap_or_else(|_| PathBuf::from(root));
            let expected = fs::canonicalize(&self.environment.root)
                .unwrap_or_else(|_| self.environment.root.clone());
            if recorded != expected {
                bail!("Sessão aponta para um ambiente externo; recuperação recusada.");
            }
        }
        cancel.check()?;
        self.environment.restore_requirements()?;
        for state_path in &states {
            cancel.check()?;
            let mut value = read_json(state_path)?;
            value["status"] = json!("interrupted");
            value["finalized"] = json!(true);
            value["ended_at"] = json!(Utc::now().to_rfc3339());
            write_json(state_path, &value)?;
        }
        Ok(states.len())
    }

    /// Abandons pending journals and sessions without restoring the private
    /// configuration. Every file is preserved as a `.bak` beside the original
    /// so a corrupt state can be inspected; the caller must ensure no session
    /// is running before calling this.
    pub fn discard_pending(&self) -> Result<usize> {
        let mut discarded = 0usize;
        let journal = self.environment.journal_path();
        if journal.exists() {
            fs::rename(&journal, discarded_backup(&journal))?;
            discarded += 1;
        }
        if self.sessions_dir.exists() {
            for entry in fs::read_dir(&self.sessions_dir)? {
                let path = entry?.path();
                if !path.is_dir() {
                    continue;
                }
                let state_path = path.join("session.json");
                if !state_path.exists() {
                    continue;
                }
                match read_json(&state_path) {
                    Ok(mut value) => {
                        if value.get("finalized").and_then(Value::as_bool) == Some(true) {
                            continue;
                        }
                        let now = Utc::now().to_rfc3339();
                        value["status"] = json!("abandoned");
                        value["finalized"] = json!(true);
                        value["discarded_at"] = json!(now);
                        value["ended_at"] = json!(now);
                        write_json(&state_path, &value)?;
                    }
                    Err(_) => {
                        // Unreadable state is exactly what blocks recovery; it
                        // is moved aside untouched instead of being rewritten.
                        fs::rename(&state_path, discarded_backup(&state_path))?;
                    }
                }
                discarded += 1;
            }
        }
        Ok(discarded)
    }

    pub fn launch<F>(
        &self,
        catalog: &Catalog,
        paths: &GamePaths,
        enabled: &[String],
        profile: &str,
        vblank_override: Option<u32>,
        options: LaunchOptions,
        cancel: &CancelToken,
        on_running: F,
    ) -> Result<LaunchResult>
    where
        F: FnOnce(&RunningInfo),
    {
        cancel.check()?;
        if self.pending_sessions()?.iter().count() > 0 {
            bail!("Existe uma sessão privada pendente. Use Concluir sessão interrompida.");
        }
        if self.environment.journal_path().exists() {
            bail!("Há requisitos privados pendentes. Use Concluir sessão interrompida.");
        }
        let expected_root = fs::canonicalize(&self.environment.root)
            .unwrap_or_else(|_| self.environment.root.clone());
        if paths.data_root != self.data_root {
            bail!("Os caminhos não pertencem a esta pasta de dados do launcher.");
        }
        if paths.base.starts_with(&expected_root)
            || paths.update.starts_with(&expected_root)
            || paths.executable.starts_with(&expected_root)
        {
            bail!("Jogo e build de origem devem ficar fora do ambiente privado.");
        }
        process::ensure_emulator_stopped(
            paths
                .executable
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(""),
        )?;
        let report = paths.validate(catalog, enabled, vblank_override, cancel)?;
        let options = options.effective(&report.executable_sha256)?;
        let selection = catalog.resolve(enabled, vblank_override)?;
        cancel.check()?;
        let copied_executable = self.environment.prepare(paths, &report, cancel)?;
        cancel.check()?;
        let session_dir = self.new_session_dir();
        fs::create_dir_all(&session_dir)?;
        let patch_path = session_dir.join("patch.xml");
        catalog.write_patch_xml(enabled, &patch_path)?;
        let state_path = session_dir.join("session.json");
        let command_display = launch_arguments(
            enabled,
            &copied_executable,
            &patch_path,
            &paths.base,
            options,
        );
        let log_baselines = [
            (
                "shad_log.txt",
                self.environment.log_dir().join("shad_log.txt"),
            ),
            (
                "shadps4.log",
                self.environment.log_dir().join("shadps4.log"),
            ),
        ]
        .into_iter()
        .map(|(name, path)| (name.to_string(), log_baseline(&path)))
        .collect::<BTreeMap<_, _>>();
        let log_append = log_append_enabled(&self.environment)?;
        let native_log_enabled = native_logs_enabled(&self.environment)?;
        let memory = report.memory_before_launch;

        let mut state = json!({
            "schema_version": 2,
            "catalog_version": catalog.package_version,
            "status": "prepared",
            "isolated": true,
            "finalized": false,
            "root": expected_root.to_string_lossy(),
            "source_executable": paths.executable.to_string_lossy(),
            "executable": copied_executable.to_string_lossy(),
            "command": command_display,
            "profile": profile,
            "launch_selection": report.enabled,
            "validation": report,
            "patch_sha256": sha256_file(&patch_path)?,
            "created_at": Utc::now().to_rfc3339(),
            "gameplay_result": "pending_user_observation",
            "capture_enabled": options.capture,
            "no_disk_cache": options.no_cache,
            "gpu_mode_requested": options.gpu_mode,
            "gpu_features_supported": supports_gpu_modes(&report.executable_sha256),
            "effective_gpu_environment": options.gpu_environment(),
            "gpu_backlog_override": match options.gpu_mode { 1 => Some(1), 2 => Some(2), 3 => Some(3), _ => None },
            "wait_for_shaders_requested": options.wait_for_shaders,
            "wait_for_shaders_override": options.wait_for_shaders || options.no_cache,
            "log_baselines": log_baselines,
            "log_append": log_append,
            "native_log_enabled": native_log_enabled,
            "memory_before_launch": memory,
        });
        write_json(&state_path, &state)?;

        cancel.check()?;
        let applied = match self
            .environment
            .apply_requirements(&selection, options.no_cache)
        {
            Ok(value) => value,
            Err(error) => {
                state["status"] = json!("interrupted");
                state["finalized"] = json!(true);
                state["error"] = json!(error.to_string());
                state["ended_at"] = json!(Utc::now().to_rfc3339());
                let _ = write_json(&state_path, &state);
                return Err(error);
            }
        };
        if cancel.is_cancelled() {
            // The temporary requirements are already applied: undo them
            // before reporting the cancellation, never leave them pending.
            return self.abort_launch(&mut state, &state_path, "Operação cancelada pelo usuário.");
        }
        state["effective_config"] = applied;
        if let Err(error) = write_json(&state_path, &state) {
            let restore_error = self.environment.restore_requirements().err();
            state["status"] = json!("interrupted");
            state["finalized"] = json!(true);
            state["ended_at"] = json!(Utc::now().to_rfc3339());
            state["error"] = json!(error.to_string());
            let _ = write_json(&state_path, &state);
            if let Some(restore) = restore_error {
                return Err(anyhow::anyhow!(
                    "{error} Também falhou a restauração da configuração: {restore}"
                ));
            }
            return Err(error);
        }

        // Last cancellation gate: once the emulator starts, the session is
        // active and stopping it is the user's decision in game. Reaching it
        // with a cancellation means the temporary requirements must be undone
        // and the session closed, exactly like the earlier gate.
        if cancel.is_cancelled() {
            return self.abort_launch(&mut state, &state_path, "Operação cancelada pelo usuário.");
        }
        let mut child = match spawn_emulator(
            &copied_executable,
            &patch_path,
            &paths.base,
            &self.environment,
            &session_dir,
            options,
            log_append,
        ) {
            Ok(child) => child,
            Err(error) => {
                let restore_error = self.environment.restore_requirements().err();
                state["status"] = json!("interrupted");
                state["finalized"] = json!(true);
                state["error"] = json!(error.to_string());
                state["ended_at"] = json!(Utc::now().to_rfc3339());
                let _ = write_json(&state_path, &state);
                return Err(match restore_error {
                    Some(restore) => anyhow::anyhow!(
                        "{error} Também falhou a restauração da configuração: {restore}"
                    ),
                    None => error,
                });
            }
        };

        let pid = child.id();
        // Drain stdout without growing a log file during normal play. Fatal
        // assertions are printed here even when capture is disabled.
        let startup_output = child.stdout.take().map(|mut stdout| {
            let (sender, receiver) = std::sync::mpsc::sync_channel(1);
            std::thread::spawn(move || {
                let _ = sender.send(read_output_tail(&mut stdout));
            });
            receiver
        });
        state["status"] = json!("running");
        state["pid"] = json!(pid);
        state["started_at"] = json!(Utc::now().to_rfc3339());
        let _ = write_json(&state_path, &state);
        let running = RunningInfo {
            session_dir: session_dir.clone(),
            pid,
            profile: profile.to_string(),
            enabled: report.enabled.clone(),
            vblank: report.vblank,
            min_dmem: report.min_dmem,
            capture: options.capture,
            no_cache: options.no_cache,
            wait_for_shaders: options.wait_for_shaders,
            log_baselines: state
                .get("log_baselines")
                .cloned()
                .and_then(|value| serde_json::from_value(value).ok())
                .unwrap_or_default(),
            log_append,
            native_log_enabled,
            memory,
        };
        on_running(&running);

        let wait_result = child.wait();
        if let Some(receiver) = startup_output {
            if let Ok(Ok(bytes)) = receiver.recv_timeout(std::time::Duration::from_millis(500)) {
                if !wait_result
                    .as_ref()
                    .map(|status| status.success())
                    .unwrap_or(false)
                {
                    let _ = fs::write(session_dir.join("startup_stdout.log"), bytes);
                }
            }
        }
        let mut error = None;
        let mut exit_code = -1;
        match wait_result {
            Ok(status) => {
                exit_code = status.code().unwrap_or(-1);
                if exit_code != 0 {
                    error = Some(failure_message(
                        exit_code,
                        &self.environment.log_dir(),
                        &session_dir,
                        options.capture,
                        log_baselines.clone(),
                    ));
                }
            }
            Err(wait_error) => {
                error = Some(format!("Falha ao aguardar o emulador: {wait_error}"));
            }
        }
        let restore_error = self.environment.restore_requirements().err();
        if let Some(restore) = restore_error {
            let message = format!("Falha ao restaurar a configuração privada: {restore}");
            error = Some(match error {
                Some(existing) => format!("{existing} {message}"),
                None => message,
            });
        }
        if options.capture || exit_code != 0 {
            copy_native_logs(&self.environment.log_dir(), &session_dir);
        }
        state["status"] = json!("finished");
        state["finalized"] = json!(true);
        state["exit_code"] = json!(exit_code);
        state["ended_at"] = json!(Utc::now().to_rfc3339());
        if let Some(message) = &error {
            state["error"] = json!(message);
        }
        let _ = write_json(&state_path, &state);
        Ok(LaunchResult {
            session_dir,
            exit_code,
            finalized: true,
            error,
            capture: options.capture,
            profile: profile.to_string(),
        })
    }

    /// Restores the private requirements and closes a prepared session that
    /// never started, then returns the error to the caller.
    fn abort_launch(&self, state: &mut Value, state_path: &Path, reason: &str) -> Result<LaunchResult> {
        let restore_error = self.environment.restore_requirements().err();
        state["status"] = json!("interrupted");
        state["finalized"] = json!(true);
        state["ended_at"] = json!(Utc::now().to_rfc3339());
        state["error"] = json!(reason);
        let _ = write_json(state_path, state);
        if let Some(restore) = restore_error {
            return Err(anyhow::anyhow!(
                "{reason} Também falhou a restauração da configuração privada: {restore}"
            ));
        }
        bail!("{reason}")
    }

    fn new_session_dir(&self) -> PathBuf {
        let stamp = Local::now().format("%Y%m%d_%H%M%S").to_string();
        self.sessions_dir.join(format!(
            "{stamp}_{}",
            &uuid::Uuid::new_v4().to_string()[..8]
        ))
    }
}

fn discarded_backup(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("estado");
    let stamp = Utc::now().format("%Y%m%d_%H%M%S");
    path.with_file_name(format!(
        "{name}.descartado-{stamp}-{}.bak",
        &uuid::Uuid::new_v4().to_string()[..6]
    ))
}

pub(crate) fn spawn_emulator(
    executable: &Path,
    patch_path: &Path,
    base: &Path,
    environment: &PrivateEnvironment,
    session_dir: &Path,
    options: LaunchOptions,
    log_append: bool,
) -> Result<std::process::Child> {
    #[cfg(windows)]
    use std::os::windows::process::CommandExt;
    let mut command = Command::new(emulator_path(executable));
    if options.capture {
        command.arg("--show-fps");
    }
    command
        .arg("--patch")
        .arg(emulator_path(patch_path))
        .arg("-g")
        .arg(emulator_path(&base.join("eboot.bin")))
        .current_dir(emulator_path(&environment.root))
        .stdin(Stdio::null());

    let (stdout, stderr) = if options.capture {
        let path = session_dir.join("emulator.log");
        let mut stdout = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("não foi possível criar {}", path.display()))?;
        let stderr = stdout
            .try_clone()
            .with_context(|| format!("não foi possível duplicar {}", path.display()))?;
        stdout.flush()?;
        (Stdio::from(stdout), Stdio::from(stderr))
    } else {
        let errors_path = session_dir.join("startup_errors.log");
        let errors = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&errors_path)
            .with_context(|| format!("não foi possível criar {}", errors_path.display()))?;
        (Stdio::piped(), Stdio::from(errors))
    };
    command.stdout(stdout).stderr(stderr);

    let mut child_environment = environment.child_environment()?;
    child_environment.retain(|(name, _)| {
        !matches!(
            name.as_str(),
            "SHADPS4_GPU_MODE" | "SHADPS4_LAZY_CACHE" | "SHADPS4_WAIT_FOR_SHADERS" | "SHADPS4_FRAME_LOG" | "SHADPS4_GOW3_EFFECT_PROBE_DIR"
        )
    });
    child_environment.extend(options.gpu_environment());
    if options.capture {
        child_environment.push((
            "SHADPS4_FRAME_LOG".into(),
            emulator_path(&session_dir.join("frame_times.tsv"))
                .to_string_lossy()
                .into_owned(),
        ));
    }
    command.env_clear().envs(child_environment);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let _ = log_append;
    command
        .spawn()
        .with_context(|| format!("não foi possível iniciar {}", executable.display()))
}

fn launch_arguments(
    enabled: &[String],
    executable: &Path,
    patch_path: &Path,
    base: &Path,
    options: LaunchOptions,
) -> Vec<String> {
    let mut command = vec![emulator_path(executable).to_string_lossy().into_owned()];
    if options.capture {
        command.push("--show-fps".into());
    }
    command.extend([
        "--patch".into(),
        emulator_path(patch_path).to_string_lossy().into_owned(),
        "-g".into(),
        emulator_path(&base.join("eboot.bin"))
            .to_string_lossy()
            .into_owned(),
    ]);
    let _ = enabled;
    command
}

fn failure_message(
    exit_code: i32,
    log_dir: &Path,
    session_dir: &Path,
    capture: bool,
    baselines: BTreeMap<String, Option<LogBaseline>>,
) -> String {
    let startup_errors = if capture {
        session_dir.join("emulator.log")
    } else {
        session_dir.join("startup_errors.log")
    };
    for path in [startup_errors, session_dir.join("startup_stdout.log")] {
        if let Some(tail) = read_tail(&path, 64 * 1024) {
            if tail.contains("Physical backing commit failed (Win32 1455)") {
                return "O Windows recusou memória para o jogo (Win32 1455: limite de memória comprometida). Libere memória de outros aplicativos e confira o espaço disponível para o arquivo de paginação.".into();
            }
            if tail.contains("Failed to create graphics pipeline: ErrorOutOfHostMemory") {
                return "O driver Vulkan não conseguiu alocar memória ao compilar um pipeline gráfico (ErrorOutOfHostMemory). Libere memória e consulte os logs desta sessão.".into();
            }
        }
    }
    if exit_code == 20001 {
        for name in ["shad_log.txt", "shadps4.log"] {
            let path = log_dir.join(name);
            let baseline = baselines.get(name).cloned().flatten();
            if log_baseline(&path) == baseline {
                continue;
            }
            if let Some(tail) = read_tail(&path, 16 * 1024) {
                if let Some(reason) = tail
                    .lines()
                    .filter_map(|line| line.split("Terminate: Exception:").nth(1))
                    .map(str::trim)
                    .last()
                {
                    if reason == "bad allocation" {
                        return "O emulador encerrou por falha de alocação de memória (bad allocation; código 20001)."
                            .into();
                    }
                    return format!(
                        "O emulador encerrou por uma exceção: {} (código 20001).",
                        truncate(reason, 240)
                    );
                }
            }
        }
    }
    if exit_code as u32 == 0x8000_0003 {
        let output = if capture {
            "emulator.log"
        } else {
            "startup_stdout.log"
        };
        if let Some(tail) = read_tail(&session_dir.join(output), 64 * 1024) {
            let lines = tail.lines().collect::<Vec<_>>();
            if let Some(index) = lines
                .iter()
                .rposition(|line| line.contains("Assertion Failed!"))
            {
                if let Some(reason) = lines
                    .get(index + 1)
                    .map(|line| line.trim())
                    .filter(|line| !line.is_empty() && !line.starts_with('['))
                {
                    return format!(
                        "O emulador interrompeu a execução (0x80000003): {}",
                        truncate(reason, 300)
                    );
                }
            }
        }
        return "O emulador interrompeu a execução por uma assertiva interna (0x80000003). Consulte os logs desta sessão para identificar a causa.".into();
    }
    format!(
        "O emulador encerrou com código {exit_code} (0x{:08X}).",
        exit_code as u32
    )
}

fn truncate(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    value.chars().take(limit).collect()
}

fn read_output_tail(reader: &mut impl Read) -> std::io::Result<Vec<u8>> {
    const LIMIT: usize = 64 * 1024;
    let mut tail = std::collections::VecDeque::with_capacity(LIMIT + 4096);
    let mut buffer = [0u8; 4096];
    loop {
        let count = match reader.read(&mut buffer) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            break;
        }
        tail.extend(&buffer[..count]);
        if tail.len() > LIMIT {
            tail.drain(..tail.len() - LIMIT);
        }
    }
    Ok(tail.into_iter().collect())
}

fn read_tail(path: &Path, limit: usize) -> Option<String> {
    let metadata = fs::metadata(path).ok()?;
    let start = metadata.len().saturating_sub(limit as u64);
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut data = Vec::new();
    file.read_to_end(&mut data).ok()?;
    Some(String::from_utf8_lossy(&data).into_owned())
}

fn copy_native_logs(log_dir: &Path, session_dir: &Path) {
    for name in ["shad_log.txt", "shadps4.log"] {
        let source = log_dir.join(name);
        if source.is_file() {
            let _ = fs::copy(&source, session_dir.join(name));
        }
    }
}

fn log_baseline(path: &Path) -> Option<LogBaseline> {
    let metadata = fs::metadata(path).ok()?;
    let modified_ns = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    Some(LogBaseline {
        size: metadata.len(),
        modified_ns,
    })
}

fn log_append_enabled(environment: &PrivateEnvironment) -> Result<bool> {
    let config = read_json(&environment.config_path()).unwrap_or_else(|_| json!({}));
    let custom = read_json(&environment.custom_config_path()).unwrap_or_else(|_| json!({}));
    Ok(custom
        .get("Log")
        .and_then(|value| value.get("append"))
        .and_then(Value::as_bool)
        .or_else(|| {
            config
                .get("Log")
                .and_then(|value| value.get("append"))
                .and_then(Value::as_bool)
        })
        .unwrap_or(false))
}

fn native_logs_enabled(environment: &PrivateEnvironment) -> Result<bool> {
    let config = read_json(&environment.config_path())?;
    let custom = read_json(&environment.custom_config_path())?;
    Ok(custom
        .get("Log")
        .and_then(|value| value.get("enable"))
        .and_then(Value::as_bool)
        .or_else(|| {
            config
                .get("Log")
                .and_then(|value| value.get("enable"))
                .and_then(Value::as_bool)
        })
        .unwrap_or(true))
}

pub struct LogTail {
    path: PathBuf,
    baseline: Option<LogBaseline>,
    append: bool,
    started: bool,
    offset: u64,
    prefix: Vec<u8>,
    identity: Option<(u64, u64)>,
    pending: Vec<u8>,
}

impl LogTail {
    pub fn new(path: PathBuf, baseline: Option<LogBaseline>, append: bool) -> Self {
        Self {
            path,
            baseline,
            append,
            started: false,
            offset: 0,
            prefix: Vec::new(),
            identity: None,
            pending: Vec::new(),
        }
    }

    pub fn read(&mut self, limit: usize) -> String {
        let metadata = match fs::metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(_) => return String::new(),
        };
        if metadata.file_type().is_symlink() {
            return String::new();
        }
        let signature = LogBaseline {
            size: metadata.len(),
            modified_ns: metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos() as u64)
                .unwrap_or(0),
        };
        let identity = identity_of(&metadata);
        if !self.started {
            if self.baseline.as_ref() == Some(&signature) {
                return String::new();
            }
            self.offset = if self.append {
                self.baseline
                    .as_ref()
                    .map(|baseline| baseline.size)
                    .unwrap_or(0)
            } else {
                0
            };
            self.started = true;
        }
        if metadata.len() < self.offset
            || self
                .identity
                .map(|value| value != identity)
                .unwrap_or(false)
        {
            self.offset = 0;
            self.pending.clear();
        }
        use std::io::{Read, Seek, SeekFrom};
        let mut file = match fs::File::open(&self.path) {
            Ok(file) => file,
            Err(_) => return String::new(),
        };
        let mut prefix = vec![0u8; 128.min(metadata.len() as usize)];
        if file.read_exact(&mut prefix).is_ok() && !self.prefix.is_empty() && prefix != self.prefix
        {
            self.offset = 0;
            self.pending.clear();
        }
        self.prefix = prefix;
        self.identity = Some(identity);
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return String::new();
        }
        let mut bytes = vec![0u8; limit];
        let read = match file.read(&mut bytes) {
            Ok(read) => read,
            Err(_) => return String::new(),
        };
        if read == 0 {
            return String::new();
        }
        self.offset += read as u64;
        bytes.truncate(read);
        self.pending.extend_from_slice(&bytes);
        let mut output = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(value) => {
                    output.push_str(value);
                    self.pending.clear();
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    if valid > 0 {
                        output.push_str(std::str::from_utf8(&self.pending[..valid]).unwrap());
                    }
                    if error.error_len().is_some() {
                        output.push('\u{FFFD}');
                        self.pending.drain(..valid + error.error_len().unwrap());
                    } else {
                        self.pending.drain(..valid);
                        break;
                    }
                }
            }
        }
        output
    }
}

fn identity_of(_metadata: &fs::Metadata) -> (u64, u64) {
    // Rotation is detected through size rollback and the prefix snapshot.
    // Keeping this portable avoids unstable Windows file-handle metadata.
    (0, 0)
}

impl Clone for LogTail {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            baseline: self.baseline.clone(),
            append: self.append,
            started: self.started,
            offset: self.offset,
            prefix: self.prefix.clone(),
            identity: self.identity,
            pending: self.pending.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_profiles_enforce_e11_lazy_wait_and_cache_precedence() {
        for mode in 1..=3 {
            let options = LaunchOptions { gpu_mode: mode, ..Default::default() };
            assert!(options.effective("52EB9CEC25404596E15B8B547C408A62F5FA27EA38779B921A6F350A9D6B3AF3").is_err());
            assert!(options.effective("unknown").is_err());
            let effective = options.effective(E11_HASH).unwrap();
            assert!(effective.lazy_cache && effective.wait_for_shaders);
            assert_eq!(effective.no_cache, mode == 1);
            let env = effective.gpu_environment().into_iter().collect::<BTreeMap<_, _>>();
            assert_eq!(env["SHADPS4_GPU_MODE"], ["", "precise", "balanced", "fast"][mode as usize]);
            assert_eq!(env["SHADPS4_LAZY_CACHE"], "1");
            assert_eq!(env["SHADPS4_WAIT_FOR_SHADERS"], "1");
            assert!(LaunchOptions { no_cache: true, ..options }.effective(E11_HASH).unwrap().no_cache);
        }
        let lazy = LaunchOptions { lazy_cache: true, ..Default::default() }.effective(E11_HASH).unwrap();
        assert_eq!(lazy.gpu_environment(), vec![("SHADPS4_LAZY_CACHE".into(), "1".into())]);
        assert!(LaunchOptions { lazy_cache: true, ..Default::default() }.effective("unknown").is_err());
        assert!(LaunchOptions::default().effective("unknown").unwrap().gpu_environment().is_empty());
    }

    #[test]
    fn startup_output_keeps_only_a_bounded_tail_and_drains_the_stream() {
        let mut input = vec![b'a'; 1024 * 1024];
        input.extend_from_slice(b"fatal assertion at the end");
        let tail = read_output_tail(&mut std::io::Cursor::new(&input)).unwrap();
        assert_eq!(tail, input[input.len() - 64 * 1024..]);
    }

    #[test]
    fn breakpoint_failure_reports_the_current_assertion_reason() {
        let (dir, _) = temp_file("assertion");
        fs::write(
            dir.join("startup_stdout.log"),
            "[Debug] <Critical> sysmodule.cpp: Assertion Failed!\nlibc.prx cannot be loaded\n",
        )
        .unwrap();
        let message = failure_message(0x8000_0003u32 as i32, &dir, &dir, false, BTreeMap::new());
        assert!(message.contains("0x80000003"));
        assert!(message.contains("libc.prx cannot be loaded"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn memory_failures_are_identified_from_stdout_without_full_capture() {
        let (dir, _) = temp_file("memory");
        let stdout = dir.join("startup_stdout.log");
        fs::write(
            &stdout,
            "Physical backing commit failed (Win32 1455): size=2450 MiB",
        )
        .unwrap();
        assert!(
            failure_message(0x8000_0003u32 as i32, &dir, &dir, false, BTreeMap::new())
                .contains("Win32 1455")
        );
        fs::write(
            &stdout,
            "Failed to create graphics pipeline: ErrorOutOfHostMemory",
        )
        .unwrap();
        assert!(
            failure_message(0x8000_0003u32 as i32, &dir, &dir, false, BTreeMap::new())
                .contains("driver Vulkan")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    fn temp_file(label: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("gow3-log-{label}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("shad_log.txt");
        (dir, file)
    }

    #[test]
    fn log_tail_does_not_repeat_previous_session_lines() {
        let (dir, file) = temp_file("baseline");
        fs::write(&file, "linha antiga\n").unwrap();
        let baseline = log_baseline(&file);
        fs::write(&file, "linha antiga\nlinha nova\n").unwrap();
        let mut tail = LogTail::new(file.clone(), baseline, true);
        assert!(tail.read(4096).contains("linha nova"));
        assert_eq!(tail.read(4096), "");
        fs::write(&file, "recomeço\n").unwrap();
        let baseline = log_baseline(&file);
        let mut tail = LogTail::new(file.clone(), baseline, false);
        fs::write(&file, "recomeço\ndepois\n").unwrap();
        assert!(tail.read(4096).contains("recomeço"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn discarding_pending_state_preserves_backups_and_unblocks_recovery() {
        let data = std::env::temp_dir().join(format!("gow3-discard-{}", uuid::Uuid::new_v4()));
        let manager = SessionManager::new(data.clone());
        fs::create_dir_all(&manager.sessions_dir).unwrap();
        fs::create_dir_all(&manager.environment.root).unwrap();

        let valid = manager.sessions_dir.join("20260101_000000_aaaa");
        fs::create_dir_all(&valid).unwrap();
        write_json(
            &valid.join("session.json"),
            &json!({
                "schema_version": 2,
                "isolated": true,
                "finalized": false,
                "status": "prepared",
                "root": manager.environment.root.to_string_lossy(),
            }),
        )
        .unwrap();

        let broken = manager.sessions_dir.join("20260101_000000_bbbb");
        fs::create_dir_all(&broken).unwrap();
        fs::write(broken.join("session.json"), b"{ nao e json").unwrap();

        let journal = manager.environment.journal_path();
        fs::write(&journal, b"{\"version\":1}").unwrap();

        // Pending state (journal or session) blocks launching until resolved.
        assert!(manager.recovery_needed().unwrap());
        assert_eq!(manager.discard_pending().unwrap(), 3);
        assert!(!journal.exists());
        assert!(!manager.recovery_needed().unwrap());

        let value = read_json(&valid.join("session.json")).unwrap();
        assert_eq!(value["finalized"], json!(true));
        assert_eq!(value["status"], json!("abandoned"));
        assert!(!broken.join("session.json").exists());
        assert!(fs::read_dir(&broken).unwrap().filter_map(Result::ok).any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("session.json.descartado-")
        }));

        fs::remove_dir_all(data).unwrap();
    }

    #[test]
    fn cancelling_recovery_leaves_pending_state_untouched() {
        let data = std::env::temp_dir().join(format!("gow3-cancel-{}", uuid::Uuid::new_v4()));
        let manager = SessionManager::new(data.clone());
        fs::create_dir_all(&manager.environment.root).unwrap();
        let journal = manager.environment.journal_path();
        fs::write(&journal, b"{\"version\":1}").unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        assert!(manager.recover_all(&cancel).is_err());
        assert!(
            journal.exists(),
            "a cancelled recovery must not restore or delete the journal"
        );
        fs::remove_dir_all(data).unwrap();
    }

    #[test]
    fn latest_summary_reads_only_private_sessions() {
        let data = std::env::temp_dir().join(format!("gow3-sessions-{}", uuid::Uuid::new_v4()));
        let manager = SessionManager::new(data.clone());
        fs::create_dir_all(&manager.sessions_dir).unwrap();
        let session = manager.sessions_dir.join("20260101_000000_abcd");
        fs::create_dir_all(&session).unwrap();
        fs::write(
            session.join("session.json"),
            serde_json::to_vec(&json!({
                "schema_version": 2,
                "isolated": true,
                "finalized": true,
                "profile": "Base60",
                "created_at": "2026-01-01T00:00:00Z",
                "status": "finished",
                "exit_code": 0,
                "capture_enabled": false
            }))
            .unwrap(),
        )
        .unwrap();
        let summary = manager.latest_summary().unwrap();
        assert_eq!(summary.profile, "Base60");
        assert_eq!(summary.exit_code, Some(0));
        fs::remove_dir_all(data).unwrap();
    }
}
