//! Recuperação de texto "humano": mensagens, e-mails, anotações com dados
//! pessoais. Texto não tem magic bytes; o que se procura são sequências de
//! bytes imprimíveis que passem por heurísticas de linguagem natural e
//! contenham indícios de dados pessoais ou de conversa entre pessoas.

use std::collections::HashSet;

/// Sequência mínima para ser considerada (bytes)
const MIN_RUN: usize = 32;
/// Sequências maiores são fechadas e avaliadas por partes
const MAX_RUN: usize = 64 * 1024;
/// Pontuação mínima (soma dos pesos dos indícios encontrados)
const MIN_SCORE: u32 = 2;

/// Indícios fortes (peso 2): dados pessoais e marcas de conversa
const STRONG: &[&str] = &[
    "senha", "password", "passwd", "pwd", "login", "usuário", "usuario", "username",
    "email", "e-mail", "gmail", "hotmail", "outlook", "yahoo", "uol", "bol",
    "cpf", "cnpj", "rg", "identidade", "telefone", "celular", "whatsapp", "zap",
    "endereço", "endereco", "cep", "nascimento", "aniversário", "aniversario",
    "cartão", "cartao", "agência", "agencia", "pix", "boleto", "salário", "salario",
    "mãe", "mae", "esposa", "marido", "namorada", "namorado",
    "oi", "olá", "ola", "oie", "eae", "tudo bem", "bom dia", "boa tarde", "boa noite",
    "obrigado", "obrigada", "valeu", "abraço", "abraco", "abraços", "abracos",
    "beijo", "beijos", "bjs", "beijinho", "querido", "querida", "saudade", "saudades",
    "desculpa", "desculpe", "por favor", "pfv", "att", "atenciosamente", "cordialmente",
    "prezado", "prezada", "prezados", "senhor", "senhora", "você", "voce", "vc", "vcs",
    "kkk", "kkkk", "rsrs", "haha", "hello", "dear", "regards", "thanks", "thank you",
    "love", "kisses", "hugs", "please", "sorry",
];

/// Indícios fracos (peso 1): palavras comuns que, junto de outra, sugerem dados pessoais
const WEAK: &[&str] = &[
    "nome", "sobrenome", "conta", "banco", "rua", "avenida", "bairro", "cidade", "idade",
    "contrato", "pai", "filho", "filha", "amigo", "amiga", "amor", "favor", "caro",
    "sr", "sra", "hi", "hey", "bye",
];

/// Nomes próprios comuns no Brasil; contam só quando aparecem capitalizados
const NAMES: &[&str] = &[
    "Maria", "José", "Jose", "Ana", "João", "Joao", "Antônio", "Antonio", "Francisco",
    "Carlos", "Paulo", "Pedro", "Lucas", "Luiz", "Luis", "Marcos", "Gabriel", "Rafael",
    "Daniel", "Marcelo", "Bruno", "Eduardo", "Felipe", "Rodrigo", "Manoel", "Manuel",
    "Nelson", "Roberto", "Fábio", "Fabio", "Alexandre", "Edson", "Sérgio", "Sergio",
    "Jorge", "Fernando", "Ricardo", "André", "Andre", "Leandro", "Leonardo", "Thiago",
    "Tiago", "Diego", "Mateus", "Matheus", "Guilherme", "Gustavo", "Henrique", "Vinícius",
    "Vinicius", "Caio", "Igor", "Renato", "Vitor", "Victor", "Wellington", "William",
    "Anderson", "Adriano", "Alan", "Alex", "Cláudio", "Claudio", "Douglas", "Elias",
    "Emerson", "Everton", "Fabrício", "Fabricio", "Flávio", "Flavio", "Geraldo", "Gilberto",
    "Hugo", "Ivan", "Jean", "Jonas", "Júlio", "Julio", "Kevin", "Márcio", "Marcio", "Mário",
    "Mario", "Maurício", "Mauricio", "Miguel", "Murilo", "Otávio", "Otavio", "Rogério",
    "Rogerio", "Samuel", "Sebastião", "Sebastiao", "Vagner", "Wagner", "Wilson",
    "Francisca", "Antônia", "Antonia", "Adriana", "Juliana", "Márcia", "Marcia", "Fernanda",
    "Patrícia", "Patricia", "Aline", "Sandra", "Camila", "Amanda", "Bruna", "Jéssica",
    "Jessica", "Letícia", "Leticia", "Júlia", "Julia", "Luciana", "Vanessa", "Mariana",
    "Gabriela", "Vera", "Vitória", "Vitoria", "Larissa", "Cláudia", "Claudia", "Beatriz",
    "Simone", "Renata", "Sônia", "Sonia", "Cristiane", "Rita", "Carla", "Natália", "Natalia",
    "Bianca", "Carolina", "Caroline", "Daniela", "Débora", "Debora", "Elaine", "Eliane",
    "Fabiana", "Fátima", "Fatima", "Helena", "Isabela", "Isabel", "Joana", "Karina", "Laura",
    "Lúcia", "Lucia", "Luana", "Luiza", "Mônica", "Monica", "Paula", "Priscila", "Raquel",
    "Regina", "Rosa", "Sabrina", "Silvia", "Sílvia", "Tatiane", "Tatiana", "Thais", "Thaís",
    "Valéria", "Valeria", "Viviane", "Alice", "Sophia", "Sofia", "Laís", "Lais", "Lorena",
    "Manuela", "Melissa", "Nicole", "Rafaela", "Yasmin", "Cecília", "Cecilia", "Clara",
    "Heloísa", "Heloisa",
];

/// Marcas de código, markup, binário e boilerplate de software (busca por substring, minúsculas)
const BLACKLIST: &[&str] = &[
    "<?xml", "<html", "<!doctype", "<div", "<span", "<svg", "</", "/>", "xmlns", "href=", "src=",
    "http-equiv", "charset=", "content-type", "encoding=", "version=", "urn:", "http://schemas",
    "function(", "function ", "#include", "#define", "import ", "return ", "var ", "const ",
    "=>", "{}", "};", "0x", "\\x", "\\u00", "std::", "printf(", "null,", "undefined", "[object",
    "package ", "namespace ", "#!/bin", "select * from", "insert into", "create table",
    "font-family", "@font-face", "@media", "px;", "base64,", "-----begin",
    "endobj", "endstream", "startxref", "%%eof",
    "this program cannot", "copyright (c)", "all rights reserved", "microsoft corporation",
    "gnu general", "mozilla/", "user-agent", "hkey_", "c:\\program", "c:\\windows", "\\system32",
    ".dll", ".exe", ".sys", ".ini",
];

// ---------------------------------------------------------------------------
// TextScanner – acumula sequências imprimíveis através dos blocos
// ---------------------------------------------------------------------------

pub struct TextScanner {
    run: Vec<u8>,
    run_start: u64,
    seen: HashSet<String>,
}

/// Texto aceito: offset no dispositivo, conteúdo UTF-8 e prévia para a tela.
pub struct FoundText {
    pub offset: u64,
    pub text: String,
    pub preview: String,
}

impl TextScanner {
    pub fn new() -> Self {
        Self { run: Vec::new(), run_start: 0, seen: HashSet::new() }
    }

    /// Processa um bloco; textos aceitos vão para `out`.
    pub fn feed(&mut self, abs: u64, data: &[u8], out: &mut Vec<FoundText>) {
        for (i, &b) in data.iter().enumerate() {
            if is_printable(b) {
                if self.run.is_empty() {
                    self.run_start = abs + i as u64;
                }
                self.run.push(b);
                if self.run.len() >= MAX_RUN {
                    self.close(out);
                }
            } else if !self.run.is_empty() {
                self.close(out);
            }
        }
    }

    /// Fim do dispositivo: avalia a sequência pendente.
    pub fn finish(&mut self, out: &mut Vec<FoundText>) {
        if !self.run.is_empty() {
            self.close(out);
        }
    }

    fn close(&mut self, out: &mut Vec<FoundText>) {
        if self.run.len() >= MIN_RUN {
            if let Some(text) = evaluate(&self.run) {
                // Dedup pelo conteúdo normalizado (mesma mensagem em vários setores)
                if self.seen.insert(text.clone()) {
                    out.push(FoundText { offset: self.run_start, preview: preview(&text), text });
                }
            }
        }
        self.run.clear();
    }
}

impl Default for TextScanner {
    fn default() -> Self {
        Self::new()
    }
}

/// Bytes que podem fazer parte de texto: controles de formatação, ASCII
/// imprimível e bytes altos (UTF-8 multibyte ou Latin-1).
fn is_printable(b: u8) -> bool {
    matches!(b, 0x09 | 0x0A | 0x0D) || (0x20..=0x7E).contains(&b) || b >= 0x80
}

/// Decodifica e aplica as heurísticas; Some(texto) se parecer texto humano
/// com dados pessoais ou marcas de conversa.
pub fn evaluate(bytes: &[u8]) -> Option<String> {
    let text = decode(bytes);
    let text = text.trim_start_matches('\u{feff}').trim();
    if text.chars().count() < MIN_RUN {
        return None;
    }
    if !looks_like_prose(text) {
        return None;
    }
    let lower = text.to_lowercase();
    if BLACKLIST.iter().any(|b| lower.contains(b)) {
        return None;
    }
    (score(text, &lower) >= MIN_SCORE).then(|| text.to_string())
}

/// UTF-8 quando válido; senão Latin-1 (arquivos antigos do Windows).
fn decode(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

/// Proporções típicas de linguagem natural, contra código, hex, base64 e ruído.
fn looks_like_prose(text: &str) -> bool {
    let mut letters = 0usize;
    let mut upper = 0usize;
    let mut symbols = 0usize;
    let mut non_ws = 0usize;
    let mut distinct = HashSet::new();
    let mut longest_repeat = 0usize;
    let mut repeat = 0usize;
    let mut prev: Option<char> = None;

    for c in text.chars() {
        if c.is_whitespace() {
            prev = None;
            repeat = 0;
            continue;
        }
        non_ws += 1;
        distinct.insert(c);
        if c.is_alphabetic() {
            letters += 1;
            if c.is_uppercase() {
                upper += 1;
            }
        } else if "{}[]<>=;|\\/#$%^*~`_".contains(c) {
            symbols += 1;
        }
        repeat = if prev == Some(c) { repeat + 1 } else { 1 };
        longest_repeat = longest_repeat.max(repeat);
        prev = Some(c);
    }

    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() < 5 || non_ws == 0 || distinct.len() < 12 {
        return false;
    }
    let longest_word = words.iter().map(|w| w.chars().count()).max().unwrap_or(0);
    let avg_word = non_ws as f64 / words.len() as f64;

    letters as f64 / non_ws as f64 >= 0.6
        && upper as f64 / letters.max(1) as f64 <= 0.6
        && symbols as f64 / non_ws as f64 <= 0.04
        && longest_word <= 80
        && (2.0..=12.0).contains(&avg_word)
        && longest_repeat <= 8
}

/// Soma dos indícios de dados pessoais / conversa.
fn score(text: &str, lower: &str) -> u32 {
    let mut s = 0u32;

    // Palavras inteiras (evita "ana" em "banana", "rg" em "carga")
    let words: HashSet<&str> = lower.split(|c: char| !c.is_alphanumeric() && c != '@').collect();
    let has = |w: &str| {
        if w.contains(' ') { lower.contains(w) } else { words.contains(w) }
    };
    s += 2 * STRONG.iter().filter(|w| has(w)).count() as u32;
    s += WEAK.iter().filter(|w| has(w)).count() as u32;

    // Nomes só quando capitalizados, como em texto escrito por pessoas
    let cap_words: HashSet<&str> = text.split(|c: char| !c.is_alphanumeric()).collect();
    s += 2 * NAMES.iter().filter(|n| cap_words.contains(*n)).count() as u32;

    // E-mail: token com "@" e um ponto depois
    if words.iter().any(|w| w.split_once('@').is_some_and(|(u, d)| !u.is_empty() && d.contains('.'))) {
        s += 3;
    }
    if has_cpf(text) {
        s += 3;
    }
    if has_phone(text) {
        s += 2;
    }
    s
}

/// ddd.ddd.ddd-dd
fn has_cpf(text: &str) -> bool {
    let b = text.as_bytes();
    b.windows(14).any(|w| {
        w.iter().enumerate().all(|(i, &c)| match i {
            3 | 7 => c == b'.',
            11 => c == b'-',
            _ => c.is_ascii_digit(),
        })
    })
}

/// Celular "9dddd-dddd" ou fixo "dddd-dddd" precedido de DDD "(dd) " / "dd ",
/// para não confundir com intervalos como "2020-2024".
fn has_phone(text: &str) -> bool {
    let b = text.as_bytes();
    let digits = |s: &[u8]| s.iter().all(u8::is_ascii_digit);
    let cell = b.windows(10).any(|w| w[0] == b'9' && w[5] == b'-' && digits(&w[..5]) && digits(&w[6..]));
    let landline = (0..b.len().saturating_sub(8)).any(|i| {
        let w = &b[i..i + 9];
        if !(w[4] == b'-' && digits(&w[..4]) && digits(&w[5..])) {
            return false;
        }
        // DDD imediatamente antes: "(11) 3456-7890" ou "11 3456-7890"
        let prefix = b[..i].strip_suffix(b" ").unwrap_or(&b[..i]);
        let prefix = prefix.strip_suffix(b")").unwrap_or(prefix);
        prefix.len() >= 2 && digits(&prefix[prefix.len() - 2..])
    });
    cell || landline
}

/// Primeira linha com conteúdo, espaços colapsados, para o card na tela.
fn preview(text: &str) -> String {
    const MAX: usize = 160;
    let joined: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut p: String = joined.chars().take(MAX).collect();
    if joined.chars().count() > MAX {
        p.push('…');
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(s: &str) -> bool {
        evaluate(s.as_bytes()).is_some()
    }

    #[test]
    fn accepts_personal_messages() {
        assert!(ok("Oi Maria, tudo bem? Minha senha nova do banco é abc123, não conta pra ninguém. Beijos, João"));
        assert!(ok("Prezado cliente, segue o contato: fulano.silva@gmail.com e telefone (11) 99876-5432 para confirmar o pedido."));
        assert!(ok("Hello Peter, please send me the report by Friday. Thanks a lot, regards, Anna"));
        // Latin-1 (Windows antigo)
        assert!(evaluate(b"Ol\xe1 Jo\xe3o, minha senha do e-mail \xe9 casa123, guarda com voc\xea por favor.").is_some());
    }

    #[test]
    fn rejects_code_markup_and_noise() {
        assert!(!ok("const senha = process.env.SENHA; function login(user) { return auth(user, senha); }"));
        assert!(!ok("<div class=\"email\"><span>senha</span><a href=\"/login\">entrar</a></div> e mais texto"));
        assert!(!ok("This program cannot be run in DOS mode. Copyright (c) Microsoft Corporation. All rights reserved."));
        assert!(!ok("UUUUUUUUUUUUUUUUUUUUUUUUUUUUUUUUUUUUUUUUUUUUUUUUUUUUUUU senha"));
        assert!(!ok("QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVphYmNkZWZnaGlqa2xtbm9wcXJzdHV2d3h5ejAxMjM0NTY3ODk= senha email"));
        // Prosa sem indício pessoal
        assert!(!ok("A reunião de planejamento do trimestre foi adiada para a próxima semana por causa da chuva na região."));
    }

    #[test]
    fn whole_word_matching() {
        assert!(!ok("A banana e a carga do navio chegaram ao porto pela manhã, segundo o relatório oficial da empresa."));
        assert!(has_cpf("cpf 123.456.789-09 ok"));
        assert!(has_phone("liga 99876-5432"));
        assert!(has_phone("fixo (11) 3456-7890"));
        assert!(!has_phone("de 2020-2024"));
        assert!(!has_phone("codigo 1234-5678 sem ddd"));
    }

    #[test]
    fn scanner_splits_runs_and_dedups() {
        let msg = b"Oi Maria, tudo bem? Minha senha nova e abc123. Beijos, Joao";
        let mut data = vec![0u8; 10];
        data.extend_from_slice(msg);
        data.extend_from_slice(&[0, 0, 0xFF, 1]);
        data.extend_from_slice(msg); // duplicado
        data.push(0);
        let mut sc = TextScanner::new();
        let mut out = Vec::new();
        sc.feed(1000, &data[..20], &mut out);
        sc.feed(1020, &data[20..], &mut out);
        sc.finish(&mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].offset, 1010);
        assert!(out[0].preview.starts_with("Oi Maria"));
    }
}
