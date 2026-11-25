//! Vigia dos saves do título: descobre a área do checkpoint atual.
//!
//! O jogo grava o cenário em que salvou em
//! `home/1000/savedata/CUSA01623/<slot>/SAVEDATA` (por exemplo `WAD_Pos050`).
//! Este módulo lê o save mais recente que tenha um cenário legível e avisa
//! quando ele mudou — sem nunca inventar área a partir do log nem de um arquivo
//! auxiliar sem cenário.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::presence::zone_from_save;

/// Carimbo da última área lida. Um save novo (checkpoint novo) muda o carimbo e
/// libera a atualização do cartão.
pub struct SaveWatch {
    stamp: Option<SystemTime>,
}

impl Default for SaveWatch {
    fn default() -> Self {
        Self::new()
    }
}

impl SaveWatch {
    pub fn new() -> Self {
        Self { stamp: None }
    }

    /// Esquece o carimbo: a próxima leitura vale como novidade (início de
    /// sessão).
    pub fn reset(&mut self) {
        self.stamp = None;
    }

    /// Todos os `SAVEDATA` do título (autosave e slots), do mais recente para o
    /// mais antigo.
    pub fn saves(root: &Path) -> Vec<(SystemTime, PathBuf)> {
        let mut files = Vec::new();
        let Ok(entries) = fs::read_dir(root) else {
            return files;
        };
        for entry in entries.flatten() {
            let path = entry.path().join("SAVEDATA");
            let Ok(metadata) = fs::metadata(&path) else {
                continue;
            };
            let Ok(modified) = metadata.modified() else {
                continue;
            };
            files.push((modified, path));
        }
        files.sort_by(|a, b| b.0.cmp(&a.0));
        files
    }

    /// Área do save mais recente que tenha um cenário legível. Um arquivo
    /// auxiliar sem cenário (32 bytes, por exemplo) é pulado: ele nunca
    /// mascara o save de verdade.
    pub fn zone_save(root: &Path) -> Option<(SystemTime, String)> {
        Self::saves(root).into_iter().find_map(|(stamp, path)| {
            let bytes = fs::read(&path).ok()?;
            zone_from_save(&bytes).map(|code| (stamp, code))
        })
    }

    /// Código do cenário quando a área mudou desde a última leitura.
    ///
    /// Uma leitura parcial (o jogo gravando o arquivo neste instante) devolve
    /// `None` e **não** consome o carimbo, então a área não congela: a próxima
    /// leitura do mesmo checkpoint ainda é detectada.
    pub fn poll(&mut self, root: &Path) -> Option<String> {
        let (stamp, code) = Self::zone_save(root)?;
        if self.stamp == Some(stamp) {
            return None;
        }
        self.stamp = Some(stamp);
        Some(code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::zone_label;
    use std::time::Duration;

    const POS050: &[u8] =
        b"\x00\x01RCM_CCGaiaHelp\x00WAD_Pos050\x00SND_POS05010L\x00Pos050_GaiaCryOutTalk1\x00WAD_Pos050\x00WAD_R_Perm\x00";
    const OLY100: &[u8] =
        b"\x00\x01RCM_CCOlyHelp\x00WAD_Oly100\x00SND_OLY10010L\x00Oly100_Intro\x00WAD_Oly100\x00WAD_R_Perm\x00";
    /// Save auxiliar sem cenário: pequeno, sem `WAD_` de área.
    const AUXILIARY: &[u8] = b"USERDATA-PROGRESS\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00";

    struct Sandbox {
        root: PathBuf,
    }

    impl Sandbox {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "gow3-savewatch-{}-{name}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).expect("sandbox de saves");
            Self { root }
        }

        /// Grava o SAVEDATA de um slot com carimbo mais novo que o anterior.
        fn write(&self, slot: &str, bytes: &[u8]) {
            let dir = self.root.join(slot);
            fs::create_dir_all(&dir).expect("slot de save");
            fs::write(dir.join("SAVEDATA"), bytes).expect("grava SAVEDATA");
            // Garante carimbo estritamente mais novo que o arquivo anterior.
            std::thread::sleep(Duration::from_millis(30));
        }

        fn path(&self) -> &Path {
            &self.root
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    /// Ida e volta de capítulo: Pos050 → Oly100 → Pos050, como o jogo faria ao
    /// gravar checkpoints em áreas diferentes.
    #[test]
    fn checkpoint_area_changes_back_and_forth() {
        let sandbox = Sandbox::new("ida-volta");
        let mut watch = SaveWatch::new();

        sandbox.write("AUTOSAVE", POS050);
        assert_eq!(watch.poll(sandbox.path()).as_deref(), Some("Pos050"));
        assert_eq!(zone_label("Pos050"), "Poseidon");

        sandbox.write("AUTOSAVE", OLY100);
        assert_eq!(watch.poll(sandbox.path()).as_deref(), Some("Oly100"));
        assert_eq!(zone_label("Oly100"), "Olimpo");

        sandbox.write("AUTOSAVE", POS050);
        assert_eq!(watch.poll(sandbox.path()).as_deref(), Some("Pos050"));
        assert_eq!(zone_label("Pos050"), "Poseidon");

        // Sem save novo, o cartão não é republicado.
        assert_eq!(watch.poll(sandbox.path()), None);
    }

    /// Um save mais novo porém truncado (gravação em andamento) não troca a
    /// área nem consome o carimbo: o checkpoint seguinte ainda é detectado.
    #[test]
    fn truncated_newer_save_neither_changes_nor_freezes() {
        let sandbox = Sandbox::new("truncado");
        let mut watch = SaveWatch::new();

        sandbox.write("AUTOSAVE", POS050);
        assert_eq!(watch.poll(sandbox.path()).as_deref(), Some("Pos050"));

        // Arquivo no meio da gravação: código sem os três dígitos.
        sandbox.write("AUTOSAVE", b"WAD_Oly1");
        assert_eq!(watch.poll(sandbox.path()), None, "área pela metade não vale");
        assert_eq!(SaveWatch::zone_save(sandbox.path()), None);

        // Gravação concluída do mesmo checkpoint: detectada na hora.
        sandbox.write("AUTOSAVE", OLY100);
        assert_eq!(
            watch.poll(sandbox.path()).as_deref(),
            Some("Oly100"),
            "o carimbo não foi consumido pela leitura parcial"
        );

        // E a volta para a área antiga continua funcionando.
        sandbox.write("AUTOSAVE", POS050);
        assert_eq!(watch.poll(sandbox.path()).as_deref(), Some("Pos050"));
    }

    /// Save auxiliar de 32 bytes, mais novo que o autosave, não pode mascarar o
    /// save real nem esconder a próxima troca de área.
    #[test]
    fn auxiliary_save_without_area_does_not_mask_the_real_one() {
        let sandbox = Sandbox::new("auxiliar");
        let mut watch = SaveWatch::new();

        sandbox.write("AUTOSAVE", POS050);
        sandbox.write("USERDATA", AUXILIARY);
        assert_eq!(
            watch.poll(sandbox.path()).as_deref(),
            Some("Pos050"),
            "o auxiliar sem cenário é pulado"
        );

        // O autosave muda depois do auxiliar: a troca aparece.
        sandbox.write("AUTOSAVE", OLY100);
        assert_eq!(watch.poll(sandbox.path()).as_deref(), Some("Oly100"));

        // E o auxiliar sendo reescrito por último não pode congelar a área.
        sandbox.write("USERDATA", AUXILIARY);
        assert_eq!(watch.poll(sandbox.path()), None);
        sandbox.write("AUTOSAVE", POS050);
        assert_eq!(watch.poll(sandbox.path()).as_deref(), Some("Pos050"));
    }

    /// Sonda local: leva a mesma troca de área do sandbox até o IPC real do
    /// cliente aberto, provando que o payload com a área nova é aceito.
    #[test]
    #[ignore = "sonda local: publica no cliente Discord aberto"]
    fn local_ipc_area_probe() {
        use crate::presence::{now_ms, Presence, CLIENT_ID};
        use discord_rich_presence::{DiscordIpc, DiscordIpcClient};

        let sandbox = Sandbox::new("ipc");
        let mut watch = SaveWatch::new();
        let mut client = DiscordIpcClient::new(CLIENT_ID);
        match client.connect() {
            Ok(()) => println!("PROBE ipc=ok"),
            Err(error) => {
                println!("PROBE ipc=indisponivel ({error})");
                return;
            }
        }
        for (bytes, expected) in [(POS050, "Poseidon"), (OLY100, "Olimpo"), (POS050, "Poseidon")] {
            sandbox.write("AUTOSAVE", bytes);
            let code = watch.poll(sandbox.path()).expect("área detectada no sandbox");
            let presence = Presence::new(
                zone_label(&code),
                "Troféus 0 de 35 · 0%",
                Some(now_ms()),
            );
            assert_eq!(presence.phase, expected);
            let sent = client.set_activity(presence.activity());
            println!(
                "PROBE codigo={code} detalhe={} estado={} aceito={sent:?}",
                presence.phase, presence.progress
            );
        }
        let _ = client.clear_activity();
        let _ = client.close();
    }

    /// Sem save nenhum (arquivo ausente), não há área: nada de valor inventado
    /// e nada de pânico.
    #[test]
    fn missing_save_yields_no_area() {
        let sandbox = Sandbox::new("ausente");
        let mut watch = SaveWatch::new();

        assert_eq!(watch.poll(sandbox.path()), None);
        assert_eq!(SaveWatch::zone_save(sandbox.path()), None);
        // Diretório do título que nem existe (jogo nunca rodou).
        assert_eq!(SaveWatch::saves(&sandbox.path().join("CUSA00000")).len(), 0);

        let presence = crate::presence::Presence::new("", "Troféus 0 de 35 · 0%", None);
        let payload = serde_json::to_value(presence.activity()).unwrap();
        assert!(
            payload.get("details").is_none(),
            "sem save o cartão não mostra área nenhuma"
        );
    }

    /// Arquivo recém-criado/parcialmente escrito, sem nenhum save completo
    /// antes: continua sem área (e depois o save completo aparece).
    #[test]
    fn partial_write_without_a_full_save_never_invents_an_area() {
        let sandbox = Sandbox::new("parcial");
        let mut watch = SaveWatch::new();

        sandbox.write("AUTOSAVE", b"WAD_Pos0");
        assert_eq!(watch.poll(sandbox.path()), None);
        assert_eq!(SaveWatch::zone_save(sandbox.path()), None);

        sandbox.write("AUTOSAVE", POS050);
        assert_eq!(watch.poll(sandbox.path()).as_deref(), Some("Pos050"));
        assert_eq!(zone_label("Pos050"), "Poseidon");
    }

    /// SAVEDATA bloqueado por outro processo (aberto sem compartilhamento): a
    /// leitura falha, a área não é inventada nem congela, e assim que o arquivo
    /// é liberado a área entra.
    #[cfg(windows)]
    #[test]
    fn save_locked_by_another_process_is_skipped_then_read() {
        use std::os::windows::fs::OpenOptionsExt;

        let sandbox = Sandbox::new("bloqueado");
        sandbox.write("AUTOSAVE", POS050);
        let path = sandbox.path().join("AUTOSAVE").join("SAVEDATA");
        // share_mode(0): ninguém mais consegue ler enquanto o handle existir.
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .expect("abrir o SAVEDATA com bloqueio exclusivo");

        let mut watch = SaveWatch::new();
        assert_eq!(SaveWatch::zone_save(sandbox.path()), None, "bloqueado = sem leitura");
        assert_eq!(watch.poll(sandbox.path()), None, "e sem área inventada");
        // O laço de 1 s do launcher continua chamando; não pode entrar em pânico.
        for _ in 0..3 {
            assert_eq!(watch.poll(sandbox.path()), None);
            std::thread::sleep(Duration::from_millis(10));
        }

        drop(lock);
        assert_eq!(
            watch.poll(sandbox.path()).as_deref(),
            Some("Pos050"),
            "liberado o arquivo, a área entra"
        );
    }

    /// Prova de ponta a ponta sem cliente externo: um pipe local imita o
    /// Discord, aceita o handshake e anota o que o launcher manda de verdade.
    /// Quando já existe um cliente atendendo o pipe, o teste é pulado e nada
    /// do cliente do usuário é tocado.
    #[cfg(windows)]
    #[test]
    fn presence_service_publishes_new_area_and_clears_over_ipc() {
        use crate::presence::{
            now_ms, trophy_progress_text, Presence, PresenceService, CLIENT_ID, IPC_TEST_LOCK,
        };

        let _ipc = IPC_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let stub = match crate::discord_stub::Stub::start() {
            crate::discord_stub::StubStart::Ready(stub) => stub,
            crate::discord_stub::StubStart::Occupied(code) => {
                println!(
                    "SKIP: discord-ipc-0 ocupado (erro {code}); nada do cliente do usuário \
                     foi tocado e a suíte segue"
                );
                return;
            }
            crate::discord_stub::StubStart::Failed(code) => {
                panic!("o stub não conseguiu criar discord-ipc-0 (erro {code})")
            }
        };

        let sandbox = Sandbox::new("ipc-stub");
        let mut watch = SaveWatch::new();
        let service = PresenceService::start();

        // Estado A: o save real do cabeçalho (Pos050 → Poseidon).
        sandbox.write("AUTOSAVE", POS050);
        let code = watch.poll(sandbox.path()).expect("área inicial no sandbox");
        let progress = trophy_progress_text(12, 35);
        println!("STUB sandbox area={code} detalhe={} estado={progress}", zone_label(&code));
        service.publish(Presence::new(
            zone_label(&code),
            progress.clone(),
            Some(now_ms()),
        ));

        // (a)+(b): handshake aceito e atividade com a área e a linha de troféus.
        let first = stub
            .wait_for(Duration::from_secs(20), |frames| {
                frames.iter().any(|(op, value)| {
                    *op == 1
                        && value["cmd"] == "SET_ACTIVITY"
                        && value["args"]["activity"]["details"] == "Poseidon"
                })
            })
            .expect("SET_ACTIVITY com a área inicial");
        assert!(
            stub.accepted(),
            "o serviço de produção precisa ter conectado no stub"
        );
        let handshake = first
            .iter()
            .find(|(op, value)| *op == 0 && value["client_id"] == CLIENT_ID)
            .expect("handshake recebido pelo stub");
        println!("STUB <- handshake {}", handshake.1);
        for (op, value) in &first {
            println!("STUB <- op={op} {value}");
        }
        let poseidon = first
            .iter()
            .find(|(_, value)| {
                value["cmd"] == "SET_ACTIVITY"
                    && value["args"]["activity"]["details"] == "Poseidon"
            })
            .expect("atividade com Poseidon")
            .1
            .clone();
        assert_eq!(poseidon["args"]["activity"]["state"], progress.as_str());
        assert_eq!(poseidon["args"]["activity"]["name"], "God of War III Remastered");

        // (c): o save muda no sandbox e um novo SET_ACTIVITY traz a área nova.
        sandbox.write("AUTOSAVE", OLY100);
        let code = watch.poll(sandbox.path()).expect("área nova no sandbox");
        assert_eq!(code, "Oly100");
        service.publish(Presence::new(
            zone_label(&code),
            progress.clone(),
            Some(now_ms()),
        ));
        let second = stub
            .wait_for(Duration::from_secs(20), |frames| {
                frames.iter().any(|(op, value)| {
                    *op == 1
                        && value["cmd"] == "SET_ACTIVITY"
                        && value["args"]["activity"]["details"] == "Olimpo"
                })
            })
            .expect("SET_ACTIVITY com a área nova");
        let olimpo = second
            .iter()
            .find(|(_, value)| {
                value["cmd"] == "SET_ACTIVITY"
                    && value["args"]["activity"]["details"] == "Olimpo"
            })
            .expect("atividade com Olimpo")
            .1
            .clone();
        println!("STUB <- nova área {olimpo}");
        assert_eq!(olimpo["args"]["activity"]["state"], progress.as_str());

        // (d): no clear chega um SET_ACTIVITY sem atividade, depois da área nova.
        service.clear();
        let cleared = stub
            .wait_for(Duration::from_secs(20), |frames| {
                let mut seen_new_area = false;
                for (op, value) in frames {
                    if *op != 1 || value["cmd"] != "SET_ACTIVITY" {
                        continue;
                    }
                    let activity = &value["args"]["activity"];
                    if activity["details"] == "Olimpo" {
                        seen_new_area = true;
                    }
                    if seen_new_area && activity.is_null() {
                        return true;
                    }
                }
                false
            })
            .expect("SET_ACTIVITY de limpeza");
        println!("STUB <- total de mensagens: {}", cleared.len());
        for (op, value) in &cleared {
            println!("STUB <- op={op} {value}");
        }

        let details: Vec<Option<&str>> = cleared
            .iter()
            .filter(|(op, value)| *op == 1 && value["cmd"] == "SET_ACTIVITY")
            .map(|(_, value)| value["args"]["activity"]["details"].as_str())
            .collect();
        let pos = details
            .iter()
            .position(|detail| *detail == Some("Poseidon"))
            .expect("Poseidon publicado");
        let oly = details
            .iter()
            .position(|detail| *detail == Some("Olimpo"))
            .expect("Olimpo republicado");
        assert!(pos < oly, "a área nova vem depois da antiga: {details:?}");
        assert!(
            details[oly + 1..].iter().any(|detail| detail.is_none()),
            "a limpeza vem depois da área nova: {details:?}"
        );
    }

    /// O requisito "nunca quebrar o cliente real", provado no próprio teste: com
    /// `discord-ipc-0` ocupado por um servidor que não aceita conexão, o stub
    /// desiste, nada é enviado ao ocupante e ele continua atendendo.
    #[cfg(windows)]
    #[test]
    fn ipc_stub_skips_while_the_pipe_is_occupied() {
        use crate::presence::IPC_TEST_LOCK;

        let _ipc = IPC_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let occupant = crate::discord_stub::Occupied::pipe_zero()
            .expect("criar o pipe ocupado para o teste");
        println!(
            "OCUPANTE pid={} pipe=discord-ipc-0 criado e escutando (sem aceitar conexão)",
            std::process::id()
        );

        let code = match crate::discord_stub::Stub::start() {
            crate::discord_stub::StubStart::Occupied(code) => code,
            crate::discord_stub::StubStart::Ready(_) => {
                panic!("o stub criou um pipe mesmo com discord-ipc-0 ocupado")
            }
            crate::discord_stub::StubStart::Failed(code) => {
                panic!("erro inesperado ao criar o pipe: {code}")
            }
        };
        println!("OCUPANTE: stub desistiu com erro {code}; nenhuma conexão foi aberta");
        assert_eq!(
            occupant.pending_bytes(),
            0,
            "nada pode ter sido enviado ao pipe ocupado"
        );
        assert!(
            occupant.still_listening(),
            "o ocupante tem de continuar atendendo depois do SKIP"
        );
    }

    /// O cliente de produção tenta `discord-ipc-0`..`discord-ipc-9` em ordem
    /// (`ipc_windows.rs` do `discord-rich-presence`). Com os cinco primeiros
    /// livres, o serviço tem de achar e usar o índice do stub — aqui o 5 —, o
    /// que prova que o laço de índices funciona de verdade.
    #[cfg(windows)]
    #[test]
    fn presence_service_uses_the_next_free_pipe_index() {
        use crate::discord_stub::{Stub, StubStart};
        use crate::presence::{
            now_ms, trophy_progress_text, Presence, PresenceService, IPC_TEST_LOCK,
        };

        let _ipc = IPC_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for index in 0..5u32 {
            if crate::discord_stub::pipe_busy(index) {
                println!(
                    "SKIP: discord-ipc-{index} está em uso; o teste não mexe no cliente do usuário"
                );
                return;
            }
        }
        let stub = match Stub::start_at(5) {
            StubStart::Ready(stub) => stub,
            StubStart::Occupied(code) => {
                println!("SKIP: discord-ipc-5 ocupado (erro {code}); nada foi tocado");
                return;
            }
            StubStart::Failed(code) => panic!("o stub não subiu no índice 5 (erro {code})"),
        };
        let progress = trophy_progress_text(3, 35);
        let service = PresenceService::start();
        service.publish(Presence::new(
            zone_label("Oly100"),
            progress.clone(),
            Some(now_ms()),
        ));
        let frames = stub
            .wait_for(Duration::from_secs(20), |frames| {
                frames.iter().any(|(op, value)| {
                    *op == 1
                        && value["cmd"] == "SET_ACTIVITY"
                        && value["args"]["activity"]["details"] == "Olimpo"
                })
            })
            .expect("SET_ACTIVITY entregue pelo pipe do índice 5");
        assert!(stub.accepted(), "o serviço conectou no índice 5");
        for (op, value) in &frames {
            println!("STUB(índice 5) <- op={op} {value}");
        }
        let activity = frames
            .iter()
            .find(|(_, value)| {
                value["cmd"] == "SET_ACTIVITY"
                    && value["args"]["activity"]["details"] == "Olimpo"
            })
            .expect("atividade com Olimpo")
            .1
            .clone();
        assert_eq!(activity["args"]["activity"]["state"], progress.as_str());
        service.clear();
    }

    /// As QUATRO sondas reais contra um servidor IPC de verdade escutando o
    /// mesmo pipe do Discord (`discord-ipc-0`): handshake, frames SET_ACTIVITY
    /// crus com a área do SAVEDATA real, troca de área e limpeza final.
    #[cfg(windows)]
    #[test]
    fn real_probes_run_against_the_local_ipc_server() {
        use crate::discord_stub::{Stub, StubStart};
        use crate::presence::tests as probes;
        use crate::presence::{
            now_ms, trophy_progress_text, Presence, PresenceService, CLIENT_ID, IPC_TEST_LOCK,
        };

        let _ipc = IPC_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut stub = match Stub::start() {
            StubStart::Ready(stub) => stub,
            StubStart::Occupied(code) => {
                println!("SKIP: discord-ipc-0 ocupado (erro {code}); nada foi tocado");
                return;
            }
            StubStart::Failed(code) => panic!("o stub não subiu (erro {code})"),
        };

        println!("== local_ipc_probe");
        probes::local_ipc_probe();
        std::thread::sleep(Duration::from_millis(200));
        println!("== local_publish_probe");
        probes::local_publish_probe();
        std::thread::sleep(Duration::from_millis(200));
        println!("== local_service_probe");
        probes::local_service_probe();
        std::thread::sleep(Duration::from_millis(200));
        println!("== local_ipc_area_probe");
        local_ipc_area_probe();
        std::thread::sleep(Duration::from_millis(200));

        // SAVEDATA real: a área vai em `details`, os troféus em `state`.
        let real = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("data")
            .join("ambiente")
            .join("user")
            .join("home")
            .join("1000")
            .join("savedata")
            .join("CUSA01623");
        let (_, code) = SaveWatch::zone_save(&real).expect("SAVEDATA real legível");
        let expected = zone_label(&code);
        let progress = trophy_progress_text(0, 35);
        println!("== SAVEDATA real area={code} detalhe={expected} estado={progress}");
        // Carimbo único no `timestamps.start`: identifica exatamente este
        // SET_ACTIVITY entre os frames das sondas anteriores.
        let started_at = now_ms();
        let service = PresenceService::start();
        service.publish(Presence::new(
            expected.clone(),
            progress.clone(),
            Some(started_at),
        ));
        let real_frames = stub
            .wait_for(Duration::from_secs(20), |frames| {
                frames.iter().any(|(op, value)| {
                    *op == 1
                        && value["cmd"] == "SET_ACTIVITY"
                        && value["args"]["activity"]["details"] == expected.as_str()
                        && value["args"]["activity"]["state"] == progress.as_str()
                        && value["args"]["activity"]["timestamps"]["start"] == started_at
                })
            })
            .unwrap_or_else(|| panic!("SET_ACTIVITY com a área do SAVEDATA real ({expected})"));
        let published_at = real_frames
            .iter()
            .position(|(op, value)| {
                *op == 1
                    && value["cmd"] == "SET_ACTIVITY"
                    && value["args"]["activity"]["timestamps"]["start"] == started_at
            })
            .expect("posição do SET_ACTIVITY real");
        service.clear();
        stub.wait_for(Duration::from_secs(20), |frames| {
            frames.len() > published_at
                && frames[published_at + 1..].iter().any(|(op, value)| {
                    *op == 1
                        && value["cmd"] == "SET_ACTIVITY"
                        && value["args"]["activity"].is_null()
                })
        })
        .expect("frame de limpeza depois da publicação real");

        let frames = stub.frames();
        println!("== conexões aceitas: {}", stub.connections());
        for (op, value) in &frames {
            println!("SERVIDOR <- op={op} {value}");
        }

        let handshakes = frames
            .iter()
            .filter(|(op, value)| *op == 0 && value["client_id"] == CLIENT_ID)
            .count();
        assert!(
            handshakes >= 4,
            "cada sonda fez handshake: {handshakes} handshakes"
        );
        assert!(stub.accepted(), "o stub aceitou conexão");
        assert!(
            stub.connections() >= 5,
            "quatro sondas + a publicação do SAVEDATA real: {} conexões",
            stub.connections()
        );

        let activities: Vec<&serde_json::Value> = frames
            .iter()
            .filter(|(op, value)| *op == 1 && value["cmd"] == "SET_ACTIVITY")
            .map(|(_, value)| &value["args"]["activity"])
            .collect();
        let details: Vec<Option<&str>> = activities
            .iter()
            .map(|activity| activity["details"].as_str())
            .collect();
        assert!(
            details.iter().any(|detail| *detail == Some("Poseidon")),
            "área inicial do save: {details:?}"
        );
        assert!(
            details.iter().any(|detail| *detail == Some("Olimpo")),
            "área trocada no sandbox: {details:?}"
        );
        assert_eq!(
            details.last().copied().flatten(),
            None,
            "o último SET_ACTIVITY não deixa atividade publicada: {details:?}"
        );

        let real_frame = activities
            .iter()
            .rev()
            .find(|activity| activity["details"] == expected.as_str())
            .expect("atividade com a área do SAVEDATA real");
        assert_eq!(real_frame["state"], progress.as_str());
        assert_eq!(real_frame["name"], "God of War III Remastered");
        stub.shutdown();
    }

    /// Recomeçar uma sessão esquece o carimbo: o mesmo save volta a valer.
    #[test]
    fn reset_makes_the_next_read_count_again() {
        let sandbox = Sandbox::new("reset");
        let mut watch = SaveWatch::new();

        sandbox.write("AUTOSAVE", POS050);
        assert_eq!(watch.poll(sandbox.path()).as_deref(), Some("Pos050"));
        assert_eq!(watch.poll(sandbox.path()), None);

        watch.reset();
        assert_eq!(watch.poll(sandbox.path()).as_deref(), Some("Pos050"));
    }
}
