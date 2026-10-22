use std::io::{Read, Seek, SeekFrom};

use serde::{Deserialize, Serialize};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::AppHandle;

use crate::extract::{self, get_device_size, MagicByte};

// ---------------------------------------------------------------------------
// Tipos expostos ao frontend
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageDevice {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    pub size_label: String,
    pub kind: String,
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockScanResult {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub percent_used: f64,
    /// Fração (0.0–1.0) de blocos com dados em cada célula do mapa
    pub cells: Vec<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BlockCellProgress {
    pub cell: usize,
    pub fill: f64,
    pub percent: f64,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn format_size(bytes: u64) -> String {
    const GB: u64 = 1024 * 1024 * 1024;
    const MB: u64 = 1024 * 1024;
    const KB: u64 = 1024;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.0} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.0} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

// ---------------------------------------------------------------------------
// list_devices
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_devices() -> Result<Vec<StorageDevice>, String> {
    let mut devices: Vec<StorageDevice> = Vec::new();

    #[cfg(target_os = "windows")]
    {
        // wmic foi removido do Windows 11 recente; usa PowerShell + CIM
        // Physical drives
        for d in query_cim("Win32_DiskDrive", "Caption,DeviceID,Size")? {
            let model = d["Caption"].as_str().unwrap_or("").trim().to_string();
            let path = d["DeviceID"].as_str().unwrap_or("").trim().to_string();
            let size_bytes = d["Size"].as_u64().unwrap_or(0);
            if path.is_empty() || size_bytes == 0 {
                continue;
            }
            devices.push(StorageDevice {
                name: model.clone(),
                path,
                size_bytes,
                size_label: format_size(size_bytes),
                kind: "disk".into(),
                model,
            });
        }

        // Logical volumes
        for d in query_cim("Win32_LogicalDisk", "Caption,Size,DriveType")? {
            let caption = d["Caption"].as_str().unwrap_or("").trim().to_string();
            if caption.is_empty() {
                continue;
            }
            let drive_type = d["DriveType"].as_u64().unwrap_or(0);
            let size_bytes = d["Size"].as_u64().unwrap_or(0);
            let kind = match drive_type {
                2 => "removable",
                3 => "partition",
                5 => "cdrom",
                _ => "partition",
            };
            let raw_path = format!("\\\\.\\{}", caption.trim_end_matches('\\'));
            devices.push(StorageDevice {
                name: format!("Volume ({})", caption),
                path: raw_path,
                size_bytes,
                size_label: format_size(size_bytes),
                kind: kind.into(),
                model: caption,
            });
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let output = Command::new("lsblk")
            .args(["-J", "-b", "-o", "NAME,SIZE,TYPE,MODEL,PATH"])
            .output()
            .map_err(|e| e.to_string())?;

        let text = String::from_utf8_lossy(&output.stdout);
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(bds) = json["blockdevices"].as_array() {
                for bd in bds {
                    push_block_device(bd, &mut devices);
                    if let Some(children) = bd["children"].as_array() {
                        for child in children {
                            push_block_device(child, &mut devices);
                        }
                    }
                }
            }
        }
    }

    if devices.is_empty() {
        devices.push(StorageDevice {
            name: "Nenhum dispositivo detectado".into(),
            path: "".into(),
            size_bytes: 0,
            size_label: "0 B".into(),
            kind: "unknown".into(),
            model: "".into(),
        });
    }

    Ok(devices)
}

#[cfg(target_os = "windows")]
fn query_cim(class: &str, props: &str) -> Result<Vec<serde_json::Value>, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let script = format!(
        "Get-CimInstance -ClassName {} | Select-Object {} | ConvertTo-Json -Compress",
        class, props
    );
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("Falha ao executar PowerShell: {}", e))?;

    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    // ConvertTo-Json retorna um objeto (não array) quando há um único item
    match serde_json::from_str::<serde_json::Value>(text).map_err(|e| e.to_string())? {
        serde_json::Value::Array(items) => Ok(items),
        obj => Ok(vec![obj]),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn push_block_device(d: &serde_json::Value, devs: &mut Vec<StorageDevice>) {
    let name = d["name"].as_str().unwrap_or("").to_string();
    let path = d["path"]
        .as_str()
        .unwrap_or(&format!("/dev/{}", name))
        .to_string();
    let size_bytes: u64 = d["size"].as_u64().unwrap_or(0);
    let kind = d["type"].as_str().unwrap_or("disk").to_string();
    let model = d["model"].as_str().unwrap_or(&name).trim().to_string();
    devs.push(StorageDevice {
        name: if model.is_empty() { name.clone() } else { model.clone() },
        path,
        size_bytes,
        size_label: format_size(size_bytes),
        kind,
        model,
    });
}

// ---------------------------------------------------------------------------
// scan_blocks
// ---------------------------------------------------------------------------

/// Incrementado a cada nova análise ou cancelamento; uma análise em curso
/// aborta assim que percebe que não é mais a mais recente.
static SCAN_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Granularidade de "bloco": 4 KiB é múltiplo de setores de 512 B e 4Kn,
/// o que mantém offsets e tamanhos alinhados como o Windows exige em I/O bruto.
const BLOCK: u64 = 4096;
/// Tamanho de cada leitura amostrada.
const SAMPLE: u64 = 64 * 1024;
/// Amostras por célula quando a célula é grande demais para ler inteira.
const SAMPLES_PER_CELL: u64 = 16;

#[tauri::command]
pub async fn scan_blocks(
    app_handle: AppHandle,
    path: String,
    cells: usize,
) -> Result<BlockScanResult, String> {
    let generation = SCAN_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    tauri::async_runtime::spawn_blocking(move || {
        scan_blocks_blocking(&app_handle, &path, cells.clamp(1, 10_000), generation)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn cancel_block_scan() {
    SCAN_GENERATION.fetch_add(1, Ordering::SeqCst);
}

fn scan_blocks_blocking(
    app_handle: &AppHandle,
    path: &str,
    cell_count: usize,
    generation: u64,
) -> Result<BlockScanResult, String> {
    use tauri::Emitter;

    let total_size = get_device_size(path)? / BLOCK * BLOCK;
    if total_size == 0 {
        return Err("Não foi possível determinar o tamanho do dispositivo.".into());
    }

    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; SAMPLE as usize];

    let cell_size = (total_size / cell_count as u64 / BLOCK * BLOCK).max(BLOCK);
    let mut cells = Vec::with_capacity(cell_count);
    let mut used_bytes: f64 = 0.0;

    for cell in 0..cell_count {
        if SCAN_GENERATION.load(Ordering::SeqCst) != generation {
            return Err("Análise cancelada.".into());
        }

        let start = cell as u64 * cell_size;
        if start >= total_size {
            cells.push(0.0);
            continue;
        }
        let end = if cell + 1 == cell_count {
            total_size
        } else {
            (start + cell_size).min(total_size)
        };
        let len = end - start;

        // Célula pequena: lê tudo. Grande: amostras espaçadas uniformemente.
        let offsets: Vec<u64> = if len <= SAMPLE * SAMPLES_PER_CELL {
            (0..len.div_ceil(SAMPLE)).map(|i| start + i * SAMPLE).collect()
        } else {
            let stride = (len - SAMPLE) / (SAMPLES_PER_CELL - 1) / BLOCK * BLOCK;
            (0..SAMPLES_PER_CELL).map(|i| start + i * stride).collect()
        };

        let mut sampled: u64 = 0;
        let mut used: u64 = 0;
        for off in offsets {
            let n = SAMPLE.min(end - off) as usize;
            file.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
            file.read_exact(&mut buf[..n])
                .map_err(|e| format!("Falha ao ler offset {}: {}", off, e))?;
            for block in buf[..n].chunks(BLOCK as usize) {
                sampled += 1;
                if block.iter().any(|&b| b != 0) {
                    used += 1;
                }
            }
        }

        let fill = if sampled > 0 { used as f64 / sampled as f64 } else { 0.0 };
        used_bytes += fill * len as f64;
        cells.push(fill);

        let _ = app_handle.emit(
            "block-progress",
            BlockCellProgress {
                cell,
                fill,
                percent: (cell + 1) as f64 / cell_count as f64 * 100.0,
            },
        );
    }

    Ok(BlockScanResult {
        total_bytes: total_size,
        used_bytes: used_bytes as u64,
        percent_used: used_bytes / total_size as f64 * 100.0,
        cells,
    })
}

// ---------------------------------------------------------------------------
// open_found_file
// ---------------------------------------------------------------------------

/// Abre um arquivo recuperado no aplicativo padrão do sistema. Só aceita
/// arquivos dentro das pastas de saída liberadas durante a busca.
#[tauri::command]
pub fn open_found_file(app_handle: AppHandle, path: String) -> Result<(), String> {
    use tauri::Manager;
    use tauri_plugin_opener::OpenerExt;

    if !app_handle.asset_protocol_scope().is_allowed(&path) {
        return Err("Arquivo fora da pasta de saída.".into());
    }
    app_handle
        .opener()
        .open_path(path, None::<&str>)
        .map_err(|e| e.to_string())
}

/// Abre a pasta de saída no gerenciador de arquivos com o item selecionado.
#[tauri::command]
pub fn reveal_found_file(app_handle: AppHandle, path: String) -> Result<(), String> {
    use tauri::Manager;
    use tauri_plugin_opener::OpenerExt;

    if !app_handle.asset_protocol_scope().is_allowed(&path) {
        return Err("Arquivo fora da pasta de saída.".into());
    }
    app_handle
        .opener()
        .reveal_item_in_dir(path)
        .map_err(|e| e.to_string())
}

/// Conteúdo de um texto recuperado (limitado, para exibição).
#[tauri::command]
pub fn read_found_text(app_handle: AppHandle, path: String) -> Result<String, String> {
    use tauri::Manager;
    const MAX: usize = 1024 * 1024;

    if !app_handle.asset_protocol_scope().is_allowed(&path) {
        return Err("Arquivo fora da pasta de saída.".into());
    }
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    let mut text = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX)]).into_owned();
    if bytes.len() > MAX {
        text.push_str("\n\n[… truncado para exibição …]");
    }
    Ok(text)
}

#[derive(Debug, Clone, Serialize)]
pub struct ZipEntry {
    pub name: String,
    pub size: u64,
    pub compressed: u64,
    pub is_dir: bool,
}

/// Entradas de um ZIP recuperado (limitado a 500).
#[tauri::command]
pub fn zip_entries(app_handle: AppHandle, path: String) -> Result<Vec<ZipEntry>, String> {
    use tauri::Manager;

    if !app_handle.asset_protocol_scope().is_allowed(&path) {
        return Err("Arquivo fora da pasta de saída.".into());
    }
    let file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    let mut entries = Vec::new();
    for i in 0..archive.len().min(500) {
        let e = archive.by_index_raw(i).map_err(|e| e.to_string())?;
        entries.push(ZipEntry {
            name: e.name().to_string(),
            size: e.size(),
            compressed: e.compressed_size(),
            is_dir: e.is_dir(),
        });
    }
    Ok(entries)
}

// ---------------------------------------------------------------------------
// Comandos de busca
// ---------------------------------------------------------------------------

/// Incrementado a cada busca nova ou cancelamento (mesmo padrão da análise de blocos).
static SEARCH_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Busca todos os `types` ("jpeg", "png", ...) numa única leitura do dispositivo.
#[tauri::command]
pub async fn search_files(
    app_handle: AppHandle,
    path: String,
    output_dir: String,
    types: Vec<String>,
    max: u64,
) -> Result<(), String> {
    let magics = types
        .iter()
        .map(|t| MagicByte::by_id(t).ok_or_else(|| format!("Tipo desconhecido: {}", t)))
        .collect::<Result<Vec<_>, _>>()?;
    run_search(app_handle, path, output_dir, magics, max).await
}

#[tauri::command]
pub fn cancel_search() {
    SEARCH_GENERATION.fetch_add(1, Ordering::SeqCst);
}

#[tauri::command]
pub async fn find_custom(
    app_handle: AppHandle,
    magic: MagicByte,
    path: String,
    output_dir: String,
    max: u64,
) -> Result<(), String> {
    run_search(app_handle, path, output_dir, vec![magic], max).await
}

async fn run_search(
    app_handle: AppHandle,
    path: String,
    output_dir: String,
    magics: Vec<MagicByte>,
    max: u64,
) -> Result<(), String> {
    use tauri::{Emitter, Manager};

    let generation = SEARCH_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    tauri::async_runtime::spawn_blocking(move || {
        ensure_output_elsewhere(&path, &output_dir)?;
        std::fs::create_dir_all(&output_dir).map_err(|e| e.to_string())?;
        // Permite que o frontend exiba os arquivos recuperados via asset protocol
        let _ = app_handle
            .asset_protocol_scope()
            .allow_directory(&output_dir, true);

        let cancel = || SEARCH_GENERATION.load(Ordering::SeqCst) != generation;
        extract::scan(
            &path,
            &output_dir,
            &magics,
            max,
            extract::DEFAULT_CHUNK,
            &cancel,
            &mut |p| {
                let _ = app_handle.emit("file-progress", p);
            },
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Gravar no dispositivo que está sendo recuperado pode sobrescrever justamente
/// os dados apagados que se quer recuperar; recusa essa combinação.
fn ensure_output_elsewhere(device: &str, output_dir: &str) -> Result<(), String> {
    const SAME_DEVICE: &str =
        "A pasta de saída está no mesmo disco que está sendo analisado. Gravar nele pode \
         sobrescrever os arquivos apagados que você quer recuperar. Escolha uma pasta em outro disco.";

    #[cfg(target_os = "windows")]
    {
        let out = std::path::absolute(output_dir).map_err(|e| e.to_string())?;
        let out = out.to_string_lossy();
        let bytes = out.as_bytes();
        // Caminhos de rede (UNC) não estão em disco local
        if bytes.len() < 2 || bytes[1] != b':' {
            return Ok(());
        }
        let out_letter = bytes[0].to_ascii_uppercase();

        let dev = device.trim_start_matches(r"\\.\").to_ascii_uppercase();
        if let Some(n) = dev.strip_prefix("PHYSICALDRIVE") {
            let disk: u32 = n.parse().map_err(|_| format!("Dispositivo inválido: {}", device))?;
            // Volume sem disco identificável (ex.: volume estendido): não há como afirmar conflito
            if let Ok(out_disk) = extract::windows_disk_number(&format!(r"\\.\{}:", out_letter as char)) {
                if out_disk == disk {
                    return Err(SAME_DEVICE.into());
                }
            }
        } else if dev.as_bytes().first() == Some(&out_letter) && dev.as_bytes().get(1) == Some(&b':') {
            return Err(SAME_DEVICE.into());
        }
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let dev_rdev = std::fs::metadata(device).map_err(|e| e.to_string())?.rdev();
        // A pasta pode ainda não existir: usa o ancestral existente mais próximo
        let mut dir = std::path::Path::new(output_dir);
        let meta = loop {
            match std::fs::metadata(dir) {
                Ok(m) => break m,
                Err(_) => match dir.parent() {
                    Some(p) => dir = p,
                    None => return Ok(()),
                },
            }
        };
        if meta.dev() == dev_rdev {
            return Err(SAME_DEVICE.into());
        }
    }

    Ok(())
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    #[test]
    fn same_volume_is_rejected() {
        assert!(ensure_output_elsewhere(r"\\.\C:", r"C:\Rscover_Output").is_err());
        assert!(ensure_output_elsewhere(r"\\.\c:", r"c:\x").is_err());
        assert!(ensure_output_elsewhere(r"\\.\E:", r"C:\Rscover_Output").is_ok());
        assert!(ensure_output_elsewhere(r"\\.\E:", r"\\server\share\out").is_ok());
    }

    /// Consulta o disco físico real de C: (não exige administrador).
    #[test]
    fn same_physical_disk_is_rejected() {
        let disk = extract::windows_disk_number(r"\\.\C:").expect("disco de C:");
        let own = format!(r"\\.\PHYSICALDRIVE{}", disk);
        let other = format!(r"\\.\PHYSICALDRIVE{}", disk + 1);
        assert!(ensure_output_elsewhere(&own, r"C:\Rscover_Output").is_err());
        assert!(ensure_output_elsewhere(&other, r"C:\Rscover_Output").is_ok());
    }
}

