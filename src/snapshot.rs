// snapshot.rs
//
// Carrega um TOML qualquer num mapa achatado `"caminho.pontuado" -> f64` —
// não sabe nada de TEP, de Reactor, de qual chave significa o quê. Cada
// tabela aninhada do TOML vira um prefixo de chave (`[state.reactor_vapor]
// A = 1.0` vira `"state.reactor_vapor.A" -> 1.0`); valores não-numéricos
// (string, bool, array, o `[meta]` de descrição etc.) são ignorados — não
// são estado.
//
// Substitui a ideia antiga de um struct rígido por seção (tep-plant tinha
// um `InitialState`/`StateSections` batendo campo a campo com o TOML) — em
// vez disso, quem quer inicializar um componente (ex.: `Reactor::new()`)
// recebe um `&Snapshot` e busca só as chaves que interessam pra ele, do
// mesmo jeito que já faz com `StateRegistry` por nome. Um "snapshot" nesse
// sentido é literalmente o que `StateRegistry::snapshot()` já produz em
// memória — este módulo só resolve o lado de ler isso de um arquivo.

use std::collections::HashMap;

pub struct Snapshot {
    values: HashMap<String, f64>,
}

impl Snapshot {
    /// Carrega e achata um arquivo TOML. `Err` se o arquivo não existir ou
    /// não for TOML válido — não valida nada sobre o *conteúdo* (isso é
    /// problema de quem consome, via `get()`).
    pub fn from_file(path: &str) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Erro lendo arquivo '{path}': {e}"))?;
        let root: toml::Value =
            toml::from_str(&content).map_err(|e| format!("Erro parseando TOML '{path}': {e}"))?;

        let mut values = HashMap::new();
        flatten(&root, String::new(), &mut values);
        Ok(Self { values })
    }

    /// Constrói direto de pares já resolvidos — útil pra teste, sem
    /// precisar de um arquivo real no disco.
    pub fn from_pairs(pairs: &[(&str, f64)]) -> Self {
        Self {
            values: pairs.iter().map(|&(k, v)| (k.to_string(), v)).collect(),
        }
    }

    /// Lê o valor de uma chave achatada (ex.: `"state.reactor_vapor.A"`).
    /// `None` se a chave não existir ou não for numérica no TOML original.
    pub fn get(&self, key: &str) -> Option<f64> {
        self.values.get(key).copied()
    }
}

fn flatten(value: &toml::Value, prefix: String, out: &mut HashMap<String, f64>) {
    match value {
        toml::Value::Table(table) => {
            for (key, nested) in table {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten(nested, path, out);
            }
        }
        toml::Value::Float(f) => {
            out.insert(prefix, *f);
        }
        toml::Value::Integer(i) => {
            out.insert(prefix, *i as f64);
        }
        // String, Boolean, Array, Datetime: não são estado numérico, ignorados.
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattens_nested_tables_into_dotted_keys() {
        let toml_content = "\
[meta]
description = \"ignorado, não é numérico\"

[state.reactor_vapor]
A = 10.5
B = 2

[state.reactor]
energy = 3.25
";
        let root: toml::Value = toml::from_str(toml_content).unwrap();
        let mut values = HashMap::new();
        flatten(&root, String::new(), &mut values);

        assert_eq!(values.get("state.reactor_vapor.A"), Some(&10.5));
        assert_eq!(values.get("state.reactor_vapor.B"), Some(&2.0));
        assert_eq!(values.get("state.reactor.energy"), Some(&3.25));
        assert_eq!(values.get("meta.description"), None);
    }

    #[test]
    fn from_pairs_builds_a_queryable_snapshot() {
        let snapshot = Snapshot::from_pairs(&[("a", 1.0), ("b", 2.0)]);
        assert_eq!(snapshot.get("a"), Some(1.0));
        assert_eq!(snapshot.get("missing"), None);
    }
}
