# NOTICE

O `monjolo` é licenciado sob a [Apache License, Version 2.0](LICENSE). Copyright 2026 Green-Cinnamon-Labs.

## Dependências de terceiros

Este projeto depende de bibliotecas de terceiros (ver `Cargo.toml`/`Cargo.lock`), cada uma sob sua própria licença — o Apache-2.0 do `monjolo` cobre apenas o código deste repositório, não o código dessas dependências. Este arquivo não reproduz as licenças de terceiros; elas são respeitadas nos termos em que cada projeto upstream as publica.

Se alguma dependência (direta ou transitiva) estiver sob uma licença copyleft (ex.: MPL, LGPL), verifique os termos específicos antes de redistribuir binários do `monjolo` — não assuma compatibilidade automática só porque o código deste repositório é Apache-2.0.

Para auditar as licenças de todo o grafo de dependências, use:

```bash
cargo install cargo-deny
cargo deny check licenses
```
