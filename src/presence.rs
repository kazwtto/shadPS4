//! Discord Rich Presence do launcher.
//!
//! Adaptado do experimento `rpc-lab`: a montagem da atividade e o laço de
//! conexão com espera vêm de lá. Aqui `details` é a área gravada no save de
//! checkpoint e `state` é o progresso de troféus. O IPC roda em uma thread
//! própria, então um Discord fechado — ou um pipe que não responde — nunca
//! trava a interface.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use discord_rich_presence::activity::{Activity, ActivityType, Assets, Timestamps};
use discord_rich_presence::{DiscordIpc, DiscordIpcClient};

/// Application ID do launcher (público: o IPC só o usa no handshake).
pub const CLIENT_ID: &str = "1571489117047496735";
/// Nome mostrado no cartão e no status da lista de membros.
pub const GAME_TITLE: &str = "God of War III Remastered";
/// Imagem grande do cartão: URL pública do ícone do próprio jogo (extraído de
/// `sce_sys/icon0.png` do pacote). O Discord busca a imagem sozinho, então não
/// depende de nenhum asset enviado no Developer Portal.
const LARGE_IMAGE: &str =
    "https://iili.io/oJLT6In.png";
/// Espera entre tentativas quando o Discord ainda não atende o IPC.
const RETRY: Duration = Duration::from_secs(5);
/// Tempo de espera por um comando antes de tentar (re)conectar.
const TICK: Duration = Duration::from_millis(250);
/// O Discord corta `details` e `state` em 128 caracteres.
const MAX_FIELD: usize = 128;

/// Serializa os testes que falam com o pipe do Discord (`discord-ipc-0`): só um
/// deles pode ser dono do pipe por vez, senão um rouba a conexão do outro.
#[cfg(test)]
pub(crate) static IPC_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// O que o cartão mostra: a área do save, o progresso de troféus e há quanto
/// tempo a sessão está aberta.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Presence {
    /// Área do checkpoint (`Poseidon`, `Olimpo`, ...). Vazia até haver save.
    pub phase: String,
    pub progress: String,
    /// Início da sessão em milissegundos; sem ele o cartão fica sem contador.
    pub started_at_ms: Option<i64>,
}

impl Presence {
    pub fn new(
        phase: impl Into<String>,
        progress: impl Into<String>,
        started_at_ms: Option<i64>,
    ) -> Self {
        Self {
            phase: phase.into(),
            progress: progress.into(),
            started_at_ms,
        }
    }

    /// Atividade pronta para envio.
    ///
    /// Sem `status_display_type`: o campo novo faz o cliente mostrar só
    /// "Jogando", sem nome nenhum depois. Sem ele, o Discord volta ao
    /// comportamento clássico e escreve "Jogando " + nome do aplicativo.
    pub fn activity(&self) -> Activity<'static> {
        let mut activity = Activity::new()
            .activity_type(ActivityType::Playing)
            .name(GAME_TITLE);

        let phase = clipped(&self.phase);
        if !phase.is_empty() {
            activity = activity.details(phase);
        }
        let progress = clipped(&self.progress);
        if !progress.is_empty() {
            activity = activity.state(progress);
        }

        activity = activity.assets(
            Assets::new()
                .large_image(LARGE_IMAGE)
                .large_text(GAME_TITLE),
        );
        if let Some(start) = self.started_at_ms {
            activity = activity.timestamps(Timestamps::new().start(start));
        }
        activity
    }
}

/// Linha de troféus do cartão: `Troféus 12 de 35 · 34%`.
///
/// Sem troféus lidos não há linha nenhuma — nada de "0 de 0" inventado.
pub fn trophy_progress_text(unlocked: usize, total: usize) -> String {
    if total == 0 {
        return String::new();
    }
    format!("Troféus {} de {} · {}%", unlocked, total, unlocked * 100 / total)
}

fn clipped(text: &str) -> String {
    text.trim().chars().take(MAX_FIELD).collect()
}

/// Nome do capítulo a partir do código do cenário gravado no save
/// (`Pos050` → `Poseidon`). O save é a única fonte confiável da área: o log do
/// emulador só despeja o manifesto de WADs, com todos os cenários do jogo.
pub fn zone_label(zone: &str) -> String {
    let prefix: String = zone.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    match prefix.as_str() {
        "Pos" => "Poseidon",
        "Oly" => "Olimpo",
        "Bas" => "Base do Olimpo",
        "Hds" | "Had" => "Hades",
        "Crn" => "Cronos",
        "Cav" => "Cavernas",
        "Ica" => "Icarus",
        "Lab" => "Labirinto",
        "Gaz" => "Cidadela Olímpica",
        "Brk" => "Olimpo",
        _ => "Explorando",
    }
    .to_string()
}

/// Área gravada no conteúdo do SAVEDATA: o jogo anota ali o cenário do
/// checkpoint (`WAD_Pos050`). Vale o último código de cenário do arquivo;
/// recursos como `WAD_R_Perm` são ignorados. Um arquivo truncado (leitura no
/// meio de uma gravação) devolve `None` — nunca uma área pela metade.
pub fn zone_from_save(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let mut found = None;
    for tail in text.split("WAD_").skip(1) {
        let code: String = tail
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        if is_zone_code(&code) {
            found = Some(code);
        }
    }
    found
}

/// Código de cenário: três a cinco letras e três dígitos (`Pos050`). Recursos
/// (`R_SoulCerberus00`) e WADs de sistema ficam de fora.
pub fn is_zone_code(code: &str) -> bool {
    if !code.starts_with(|c: char| c.is_ascii_uppercase()) {
        return false;
    }
    let letters = code.chars().take_while(|c| c.is_ascii_alphabetic()).count();
    let digits = code.len() - letters;
    (3..=5).contains(&letters)
        && digits == 3
        && code.as_bytes()[letters..].iter().all(u8::is_ascii_digit)
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

enum Command {
    Publish(Presence),
    Clear,
    Shutdown,
}

/// Dono da thread do IPC. Publicar é barato; a conexão acontece na thread.
pub struct PresenceService {
    sender: Sender<Command>,
    worker: Option<thread::JoinHandle<()>>,
    /// Avisa quando a thread terminou; a saída não espera por ela além de um
    /// limite curto.
    finished: Receiver<()>,
    /// CPU da própria thread (kernel + user, em ms), amostrado por ela a cada
    /// volta do laço. Medir o processo inteiro misturaria o trabalho de outras
    /// threads; este contador é só do worker.
    cpu_ms: Arc<AtomicU64>,
}

impl PresenceService {
    pub fn start() -> Self {
        let (sender, receiver) = channel();
        let (done, finished) = channel();
        let cpu_ms = Arc::new(AtomicU64::new(0));
        let cpu_for_worker = cpu_ms.clone();
        let worker = thread::Builder::new()
            .name("discord-presence".into())
            .spawn(move || {
                run(receiver, Some(cpu_for_worker));
                let _ = done.send(());
            })
            .ok();
        Self {
            sender,
            worker,
            finished,
            cpu_ms,
        }
    }

    /// CPU consumido pela thread do worker (ms, kernel + user). Prova, sem
    /// depender do resto do processo, que ela não gira em laço apertado.
    pub(crate) fn worker_cpu_ms(&self) -> u64 {
        self.cpu_ms.load(Ordering::Relaxed)
    }

    pub fn publish(&self, presence: Presence) {
        let _ = self.sender.send(Command::Publish(presence));
    }

    pub fn clear(&self) {
        let _ = self.sender.send(Command::Clear);
    }
}

impl Drop for PresenceService {
    fn drop(&mut self) {
        let _ = self.sender.send(Command::Shutdown);
        // Um pipe de RPC que não responde não pode travar o fechamento do
        // launcher: espera só um instante e segue.
        let _ = self.finished.recv_timeout(Duration::from_millis(500));
        self.worker.take();
    }
}

/// Laço único da thread: guarda o que se quer mostrar, tenta conectar com
/// espera, reenvia só quando o conteúdo muda e limpa ao sair.
fn run(commands: Receiver<Command>, cpu_ms: Option<Arc<AtomicU64>>) {
    let mut wanted: Option<Presence> = None;
    let mut sent: Option<Presence> = None;
    let mut client: Option<DiscordIpcClient> = None;
    let mut next_attempt = Instant::now();
    // Cartão deixado por um processo morto (ou por uma ponte de RPC) não pode
    // continuar no perfil: a primeira conexão limpa antes de qualquer envio —
    // mesmo que não haja nada para publicar.
    let mut wipe = true;
    let mut wipe_attempts: u8 = 0;

    loop {
        if let Some(cell) = &cpu_ms {
            cell.store(thread_cpu_ms(), Ordering::Relaxed);
        }
        let mut stop = false;
        match commands.recv_timeout(TICK) {
            Ok(Command::Publish(presence)) => {
                wanted = Some(presence);
                sent = None;
            }
            Ok(Command::Clear) => {
                wanted = None;
                sent = None;
                match client.as_mut().map(|active| active.clear_activity()) {
                    Some(Ok(())) => {
                        // Sem nada para publicar, a conexão é encerrada: o
                        // cliente limpa o cartão ao ver o pipe fechar.
                        if let Some(mut idle) = client.take() {
                            let _ = idle.close();
                        }
                        wipe = false;
                    }
                    // Sem cliente conectado (ou com o pipe morto) a limpeza é
                    // refeita assim que houver conexão.
                    _ => {
                        if let Some(mut stale) = client.take() {
                            let _ = stale.close();
                        }
                        wipe = true;
                        next_attempt = Instant::now();
                    }
                }
            }
            Ok(Command::Shutdown) => stop = true,
            Err(RecvTimeoutError::Timeout) => {}
            // O processo está encerrando: limpa o que estiver no ar.
            Err(RecvTimeoutError::Disconnected) => stop = true,
        }
        if stop {
            break;
        }

        let target = wanted.clone();
        let settled = target.is_none() && !wipe
            || target.is_some() && !wipe && sent.as_ref() == target.as_ref();
        if settled {
            continue;
        }

        if client.is_none() {
            if Instant::now() < next_attempt {
                continue;
            }
            let mut candidate = DiscordIpcClient::new(CLIENT_ID);
            client = match candidate.connect() {
                Ok(()) => Some(candidate),
                Err(_) => None,
            };
            if client.is_none() {
                next_attempt = Instant::now() + RETRY;
                if wipe && target.is_none() {
                    wipe_attempts = wipe_attempts.saturating_add(1);
                    // Sem Discord atendendo, para de tentar limpar sozinho.
                    if wipe_attempts >= 12 {
                        wipe = false;
                    }
                }
                continue;
            }
        }

        if wipe {
            if let Some(active) = client.as_mut() {
                // Um CLEAR sem nada publicado antes é ignorado por algumas
                // builds do cliente; publicar algo mínimo e limpar em seguida
                // garante que o cartão antigo saia do perfil.
                let _ = active.set_activity(
                    Activity::new()
                        .activity_type(ActivityType::Playing)
                        .name(GAME_TITLE),
                );
                thread::sleep(Duration::from_millis(250));
                let _ = active.clear_activity();
            }
            wipe = false;
            wipe_attempts = 0;
            if target.is_none() {
                if let Some(mut idle) = client.take() {
                    let _ = idle.close();
                }
                continue;
            }
        }

        let Some(target) = target else {
            continue;
        };
        if sent.as_ref() == Some(&target) {
            continue;
        }

        let result = client
            .as_mut()
            .map(|active| active.set_activity(target.activity()));
        match result {
            Some(Ok(())) => sent = Some(target),
            // Uma conexão morta não deve ser reusada: fecha e tenta de novo.
            _ => {
                if let Some(mut stale) = client.take() {
                    let _ = stale.close();
                }
                sent = None;
                next_attempt = Instant::now() + RETRY;
            }
        }
    }

    if let Some(mut active) = client.take() {
        let _ = active.clear_activity();
        let _ = active.close();
    }
}

/// CPU da thread que chama, em ms (kernel + user). Amostrar a própria thread do
/// worker isola o consumo dele do trabalho das outras threads do processo.
fn thread_cpu_ms() -> u64 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::{GetCurrentThread, GetThreadTimes};

        fn ticks(time: FILETIME) -> u64 {
            ((time.dwHighDateTime as u64) << 32) | time.dwLowDateTime as u64
        }

        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut creation = zero;
        let mut exit = zero;
        let mut kernel = zero;
        let mut user = zero;
        let ok = unsafe {
            GetThreadTimes(
                GetCurrentThread(),
                &mut creation,
                &mut exit,
                &mut kernel,
                &mut user,
            )
        };
        if ok == 0 {
            return 0;
        }
        (ticks(kernel) + ticks(user)) / 10_000
    }
    #[cfg(not(windows))]
    {
        0
    }
}

#[cfg(test)]
pub(crate) mod tests {
    /// CPU do processo em ms (kernel + user). Continua disponível como leitura
    /// auxiliar; a prova do laço apertado usa o CPU da própria thread do worker,
    /// porque o processo também roda o resto dos testes.
    #[cfg(windows)]
    pub(crate) fn process_cpu_ms() -> u64 {
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

        fn ticks(time: FILETIME) -> u64 {
            ((time.dwHighDateTime as u64) << 32) | time.dwLowDateTime as u64
        }

        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut creation = zero;
        let mut exit = zero;
        let mut kernel = zero;
        let mut user = zero;
        let ok = unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut creation,
                &mut exit,
                &mut kernel,
                &mut user,
            )
        };
        assert_ne!(ok, 0, "GetProcessTimes falhou");
        (ticks(kernel) + ticks(user)) / 10_000
    }

    use super::*;
    use serde_json::Value;

    const NOW: i64 = 1_760_000_000_000;

    fn value(presence: &Presence) -> Value {
        serde_json::to_value(presence.activity()).unwrap()
    }

    #[test]
    fn area_becomes_details_and_progress_becomes_state() {
        let presence = Presence::new(
            zone_label("Pos050"),
            "Troféus 12 de 35 · 34%",
            Some(NOW),
        );
        let activity = value(&presence);

        assert_eq!(activity["type"], 0, "o cartão é sempre Playing");
        assert_eq!(activity["name"], GAME_TITLE);
        assert_eq!(activity["details"], "Poseidon");
        assert_eq!(activity["state"], "Troféus 12 de 35 · 34%");
        assert_eq!(activity["timestamps"]["start"], NOW);
        assert_eq!(activity["assets"]["large_image"], LARGE_IMAGE);
        assert_eq!(activity["assets"]["large_text"], GAME_TITLE);
    }

    /// A linha de baixo mantém a contagem sobre 35 e a porcentagem.
    #[test]
    fn trophy_line_counts_out_of_35() {
        assert_eq!(trophy_progress_text(0, 35), "Troféus 0 de 35 · 0%");
        assert_eq!(trophy_progress_text(12, 35), "Troféus 12 de 35 · 34%");
        assert_eq!(trophy_progress_text(35, 35), "Troféus 35 de 35 · 100%");
        assert_eq!(trophy_progress_text(0, 0), "");
    }

    #[test]
    fn zone_codes_become_chapter_names() {
        assert_eq!(zone_label("Pos050"), "Poseidon");
        assert_eq!(zone_label("Oly350"), "Olimpo");
        assert_eq!(zone_label("Lab200"), "Labirinto");
        assert_eq!(zone_label("Cav610"), "Cavernas");
        assert_eq!(zone_label("Ica125"), "Icarus");
        assert_eq!(zone_label("Hds750"), "Hades");
        assert_eq!(zone_label("Crn200"), "Cronos");
        assert_eq!(zone_label("Bas100"), "Base do Olimpo");
        assert_eq!(zone_label("Zzz999"), "Explorando");
    }

    #[test]
    fn zone_code_filter_ignores_resources() {
        assert!(is_zone_code("Cav610"));
        assert!(is_zone_code("Pos050"));
        assert!(!is_zone_code("R_SoulCerberus00"));
        assert!(!is_zone_code("R_FX"));
        assert!(!is_zone_code("R_Perm"));
        assert!(!is_zone_code("Bas1000"));
        assert!(!is_zone_code("bas100"));
    }

    /// O save real do título traz `WAD_Pos050` junto de recursos (`WAD_R_Perm`):
    /// só o cenário vira área.
    #[test]
    fn save_content_yields_the_checkpoint_area() {
        let save = b"\x00\x01RCM_CCGaiaHelp\x00WAD_Pos050\x00SND_POS05010L\x00\
Pos050_GaiaCryOutTalk1\x00WAD_Pos050\x00MIX_Pos050_GaiaCryOutTalk1\x00WAD_R_Perm\x00";
        assert_eq!(zone_from_save(save).as_deref(), Some("Pos050"));
        assert_eq!(zone_label(&zone_from_save(save).unwrap()), "Poseidon");
    }

    /// Leitura no meio da gravação não pode virar área nem consumir o carimbo.
    #[test]
    fn truncated_save_is_not_an_area() {
        assert_eq!(zone_from_save(b"WAD_Pos0").as_deref(), None);
        assert_eq!(zone_from_save(b"WAD_").as_deref(), None);
        assert_eq!(zone_from_save(b"").as_deref(), None);
    }

    #[test]
    fn payload_has_no_status_display_type() {
        // O campo fazia o status virar só "Jogando"; o cartão não precisa dele.
        let activity = value(&Presence::new("Poseidon", "Troféus 0 de 35 · 0%", None));
        assert!(activity.get("status_display_type").is_none());
        assert_eq!(activity["name"], GAME_TITLE);
        assert_eq!(activity["state"], "Troféus 0 de 35 · 0%");
    }

    #[test]
    fn empty_fields_are_omitted() {
        let activity = value(&Presence::new("  ", "", None));
        assert!(activity.get("details").is_none(), "sem save, sem área inventada");
        assert!(activity.get("state").is_none());
        assert!(activity.get("timestamps").is_none());
        assert_eq!(activity["type"], 0);
    }

    #[test]
    fn long_fields_are_clipped_to_the_discord_limit() {
        let long = "a".repeat(200);
        let activity = value(&Presence::new(long.clone(), long, None));
        assert_eq!(activity["details"].as_str().unwrap().chars().count(), MAX_FIELD);
        assert_eq!(activity["state"].as_str().unwrap().chars().count(), MAX_FIELD);
    }

    /// Sonda local: publica o cartão por ~2 s para conferir se o cliente
    /// aceita o payload inteiro (e se não aceita, se o básico passa).
    #[test]
    #[ignore = "sonda local: publica no Discord por alguns segundos"]
    pub(crate) fn local_publish_probe() {
        let mut client = DiscordIpcClient::new(CLIENT_ID);
        match client.connect() {
            Ok(()) => {
                let presence = Presence::new(
                    zone_label("Pos050"),
                    "Troféus 0 de 35 · 0%",
                    Some(now_ms()),
                );
                let sent = client.set_activity(presence.activity());
                println!("PROBE rich={sent:?}");
                std::thread::sleep(Duration::from_secs(2));
                let _ = client.clear_activity();
                let _ = client.close();
            }
            Err(error) => println!("PROBE ipc=indisponivel ({error})"),
        }
    }

    /// Sonda local: imprime exatamente o JSON que vai no SET_ACTIVITY.
    #[test]
    #[ignore = "sonda local: confere o payload"]
    pub(crate) fn local_payload_probe() {
        let presence = Presence::new(
            zone_label("Pos050"),
            "Troféus 0 de 35 · 0%",
            Some(now_ms()),
        );
        let payload = serde_json::json!({
            "cmd": "SET_ACTIVITY",
            "args": { "pid": 0, "activity": presence.activity() },
        });
        println!(
            "PROBE {}",
            serde_json::to_string_pretty(&payload).unwrap()
        );
    }

    /// Sonda local: só o handshake, sem publicar atividade nenhuma.
    #[test]
    #[ignore = "depende de um Discord ou ponte local no pipe"]
    pub(crate) fn local_ipc_probe() {
        let mut client = DiscordIpcClient::new(CLIENT_ID);
        match client.connect() {
            Ok(()) => {
                println!("PROBE ipc=ok");
                let _ = client.close();
            }
            Err(error) => println!("PROBE ipc=indisponivel ({error})"),
        }
    }

    /// Sonda local: exercita o serviço inteiro (conexão, publicação, limpeza)
    /// contra um servidor IPC de mentira — sem precisar do cliente aberto.
    #[test]
    #[ignore = "sonda local: conversa com um servidor IPC de teste"]
    pub(crate) fn local_service_probe() {
        let service = PresenceService::start();
        service.publish(Presence::new(
            zone_label("Pos050"),
            "Troféus 12 de 35 · 34%",
            Some(now_ms()),
        ));
        thread::sleep(Duration::from_secs(4));
        service.clear();
        thread::sleep(Duration::from_secs(2));
        println!("PROBE servico concluido");
    }

    /// Sem cliente nenhum nesta máquina: publicar e limpar voltam na hora, o
    /// worker não gira em laço apertado (retry de 5 s) e o processo quase não
    /// gasta CPU — é isso que garante que a ausência do Discord não atrapalha
    /// o jogo nem o launcher.
    #[cfg(windows)]
    #[test]
    fn service_stays_idle_without_discord() {
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

        fn ticks(time: FILETIME) -> u64 {
            ((time.dwHighDateTime as u64) << 32) | time.dwLowDateTime as u64
        }

        fn cpu_time() -> u64 {
            let mut creation = FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            let mut exit = FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            let mut kernel = FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            let mut user = FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            let ok = unsafe {
                GetProcessTimes(
                    GetCurrentProcess(),
                    &mut creation,
                    &mut exit,
                    &mut kernel,
                    &mut user,
                )
            };
            assert_ne!(ok, 0, "GetProcessTimes falhou");
            ticks(kernel) + ticks(user)
        }

        let _ipc = IPC_TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let service = PresenceService::start();
        let published = Instant::now();
        service.publish(Presence::new("Poseidon", "Troféus 0 de 35 · 0%", Some(NOW)));
        let publish_elapsed = published.elapsed();
        assert!(
            publish_elapsed < Duration::from_millis(50),
            "publicar não pode bloquear quem chamou: {publish_elapsed:?}"
        );

        let before = cpu_time();
        let worker_before = service.worker_cpu_ms();
        thread::sleep(Duration::from_secs(6));
        let used_ms = service.worker_cpu_ms().saturating_sub(worker_before);
        let process_ms = (cpu_time() - before) / 10_000;
        println!(
            "PROBE cpu_sem_discord={used_ms} ms da thread do worker em 6 s (processo={process_ms} ms, publicar={publish_elapsed:?})"
        );
        assert!(
            used_ms < 1_500,
            "sem Discord o worker não pode reabrir conexão em laço apertado: {used_ms} ms em 6 s"
        );

        let cleared = Instant::now();
        service.clear();
        drop(service);
        let shutdown = cleared.elapsed();
        println!("PROBE shutdown={shutdown:?}");
        assert!(
            shutdown < Duration::from_secs(1),
            "derrubar o serviço sem Discord não pode travar: {shutdown:?}"
        );
    }

    #[test]
    fn service_survives_without_discord() {
        // Sem Discord o worker só acumula tentativas; publicar e limpar devem
        // retornar imediatamente, sem bloquear quem chamou.
        let _ipc = IPC_TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let service = PresenceService::start();
        let started = Instant::now();
        service.publish(Presence::new("Poseidon", "Troféus 0 de 35 · 0%", Some(NOW)));
        service.clear();
        assert!(started.elapsed() < Duration::from_millis(200));
        // E derrubar o serviço também não pode travar a saída.
        let shutdown = Instant::now();
        drop(service);
        assert!(shutdown.elapsed() < Duration::from_secs(2));
    }
}
