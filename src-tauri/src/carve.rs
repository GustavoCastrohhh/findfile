//! Carving estrutural: a partir do offset onde a assinatura foi encontrada,
//! percorre a estrutura interna do formato para descobrir onde o arquivo
//! termina e se ele é legível.
//!
//! Critério: rejeitar apenas o que um visualizador certamente não abre
//! (estrutura quebrada, cabeçalho truncado, CRC inválido em chunk crítico).
//! Na dúvida, o arquivo é mantido.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

/// Alinhamento para leituras em dispositivos brutos (múltiplo de setores 512 B e 4Kn)
pub const ALIGN: u64 = 4096;
const WINDOW: u64 = 1024 * 1024;

type CarveResult = Result<Option<u64>, String>;

// ---------------------------------------------------------------------------
// DevReader – acesso aleatório com cache alinhado
// ---------------------------------------------------------------------------

pub struct DevReader {
    file: File,
    size: u64,
    buf: Vec<u8>,
    buf_start: u64,
    /// Bloco da varredura sequencial já em memória: (início absoluto, dados, tamanho)
    chunk: Option<(u64, Arc<Vec<u8>>, usize)>,
}

impl DevReader {
    pub fn open(path: &str, size: u64) -> Result<Self, String> {
        Ok(Self {
            file: File::open(path).map_err(|e| e.to_string())?,
            size,
            buf: Vec::new(),
            buf_start: 0,
            chunk: None,
        })
    }

    /// Passa a servir leituras dentro de [start, start+len) direto da memória.
    pub fn set_chunk(&mut self, start: u64, data: Arc<Vec<u8>>, len: usize) {
        self.chunk = Some((start, data, len));
    }

    pub fn clear_chunk(&mut self) {
        self.chunk = None;
    }

    /// Fatia do bloco em memória para [pos, pos+len), se estiver inteira nele.
    fn from_chunk(&self, pos: u64, len: usize) -> Option<&[u8]> {
        let (start, data, clen) = self.chunk.as_ref()?;
        let rel = pos.checked_sub(*start)? as usize;
        (rel + len <= *clen).then(|| &data[rel..rel + len])
    }

    fn fill(&mut self, pos: u64) -> Result<(), String> {
        let start = pos / ALIGN * ALIGN;
        let len = WINDOW.min(self.size - start) as usize;
        self.buf.resize(len, 0);
        self.file.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
        self.file
            .read_exact(&mut self.buf)
            .map_err(|e| format!("Falha ao ler offset {}: {}", start, e))?;
        self.buf_start = start;
        Ok(())
    }

    /// Byte em `pos`, ou None além do fim do dispositivo.
    pub fn byte(&mut self, pos: u64) -> Result<Option<u8>, String> {
        if pos >= self.size {
            return Ok(None);
        }
        if let Some(b) = self.from_chunk(pos, 1) {
            return Ok(Some(b[0]));
        }
        if pos < self.buf_start || pos >= self.buf_start + self.buf.len() as u64 {
            self.fill(pos)?;
        }
        Ok(Some(self.buf[(pos - self.buf_start) as usize]))
    }

    /// Preenche `out` a partir de `pos`; false se ultrapassar o fim.
    pub fn read(&mut self, pos: u64, out: &mut [u8]) -> Result<bool, String> {
        if pos + out.len() as u64 > self.size {
            return Ok(false);
        }
        if let Some(src) = self.from_chunk(pos, out.len()) {
            out.copy_from_slice(src);
            return Ok(true);
        }
        let mut done = 0;
        while done < out.len() {
            let p = pos + done as u64;
            if p < self.buf_start || p >= self.buf_start + self.buf.len() as u64 {
                self.fill(p)?;
            }
            let rel = (p - self.buf_start) as usize;
            let n = (self.buf.len() - rel).min(out.len() - done);
            out[done..done + n].copy_from_slice(&self.buf[rel..rel + n]);
            done += n;
        }
        Ok(true)
    }

    fn u16_be(&mut self, pos: u64) -> Result<Option<u16>, String> {
        let mut b = [0u8; 2];
        Ok(self.read(pos, &mut b)?.then(|| u16::from_be_bytes(b)))
    }

    /// Posição da próxima ocorrência de `pat` em [from, limit).
    fn find(&mut self, pat: &[u8], from: u64, limit: u64) -> Result<Option<u64>, String> {
        let limit = limit.min(self.size);
        let mut pos = from;
        while pos + pat.len() as u64 <= limit {
            if self.byte(pos)? == Some(pat[0]) {
                let mut ok = true;
                for (i, &c) in pat.iter().enumerate().skip(1) {
                    if self.byte(pos + i as u64)? != Some(c) {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    return Ok(Some(pos));
                }
            }
            pos += 1;
        }
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// JPEG – percorre segmentos; o APP1 (EXIF) é pulado pelo tamanho, então a
// miniatura embutida (com seu próprio FFD9) não corta a imagem principal.
// ---------------------------------------------------------------------------

pub fn jpeg(r: &mut DevReader, start: u64, max: u64) -> CarveResult {
    let limit = start + max;
    let mut pos = start + 2; // após SOI
    let (mut has_sof, mut has_sos) = (false, false);

    loop {
        if pos >= limit || r.byte(pos)? != Some(0xFF) {
            return Ok(None);
        }
        // Bytes FF de preenchimento antes do marcador
        let mut marker = match r.byte(pos + 1)? { Some(m) => m, None => return Ok(None) };
        while marker == 0xFF {
            pos += 1;
            marker = match r.byte(pos + 1)? { Some(m) => m, None => return Ok(None) };
        }
        pos += 2;

        match marker {
            0xD9 => {
                return Ok((has_sof && has_sos).then(|| pos - start));
            }
            0x01 | 0xD0..=0xD7 => continue, // sem campo de tamanho
            0x00 | 0xD8 => return Ok(None),
            _ => {
                let len = match r.u16_be(pos)? { Some(l) if l >= 2 => l as u64, _ => return Ok(None) };
                if matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
                    has_sof = true;
                }
                pos += len;

                if marker == 0xDA {
                    has_sos = true;
                    // Dados entrópicos: termina no primeiro FF seguido de
                    // algo que não seja 00 (stuffing) nem RSTn.
                    loop {
                        if pos >= limit {
                            return Ok(None);
                        }
                        match r.byte(pos)? {
                            None => return Ok(None),
                            Some(0xFF) => match r.byte(pos + 1)? {
                                None => return Ok(None),
                                Some(0x00) | Some(0xD0..=0xD7) => pos += 2,
                                Some(0xFF) => pos += 1,
                                Some(_) => break,
                            },
                            Some(_) => pos += 1,
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// PNG – percorre chunks conferindo CRC. CRC errado em chunk crítico
// (IHDR/PLTE/IDAT/IEND) = dados de imagem corrompidos; em chunk auxiliar é
// ignorado, como fazem os decodificadores.
// ---------------------------------------------------------------------------

pub fn png(r: &mut DevReader, start: u64, max: u64) -> CarveResult {
    let limit = start + max;
    let mut pos = start + 8;
    let mut first = true;
    let mut has_idat = false;
    let mut data = Vec::new();

    loop {
        let mut hdr = [0u8; 8];
        if !r.read(pos, &mut hdr)? {
            return Ok(None);
        }
        let len = u32::from_be_bytes(hdr[0..4].try_into().unwrap()) as u64;
        let kind: [u8; 4] = hdr[4..8].try_into().unwrap();
        if !kind.iter().all(u8::is_ascii_alphabetic) || pos + 12 + len > limit {
            return Ok(None);
        }
        if first && &kind != b"IHDR" {
            return Ok(None);
        }
        first = false;

        // CRC cobre tipo + dados
        data.resize(len as usize + 4, 0);
        let mut stored = [0u8; 4];
        if !r.read(pos + 8, &mut data[4..])? || !r.read(pos + 8 + len, &mut stored)? {
            return Ok(None);
        }
        data[..4].copy_from_slice(&kind);
        let critical = kind[0].is_ascii_uppercase();
        if critical && crc32(&data) != u32::from_be_bytes(stored) {
            return Ok(None);
        }

        has_idat |= &kind == b"IDAT";
        pos += 12 + len;
        if &kind == b"IEND" {
            return Ok(has_idat.then(|| pos - start));
        }
    }
}

// ---------------------------------------------------------------------------
// ZIP – localiza o End Of Central Directory e confere que o diretório central
// aponta exatamente para este início (offset + tamanho == posição do EOCD).
// ---------------------------------------------------------------------------

pub fn zip(r: &mut DevReader, start: u64, max: u64) -> CarveResult {
    let limit = start + max;
    let eocd = match r.find(b"PK\x05\x06", start + 4, limit)? {
        Some(p) => p,
        None => return Ok(None),
    };
    let mut rec = [0u8; 22];
    if !r.read(eocd, &mut rec)? {
        return Ok(None);
    }
    let cd_size = u32::from_le_bytes(rec[12..16].try_into().unwrap()) as u64;
    let cd_offset = u32::from_le_bytes(rec[16..20].try_into().unwrap()) as u64;
    let comment_len = u16::from_le_bytes(rec[20..22].try_into().unwrap()) as u64;
    let end = eocd + 22 + comment_len;

    // ZIP64 usa 0xFFFFFFFF e guarda os valores reais em outro registro;
    // mantém sem conferir para não descartar arquivo válido.
    let zip64 = cd_offset == 0xFFFF_FFFF || cd_size == 0xFFFF_FFFF;
    if !zip64 {
        if start + cd_offset + cd_size != eocd {
            return Ok(None);
        }
        let mut sig = [0u8; 4];
        if cd_size > 0 && (!r.read(start + cd_offset, &mut sig)? || &sig != b"PK\x01\x02") {
            return Ok(None);
        }
    }
    Ok((end <= limit && end <= r.size).then(|| end - start))
}

// ---------------------------------------------------------------------------
// MP4 – percorre as caixas ISO-BMFF de nível raiz; exige moov + mdat.
// ---------------------------------------------------------------------------

const MP4_TOP_LEVEL_BOXES: &[&[u8; 4]] = &[
    b"ftyp", b"moov", b"mdat", b"free", b"skip", b"wide", b"uuid", b"meta",
    b"pdin", b"moof", b"mfra", b"styp", b"sidx", b"udta", b"pnot", b"prfl",
];

pub fn mp4(r: &mut DevReader, start: u64, max: u64) -> CarveResult {
    let mut pos = start;
    let mut first = true;
    let (mut saw_moov, mut saw_mdat) = (false, false);

    loop {
        let mut hdr = [0u8; 16];
        let n = if r.read(pos, &mut hdr)? {
            16
        } else if r.read(pos, &mut hdr[..8])? {
            8
        } else {
            break;
        };

        let kind: [u8; 4] = hdr[4..8].try_into().unwrap();
        if !MP4_TOP_LEVEL_BOXES.contains(&&kind) {
            break;
        }
        let mut size = u32::from_be_bytes(hdr[0..4].try_into().unwrap()) as u64;
        if size == 1 {
            if n < 16 {
                break;
            }
            size = u64::from_be_bytes(hdr[8..16].try_into().unwrap());
        } else if size == 0 {
            size = r.size - pos; // caixa vai até o fim
        }
        if first && (&kind != b"ftyp" || !(8..=256).contains(&size)) {
            return Ok(None);
        }
        if size < 8 || pos + size > r.size {
            break; // caixa truncada: termina no fim da anterior
        }
        first = false;
        saw_moov |= &kind == b"moov";
        saw_mdat |= &kind == b"mdat";

        pos += size;
        if pos - start > max {
            return Ok(None);
        }
    }

    Ok((saw_moov && saw_mdat).then(|| pos - start))
}

// ---------------------------------------------------------------------------
// PDF – vai até o %%EOF; se logo depois vier uma atualização incremental
// (novos objetos/xref), continua até o %%EOF seguinte.
// ---------------------------------------------------------------------------

pub fn pdf(r: &mut DevReader, start: u64, max: u64) -> CarveResult {
    let limit = start + max;
    let mut from = start + 5;
    let mut end = None;

    while let Some(eof) = r.find(b"%%EOF", from, limit)? {
        let mut e = eof + 5;
        // Inclui a quebra de linha final
        while matches!(r.byte(e)?, Some(b'\r' | b'\n')) {
            e += 1;
        }
        end = Some(e);
        if !pdf_update_follows(r, e)? {
            break;
        }
        from = e;
    }

    let Some(end) = end else { return Ok(None) };
    // Precisa ter ao menos um objeto e o /Root (catálogo) no trailer ou no
    // xref stream; sem ele nenhum visualizador abre, mesmo reconstruindo o xref.
    let has_obj = r.find(b" obj", start, end)?.is_some();
    let has_root = r.find(b"/Root", start, end)?.is_some();
    Ok((has_obj && has_root).then(|| end - start))
}

fn pdf_update_follows(r: &mut DevReader, pos: u64) -> Result<bool, String> {
    let mut peek = [0u8; 32];
    let n = (r.size.saturating_sub(pos)).min(32) as usize;
    if n == 0 || !r.read(pos, &mut peek[..n])? {
        return Ok(false);
    }
    let text = String::from_utf8_lossy(&peek[..n]);
    let t = text.trim_start();
    if t.starts_with("xref") || t.starts_with("%PDF") {
        return Ok(t.starts_with("xref"));
    }
    // "12 0 obj"
    let mut parts = t.split_ascii_whitespace();
    Ok(matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(a), Some(b), Some(c))
            if a.bytes().all(|x| x.is_ascii_digit()) && b.bytes().all(|x| x.is_ascii_digit()) && c.starts_with("obj")
    ))
}

// ---------------------------------------------------------------------------
// Genérico (assinaturas customizadas) – cabeçalho até rodapé.
// ---------------------------------------------------------------------------

pub fn by_footer(r: &mut DevReader, start: u64, header_len: u64, footer: &[u8], max: u64) -> CarveResult {
    Ok(r
        .find(footer, start + header_len, start + max)?
        .map(|p| p + footer.len() as u64 - start))
}

// ---------------------------------------------------------------------------
// CRC-32 (IEEE), usado pelo PNG
// ---------------------------------------------------------------------------

fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, v) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *v = c;
        }
        t
    });
    !data.iter().fold(!0u32, |c, &b| table[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_known_value() {
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
    }

    /// Imagem de disco sintética com arquivos válidos, truncados e corrompidos.
    /// Gerar e rodar com: CARVE_FIXTURE=<pasta com disk.img e manifest.json>
    /// cargo test -- --ignored fixture
    #[test]
    #[ignore]
    fn fixture_disk_image() {
        let dir = std::env::var("CARVE_FIXTURE").expect("CARVE_FIXTURE");
        let img = format!("{}/disk.img", dir);
        let manifest: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(format!("{}/manifest.json", dir)).unwrap())
                .unwrap();
        let size = std::fs::metadata(&img).unwrap().len();
        let mut r = DevReader::open(&img, size).unwrap();

        let mut failures = Vec::new();
        for (i, item) in manifest.iter().enumerate() {
            let kind = item["kind"].as_str().unwrap();
            let off = item["offset"].as_u64().unwrap();
            // Arquivo estruturalmente ok mas que não decodifica: o carver aceita,
            // a varredura completa rejeita (ver extract::tests)
            let expected = item["carve_expected"].as_u64().or(item["expected"].as_u64());
            let max = 500 * 1024 * 1024;
            let got = match kind {
                "jpg" => jpeg(&mut r, off, max),
                "png" => png(&mut r, off, max),
                "zip" => zip(&mut r, off, max),
                "mp4" => mp4(&mut r, off, max),
                "pdf" => pdf(&mut r, off, max),
                "txt" => continue, // texto é testado em text.rs / extract.rs
                _ => unreachable!(),
            }
            .unwrap();
            let status = if got == expected { "ok" } else { "FALHOU" };
            println!("#{:02} {:<4} esperado={:?} obtido={:?} {}", i, kind, expected, got, status);
            if got != expected {
                failures.push(i);
            }
        }
        assert!(failures.is_empty(), "falhas: {:?}", failures);
    }
}
