//! Stub do IPC do Discord para testes: sobe um named pipe local com o mesmo
//! nome que o cliente usa (`discord-ipc-0`), implementa o handshake de verdade
//! (`OP 0` com `{v:1, client_id}` → `DISPATCH READY`) e registra todo frame
//! recebido, para provar o transporte real do `PresenceService` sem depender de
//! um Discord/Dorion aberto.
//!
//! Segurança: a ocupação é consultada com `WaitNamedPipeW(nome, 0)`, que só
//! verifica — não abre conexão. Se alguém já atende o nome, o stub devolve
//! [`StubStart::Occupied`] sem criar instância e sem tocar no ocupante; o teste
//! imprime SKIP e segue. O `Drop` libera o pipe.

#[cfg(windows)]
#[allow(unused_imports)]
pub use imp::{pipe_busy, Occupied, Stub, StubStart};

#[cfg(windows)]
mod imp {
    use std::ffi::OsStr;
    use std::fs::OpenOptions;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use serde_json::{json, Value};
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY,
        ERROR_PIPE_CONNECTED, ERROR_SEM_TIMEOUT, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        ReadFile, WriteFile, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PeekNamedPipe, WaitNamedPipeW,
        PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
    };

    /// Índice que o cliente do launcher tenta primeiro.
    const PIPE_INDEX: u32 = 0;

    fn pipe_path(index: u32) -> String {
        format!(r"\\.\pipe\discord-ipc-{index}")
    }

    fn wide(text: &str) -> Vec<u16> {
        OsStr::new(text)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// Consulta se alguém já atende esse índice, **sem abrir conexão**.
    /// `None` = livre; `Some(código)` = ocupado (ou sem instância livre).
    fn pipe_taken(index: u32) -> Option<u32> {
        let name = wide(&pipe_path(index));
        if unsafe { WaitNamedPipeW(name.as_ptr(), 0) } != 0 {
            return Some(ERROR_PIPE_BUSY);
        }
        match unsafe { GetLastError() } {
            ERROR_FILE_NOT_FOUND => None,
            ERROR_SEM_TIMEOUT => Some(ERROR_SEM_TIMEOUT),
            other => Some(other),
        }
    }

    /// `true` quando alguém já atende esse índice (cliente real, por exemplo).
    pub fn pipe_busy(index: u32) -> bool {
        pipe_taken(index).is_some()
    }

    fn create_instance(index: u32) -> Result<HANDLE, u32> {
        let name = wide(&pipe_path(index));
        let handle = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                1,
                4096,
                4096,
                0,
                std::ptr::null(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            Err(unsafe { GetLastError() })
        } else {
            Ok(handle)
        }
    }

    pub enum StubStart {
        Ready(Stub),
        /// Já existe alguém atendendo o nome: nada foi criado nem conectado.
        Occupied(u32),
        Failed(u32),
    }

    /// Servidor de mentira que aceita uma conexão por vez, como o pipe real.
    pub struct Stub {
        frames: Arc<Mutex<Vec<(u32, Value)>>>,
        connections: Arc<AtomicU32>,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
        index: u32,
    }

    impl Stub {
        pub fn start() -> StubStart {
            Self::start_at(PIPE_INDEX)
        }

        /// Como [`Stub::start`], no índice pedido (o cliente tenta 0..9 em ordem).
        pub fn start_at(index: u32) -> StubStart {
            if let Some(code) = pipe_taken(index) {
                println!(
                    "SKIP stub IPC: discord-ipc-{index} já está em uso (erro {code}); \
                     nenhum pipe foi criado, nenhuma conexão foi aberta e o ocupante \
                     não foi tocado"
                );
                return StubStart::Occupied(code);
            }
            let handle = match create_instance(index) {
                Ok(handle) => handle,
                Err(code) if code == ERROR_ACCESS_DENIED || code == ERROR_PIPE_BUSY => {
                    println!(
                        "SKIP stub IPC: discord-ipc-{index} já está em uso (erro {code}); \
                         nenhum pipe foi criado, nenhuma conexão foi aberta e o ocupante \
                         não foi tocado"
                    );
                    return StubStart::Occupied(code);
                }
                Err(code) => return StubStart::Failed(code),
            };
            let frames = Arc::new(Mutex::new(Vec::new()));
            let connections = Arc::new(AtomicU32::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            let thread = {
                let frames = Arc::clone(&frames);
                let connections = Arc::clone(&connections);
                let stop = Arc::clone(&stop);
                // `HANDLE` é um ponteiro opaco: atravessa a thread como `usize`.
                let raw = handle as usize;
                thread::Builder::new()
                    .name("discord-ipc-stub".into())
                    .spawn(move || serve(raw as HANDLE, frames, connections, stop))
                    .ok()
            };
            if thread.is_none() {
                println!("SKIP stub IPC: a thread do stub não subiu");
                unsafe {
                    CloseHandle(handle);
                }
                return StubStart::Failed(0);
            }
            StubStart::Ready(Self {
                frames,
                connections,
                stop,
                thread,
                index,
            })
        }

        /// Cópia das mensagens recebidas: `(opcode, json)` na ordem de chegada.
        pub fn frames(&self) -> Vec<(u32, Value)> {
            self.frames
                .lock()
                .map(|all| all.clone())
                .unwrap_or_default()
        }

        /// Espera até `check` aceitar as mensagens recebidas (ou estourar o tempo).
        pub fn wait_for(
            &self,
            timeout: Duration,
            mut check: impl FnMut(&[(u32, Value)]) -> bool,
        ) -> Option<Vec<(u32, Value)>> {
            let deadline = std::time::Instant::now() + timeout;
            loop {
                let frames = self.frames();
                if check(&frames) {
                    return Some(frames);
                }
                if std::time::Instant::now() > deadline {
                    return None;
                }
                thread::sleep(Duration::from_millis(20));
            }
        }

        /// Houve pelo menos uma conexão aceita (o handshake chegou aqui).
        pub fn accepted(&self) -> bool {
            self.connections() > 0
        }

        /// Quantas conexões o stub aceitou até agora.
        pub fn connections(&self) -> u32 {
            self.connections.load(Ordering::SeqCst)
        }

        /// Fecha o pipe e encerra a thread (para o próximo teste poder usar o nome).
        pub fn shutdown(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            // Acorda um `ConnectNamedPipe` pendente com uma conexão efêmera.
            let _ = OpenOptions::new()
                .access_mode(0x3)
                .open(pipe_path(self.index));
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    impl Drop for Stub {
        fn drop(&mut self) {
            self.shutdown();
        }
    }

    /// Ocupa `discord-ipc-0` como um cliente de verdade já aberto: cria a
    /// instância e fica com o handle, **sem** aceitar conexão.
    pub struct Occupied {
        handle: HANDLE,
    }

    impl Occupied {
        pub fn pipe_zero() -> Option<Self> {
            create_instance(PIPE_INDEX)
                .ok()
                .map(|handle| Self { handle })
        }

        /// Bytes no pipe sem serem lidos (0 = ninguém conectou nem enviou nada).
        pub fn pending_bytes(&self) -> u32 {
            let mut available: u32 = 0;
            let ok = unsafe {
                PeekNamedPipe(
                    self.handle,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut available,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                0
            } else {
                available
            }
        }

        /// O pipe continua atendendo (o ocupante não foi derrubado).
        pub fn still_listening(&self) -> bool {
            OpenOptions::new()
                .access_mode(0x3)
                .open(pipe_path(PIPE_INDEX))
                .is_ok()
        }
    }

    impl Drop for Occupied {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }

    fn read_exact(handle: HANDLE, buffer: &mut [u8]) -> bool {
        let mut done = 0;
        while done < buffer.len() {
            let mut read: u32 = 0;
            let ok = unsafe {
                ReadFile(
                    handle,
                    buffer[done..].as_mut_ptr(),
                    (buffer.len() - done) as u32,
                    &mut read,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 || read == 0 {
                return false;
            }
            done += read as usize;
        }
        true
    }

    fn write_all(handle: HANDLE, buffer: &[u8]) -> bool {
        let mut done = 0;
        while done < buffer.len() {
            let mut written: u32 = 0;
            let ok = unsafe {
                WriteFile(
                    handle,
                    buffer[done..].as_ptr(),
                    (buffer.len() - done) as u32,
                    &mut written,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 || written == 0 {
                return false;
            }
            done += written as usize;
        }
        true
    }

    fn send(handle: HANDLE, opcode: u32, message: &Value) -> bool {
        let body = message.to_string();
        let mut frame = Vec::with_capacity(8 + body.len());
        frame.extend_from_slice(&opcode.to_le_bytes());
        frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
        frame.extend_from_slice(body.as_bytes());
        write_all(handle, &frame)
    }

    fn serve(
        handle: HANDLE,
        frames: Arc<Mutex<Vec<(u32, Value)>>>,
        connections: Arc<AtomicU32>,
        stop: Arc<AtomicBool>,
    ) {
        while !stop.load(Ordering::SeqCst) {
            let connected = unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) } != 0
                || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
            if !connected || stop.load(Ordering::SeqCst) {
                break;
            }
            connections.fetch_add(1, Ordering::SeqCst);
            loop {
                let mut header = [0u8; 8];
                if !read_exact(handle, &mut header) {
                    break;
                }
                let opcode = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
                let length =
                    u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
                if length > 1 << 20 {
                    break;
                }
                let mut body = vec![0u8; length];
                if !read_exact(handle, &mut body) {
                    break;
                }
                let message: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                if let Ok(mut all) = frames.lock() {
                    all.push((opcode, message.clone()));
                }
                if opcode == 0 {
                    // Handshake: sem o DISPATCH READY o cliente aborta.
                    let reply = json!({
                        "cmd": "DISPATCH",
                        "evt": "READY",
                        "data": {
                            "v": 1,
                            "config": {
                                "cdn_host": "cdn.discordapp.com",
                                "api_endpoint": "//discord.com/api",
                                "environment": "production",
                            },
                            "user": {
                                "id": "0",
                                "username": "stub",
                                "discriminator": "0001",
                                "avatar": null,
                            },
                        },
                    });
                    if !send(handle, 1, &reply) {
                        break;
                    }
                }
            }
            unsafe {
                DisconnectNamedPipe(handle);
            }
            if stop.load(Ordering::SeqCst) {
                break;
            }
        }
        unsafe {
            CloseHandle(handle);
        }
    }
}
