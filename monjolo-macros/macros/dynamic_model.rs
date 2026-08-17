/** monjolo-macros/dynamic_model.rs

`#[dynamic_model]` — o mais geral dos quatro atributos: DynamicModel não tem uma forma fixa (ao
contrário de Actuator: comando/estado/derivada sempre iguais) — um Reactor oferece 49 chaves, um
Compressor 19, um precisa de outro, nenhum precisa de nada. A macro gera `impl DynamicModel for X`
inteiro (name/evaluate/state_keys) — o usuário nunca escreve essa `impl` — mas `evaluate()` gerado
só chama `self.compute()`, um método INERENTE que o usuário escreve à mão em `impl X { fn
compute(&self) {...} }`, usando os getters/setters gerados (exatamente como já escrevia usando
`self.campo` antes, só que agora `self.campo()`/`self.set_campo(...)`). Mesma divisão de
responsabilidade que `#[actuator(...)]` já usa com `dynamics()` — a macro nunca inventa física,
só a fiação em volta dela.

NOTA (2026-08-17): antes desta revisão, `state_keys()` era escrito à mão (ou esquecido — foi
esquecido em Reactor/Separator/Stripper/Compressor, um bug real: nada dizia ao Integrator que
aquele estado existia, então RK4 nunca avançava a química da planta). Agora `state_keys()` é
gerado automaticamente. Ver `CONTRIBUTING.md` sobre o mesmo bug reportado em
`Composite::state_keys()`.

NOTA (2026-08-17, revisada no mesmo dia): as duas primeiras versões desta revisão usavam
`#[config]+#[offer]` como gatilho de `state_keys()` — e uma delas também auto-ofertava
`"{chave}.derivative"` no MESMO struct dono do valor. As duas decisões foram erradas pelo mesmo
motivo: `#[config]` significa só "isto tem valor inicial vindo do Snapshot", um conceito
diferente de "isto é estado integrável" — usar um pra inferir o outro é implícito e, no caso da
derivada, ativamente errado (quem CALCULA uma derivada nem sempre é quem DECLARA o estado; Reactor
declara `vapor`, mas só Flows tem os quatro subsistemas termodinâmicos ao mesmo tempo pra saber
entrada/saída de cada um — auto-ofertar no dono do valor forçava Reactor a "possuir" uma chave que
semanticamente pertence a Flows). Existe agora um atributo próprio, `#[state]`, ortogonal a
`#[config]`: só ele decide o que entra em `state_keys()`. A oferta de `"{chave}.derivative"`
continua responsabilidade de quem CALCULA, um `#[offer(key = "...derivative")]` comum onde quer
que more o `compute()` certo — ver `flows.rs`, onde moram as derivadas de Reactor/Separator/
Stripper/Compressor, não nos próprios structs desses quatro.

Atributos de campo, todos com duas formas — `key = "..."` (escalar) OU `prefix = "...", components
= [...]` (array, um campo `[f64; N]` vira N sinais individualmente endereçáveis no StateRegistry,
chave = "{prefix}.{component}"; `components.len()` precisa bater com N, checado aqui):

- `#[state]`: marcador puro, sem chave própria (reaproveita a de `#[offer]`, que é obrigatório
  junto). Diz "este sinal participa do vetor que o Integrator avança" — entra em `state_keys()`,
  e o campo perde o setter (só o Integrator escreve neste Proxy, via referência bruta em
  `spawn_plant_thread`; o próprio componente nunca deveria sobrescrever um estado que o RK4 está
  ativamente perturbando em sub-passos). Ortogonal a `#[config]` — um pode existir sem o outro.
- `#[config(...)]`: semeia o valor inicial a partir do Snapshot de config (`config.get(key)`,
  default 0.0 se ausente) — não cria sinal nenhum sozinho, só decide o valor inicial de um campo
  que também tenha `#[offer(...)]`. NÃO implica `#[state]` — um valor pode ter origem configurável
  sem ser estado integrável (ex.: um parâmetro ajustável recalculado toda vez).
- `#[offer(...)]`: publica um slot real no StateRegistry (a metade "offers" de subscribe()).
  Getter sempre gerado; setter também, A MENOS que o campo também tenha `#[state]` (não
  `#[config]` — as duas coisas são independentes agora).
- `#[need(...)]`: depende de uma chave de outro componente (a metade "needs" de subscribe()) —
  aceita as duas formas, igual `#[offer(...)]`. Getter só (nunca setter — não é este componente
  quem publica). Não se combina com `#[config]`/`#[offer]`/`#[state]`.
- Nenhum atributo: campo comum, inicializado via `Default::default()` (ex.: `constants:
  TepConstants`, que já implementa `Default`) — a macro não inventa um jeito de construir tipos
  arbitrários além disso.
*/

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{Fields, Ident, Type};

enum FieldKeySpec {
    Scalar(String),
    Array(String, Vec<String>),
}

impl FieldKeySpec {
    fn keys(&self) -> Vec<String> {
        match self {
            FieldKeySpec::Scalar(key) => vec![key.clone()],
            FieldKeySpec::Array(prefix, components) => {
                components.iter().map(|c| format!("{prefix}.{c}")).collect()
            }
        }
    }
}

enum FieldShape {
    Scalar,
    Array(usize),
}

struct FieldPlan {
    ident: Ident,
    /* Tipo ORIGINAL, sempre preservado — campo comum (Plain) usa isto direto na struct gerada,
    sem exigir f64/[f64; N] nenhum (ex.: `constants: TepConstants`). Só Attributed valida/usa
    FieldShape, porque só esses viram Proxy.
    */
    original_ty: Type,
    shape: FieldShapeKind,
    config: Option<FieldKeySpec>,
    offer: Option<FieldKeySpec>,
    need: Option<FieldKeySpec>,
    /* `#[state]` — marcador puro (sem chave própria, reaproveita a de `#[offer]`): "isto participa
    do vetor que o Integrator avança", ortogonal a `#[config]` ("isto tem valor inicial vindo do
    Snapshot"). Os dois costumam andar juntos (own_state de verdade), mas são conceitos diferentes
    — nem todo `#[config]` é estado integrável, nem todo estado integrável precisa de semente.
    */
    state: bool,
}

enum FieldShapeKind {
    Plain,
    Attributed(FieldShape),
}

pub fn expand(attr: TokenStream, item: TokenStream) -> TokenStream {
    let after = match parse_after_arg(attr) {
        Ok(after) => after,
        Err(err) => return err.to_compile_error().into(),
    };

    let input = syn::parse_macro_input!(item as syn::ItemStruct);
    let struct_name = &input.ident;
    let visibility = &input.vis;

    let named_fields = match &input.fields {
        Fields::Named(named) => &named.named,
        _ => {
            return syn::Error::new_spanned(&input, "#[dynamic_model] só suporta struct com campos nomeados")
                .to_compile_error()
                .into();
        }
    };

    let mut plans = Vec::new();
    for field in named_fields {
        match build_field_plan(field) {
            Ok(plan) => plans.push(plan),
            Err(err) => return err.to_compile_error().into(),
        }
    }

    let mut offer_key_strs: Vec<String> = Vec::new();
    let mut need_key_strs: Vec<String> = Vec::new();
    /* Chaves dos campos "own_state" (`#[config]`+`#[offer]` juntos) — vira `state_keys()` gerado.
    Só a chave do VALOR, nunca a `.derivative` companheira (essa é implementação, não declaração).
    */
    let mut state_key_strs: Vec<String> = Vec::new();
    let mut struct_fields = Vec::new();
    let mut getters = Vec::new();
    let mut setters = Vec::new();
    let mut config_seed_stmts = Vec::new();
    let mut field_inits = Vec::new();

    for plan in &plans {
        let ident = &plan.ident;

        let shape = match &plan.shape {
            FieldShapeKind::Plain => {
                // Campo comum, sem Proxy nenhum — mantém o tipo ORIGINAL, Default::default().
                let ty = &plan.original_ty;
                struct_fields.push(quote! { #ident: #ty });
                field_inits.push(quote! { #ident: ::std::default::Default::default() });
                continue;
            }
            FieldShapeKind::Attributed(shape) => shape,
        };

        let n = match shape {
            FieldShape::Scalar => None,
            FieldShape::Array(n) => Some(*n),
        };

        let proxy_ty = match n {
            None => quote! { ::monjolo::state_registry::Proxy },
            Some(n) => quote! { [::monjolo::state_registry::Proxy; #n] },
        };
        struct_fields.push(quote! { #ident: #proxy_ty });

        if let Some(offer) = &plan.offer {
            let start = offer_key_strs.len();
            let keys = offer.keys();
            let len = keys.len();
            offer_key_strs.extend(keys.clone());

            field_inits.push(field_init_from_slice(ident, "__offered", start, len, n));
            getters.push(getter_tokens(ident, n));

            /* `#[config]` (semear do Snapshot) e `#[state]` (entrar em state_keys()) são
            ORTOGONAIS — cada um checado por si, não um implicando o outro. A maioria dos campos
            "own_state" reais tem os dois juntos, mas nada aqui exige isso.
            */
            if let Some(config) = &plan.config {
                config_seed_stmts.push(config_seed_tokens(config, start, n));
            }

            if plan.state {
                /* NÃO oferece a derivada aqui — quem calcula a derivada de um estado nem sempre é
                quem o declara (Reactor declara `vapor`, mas quem tem os quatro subsistemas ao
                mesmo tempo pra saber entrada/saída é Flows) — dono da OFERTA de
                `"{chave}.derivative"` tem que ser quem de fato vai escrever nela, um
                `#[offer(key = "...derivative")]` comum onde quer que more o `compute()` certo, não
                um campo-irmão automático aqui. Sem setter também: só o Integrator escreve neste
                Proxy (via referência bruta em `spawn_plant_thread`), nunca este componente.
                */
                state_key_strs.extend(keys.iter().cloned());
            } else {
                setters.push(setter_tokens(ident, n));
            }
        } else if let Some(need) = &plan.need {
            let start = need_key_strs.len();
            let keys = need.keys();
            let len = keys.len();
            need_key_strs.extend(keys);

            field_inits.push(field_init_from_slice(ident, "__needed", start, len, n));
            getters.push(getter_tokens(ident, n));
        }
    }

    let offer_refs = quote! { &[#(#offer_key_strs),*] };
    let need_refs = quote! { &[#(#need_key_strs),*] };

    let expanded = quote! {
        #visibility struct #struct_name {
            #(#struct_fields),*
        }

        impl #struct_name {
            #(#getters)*
            #(#setters)*

            /** Gerado por `#[dynamic_model]`: um único `subscribe()` cobre todos os `#[offer(...)]`/
            `#[need(...)]` declarados nos campos, na ordem em que aparecem na struct — mesma
            mecânica que `Reactor::new()`/`Separator::new()`/etc. já faziam à mão (`offer_keys:
            Vec<String>` montado antes, um `subscribe()` só). `#[config(...)]` semeia os campos
            correspondentes logo em seguida, antes de `Self` existir.
            */
            pub fn new(
                registry: &mut ::monjolo::state_registry::StateRegistry,
                config: &::monjolo::snapshot::Snapshot,
            ) -> Self {
                let (__offered, __needed) = registry.subscribe(#offer_refs, #need_refs);
                #(#config_seed_stmts)*

                Self {
                    #(#field_inits),*
                }
            }
        }

        /** Gerado por `#[dynamic_model]` — `evaluate()` só chama `compute()` (método inerente que
        o usuário escreve à mão em `impl #struct_name`, nunca aqui); `state_keys()` vem dos campos
        `#[config]`+`#[offer]` ("own_state"), coletados durante a expansão — impossível declarar
        um sem o outro passar a existir também.
        */
        impl ::monjolo::dynamic_model::DynamicModel for #struct_name {
            fn name(&self) -> &str {
                ::std::stringify!(#struct_name)
            }

            fn evaluate(&self) {
                self.compute();
            }

            fn state_keys(&self) -> ::std::vec::Vec<::std::string::String> {
                ::std::vec![#(#state_key_strs.to_string()),*]
            }
        }

        /* Anúncio escondido pro bootstrap de Simulation — fase (A). `after` (se declarado no
        atributo, `#[dynamic_model(after = ["Outro"])]`) diz a `attach_discovered_components` pra
        ordenar esta struct depois de outra(s), pelo nome — só importa quando um componente desta
        fase depende do resultado de outro NO MESMO tick (ver monjolo::component).
        */
        ::monjolo::inventory::submit! {
            ::monjolo::ComponentDescriptor {
                name: ::std::stringify!(#struct_name),
                kind: ::monjolo::ComponentKind::Dynamic,
                after: &[#(#after),*],
                construct: |registry: &mut ::monjolo::state_registry::StateRegistry, config: &::monjolo::snapshot::Snapshot| {
                    ::std::option::Option::Some(
                        ::std::boxed::Box::new(#struct_name::new(registry, config))
                            as ::std::boxed::Box<dyn ::monjolo::dynamic_model::DynamicModel>,
                    )
                },
            }
        }
    };

    expanded.into()
}

fn getter_tokens(ident: &Ident, n: Option<usize>) -> TokenStream2 {
    match n {
        None => quote! {
            pub fn #ident(&self) -> f64 {
                self.#ident.get()
            }
        },
        Some(n) => {
            let indices = (0..n).map(syn::Index::from);
            quote! {
                pub fn #ident(&self) -> [f64; #n] {
                    [#(self.#ident[#indices].get()),*]
                }
            }
        }
    }
}

fn setter_tokens(ident: &Ident, n: Option<usize>) -> TokenStream2 {
    let setter_name = format_ident!("set_{}", ident);
    match n {
        None => quote! {
            pub fn #setter_name(&self, value: f64) {
                self.#ident.set(value);
            }
        },
        Some(n) => {
            let indices: Vec<syn::Index> = (0..n).map(syn::Index::from).collect();
            quote! {
                pub fn #setter_name(&self, values: [f64; #n]) {
                    #(self.#ident[#indices].set(values[#indices]);)*
                }
            }
        }
    }
}

/* Monta o inicializador de campo (dentro de `Self { ... }`), fatiando `__offered`/`__needed` (Vec
de Proxy) a partir de `start`/`len` — escalar clona um índice, array clona `len` índices num
array literal.
*/
fn field_init_from_slice(ident: &Ident, source: &str, start: usize, len: usize, n: Option<usize>) -> TokenStream2 {
    let source = format_ident!("{}", source);
    match n {
        None => {
            let idx = syn::Index::from(start);
            quote! { #ident: #source[#idx].clone() }
        }
        Some(array_len) => {
            debug_assert_eq!(array_len, len);
            let indices = (start..start + len).map(syn::Index::from);
            quote! { #ident: [#(#source[#indices].clone()),*] }
        }
    }
}

/* Semeia o(s) Proxy(s) recém-fatiado(s) de `__offered` com o valor de config correspondente —
roda ANTES de `Self` existir, direto no slice bruto (`__offered[i].set(...)`), já que o valor é o
mesmo em qualquer clone do mesmo Proxy.
*/
fn config_seed_tokens(config: &FieldKeySpec, offer_start: usize, n: Option<usize>) -> TokenStream2 {
    let keys = config.keys();
    match n {
        None => {
            let idx = syn::Index::from(offer_start);
            let key = &keys[0];
            quote! {
                __offered[#idx].set(config.get(#key).unwrap_or(0.0));
            }
        }
        Some(array_len) => {
            let stmts = (0..array_len).map(|i| {
                let idx = syn::Index::from(offer_start + i);
                let key = &keys[i];
                quote! { __offered[#idx].set(config.get(#key).unwrap_or(0.0)); }
            });
            quote! { #(#stmts)* }
        }
    }
}

fn build_field_plan(field: &syn::Field) -> syn::Result<FieldPlan> {
    let ident = field.ident.clone().expect("Fields::Named sempre tem ident");

    let has_relevant_attr = field.attrs.iter().any(|attr| {
        attr.path().is_ident("config")
            || attr.path().is_ident("offer")
            || attr.path().is_ident("need")
            || attr.path().is_ident("state")
    });

    if !has_relevant_attr {
        // Campo comum, sem #[config]/#[offer]/#[need]/#[state] — tipo original preservado, sem
        // exigir f64/[f64; N]; Default::default() na hora de construir (ex.: `constants: TepConstants`).
        return Ok(FieldPlan {
            ident,
            original_ty: field.ty.clone(),
            shape: FieldShapeKind::Plain,
            config: None,
            offer: None,
            need: None,
            state: false,
        });
    }

    let shape = field_shape(&field.ty)?;
    let expected_len = match shape {
        FieldShape::Scalar => 1,
        FieldShape::Array(n) => n,
    };

    let mut config = None;
    let mut offer = None;
    let mut need = None;
    let mut state = false;

    for attr in &field.attrs {
        if attr.path().is_ident("state") {
            if state {
                return Err(syn::Error::new_spanned(attr, "atributo repetido no mesmo campo"));
            }
            state = true;
            continue;
        }

        let target = if attr.path().is_ident("config") {
            &mut config
        } else if attr.path().is_ident("offer") {
            &mut offer
        } else if attr.path().is_ident("need") {
            &mut need
        } else {
            continue;
        };

        if target.is_some() {
            return Err(syn::Error::new_spanned(attr, "atributo repetido no mesmo campo"));
        }

        let spec = parse_key_spec(attr)?;
        let actual_len = spec.keys().len();
        if actual_len != expected_len {
            return Err(syn::Error::new_spanned(
                attr,
                format!(
                    "campo declarado como {} valor(es) mas o atributo descreve {} chave(s) — components.len() precisa bater com o tamanho do array",
                    expected_len, actual_len
                ),
            ));
        }

        *target = Some(spec);
    }

    if need.is_some() && (config.is_some() || offer.is_some() || state) {
        return Err(syn::Error::new_spanned(
            &field.ty,
            "#[need] não se combina com #[config]/#[offer]/#[state] no mesmo campo — precisa vem de outro componente, não é publicado por este",
        ));
    }
    if config.is_some() && offer.is_none() {
        return Err(syn::Error::new_spanned(
            &field.ty,
            "#[config] sozinho não cria sinal nenhum — combine com #[offer(...)] (config só decide o valor inicial de um sinal publicado)",
        ));
    }
    if state && offer.is_none() {
        return Err(syn::Error::new_spanned(
            &field.ty,
            "#[state] sozinho não cria sinal nenhum — combine com #[offer(...)] (state marca um sinal publicado como parte do vetor de integração)",
        ));
    }

    Ok(FieldPlan {
        ident,
        original_ty: field.ty.clone(),
        shape: FieldShapeKind::Attributed(shape),
        config,
        offer,
        need,
        state,
    })
}

fn field_shape(ty: &Type) -> syn::Result<FieldShape> {
    match ty {
        Type::Path(path) if path.path.is_ident("f64") => Ok(FieldShape::Scalar),
        Type::Array(array) => {
            let elem_is_f64 = matches!(&*array.elem, Type::Path(p) if p.path.is_ident("f64"));
            if !elem_is_f64 {
                return Err(syn::Error::new_spanned(ty, "#[dynamic_model] só suporta campos `f64` ou `[f64; N]`"));
            }
            match &array.len {
                syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Int(n), .. }) => {
                    Ok(FieldShape::Array(n.base10_parse::<usize>()?))
                }
                other => Err(syn::Error::new_spanned(other, "tamanho do array precisa ser um inteiro literal")),
            }
        }
        other => Err(syn::Error::new_spanned(other, "#[dynamic_model] só suporta campos `f64` ou `[f64; N]`")),
    }
}

/* Parseia o argumento do PRÓPRIO #[dynamic_model(...)] (não de um campo) — só aceita `after =
[...]`, opcional; vazio (`vec![]`) se o atributo não recebeu nada (`#[dynamic_model]` puro).
*/
fn parse_after_arg(attr: TokenStream) -> syn::Result<Vec<String>> {
    if attr.is_empty() {
        return Ok(Vec::new());
    }

    let pairs = {
        use syn::parse::Parser;
        syn::punctuated::Punctuated::<syn::MetaNameValue, syn::Token![,]>::parse_terminated.parse(attr)?
    };

    let mut after = None;
    for pair in &pairs {
        if pair.path.is_ident("after") {
            after = Some(expect_str_array(&pair.value)?);
        } else {
            return Err(syn::Error::new_spanned(&pair.path, "esperado `after`"));
        }
    }

    Ok(after.unwrap_or_default())
}

fn parse_key_spec(attr: &syn::Attribute) -> syn::Result<FieldKeySpec> {
    let pairs = attr.parse_args_with(
        syn::punctuated::Punctuated::<syn::MetaNameValue, syn::Token![,]>::parse_terminated,
    )?;

    let mut key = None;
    let mut prefix = None;
    let mut components = None;

    for pair in &pairs {
        if pair.path.is_ident("key") {
            key = Some(expect_str_lit(&pair.value)?);
        } else if pair.path.is_ident("prefix") {
            prefix = Some(expect_str_lit(&pair.value)?);
        } else if pair.path.is_ident("components") {
            components = Some(expect_str_array(&pair.value)?);
        } else {
            return Err(syn::Error::new_spanned(&pair.path, "esperado `key`, `prefix` ou `components`"));
        }
    }

    match (key, prefix, components) {
        (Some(key), None, None) => Ok(FieldKeySpec::Scalar(key)),
        (None, Some(prefix), Some(components)) => Ok(FieldKeySpec::Array(prefix, components)),
        (None, None, None) => {
            Err(syn::Error::new_spanned(attr, "esperado `key = \"...\"` ou `prefix = \"...\", components = [...]`"))
        }
        _ => Err(syn::Error::new_spanned(
            attr,
            "use `key = \"...\"` (escalar) OU `prefix`+`components` (array), não uma mistura",
        )),
    }
}

fn expect_str_lit(expr: &syn::Expr) -> syn::Result<String> {
    match expr {
        syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) => Ok(s.value()),
        other => Err(syn::Error::new_spanned(other, "esperada uma string literal")),
    }
}

fn expect_str_array(expr: &syn::Expr) -> syn::Result<Vec<String>> {
    match expr {
        syn::Expr::Array(array) => array.elems.iter().map(expect_str_lit).collect(),
        other => Err(syn::Error::new_spanned(other, "esperado um array de strings, ex.: [\"a\", \"b\"]")),
    }
}
