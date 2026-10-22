# Rscover

Aplicativo desktop para recuperação de arquivos por assinaturas de bytes em dispositivos de armazenamento. O Rscover encontra e exporta JPEG, PNG, PDF, ZIP, texto e MP4, com acompanhamento de progresso e visualização dos arquivos localizados.

> Atenção: recuperar arquivos pode sobrescrever dados se a pasta de saída estiver no mesmo dispositivo analisado. Escolha sempre outro disco ou partição como destino.

## Requisitos

- [Node.js](https://nodejs.org/) (LTS)
- [Rust](https://www.rust-lang.org/tools/install) com Cargo
- Dependências de compilação do Tauri para o seu sistema operacional

## Desenvolvimento

```bash
npm install
npm run dev
```

No Windows, você também pode executar `./dev.ps1` no PowerShell.

## Build de produção

```bash
npm run build
```

Os instaladores e binários são gerados em `src-tauri/target/release/bundle/` e não são versionados.

## Estrutura

- `src/`: interface HTML, CSS e JavaScript
- `src-tauri/src/`: código Rust e comandos nativos
- `src-tauri/icons/`: ícones do aplicativo

## Publicação no GitHub

```bash
git add .
git commit -m "Initial commit"
git branch -M main
git remote add origin https://github.com/SEU-USUARIO/rscover.git
git push -u origin main
```

Este repositório não inclui uma licença. Antes de torná-lo público, defina a licença e as regras de uso que deseja aplicar.
