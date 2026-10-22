use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};

use md5;
use memchr::memmem;
use serde::{Deserialize, Serialize};

use crate::carve::{self, DevReader};
use crate::text::{FoundText, TextScanner};

// ---------------------------------------------------------------------------
// Progress – payload emitido ao frontend
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Progress {
    pub bytes_read: u64,
    pub total_size: u64,
    pub files_found: u64,
    pub percent: f64,
    pub status: String,
    /// Preenchido apenas no evento em que um arquivo novo foi salvo
    #[serde(skip_serializing_if = "Option::is_none")]
    pub found_file: Option<FoundFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FoundFile {
    pub path: String,
    pub extension: String,
    pub type_name: String,
    pub size: u64,
    /// Posição do arquivo no dispositivo
    pub offset: u64,
    /// Trecho inicial, só para texto
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

/// Resultado de uma tentativa de salvar um candidato
enum Saved {
    File(FoundFile),
    /// Mesmo conteúdo (MD5) já salvo
    Duplicate,
    /// Estrutura ok, mas a decodificação falhou: não abriria num visualizador
    Corrupt,
}

// ---------------------------------------------------------------------------
// MagicByte
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MagicByte {
    pub extension: String,
    pub header: Vec<u8>,
    pub footer: Option<Vec<u8>>,
    pub max_size: u64,
    pub name: String,
    pub is_image: bool,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub fn get_device_size(path: &str) -> Result<u64, String> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;

    // Volumes/discos brutos no Windows (\\.\E:, \\.\PHYSICALDRIVE0) não suportam
    // seek até o fim (os error 87); o tamanho vem de um IOCTL.
    #[cfg(target_os = "windows")]
    if let Ok(size) = windows_device_length(&f) {
        return Ok(size);
    }

    let size = f.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    Ok(size)
}

#[cfg(target_os = "windows")]
fn windows_device_length(file: &File) -> std::io::Result<u64> {
    // CTL_CODE(IOCTL_DISK_BASE, 0x17, METHOD_BUFFERED, FILE_READ_ACCESS)
    const IOCTL_DISK_GET_LENGTH_INFO: u32 = 0x0007_405C;
    let length: i64 = device_ioctl(file, IOCTL_DISK_GET_LENGTH_INFO)?;
    Ok(length as u64)
}

/// Número do disco físico onde está o volume `\\.\X:` (PhysicalDriveN).
#[cfg(target_os = "windows")]
pub fn windows_disk_number(volume: &str) -> std::io::Result<u32> {
    use std::os::windows::fs::OpenOptionsExt;

    // CTL_CODE(IOCTL_STORAGE_BASE, 0x420, METHOD_BUFFERED, FILE_ANY_ACCESS)
    const IOCTL_STORAGE_GET_DEVICE_NUMBER: u32 = 0x002D_1080;

    #[repr(C)]
    #[derive(Default)]
    struct StorageDeviceNumber {
        device_type: u32,
        device_number: u32,
        partition_number: u32,
    }

    // Acesso 0: só consulta, não exige permissão de leitura do volume
    let file = std::fs::OpenOptions::new()
        .access_mode(0)
        .share_mode(0x1 | 0x2) // FILE_SHARE_READ | FILE_SHARE_WRITE
        .open(volume)?;
    let info: StorageDeviceNumber = device_ioctl(&file, IOCTL_STORAGE_GET_DEVICE_NUMBER)?;
    Ok(info.device_number)
}

/// DeviceIoControl sem buffer de entrada, com a saída em `T`.
#[cfg(target_os = "windows")]
fn device_ioctl<T: Default>(file: &File, code: u32) -> std::io::Result<T> {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;

    #[link(name = "kernel32")]
    extern "system" {
        fn DeviceIoControl(
            device: *mut c_void,
            code: u32,
            in_buf: *const c_void,
            in_size: u32,
            out_buf: *mut c_void,
            out_size: u32,
            returned: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
    }

    let mut out = T::default();
    let mut returned: u32 = 0;
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle() as *mut c_void,
            code,
            std::ptr::null(),
            0,
            &mut out as *mut T as *mut c_void,
            std::mem::size_of::<T>() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Construtores
// ---------------------------------------------------------------------------

impl MagicByte {
    pub fn pdf() -> Self {
        Self {
            extension: "pdf".into(),
            header: vec![0x25, 0x50, 0x44, 0x46, 0x2D], // %PDF-
            footer: Some(vec![0x25, 0x25, 0x45, 0x4F, 0x46]), // %%EOF
            max_size: 500 * 1024 * 1024,
            name: "PDF".into(),
            is_image: false,
        }
    }

    pub fn jpeg() -> Self {
        Self {
            extension: "jpg".into(),
            header: vec![0xFF, 0xD8, 0xFF],
            footer: Some(vec![0xFF, 0xD9]),
            max_size: 50 * 1024 * 1024,
            name: "JPEG".into(),
            is_image: true,
        }
    }

    pub fn png() -> Self {
        Self {
            extension: "png".into(),
            header: vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A],
            footer: Some(vec![0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82]),
            max_size: 50 * 1024 * 1024,
            name: "PNG".into(),
            is_image: true,
        }
    }

    pub fn zip() -> Self {
        Self {
            extension: "zip".into(),
            header: vec![0x50, 0x4B, 0x03, 0x04], // PK..
            footer: Some(vec![0x50, 0x4B, 0x05, 0x06]), // PK end of central dir
            max_size: 2 * 1024 * 1024 * 1024,
            name: "ZIP".into(),
            is_image: false,
        }
    }

    pub fn mp4() -> Self {
        Self {
            extension: "mp4".into(),
            // ftyp box (offset 4): 66 74 79 70
            header: vec![0x66, 0x74, 0x79, 0x70],
            footer: None,
            max_size: 4 * 1024 * 1024 * 1024,
            name: "MP4".into(),
            is_image: false,
        }
    }

    pub fn text() -> Self {
        Self {
            extension: "txt".into(),
            // Texto não tem assinatura: é achado por varredura de bytes
            // imprimíveis + heurísticas (ver text.rs). Header não é usado.
            header: vec![0xEF, 0xBB, 0xBF],
            footer: None,
            max_size: 10 * 1024 * 1024,
            name: "Text".into(),
            is_image: false,
        }
    }

    // -----------------------------------------------------------------------
    // Extração (file carving)
    // -----------------------------------------------------------------------

    /// Tipo pelo identificador usado no frontend.
    pub fn by_id(id: &str) -> Option<Self> {
        Some(match id {
            "jpeg" => Self::jpeg(),
            "png" => Self::png(),
            "pdf" => Self::pdf(),
            "zip" => Self::zip(),
            "mp4" => Self::mp4(),
            "text" => Self::text(),
            _ => return None,
        })
    }

    fn is_text(&self) -> bool {
        self.extension == "txt"
    }

    /// Onde o arquivo começa, dado o offset da assinatura.
    fn file_start(&self, sig_abs: u64) -> Option<u64> {
        // "ftyp" fica no offset 4 da primeira caixa do MP4
        if self.extension == "mp4" {
            sig_abs.checked_sub(4)
        } else {
            Some(sig_abs)
        }
    }

    /// Tamanho do arquivo que começa em `start`, ou None se a estrutura
    /// estiver quebrada / truncada.
    fn carve(&self, r: &mut DevReader, start: u64) -> Result<Option<u64>, String> {
        let max = self.max_size;
        match self.extension.as_str() {
            "jpg" => carve::jpeg(r, start, max),
            "png" => carve::png(r, start, max),
            "zip" => carve::zip(r, start, max),
            "mp4" => carve::mp4(r, start, max),
            "pdf" => carve::pdf(r, start, max),
            _ => match &self.footer {
                Some(footer) => carve::by_footer(r, start, self.header.len() as u64, footer, max),
                None => Ok(None),
            },
        }
    }

    /// Copia [start, start+len) para a pasta de saída, decodifica para
    /// confirmar que abre, e descarta duplicados (mesmo MD5).
    fn save_range(
        &self,
        r: &mut DevReader,
        start: u64,
        len: u64,
        output_dir: &str,
        index: u64,
        hashes: &mut HashSet<String>,
    ) -> Result<Saved, String> {
        let tmp: PathBuf = [output_dir, &format!("{:06}.{}.part", index, self.extension)].iter().collect();
        let mut out = File::create(&tmp).map_err(|e| e.to_string())?;
        let mut ctx = md5::Context::new();
        let mut buf = vec![0u8; (4 * 1024 * 1024).min(len as usize)];
        let mut off = 0;
        while off < len {
            let n = (buf.len() as u64).min(len - off) as usize;
            if !r.read(start + off, &mut buf[..n])? {
                drop(out);
                let _ = std::fs::remove_file(&tmp);
                return Ok(Saved::Corrupt);
            }
            ctx.consume(&buf[..n]);
            out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            off += n as u64;
        }
        drop(out);

        let hash = format!("{:x}", ctx.finalize());
        if hashes.contains(&hash) {
            let _ = std::fs::remove_file(&tmp);
            return Ok(Saved::Duplicate);
        }
        // "Tenta visualizar": se não decodifica, está corrompido
        if !self.decodes(&tmp) {
            let _ = std::fs::remove_file(&tmp);
            return Ok(Saved::Corrupt);
        }
        hashes.insert(hash.clone());

        let path: PathBuf = [output_dir, &format!("{:06}_{}.{}", index, &hash[..8], self.extension)]
            .iter()
            .collect();
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;

        Ok(Saved::File(FoundFile {
            path: path.to_string_lossy().into_owned(),
            extension: self.extension.clone(),
            type_name: self.name.clone(),
            size: len,
            offset: start,
            preview: None,
        }))
    }

    /// Abre o arquivo com um decodificador de verdade. Tipos sem
    /// decodificador (PDF, MP4, customizados) ficam com a checagem estrutural.
    fn decodes(&self, path: &Path) -> bool {
        match self.extension.as_str() {
            "jpg" | "png" => image::ImageReader::open(path)
                .and_then(|r| r.with_guessed_format())
                .ok()
                .and_then(|r| r.decode().ok())
                .is_some(),
            "zip" => zip_decodes(path),
            _ => true,
        }
    }

    /// Grava um texto aceito pelas heurísticas.
    fn save_text(
        &self,
        t: &FoundText,
        output_dir: &str,
        index: u64,
        hashes: &mut HashSet<String>,
    ) -> Result<Option<FoundFile>, String> {
        let hash = format!("{:x}", md5::compute(t.text.as_bytes()));
        if !hashes.insert(hash.clone()) {
            return Ok(None);
        }
        let path: PathBuf = [output_dir, &format!("{:06}_{}.txt", index, &hash[..8])].iter().collect();
        std::fs::write(&path, t.text.as_bytes()).map_err(|e| e.to_string())?;
        Ok(Some(FoundFile {
            path: path.to_string_lossy().into_owned(),
            extension: self.extension.clone(),
            type_name: self.name.clone(),
            size: t.text.len() as u64,
            offset: t.offset,
            preview: Some(t.preview.clone()),
        }))
    }
}

/// Lê o diretório central e descomprime as entradas (até um limite) conferindo
/// o CRC de cada uma, como um gerenciador de arquivos faria ao abrir.
fn zip_decodes(path: &Path) -> bool {
    const BUDGET: u64 = 16 * 1024 * 1024;
    let Ok(file) = File::open(path) else { return false };
    let Ok(mut archive) = zip::ZipArchive::new(file) else { return false };
    let mut spent = 0u64;
    for i in 0..archive.len() {
        let Ok(mut entry) = archive.by_index(i) else { return false };
        if entry.is_dir() {
            continue;
        }
        if spent >= BUDGET {
            break;
        }
        // read_to_end valida o CRC ao chegar no fim da entrada
        let mut sink = Vec::new();
        if entry.read_to_end(&mut sink).is_err() {
            return false;
        }
        spent += sink.len() as u64;
    }
    true
}

// ---------------------------------------------------------------------------
// scan – uma única leitura sequencial do dispositivo para todos os tipos
// ---------------------------------------------------------------------------

/// Tamanho padrão de cada bloco lido (3 blocos em memória ≈ 192 MB).
pub const DEFAULT_CHUNK: usize = 64 * 1024 * 1024;
const POOL_BUFFERS: usize = 3;

struct Chunk {
    start: u64,
    buf: Vec<u8>,
    len: usize,
}

/// Estado de busca de cada tipo
struct TypeState {
    finder: memmem::Finder<'static>,
    /// Texto não usa o finder (ver TextScanner)
    is_text: bool,
    /// Tudo antes deste offset já pertence a um arquivo recuperado deste tipo
    skip_until: u64,
    count: u64,
    skipped: u64,
    hashes: HashSet<String>,
    done: bool,
}

/// Lê o dispositivo uma vez e procura todas as assinaturas de `magics` em
/// cada bloco. A leitura do disco roda numa thread própria (enquanto um
/// bloco é analisado, o próximo já está sendo lido) e a busca de cada tipo
/// roda em paralelo sobre o mesmo bloco.
///
/// `max` limita a quantidade por tipo (0 = sem limite). `cancel` é consultado
/// a cada bloco e a cada candidato.
pub fn scan(
    path: &str,
    output_dir: &str,
    magics: &[MagicByte],
    max: u64,
    chunk_size: usize,
    cancel: &(dyn Fn() -> bool + Sync),
    on_event: &mut dyn FnMut(Progress),
) -> Result<(), String> {
    if magics.is_empty() {
        return Err("Nenhum tipo de arquivo selecionado.".into());
    }
    let total_size = get_device_size(path)?;
    std::fs::create_dir_all(output_dir).map_err(|e| e.to_string())?;

    // Blocos múltiplos de ALIGN mantêm as leituras válidas em dispositivos brutos
    let chunk_size = ((chunk_size as u64).min(total_size.max(carve::ALIGN)) / carve::ALIGN * carve::ALIGN)
        .max(carve::ALIGN) as usize;
    let file = open_sequential(path)?;
    let mut reader = DevReader::open(path, total_size)?;

    let mut states: Vec<TypeState> = magics
        .iter()
        .map(|m| TypeState {
            finder: memmem::Finder::new(&m.header).into_owned(),
            is_text: m.is_text(),
            skip_until: 0,
            count: 0,
            skipped: 0,
            hashes: HashSet::new(),
            done: false,
        })
        .collect();
    let overlap = magics.iter().map(|m| m.header.len()).max().unwrap() - 1;
    let names = magics.iter().map(|m| m.name.as_str()).collect::<Vec<_>>().join(", ");

    // Texto: varredura própria, sem assinatura
    let text_idx = magics.iter().position(|m| m.is_text());
    let mut text_scanner = TextScanner::new();
    let mut texts: Vec<FoundText> = Vec::new();

    let mut found_total: u64 = 0;
    let emit = |on_event: &mut dyn FnMut(Progress), bytes_read: u64, found: u64, status: String, file: Option<FoundFile>| {
        let percent = (bytes_read as f64 / total_size.max(1) as f64 * 100.0).min(100.0);
        on_event(Progress { bytes_read, total_size, files_found: found, percent, status, found_file: file });
    };
    emit(on_event, 0, 0, format!("Iniciando varredura de {}...", names), None);

    let mut cancelled = false;
    let mut bytes_done: u64 = 0;

    std::thread::scope(|s| -> Result<(), String> {
        let (data_tx, data_rx) = mpsc::sync_channel::<Result<Chunk, String>>(POOL_BUFFERS - 1);
        let (pool_tx, pool_rx) = mpsc::channel::<Vec<u8>>();
        for _ in 0..POOL_BUFFERS {
            let _ = pool_tx.send(vec![0u8; chunk_size]);
        }
        s.spawn(move || read_chunks(file, total_size, pool_rx, data_tx));

        // Fim do bloco anterior, para achar assinaturas que cruzam a fronteira
        let mut tail: Vec<u8> = Vec::new();
        let mut join: Vec<u8> = Vec::with_capacity(overlap * 2);

        for chunk in data_rx.iter() {
            let chunk = chunk?;
            if cancel() {
                cancelled = true;
                break;
            }
            let data = &chunk.buf[..chunk.len];

            // Candidatos: (offset absoluto da assinatura, índice do tipo)
            let mut cands: Vec<(u64, usize)> = Vec::new();

            // Assinaturas que começam no fim do bloco anterior
            if !tail.is_empty() {
                join.clear();
                join.extend_from_slice(&tail);
                join.extend_from_slice(&data[..overlap.min(data.len())]);
                let join_abs = chunk.start - tail.len() as u64;
                for (t, st) in states.iter().enumerate().filter(|(_, st)| !st.done && !st.is_text) {
                    let hlen = magics[t].header.len();
                    // Só as que cruzam a fronteira; as inteiras no bloco anterior já foram vistas
                    for p in st.finder.find_iter(&join).filter(|&p| p < tail.len() && p + hlen > tail.len()) {
                        cands.push((join_abs + p as u64, t));
                    }
                }
            }
            tail.clear();
            tail.extend_from_slice(&data[data.len() - overlap.min(data.len())..]);

            // Busca de cada tipo em paralelo sobre o mesmo bloco
            let active: Vec<usize> =
                (0..states.len()).filter(|&t| !states[t].done && !states[t].is_text).collect();
            if active.is_empty() {
                // só texto
            } else if active.len() == 1 {
                let t = active[0];
                cands.extend(states[t].finder.find_iter(data).map(|p| (chunk.start + p as u64, t)));
            } else {
                let found: Vec<Vec<(u64, usize)>> = std::thread::scope(|ss| {
                    let handles: Vec<_> = active
                        .iter()
                        .map(|&t| {
                            let finder = &states[t].finder;
                            ss.spawn(move || {
                                finder.find_iter(data).map(|p| (chunk.start + p as u64, t)).collect::<Vec<_>>()
                            })
                        })
                        .collect();
                    handles.into_iter().map(|h| h.join().unwrap_or_default()).collect()
                });
                cands.extend(found.into_iter().flatten());
            }
            cands.sort_unstable();

            // Validação a partir da memória sempre que o arquivo couber no bloco
            let shared = Arc::new(chunk.buf);
            reader.set_chunk(chunk.start, Arc::clone(&shared), chunk.len);

            for (sig_abs, t) in cands {
                if cancel() {
                    cancelled = true;
                    break;
                }
                let (magic, st) = (&magics[t], &mut states[t]);
                if st.done || sig_abs < st.skip_until {
                    continue;
                }
                let Some(start) = magic.file_start(sig_abs) else { continue };

                // Erro de leitura (setor ruim) invalida só este candidato
                let len = match magic.carve(&mut reader, start) {
                    Ok(Some(len)) => len,
                    Ok(None) | Err(_) => {
                        st.skipped += 1;
                        continue;
                    }
                };
                st.skip_until = start + len;

                let found = match magic.save_range(&mut reader, start, len, output_dir, found_total, &mut st.hashes)? {
                    Saved::File(f) => f,
                    Saved::Duplicate => continue,
                    Saved::Corrupt => {
                        st.skipped += 1;
                        continue;
                    }
                };
                st.count += 1;
                found_total += 1;
                emit(on_event, sig_abs, found_total,
                    format!("{} #{} encontrado ({:.1}%)", magic.name, st.count,
                        sig_abs as f64 / total_size as f64 * 100.0),
                    Some(found));

                if max > 0 && st.count >= max {
                    st.done = true;
                }
            }

            // Texto: sequências imprimíveis deste bloco (e as que vieram do anterior)
            if let Some(t) = text_idx.filter(|&t| !states[t].done && !cancelled) {
                text_scanner.feed(chunk.start, &shared[..chunk.len], &mut texts);
                for txt in texts.drain(..) {
                    if cancel() {
                        cancelled = true;
                        break;
                    }
                    let (magic, st) = (&magics[t], &mut states[t]);
                    let Some(found) = magic.save_text(&txt, output_dir, found_total, &mut st.hashes)? else {
                        continue;
                    };
                    st.count += 1;
                    found_total += 1;
                    emit(on_event, txt.offset, found_total,
                        format!("{} #{} encontrado ({:.1}%)", magic.name, st.count,
                            txt.offset as f64 / total_size as f64 * 100.0),
                        Some(found));
                    if max > 0 && st.count >= max {
                        st.done = true;
                        break;
                    }
                }
            }

            // Devolve o buffer ao pool (o reader deixou de referenciá-lo)
            reader.clear_chunk();
            if let Ok(buf) = Arc::try_unwrap(shared) {
                let _ = pool_tx.send(buf);
            }

            bytes_done = chunk.start + chunk.len as u64;
            if cancelled || states.iter().all(|st| st.done) {
                break;
            }
            emit(on_event, bytes_done, found_total,
                format!("Varrendo... {:.1}%", bytes_done as f64 / total_size as f64 * 100.0), None);
        }
        // Soltar data_rx/pool_tx encerra a thread de leitura
        Ok(())
    })?;

    // Sequência de texto que ia até o fim do dispositivo
    if let Some(t) = text_idx.filter(|&t| !states[t].done && !cancelled) {
        text_scanner.finish(&mut texts);
        for txt in texts.drain(..) {
            let (magic, st) = (&magics[t], &mut states[t]);
            if let Some(found) = magic.save_text(&txt, output_dir, found_total, &mut st.hashes)? {
                st.count += 1;
                found_total += 1;
                emit(on_event, total_size, found_total,
                    format!("{} #{} encontrado (100.0%)", magic.name, st.count), Some(found));
            }
        }
    }

    let summary = magics
        .iter()
        .zip(&states)
        .map(|(m, st)| format!("{} {}", st.count, m.name))
        .collect::<Vec<_>>()
        .join(", ");
    let skipped: u64 = states.iter().map(|st| st.skipped).sum();
    let status = if cancelled {
        format!("Busca interrompida. {} arquivo(s) extraido(s) ({}).", found_total, summary)
    } else {
        format!(
            "Concluido. {} arquivo(s) extraido(s) ({}), {} candidato(s) corrompido(s) ignorado(s).",
            found_total, summary, skipped
        )
    };
    let final_pos = if cancelled { bytes_done } else { total_size };
    emit(on_event, final_pos, found_total, status, None);
    Ok(())
}

/// Thread de leitura: preenche buffers do pool em sequência até o fim do
/// dispositivo ou até o consumidor desistir (canais fechados).
fn read_chunks(
    mut file: File,
    total_size: u64,
    pool: mpsc::Receiver<Vec<u8>>,
    out: mpsc::SyncSender<Result<Chunk, String>>,
) {
    let mut pos: u64 = 0;
    while pos < total_size {
        let Ok(mut buf) = pool.recv() else { return };
        // Não ler além do fim: em dispositivos brutos isso gera erro em vez de EOF
        let len = (buf.len() as u64).min(total_size - pos) as usize;
        let msg = match file.read_exact(&mut buf[..len]) {
            Ok(()) => Ok(Chunk { start: pos, buf, len }),
            Err(e) => Err(format!("Falha ao ler offset {}: {}", pos, e)),
        };
        let failed = msg.is_err();
        if out.send(msg).is_err() || failed {
            return;
        }
        pos += len as u64;
    }
}

/// Abre o dispositivo avisando o SO que a leitura é sequencial (read-ahead).
fn open_sequential(path: &str) -> Result<File, String> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;
        opts.custom_flags(FILE_FLAG_SEQUENTIAL_SCAN);
    }
    opts.open(path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const ALL: [&str; 6] = ["jpeg", "png", "pdf", "zip", "mp4", "text"];

    fn fixture() -> (String, Vec<serde_json::Value>) {
        let dir = std::env::var("CARVE_FIXTURE").expect("CARVE_FIXTURE");
        let manifest = serde_json::from_str(
            &std::fs::read_to_string(format!("{}/manifest.json", dir)).unwrap(),
        )
        .unwrap();
        (format!("{}/disk.img", dir), manifest)
    }

    fn run(img: &str, chunk: usize, cancel: &(dyn Fn() -> bool + Sync)) -> (Vec<FoundFile>, String) {
        static RUN: AtomicUsize = AtomicUsize::new(0);
        let out = std::env::temp_dir().join(format!(
            "rscover_test_{}_{}", std::process::id(), RUN.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&out);
        let magics: Vec<_> = ALL.iter().map(|t| MagicByte::by_id(t).unwrap()).collect();
        let (mut found, mut last) = (Vec::new(), String::new());
        scan(img, out.to_str().unwrap(), &magics, 0, chunk, cancel, &mut |p| {
            last = p.status.clone();
            found.extend(p.found_file);
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(&out);
        (found, last)
    }

    /// Todos os tipos numa passada: cada arquivo válido do fixture é achado com o
    /// offset/tamanho certos e nenhum corrompido é salvo. Blocos de 8 KB forçam
    /// assinaturas cruzando fronteiras.
    #[test]
    #[ignore]
    fn fixture_single_pass() {
        let (img, manifest) = fixture();
        for chunk in [8192, DEFAULT_CHUNK] {
            let (found, last) = run(&img, chunk, &|| false);
            println!("chunk={} -> {} arquivos; {}", chunk, found.len(), last);
            for f in &found {
                println!("  {:<4} offset={:<7} size={}", f.extension, f.offset, f.size);
            }
            for item in &manifest {
                let kind = item["kind"].as_str().unwrap();
                let off = item["offset"].as_u64().unwrap();
                if kind == "txt" {
                    let marker = item["marker"].as_str().unwrap();
                    let hit = found.iter().any(|f| f.extension == "txt"
                        && f.preview.as_deref().is_some_and(|p| p.contains(marker)));
                    assert_eq!(hit, item["accepted"].as_bool().unwrap(), "txt '{}' (chunk {})", marker, chunk);
                    continue;
                }
                let hit = found.iter().find(|f| f.extension == kind && f.offset == off);
                match item["expected"].as_u64() {
                    Some(size) => assert_eq!(hit.map(|f| f.size), Some(size), "{} @{} (chunk {})", kind, off, chunk),
                    None => assert!(hit.is_none(), "corrompido salvo: {} @{} (chunk {})", kind, off, chunk),
                }
            }
        }
    }

    #[test]
    #[ignore]
    fn fixture_cancel() {
        let (img, _) = fixture();
        let calls = AtomicUsize::new(0);
        let (_, last) = run(&img, 8192, &|| calls.fetch_add(1, Ordering::SeqCst) > 3);
        assert!(last.starts_with("Busca interrompida"), "{}", last);
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    use std::time::Instant;

    fn timed(label: &str, bytes: u64, f: impl FnOnce()) -> f64 {
        let t = Instant::now();
        f();
        let s = t.elapsed().as_secs_f64();
        println!("{:<48} {:>7.2} s  {:>7.0} MB/s", label, s, bytes as f64 / s / 1e6);
        s
    }

    fn scan_types(img: &str, ids: &[&str], tag: &str) {
        let out = std::env::temp_dir().join(format!("rscover_bench_{}", tag));
        let _ = std::fs::remove_dir_all(&out);
        let magics: Vec<_> = ids.iter().map(|t| MagicByte::by_id(t).unwrap()).collect();
        scan(img, out.to_str().unwrap(), &magics, 0, DEFAULT_CHUNK, &|| false, &mut |_| {}).unwrap();
        let _ = std::fs::remove_dir_all(&out);
    }

    /// Antes: uma leitura completa por tipo, em paralelo (como o frontend fazia).
    /// Depois: uma leitura para todos os tipos.
    /// BENCH_IMG=<arquivo> cargo test --release --lib bench -- --ignored --nocapture
    #[test]
    #[ignore]
    fn single_pass_vs_per_type() {
        let img = std::env::var("BENCH_IMG").expect("BENCH_IMG");
        let size = std::fs::metadata(&img).unwrap().len();
        // Aquece o cache do SO para medir CPU, não o SSD
        scan_types(&img, &["zip"], "warm");

        for ids in [&["jpeg", "png", "pdf"][..], &["jpeg", "png", "pdf", "zip", "mp4", "text"][..]] {
            let n = ids.len();
            let before = timed(&format!("antes: {} leituras paralelas (1 tipo cada)", n), size, || {
                std::thread::scope(|s| {
                    for (i, id) in ids.iter().enumerate() {
                        let img = &img;
                        s.spawn(move || scan_types(img, &[id], &format!("old{}", i)));
                    }
                });
            });
            let after = timed(&format!("depois: 1 leitura, {} tipos", n), size, || {
                scan_types(&img, ids, "new")
            });
            println!("  -> {:.1}x mais rapido (CPU/cache); em HD: {} leituras -> 1\n", before / after, n);
        }
    }

    /// Busca da assinatura: laço escalar antigo vs memmem (SIMD).
    #[test]
    #[ignore]
    fn signature_search() {
        let data: Vec<u8> = (0..256u64 * 1024 * 1024)
            .map(|i| (i.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 56) as u8)
            .collect();
        let header = [0xFFu8, 0xD8, 0xFF];
        let mut old_hits = 0;
        timed("busca escalar (antiga), JPEG", data.len() as u64, || {
            let mut i = 0;
            while let Some(p) = data[i..].iter().position(|&b| b == header[0]) {
                let at = i + p;
                i = at + 1;
                if at + 3 <= data.len() && data[at..at + 3] == header {
                    old_hits += 1;
                }
            }
        });
        let mut new_hits = 0;
        timed("memmem SIMD (nova), JPEG", data.len() as u64, || {
            new_hits = memmem::Finder::new(&header).find_iter(&data).count();
        });
        assert_eq!(old_hits, new_hits);
    }
}
