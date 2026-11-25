//! Prova do caminho de lançamento com a presença ativa e SEM cliente Discord.
//!
//! Usa a mesma `session::spawn_emulator` que o launcher usa em produção, com um
//! alvo inofensivo no lugar do shadPS4, e confirma que: o processo realmente
//! inicia, nada trava nem entra em pânico, o worker de presença fica ocioso
//! (sem laço de reconexão) e o serviço encerra limpo.
//!
//! Fica num módulo do binário (e não em `session.rs`) para o crate de
//! `tests/core.rs` continuar sem depender do módulo de presença.

#[cfg(all(test, windows))]
mod tests {
    use std::time::{Duration, Instant};

    use crate::game::PrivateEnvironment;
    use crate::presence::{now_ms, tests::process_cpu_ms, Presence, PresenceService};
    use crate::session::{spawn_emulator, LaunchOptions};

    /// Compila `examples/trivial_target.rs` num executável dentro da sandbox.
    fn compile_trivial_target(dir: &std::path::Path) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).expect("pasta do alvo trivial");
        let target = dir.join("trivial_target.exe");
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("examples")
            .join("trivial_target.rs");
        let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
        let output = std::process::Command::new(rustc)
            .arg(&source)
            .arg("-o")
            .arg(&target)
            .arg("--edition=2021")
            .output()
            .expect("rustc disponível para compilar o alvo inofensivo");
        assert!(
            output.status.success(),
            "não foi possível compilar {}: {}",
            source.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        target
    }

    #[test]
    fn spawn_starts_the_target_with_presence_active_and_no_discord() {
        let temp = std::env::temp_dir().join(format!(
            "gow3-spawn-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).expect("sandbox de lançamento");
        let environment = PrivateEnvironment::new(&temp);
        let base = temp.join("jogo");
        std::fs::create_dir_all(&base).expect("pasta do jogo");
        std::fs::write(base.join("eboot.bin"), b"eboot").expect("eboot de teste");
        let patch = temp.join("patch.xml");
        std::fs::write(&patch, b"<patch/>").expect("patch de teste");
        let session = temp.join("sessao");
        std::fs::create_dir_all(&session).expect("pasta da sessão");

        // Presença ativa, Discord fechado: publicar não pode bloquear.
        let service = PresenceService::start();
        service.publish(Presence::new(
            "Poseidon",
            "Troféus 0 de 35 · 0%",
            Some(now_ms()),
        ));

        // Alvo inofensivo de verdade: `examples/trivial_target.rs` existe, ignora
        // os argumentos do launcher e termina com código 0. É compilado aqui com
        // o `rustc` do ambiente do cargo, dentro da sandbox do teste.
        let target = compile_trivial_target(&environment.root.join("alvo"));
        let target = target.as_path();
        assert!(
            target.is_file(),
            "o alvo inofensivo precisa existir: {}",
            target.display()
        );
        let started = Instant::now();
        let mut child = spawn_emulator(
            target,
            &patch,
            &base,
            &environment,
            &session,
            LaunchOptions {
                capture: false,
                no_cache: false,
                wait_for_shaders: false,
                gpu_mode: 0,
                lazy_cache: false,
            },
            true,
        )
        .expect("o caminho de lançamento iniciou o alvo");
        let spawn_ms = started.elapsed();
        let pid = child.id();
        println!("PROBE spawn pid={pid} em {spawn_ms:?} (presença ativa, sem Discord)");
        assert!(pid > 0, "o processo do alvo realmente iniciou");
        let status = child.wait().expect("esperar o alvo terminar");
        println!("PROBE alvo terminou: {status:?} codigo={:?}", status.code());
        assert_eq!(
            status.code(),
            Some(0),
            "o alvo do caminho de lançamento tem de sair com 0: {status:?}"
        );
        let startup_errors = std::fs::read_to_string(session.join("startup_errors.log"))
            .unwrap_or_default();
        println!("PROBE startup_errors.log={startup_errors:?}");

        let cpu_before = process_cpu_ms();
        let worker_before = service.worker_cpu_ms();
        std::thread::sleep(Duration::from_secs(3));
        let cpu_used = service.worker_cpu_ms().saturating_sub(worker_before);
        let process_used = process_cpu_ms().saturating_sub(cpu_before);
        println!(
            "PROBE cpu_com_presenca_sem_discord={cpu_used} ms da thread do worker em 3 s (processo={process_used} ms)"
        );
        assert!(
            cpu_used < 1_500,
            "sem cliente, o worker de presença não pode girar em laço: {cpu_used} ms em 3 s"
        );

        let cleared = Instant::now();
        service.clear();
        drop(service);
        println!("PROBE presence_shutdown={:?}", cleared.elapsed());
        assert!(
            cleared.elapsed() < Duration::from_secs(1),
            "encerrar a presença sem Discord não pode travar"
        );
        let _ = std::fs::remove_dir_all(&temp);
    }
}
