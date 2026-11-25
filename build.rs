use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("fluent-dark".into());
    slint_build::compile_with_config("ui/app.slint", config)
        .expect("falha ao compilar a interface Slint");
    println!("cargo:rerun-if-changed=ui/app.slint");
    println!("cargo:rerun-if-changed=ui/components.slint");
    println!("cargo:rerun-if-changed=ui/theme.slint");
    println!("cargo:rerun-if-changed=ui/combobox.slint");
    println!("cargo:rerun-if-changed=ui/combobox-base.slint");
    println!("cargo:rerun-if-changed=ui/icons");
    println!("cargo:rerun-if-changed=assets");
    println!("cargo:rerun-if-changed=res/app.rc");
    println!("cargo:rerun-if-changed=res/app.ico");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_icon();
    }
}

/// Embute `res/app.ico` no executável para que o Explorer, a barra de tarefas e
/// as janelas do sistema mostrem o ícone do launcher em vez do genérico do
/// Windows. O recurso é opcional: sem o compilador de recursos (rc.exe) do
/// Windows SDK a compilação continua, apenas com um aviso.
fn embed_icon() {
    let res_file = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR ausente"))
        .join("gow3-launcher.res");

    let Some(rc) = find_resource_compiler() else {
        println!(
            "cargo:warning=rc.exe do Windows SDK não encontrado; o executável ficará sem ícone próprio"
        );
        return;
    };

    let status = Command::new(&rc)
        .arg("/nologo")
        .arg(format!("/fo{}", res_file.display()))
        .arg("res/app.rc")
        .status();

    match status {
        Ok(status) if status.success() => {
            println!("cargo:rustc-link-arg={}", res_file.display());
        }
        _ => println!(
            "cargo:warning=rc.exe não compilou res/app.rc; o executável ficará sem ícone próprio"
        ),
    }
}

fn find_resource_compiler() -> Option<PathBuf> {
    if let Some(path) = env::var_os("RC").map(PathBuf::from) {
        if path.is_file() {
            return Some(path);
        }
    }

    if let Some(path) = find_in_path("rc.exe") {
        return Some(path);
    }

    let kits = env::var_os("ProgramFiles(x86)")
        .or_else(|| env::var_os("ProgramFiles"))
        .map(PathBuf::from)?
        .join("Windows Kits")
        .join("10")
        .join("bin");

    let mut versions: Vec<PathBuf> = std::fs::read_dir(&kits)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.join("x64").join("rc.exe").is_file()
                || path.join("x86").join("rc.exe").is_file()
                || path.join("arm64").join("rc.exe").is_file()
        })
        .collect();

    versions.sort_by_key(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .map(version_key)
            .unwrap_or_default()
    });

    let newest = versions.pop()?;
    for arch in ["x64", "arm64", "x86"] {
        let candidate = newest.join(arch).join("rc.exe");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn version_key(name: &str) -> Vec<u32> {
    name.split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

fn find_in_path(executable: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH")?)
        .map(|dir| dir.join(executable))
        .find(|candidate| candidate.is_file())
}
