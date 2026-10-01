/** monjolo-macros/tasks.rs

`#[monjolo::tasks]`, aplicado ao `impl X { ... }` INTEIRO de uma unidade `#[dynamic_model]` — não a
métodos individuais. Motivo: um atributo aplicado a um item DENTRO de um `impl` só pode ser
substituído por outros itens ASSOCIADOS (mais métodos) — a gramática de Rust não permite que a
expansão de um método solto produza `struct`/`impl`/`inventory::submit!` avulsos (só é válido onde
o item ORIGINAL já estava no nível de módulo). Aplicado ao `impl` inteiro, a macro já está nessa
posição e pode emitir, por método marcado, um struct-tarefa + `impl DynamicModel` +
`inventory::submit!` novos, ao lado do próprio `impl` (reescrito, com os métodos marcados
renomeados).

Cada método com `#[need(...)]`/`#[offer(...)]` empilhados vira uma tarefa: os PARÂMETROS (na ordem
declarada) são os `needs` — 1 `#[need(...)]` por parâmetro, mesma gramática `key = "..."` /
`prefix = "...", components = [...]` de `#[dynamic_model]` (reaproveitada de `dynamic_model.rs`,
não duplicada). O RETORNO é o(s) `offer(s)`: um `#[offer(...)]` só = tipo de retorno direto; 2+ =
tupla, na mesma ordem dos atributos. Métodos SEM `#[need]`/`#[offer]` ficam intocados, comuns, fora
do scheduler — únicos que podem ler `self.campo()` livremente por dentro do `impl` reescrito.

Cada tarefa gerada guarda `Rc<X>` (a instância compartilhada da unidade, buscada via
`registry.instance::<X>(...)` — ver `state_registry.rs`) + um `Proxy`/`[Proxy; N]` por need/offer,
resolvidos UMA vez em `construct()` (bootstrap), nunca por tick. `after` é auto-injetado com o nome
da própria unidade (`stringify!(X)`) — garante que ela já foi construída (e já chamou
`offer_instance`) antes de qualquer tarefa seguir em frente; ver `component::sort_phase_a`.
*/
use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::spanned::Spanned;
use syn::visit_mut::{self, VisitMut};
use syn::{ImplItem, ImplItemFn, ItemImpl, Type};

use crate::dynamic_model::{field_init_from_slice, parse_key_spec, FieldKeySpec};

pub fn expand(attr: TokenStream, item: TokenStream) -> TokenStream {
    let task_args = match parse_task_args(attr) {
        Ok(args) => args,
        Err(err) => return err.to_compile_error().into(),
    };

    let input = syn::parse_macro_input!(item as ItemImpl);

    if input.trait_.is_some() {
        return syn::Error::new_spanned(&input, "#[monjolo::tasks] só suporta impl inerente (sem trait)")
            .to_compile_error()
            .into();
    }

    let self_ty = input.self_ty.as_ref();
    let owner_ident = match self_ty_ident(self_ty) {
        Ok(ident) => ident,
        Err(err) => return err.to_compile_error().into(),
    };
    let impl_attrs = &input.attrs;
    let generics = &input.generics;

    let mut kept_items: Vec<TokenStream2> = Vec::new();
    let mut task_defs: Vec<TokenStream2> = Vec::new();

    for item in input.items {
        match item {
            ImplItem::Fn(method) => {
                let has_marker = method.attrs.iter().any(|a| {
                    a.path().is_ident("need") || a.path().is_ident("offer") || a.path().is_ident("disturbance")
                });
                let is_signal_task = method.attrs.iter().any(|a| a.path().is_ident("task"));

                if is_signal_task {
                    if has_marker {
                        return syn::Error::new_spanned(
                            &method.sig,
                            "#[task] não se combina com #[need]/#[offer]/#[disturbance] — num #[task] \
                            o que o método lê/escreve vem de `sig::campo` no corpo, não de atributos",
                        )
                        .to_compile_error()
                        .into();
                    }
                    match build_signal_task(self_ty, owner_ident, &method, &task_args) {
                        Ok((impl_method, task_def)) => {
                            kept_items.push(impl_method);
                            task_defs.push(task_def);
                        }
                        Err(err) => return err.to_compile_error().into(),
                    }
                    continue;
                }

                if !has_marker {
                    kept_items.push(quote! { #method });
                    continue;
                }

                match build_task(self_ty, owner_ident, &method) {
                    Ok((impl_method, task_def)) => {
                        kept_items.push(impl_method);
                        task_defs.push(task_def);
                    }
                    Err(err) => return err.to_compile_error().into(),
                }
            }
            other => kept_items.push(quote! { #other }),
        }
    }

    let expanded = quote! {
        #(#impl_attrs)*
        impl #generics #self_ty {
            #(#kept_items)*
        }

        #(#task_defs)*
    };

    expanded.into()
}

/** Argumentos opcionais de `#[monjolo::tasks(species = CAMINHO, len = N)]`: só são necessários se
algum `#[task]` do `impl` usa um sinal-mistura (`need::nome::<Fase>` / `offer::nome::<Fase>`), pra
macro saber quantas componentes ele tem (`len`) e qual catálogo de nomes de espécie a `Mixture`
montada carrega (`species`). Sem isso, só sinais escalares (`need::nome`/`offer::nome`) funcionam.
*/
struct TaskArgs {
    species: Option<syn::Expr>,
    len: Option<usize>,
}

fn parse_task_args(attr: TokenStream) -> syn::Result<TaskArgs> {
    let mut species = None;
    let mut len = None;
    if attr.is_empty() {
        return Ok(TaskArgs { species, len });
    }

    use syn::parse::Parser;
    let metas = syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated.parse(attr)?;
    for meta in metas {
        match meta {
            syn::Meta::NameValue(pair) if pair.path.is_ident("species") => species = Some(pair.value),
            syn::Meta::NameValue(pair) if pair.path.is_ident("len") => match &pair.value {
                syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Int(n), .. }) => len = Some(n.base10_parse::<usize>()?),
                other => return Err(syn::Error::new_spanned(other, "`len` precisa ser um inteiro literal")),
            },
            other => {
                return Err(syn::Error::new_spanned(
                    other,
                    "#[monjolo::tasks] só aceita `species = CAMINHO, len = N` (e só se algum #[task] usa \
                    um sinal-mistura); `disturbance = ...` é um atributo de MÉTODO agora \
                    (#[disturbance(key = \"...\")])",
                ))
            }
        }
    }
    Ok(TaskArgs { species, len })
}

/** Um sinal usado no corpo de um `#[task]`: `need::a__b` (lido) ou `offer::a__b = valor` (escrito).
O nome do identificador É a chave, com `__` virando `.` (`reactor__temperature` →
`"reactor.temperature"`). Com `::<Mixture>` no fim (`need::a__b::<Mixture>`), o
sinal é uma MISTURA: uma `Mixture` de `len` componentes, publicada como `len` chaves (`a.b.a`,
`a.b.b`, ...) e lida/escrita como um valor só — ninguém escreve array nenhum.
*/
struct SigUse {
    field: syn::Ident,
    keys: Vec<String>,
    /* `false` = escalar; `true` = mistura (`::<Mixture>`). */
    is_mixture: bool,
}

struct SignalRewriter<'a> {
    args: &'a TaskArgs,
    needs: Vec<(syn::Ident, SigUse)>,
    offers: Vec<(syn::Ident, SigUse)>,
    error: Option<syn::Error>,
}

/* `need::nome` / `offer::nome` (opcionalmente `::<Mixture>` no último segmento) → (é_offer, nome, tipo). */
fn sig_path(expr: &syn::Expr) -> Option<(bool, syn::Ident, Option<syn::Ident>)> {
    let syn::Expr::Path(path_expr) = expr else { return None };
    if path_expr.qself.is_some() || path_expr.path.leading_colon.is_some() || path_expr.path.segments.len() != 2 {
        return None;
    }
    let first = &path_expr.path.segments[0];
    let second = &path_expr.path.segments[1];
    if !first.arguments.is_none() {
        return None;
    }
    let is_offer = if first.ident == "offer" {
        true
    } else if first.ident == "need" {
        false
    } else {
        return None;
    };

    let phase = match &second.arguments {
        syn::PathArguments::None => None,
        syn::PathArguments::AngleBracketed(generic) if generic.args.len() == 1 => match generic.args.first() {
            Some(syn::GenericArgument::Type(syn::Type::Path(type_path))) if type_path.path.segments.len() == 1 => {
                Some(type_path.path.segments[0].ident.clone())
            }
            _ => return None,
        },
        _ => return None,
    };
    Some((is_offer, second.ident.clone(), phase))
}

fn is_compound_assign(op: &syn::BinOp) -> bool {
    matches!(
        op,
        syn::BinOp::AddAssign(_)
            | syn::BinOp::SubAssign(_)
            | syn::BinOp::MulAssign(_)
            | syn::BinOp::DivAssign(_)
            | syn::BinOp::RemAssign(_)
    )
}

impl SignalRewriter<'_> {
    fn fail(&mut self, span: proc_macro2::Span, message: &str) {
        if self.error.is_none() {
            self.error = Some(syn::Error::new(span, message));
        }
    }

    /* Registra (ou reencontra) um sinal; devolve `None` (e guarda o erro) se algo não bate. */
    fn register(&mut self, is_offer: bool, name: &syn::Ident, phase: Option<&syn::Ident>) -> Option<usize> {
        let span = name.span();
        let is_mixture = match phase {
            None => false,
            Some(ident) => {
                if ident != "Mixture" {
                    self.fail(ident.span(), "o único tipo aceito depois de `::<>` é `Mixture`");
                    return None;
                }
                true
            }
        };

        let other_list = if is_offer { &self.needs } else { &self.offers };
        if other_list.iter().any(|(existing, _)| existing == name) {
            self.fail(
                span,
                &format!(
                    "`{name}` é lido (`need::`) E escrito (`offer::`) no mesmo #[task] — leia o valor numa \
                    variável local antes, ou separe em duas tarefas"
                ),
            );
            return None;
        }

        let list = if is_offer { &self.offers } else { &self.needs };
        if let Some(position) = list.iter().position(|(existing, _)| existing == name) {
            if list[position].1.is_mixture != is_mixture {
                self.fail(span, &format!("`{name}` é usado ora como escalar, ora como mistura"));
                return None;
            }
            return Some(position);
        }

        let base_key = name.to_string().replace("__", ".");
        let keys = if is_mixture {
            let (Some(len), Some(_)) = (self.args.len, &self.args.species) else {
                self.fail(
                    span,
                    "sinal-mistura precisa de `#[monjolo::tasks(species = CAMINHO, len = N)]` no `impl`",
                );
                return None;
            };
            (0..len).map(|i| format!("{base_key}.{}", (b'a' + i as u8) as char)).collect()
        } else {
            vec![base_key]
        };

        let prefix = if is_offer { "__offer_" } else { "__need_" };
        let field = format_ident!("{}{}", prefix, name);
        let entry = (name.clone(), SigUse { field, keys, is_mixture });
        let list = if is_offer { &mut self.offers } else { &mut self.needs };
        list.push(entry);
        Some(list.len() - 1)
    }

    fn read_expr(&self, sig: &SigUse) -> syn::Expr {
        let field = &sig.field;
        if !sig.is_mixture {
            syn::parse_quote!(__sig.#field.get())
        } else {
            let len = self.args.len.expect("checado em register()");
            let species = self.args.species.as_ref().expect("checado em register()");
            syn::parse_quote!(
                ::monjolo::chemistry::Mixture::<#len>::new(
                    ::std::array::from_fn(|__i| __sig.#field[__i].get()),
                    &#species,
                )
            )
        }
    }

    fn write_expr(&self, sig: &SigUse, value: &syn::Expr) -> syn::Expr {
        let field = &sig.field;
        if !sig.is_mixture {
            syn::parse_quote!(__sig.#field.set(#value))
        } else {
            let len = self.args.len.expect("checado em register()");
            syn::parse_quote!({
                let __mixture = #value;
                for __i in 0..#len {
                    __sig.#field[__i].set(__mixture.component(__i));
                }
            })
        }
    }
}

impl VisitMut for SignalRewriter<'_> {
    fn visit_expr_mut(&mut self, expr: &mut syn::Expr) {
        let replacement: Option<syn::Expr> = match expr {
            syn::Expr::Assign(assign) => match sig_path(&assign.left) {
                Some((true, name, phase)) => {
                    self.visit_expr_mut(&mut assign.right);
                    match self.register(true, &name, phase.as_ref()) {
                        Some(position) => {
                            let value = assign.right.clone();
                            let sig = &self.offers[position].1;
                            Some(self.write_expr(sig, &value))
                        }
                        None => None,
                    }
                }
                Some((false, name, _)) => {
                    self.fail(name.span(), "`need::` só se lê — pra escrever um sinal use `offer::nome = valor;`");
                    None
                }
                None => None,
            },
            syn::Expr::Binary(binary) if is_compound_assign(&binary.op) => {
                if sig_path(&binary.left).is_some() {
                    self.fail(
                        binary.left.span(),
                        "`offer::nome += ...` lê E escreve o mesmo sinal — calcule numa variável local e \
                        escreva `offer::nome = valor;`",
                    );
                }
                None
            }
            syn::Expr::Path(_) => match sig_path(expr) {
                Some((false, name, phase)) => match self.register(false, &name, phase.as_ref()) {
                    Some(position) => Some(self.read_expr(&self.needs[position].1)),
                    None => None,
                },
                Some((true, name, _)) => {
                    self.fail(name.span(), "`offer::` só se escreve (`offer::nome = valor;`) — pra ler use `need::nome`");
                    None
                }
                None => None,
            },
            _ => None,
        };

        match replacement {
            Some(new_expr) => *expr = new_expr,
            None => visit_mut::visit_expr_mut(self, expr),
        }
    }
}

/** `#[task]` num método SEM parâmetros (além de `&self`) e SEM retorno: o corpo lê sinais com
`need::nome` e escreve com `offer::nome = valor;` (ver `SignalRewriter`) — nenhum atributo, nenhum
campo de struct. A tarefa gerada guarda um `Proxy` por sinal usado, `needs`/`offers` (e portanto a
ordem de execução) saem da varredura, e o método reescrito recebe esses proxies como um parâmetro
extra (`__sig`) — a macro é dona de tudo, do registro ao acesso, então nem precisa enxergar o struct.
*/
fn build_signal_task(
    self_ty: &Type,
    owner_ident: &syn::Ident,
    method: &ImplItemFn,
    args: &TaskArgs,
) -> syn::Result<(TokenStream2, TokenStream2)> {
    let method_name = &method.sig.ident;
    let impl_name = format_ident!("__{}_impl", method_name);
    let task_struct_name = format_ident!("__{}_{}_Task", owner_ident, method_name);

    let extra_params = method.sig.inputs.iter().filter(|arg| !matches!(arg, syn::FnArg::Receiver(_))).count();
    if extra_params != 0 || !matches!(method.sig.output, syn::ReturnType::Default) {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "#[task] não aceita parâmetros nem retorno — leia sinais com `need::nome` e escreva com \
            `offer::nome = valor;` no corpo",
        ));
    }

    let mut impl_method = method.clone();
    impl_method.attrs.retain(|a| !a.path().is_ident("task"));
    impl_method.sig.ident = impl_name.clone();
    impl_method.sig.inputs.push(syn::parse_quote!(__sig: &#task_struct_name));

    let mut rewriter = SignalRewriter {
        args,
        needs: Vec::new(),
        offers: Vec::new(),
        error: None,
    };
    rewriter.visit_block_mut(&mut impl_method.block);
    if let Some(err) = rewriter.error {
        return Err(err);
    }

    if rewriter.offers.is_empty() {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "#[task] sem nenhum `offer::nome = ...` — sem escrever sinal nenhum, esta tarefa não publicaria nada",
        ));
    }

    let mut need_keys: Vec<String> = Vec::new();
    let mut offer_keys: Vec<String> = Vec::new();
    let mut proxy_fields: Vec<TokenStream2> = Vec::new();
    let mut field_inits: Vec<TokenStream2> = Vec::new();

    for (uses, keys, source) in [
        (&rewriter.needs, &mut need_keys, "__needed"),
        (&rewriter.offers, &mut offer_keys, "__offered"),
    ] {
        for (_, sig) in uses {
            let start = keys.len();
            keys.extend(sig.keys.iter().cloned());
            let field = &sig.field;
            if !sig.is_mixture {
                proxy_fields.push(quote! { #field: ::monjolo::state_registry::Proxy });
                field_inits.push(field_init_from_slice(field, source, start, 1, None));
            } else {
                let len = sig.keys.len();
                proxy_fields.push(quote! { #field: [::monjolo::state_registry::Proxy; #len] });
                field_inits.push(field_init_from_slice(field, source, start, len, Some(len)));
            }
        }
    }

    let descriptor_name: String = format!("{}::{}", owner_ident, method_name);
    let need_refs = quote! { &[#(#need_keys),*] };
    let offer_refs = quote! { &[#(#offer_keys),*] };

    let task_def = quote! {
        #[allow(non_camel_case_types, non_snake_case)]
        struct #task_struct_name {
            __owner: ::std::rc::Rc<#self_ty>,
            #(#proxy_fields,)*
        }

        impl ::monjolo::dynamic_model::DynamicModel for #task_struct_name {
            fn name(&self) -> &str {
                #descriptor_name
            }

            fn evaluate(&self) {
                self.__owner.#impl_name(self);
            }
        }

        ::monjolo::inventory::submit! {
            ::monjolo::ComponentDescriptor {
                name: #descriptor_name,
                kind: ::monjolo::ComponentKind::Dynamic,
                after: &[::std::stringify!(#self_ty)],
                needs: #need_refs,
                offers: #offer_refs,
                construct: |registry: &mut ::monjolo::state_registry::StateRegistry, _config: &::monjolo::snapshot::Snapshot| {
                    let __owner = registry.instance::<#self_ty>(::std::stringify!(#self_ty)).unwrap_or_else(|| {
                        ::std::panic!(
                            "'{}' deveria já ter sido construída (offer_instance) antes de suas \
                            tarefas — `after` deveria garantir isso",
                            ::std::stringify!(#self_ty),
                        )
                    });
                    #[allow(unused_variables)]
                    let (__offered, __needed) = registry.subscribe(#offer_refs, #need_refs);
                    ::std::option::Option::Some(::std::boxed::Box::new(#task_struct_name {
                        __owner,
                        #(#field_inits,)*
                    }) as ::std::boxed::Box<dyn ::monjolo::dynamic_model::DynamicModel>)
                },
            }
        }
    };

    Ok((quote! { #impl_method }, task_def))
}

/** `#[disturbance(key = "chave")]`, empilhado junto de `#[need]`/`#[offer]` num MÉTODO (não no
`impl` inteiro — cada método tem sua própria chave, "duas tarefas no mesmo `impl` compartilhando o
comando liga/desliga" seria um bug, não um recurso) — marca essa tarefa como `ComponentKind::
Disturbance` em vez de `Dynamic` (MESMO grafo de `needs`/`offers`, MESMA fase (A), só rótulo de
identidade diferente pra diagnóstico) E faz o próprio método virar o comando externo liga/desliga do
distúrbio: a tarefa gerada TAMBÉM implementa `Actuator` (`write()` seta um `Proxy` escondido,
catalogado sob `chave` via `offer_actuator()` — mesma exposição OPC-UA automática de qualquer outro
atuador) e o valor atual desse comando chega ao método marcado como um parâmetro A MAIS, sempre o
PRIMEIRO, na frente de qualquer `#[need(...)]` declarado — sem o usuário escrever `#[need(key =
"chave")]` à mão nem manter um `inventory::submit!` separado só pra essa flag.

MECANISMO ANTIGO — mantido por compatibilidade, mas não é mais o caminho recomendado pra um
distúrbio por interceptação: ver `#[monjolo::disturbance(...)]` em `lib.rs` (struct + getters, no
mesmo padrão de `#[actuator]`/`#[sensor]`/`#[controller]`), usado por `tep-plant/src/disturbance/
idv1.rs`.
*/
fn parse_disturbance_attr(attr: &syn::Attribute) -> syn::Result<String> {
    let meta: syn::MetaNameValue = attr.parse_args()?;
    if !meta.path.is_ident("key") {
        return Err(syn::Error::new_spanned(&meta.path, "esperado `key = \"chave\"`"));
    }
    crate::dynamic_model::expect_str_lit(&meta.value)
}

fn self_ty_ident(self_ty: &Type) -> syn::Result<&syn::Ident> {
    match self_ty {
        Type::Path(type_path) => type_path
            .path
            .segments
            .last()
            .map(|segment| &segment.ident)
            .ok_or_else(|| {
                syn::Error::new_spanned(self_ty, "#[monjolo::tasks] precisa de um tipo simples (ex.: `impl Reactor`)")
            }),
        _ => Err(syn::Error::new_spanned(
            self_ty,
            "#[monjolo::tasks] precisa de um tipo simples (ex.: `impl Reactor`)",
        )),
    }
}

/* Constrói UMA tarefa a partir de um método marcado: devolve (a) o método reescrito — mesmo corpo,
renomeado `__{nome}_impl`, `#[need]`/`#[offer]` removidos — pra ficar dentro do `impl X` reescrito;
(b) a definição completa da tarefa (struct + `impl DynamicModel` + `inventory::submit!`), pra ficar
ao lado, no nível de módulo.
*/
fn build_task(
    self_ty: &Type,
    owner_ident: &syn::Ident,
    method: &ImplItemFn,
) -> syn::Result<(TokenStream2, TokenStream2)> {
    let method_name = &method.sig.ident;
    let impl_name = format_ident!("__{}_impl", method_name);

    let mut need_specs: Vec<FieldKeySpec> = Vec::new();
    let mut offer_specs: Vec<FieldKeySpec> = Vec::new();
    let mut disturbance_key: Option<String> = None;
    /* Atributos que não são `#[need]`/`#[offer]`/`#[disturbance]` (doc comments, `#[allow(...)]`,
    etc.) não são erro — `impl_method.attrs.retain(...)` abaixo já os preserva no método renomeado,
    intocados. Só esses três são consumidos aqui.
    */
    for attr in &method.attrs {
        if attr.path().is_ident("need") {
            need_specs.push(parse_key_spec(attr)?);
        } else if attr.path().is_ident("offer") {
            offer_specs.push(parse_key_spec(attr)?);
        } else if attr.path().is_ident("disturbance") {
            if disturbance_key.is_some() {
                return Err(syn::Error::new_spanned(attr, "atributo `disturbance` repetido no mesmo método"));
            }
            disturbance_key = Some(parse_disturbance_attr(attr)?);
        }
    }
    let disturbance_key = disturbance_key.as_deref();
    let kind = if disturbance_key.is_some() {
        quote! { ::monjolo::ComponentKind::Disturbance }
    } else {
        quote! { ::monjolo::ComponentKind::Dynamic }
    };

    if offer_specs.is_empty() {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "método marcado com #[need]/#[offer] precisa de pelo menos um #[offer(...)] — sem \
            offer nenhum, esta tarefa não publicaria nada",
        ));
    }

    let params: Vec<&syn::PatType> = method
        .sig
        .inputs
        .iter()
        .filter_map(|arg| match arg {
            syn::FnArg::Typed(pat_ty) => Some(pat_ty),
            syn::FnArg::Receiver(_) => None,
        })
        .collect();

    /* `disturbance = "chave"` injeta um parâmetro A MAIS (o comando liga/desliga), sempre o
    PRIMEIRO — não corresponde a nenhum `#[need(...)]` escrito pelo usuário, então a checagem 1:1
    conta esse parâmetro implícito à parte.
    */
    let expected_params = need_specs.len() + if disturbance_key.is_some() { 1 } else { 0 };
    if params.len() != expected_params {
        return Err(syn::Error::new_spanned(
            &method.sig,
            format!(
                "{} parâmetro(s) declarado(s) mas {} esperado(s) ({} atributo(s) #[need(...)]{}) — \
                precisa bater 1:1, na ordem em que aparecem",
                params.len(),
                expected_params,
                need_specs.len(),
                if disturbance_key.is_some() {
                    " + 1 pro comando liga/desliga injetado por `disturbance = \"...\"`, sempre o primeiro"
                } else {
                    ""
                },
            ),
        ));
    }

    let mut need_keys: Vec<String> = Vec::new();
    let mut need_proxy_fields: Vec<TokenStream2> = Vec::new();
    let mut need_field_inits: Vec<TokenStream2> = Vec::new();
    let mut need_value_exprs: Vec<TokenStream2> = Vec::new();

    for spec in &need_specs {
        let field_ident = format_ident!("__need_{}", need_value_exprs.len());
        let start = need_keys.len();
        let keys = spec.keys();
        let len = keys.len();
        need_keys.extend(keys);

        let n = match spec {
            FieldKeySpec::Scalar(_) => None,
            FieldKeySpec::Array(_, components) => Some(components.len()),
        };

        need_field_inits.push(field_init_from_slice(&field_ident, "__needed", start, len, n));

        match n {
            None => {
                need_proxy_fields.push(quote! { #field_ident: ::monjolo::state_registry::Proxy });
                need_value_exprs.push(quote! { self.#field_ident.get() });
            }
            Some(count) => {
                need_proxy_fields.push(quote! { #field_ident: [::monjolo::state_registry::Proxy; #count] });
                let indices = (0..count).map(syn::Index::from);
                need_value_exprs.push(quote! { [#(self.#field_ident[#indices].get()),*] });
            }
        }
    }

    let multiple_offers = offer_specs.len() > 1;

    let mut offer_keys: Vec<String> = Vec::new();
    let mut offer_proxy_fields: Vec<TokenStream2> = Vec::new();
    let mut offer_field_inits: Vec<TokenStream2> = Vec::new();
    let mut offer_set_stmts: Vec<TokenStream2> = Vec::new();
    let mut result_bindings: Vec<syn::Ident> = Vec::new();

    for spec in &offer_specs {
        let index = offer_set_stmts.len();
        let field_ident = format_ident!("__offer_{}", index);
        let start = offer_keys.len();
        let keys = spec.keys();
        let len = keys.len();
        offer_keys.extend(keys);

        let n = match spec {
            FieldKeySpec::Scalar(_) => None,
            FieldKeySpec::Array(_, components) => Some(components.len()),
        };

        offer_field_inits.push(field_init_from_slice(&field_ident, "__offered", start, len, n));

        let result_binding = if multiple_offers {
            format_ident!("__result_{}", index)
        } else {
            format_ident!("__result")
        };

        match n {
            None => {
                offer_proxy_fields.push(quote! { #field_ident: ::monjolo::state_registry::Proxy });
                offer_set_stmts.push(quote! { self.#field_ident.set(#result_binding); });
            }
            Some(count) => {
                offer_proxy_fields.push(quote! { #field_ident: [::monjolo::state_registry::Proxy; #count] });
                let indices = (0..count).map(syn::Index::from);
                offer_set_stmts.push(quote! {
                    #(self.#field_ident[#indices].set(#result_binding[#indices]);)*
                });
            }
        }
        result_bindings.push(result_binding);
    }

    /* Se `disturbance_key` existe, o valor do comando liga/desliga (lido do `Proxy` escondido
    `__active`) entra como o PRIMEIRO argumento da chamada — antes de qualquer `#[need(...)]`
    declarado pelo usuário, casando com a checagem de parâmetros acima.
    */
    let active_value_expr = disturbance_key.map(|_| quote! { self.__active.get() });
    let call_value_exprs: Vec<TokenStream2> =
        active_value_expr.into_iter().chain(need_value_exprs.iter().cloned()).collect();

    let call_and_distribute = if multiple_offers {
        quote! {
            let (#(#result_bindings),*) = self.__owner.#impl_name(#(#call_value_exprs),*);
            #(#offer_set_stmts)*
        }
    } else {
        quote! {
            let __result = self.__owner.#impl_name(#(#call_value_exprs),*);
            #(#offer_set_stmts)*
        }
    };

    // Método reescrito: mesmo corpo/assinatura, renomeado, sem os atributos #[need]/#[offer]/
    // #[disturbance] (não são atributos de verdade — sobreviver até o compilador seria "cannot
    // find attribute").
    let mut impl_method = method.clone();
    impl_method.attrs.retain(|a| {
        !(a.path().is_ident("need") || a.path().is_ident("offer") || a.path().is_ident("disturbance"))
    });
    impl_method.sig.ident = impl_name.clone();
    let impl_method_tokens = quote! { #impl_method };

    let task_struct_name = format_ident!("__{}_{}_Task", owner_ident, method_name);
    let descriptor_name: String = format!("{}::{}", owner_ident, method_name);
    let offer_refs = quote! { &[#(#offer_keys),*] };
    let need_refs = quote! { &[#(#need_keys),*] };

    /* Peças geradas SÓ quando `disturbance = "chave"`: campo `__active` (o comando, um `Proxy`
    comum — mesma mecânica de `#[need]`/`#[offer]`, só que nunca exposto como parâmetro do método
    do usuário além do valor já embutido em `call_value_exprs` acima), `impl Actuator` (`write()`
    seta esse `Proxy`), e a construção via `Rc` (não `Box` direto) pra poder catalogar a MESMA
    instância em `offer_actuator()` e ainda entrar na árvore de `evaluate()` — mesmo truque de
    `impl<T: DynamicModel + ?Sized> DynamicModel for Rc<T>` que `#[actuator(...)]` já usa.
    */
    let active_field = disturbance_key.map(|_| quote! { __active: ::monjolo::state_registry::Proxy, });
    let active_field_init = disturbance_key.map(|_| quote! { __active: __active_offered[0].clone(), });
    let active_subscribe = disturbance_key.map(|key| quote! {
        let (__active_offered, _) = registry.subscribe(&[#key], &[]);
    });
    let actuator_impl = disturbance_key.map(|_| quote! {
        impl ::monjolo::actuator::Actuator for #task_struct_name {
            fn write(&self, value: f64) {
                self.__active.set(value);
            }
        }
    });

    let construct_body = if let Some(key) = disturbance_key {
        quote! {
            #active_subscribe
            let (__offered, __needed) = registry.subscribe(#offer_refs, #need_refs);
            let __instance = ::std::rc::Rc::new(#task_struct_name {
                __owner,
                #active_field_init
                #(#need_field_inits,)*
                #(#offer_field_inits,)*
            });
            registry.offer_actuator(#key, __instance.clone());
            ::std::option::Option::Some(
                ::std::boxed::Box::new(__instance) as ::std::boxed::Box<dyn ::monjolo::dynamic_model::DynamicModel>
            )
        }
    } else {
        quote! {
            let (__offered, __needed) = registry.subscribe(#offer_refs, #need_refs);
            ::std::option::Option::Some(::std::boxed::Box::new(#task_struct_name {
                __owner,
                #(#need_field_inits,)*
                #(#offer_field_inits,)*
            }) as ::std::boxed::Box<dyn ::monjolo::dynamic_model::DynamicModel>)
        }
    };

    let task_def = quote! {
        #[allow(non_camel_case_types)]
        struct #task_struct_name {
            __owner: ::std::rc::Rc<#self_ty>,
            #active_field
            #(#need_proxy_fields,)*
            #(#offer_proxy_fields,)*
        }

        impl ::monjolo::dynamic_model::DynamicModel for #task_struct_name {
            fn name(&self) -> &str {
                #descriptor_name
            }

            fn evaluate(&self) {
                #call_and_distribute
            }
        }

        #actuator_impl

        ::monjolo::inventory::submit! {
            ::monjolo::ComponentDescriptor {
                name: #descriptor_name,
                kind: #kind,
                after: &[::std::stringify!(#self_ty)],
                needs: #need_refs,
                offers: #offer_refs,
                construct: |registry: &mut ::monjolo::state_registry::StateRegistry, _config: &::monjolo::snapshot::Snapshot| {
                    let __owner = registry.instance::<#self_ty>(::std::stringify!(#self_ty)).unwrap_or_else(|| {
                        ::std::panic!(
                            "'{}' deveria já ter sido construída (offer_instance) antes de suas \
                            tarefas — `after` deveria garantir isso",
                            ::std::stringify!(#self_ty),
                        )
                    });
                    #construct_body
                },
            }
        }
    };

    Ok((impl_method_tokens, task_def))
}
