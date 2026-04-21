//! Proc-macro entry points for rustcc.
//!
//! Exports:
//!
//! - `#[cpp_class]` — attribute macro; apply to a struct/enum to
//!   declare it as a rustcc C++ interop type. Stable-rustc path.
//! - `#[cpp_name(...)]` — passthrough attribute, consumed by the
//!   rustcc scanner.
//! - `cxx_class! { ... }` — function-like macro; takes a
//!   `class`-syntax block and desugars to a `#[repr(cpp)]` struct
//!   + extern block with Itanium-mangled `#[link_name]` + inherent
//!   `impl` wrappers. Gives users the `class Foo { ... }` surface
//!   syntax without compiler-level parser changes.
//!
//! See each macro's docs for scope and limitations.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{quote, format_ident};
use syn::{
    parse::{Parse, ParseStream},
    parse_macro_input,
    Attribute, Expr, ExprLit, FnArg, Ident, Item, ItemStruct, Lit, LitInt,
    LitStr, Meta, Pat, PatType, ReturnType, Signature, Token, Type, TypePath,
    TypePtr, TypeReference, Visibility,
};

// ============================================================
// Existing macros — unchanged.
// ============================================================

#[proc_macro_attribute]
pub fn cpp_class(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(item as Item);
    let out = match parsed {
        Item::Struct(s) => quote! { #[repr(C)] #s },
        Item::Enum(e) => quote! { #[repr(C)] #e },
        other => {
            let err = syn::Error::new_spanned(
                &other,
                "#[cpp_class] only supports structs and enums (v1)",
            )
            .to_compile_error();
            quote! { #err #other }
        }
    };
    out.into()
}

#[proc_macro_attribute]
pub fn cpp_name(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

// ============================================================
// cxx_class! — class-syntax sugar for C++ interop bindings.
// ============================================================

/// Expand a `class`-syntax block into the full rustcc binding
/// form: a `#[repr(cpp)]` struct + an `extern "C++"` block with
/// Itanium-mangled `#[link_name]` per method + a thin inherent
/// `impl` block that wraps each extern fn so callers can write
/// `t.width()` style.
///
/// Syntax:
///
/// ```ignore
/// cxx_class! {
///     #[size = 8] #[align = 8]
///     pub class Texture {
///         #[ctor] fn new(path: *const u8) -> Self;
///         fn width(&self) -> u32;
///         fn bind(&self, slot: u32);
///         // `drop` is auto-generated — user may override with
///         // #[dtor] fn custom_drop(&mut self);
///     }
/// }
/// ```
///
/// Expansion (simplified):
///
/// ```ignore
/// #[repr(cpp)]
/// pub struct Texture {
///     __opaque: [u8; 8],
/// }
///
/// unsafe extern "C++" {
///     #[link_name = "_ZN7TextureC1EPKh"]
///     fn __Texture__new(this: *mut Texture, path: *const u8);
///     #[link_name = "_ZNK7Texture5widthEv"]
///     fn __Texture__width(this: *const Texture) -> u32;
///     #[link_name = "_ZNK7Texture4bindEj"]
///     fn __Texture__bind(this: *const Texture, slot: u32);
///     #[link_name = "_ZN7TextureD1Ev"]
///     fn __Texture__dtor(this: *mut Texture);
/// }
///
/// impl Texture {
///     pub fn new(path: *const u8) -> Self {
///         let mut uninit = core::mem::MaybeUninit::<Texture>::uninit();
///         unsafe {
///             __Texture__new(uninit.as_mut_ptr(), path);
///             uninit.assume_init()
///         }
///     }
///     pub fn width(&self) -> u32 { unsafe { __Texture__width(self) } }
///     pub fn bind(&self, slot: u32) { unsafe { __Texture__bind(self, slot) } }
/// }
///
/// impl Drop for Texture {
///     fn drop(&mut self) { unsafe { __Texture__dtor(self) } }
/// }
/// ```
///
/// # Limitations (v1)
///
/// - **Size / alignment**: the user must provide `#[size = N]`
///   (required) and optionally `#[align = N]` attributes. The
///   macro has no access to the C++ side's layout computation.
/// - **Parameter types**: primitive scalars (`i8`..`i64`,
///   `u8`..`u64`, `f32`, `f64`, `bool`), `()`, raw pointers
///   (`*const T` / `*mut T`), and references (`&T` / `&mut T`).
///   Records, arrays, and fn-pointers require extending
///   `itanium_code` below.
/// - **Namespaces**: the class is assumed to live at the global
///   C++ namespace. Nested namespaces require the fork's
///   per-module mangling, which this macro doesn't mirror.
/// - **Ctors return Self by value**: the macro emits an sret
///   convention to match what Itanium uses for non-trivial
///   constructors (matches the fork's P09.12 ctor path).
#[proc_macro]
pub fn cxx_class(input: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(input as ClassBlock);
    expand_class(parsed)
        .unwrap_or_else(|e| e.to_compile_error())
        .into()
}

// -----------------------------------------------------------
// AST for the macro input.
// -----------------------------------------------------------

struct ClassBlock {
    size: usize,
    align: Option<usize>,
    vis: Visibility,
    name: Ident,
    methods: Vec<Method>,
}

struct Method {
    kind: MethodKind,
    name: Ident,
    sig: Signature,
}

enum MethodKind {
    /// Regular instance method — takes `&self` or `&mut self`.
    Instance { is_const: bool },
    /// `#[ctor]`-tagged fn — maps to `_ZN<class>C1E<args>`.
    Ctor,
    /// `#[dtor]`-tagged fn — maps to `_ZN<class>D1Ev`. Rare; most
    /// users rely on the auto-synthesized `Drop` impl.
    Dtor,
    /// Static method (no `self` param).
    Static,
}

impl Parse for ClassBlock {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut size: Option<usize> = None;
        let mut align: Option<usize> = None;

        // Block-level attributes: #[size = N], #[align = N].
        while input.peek(Token![#]) {
            let attr_content;
            input.parse::<Token![#]>()?;
            syn::bracketed!(attr_content in input);
            let key: Ident = attr_content.parse()?;
            attr_content.parse::<Token![=]>()?;
            let n: LitInt = attr_content.parse()?;
            let value: usize = n.base10_parse()?;
            match key.to_string().as_str() {
                "size" => size = Some(value),
                "align" => align = Some(value),
                other => {
                    return Err(syn::Error::new_spanned(
                        key,
                        format!("unknown class attribute `{other}`; expected `size` or `align`"),
                    ));
                }
            }
        }

        let vis: Visibility = input.parse()?;
        let class_kw: Ident = input.parse()?;
        if class_kw != "class" {
            return Err(syn::Error::new_spanned(
                class_kw,
                "expected `class` keyword",
            ));
        }
        let name: Ident = input.parse()?;

        let body;
        syn::braced!(body in input);

        let mut methods = Vec::new();
        while !body.is_empty() {
            methods.push(body.parse::<Method>()?);
        }

        let size = size.ok_or_else(|| {
            syn::Error::new_spanned(
                &name,
                "#[size = N] attribute is required (C++ side's sizeof for this class)",
            )
        })?;

        Ok(ClassBlock { size, align, vis, name, methods })
    }
}

impl Parse for Method {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // Attributes — we care about #[ctor] and #[dtor].
        let mut is_ctor = false;
        let mut is_dtor = false;
        while input.peek(Token![#]) {
            let c;
            input.parse::<Token![#]>()?;
            syn::bracketed!(c in input);
            let key: Ident = c.parse()?;
            match key.to_string().as_str() {
                "ctor" => is_ctor = true,
                "dtor" => is_dtor = true,
                other => {
                    return Err(syn::Error::new_spanned(
                        key,
                        format!("unknown method attribute `#[{other}]` inside cxx_class!"),
                    ));
                }
            }
        }

        let _vis: Visibility = input.parse()?; // accepted but ignored
        let sig: Signature = input.parse()?;
        input.parse::<Token![;]>()?;

        let kind = if is_ctor {
            MethodKind::Ctor
        } else if is_dtor {
            MethodKind::Dtor
        } else {
            match sig.inputs.first() {
                Some(FnArg::Receiver(rcv)) => MethodKind::Instance {
                    is_const: rcv.mutability.is_none(),
                },
                _ => MethodKind::Static,
            }
        };

        Ok(Method { kind, name: sig.ident.clone(), sig })
    }
}

// -----------------------------------------------------------
// Expansion.
// -----------------------------------------------------------

fn expand_class(block: ClassBlock) -> syn::Result<TokenStream2> {
    let ClassBlock { size, align, vis, name, methods } = block;
    let size_lit = proc_macro2::Literal::usize_unsuffixed(size);
    let align_attr = align.map(|a| {
        let a_lit = proc_macro2::Literal::usize_unsuffixed(a);
        quote! { , align(#a_lit) }
    }).unwrap_or_default();
    let class_name_str = name.to_string();

    let mut extern_items: Vec<TokenStream2> = Vec::new();
    let mut impl_items: Vec<TokenStream2> = Vec::new();
    let mut dtor_extern_name: Option<Ident> = None;

    let has_explicit_dtor = methods.iter().any(|m| matches!(m.kind, MethodKind::Dtor));

    for m in &methods {
        let extern_ident = format_ident!("__{}__{}", class_name_str, m.name);
        let link_name = mangled_name(&class_name_str, m)?;

        let (extern_sig, wrapper_body) = lower_method(&name, &extern_ident, m)?;

        extern_items.push(quote! {
            #[link_name = #link_name]
            #extern_sig;
        });

        match &m.kind {
            MethodKind::Dtor => {
                dtor_extern_name = Some(extern_ident.clone());
            }
            _ => {
                impl_items.push(wrapper_body);
            }
        }
    }

    // If no user-supplied dtor, synthesize an auto-dtor extern and
    // auto Drop impl so `impl Drop` links against the C++ `~Foo()`.
    let drop_impl = if let Some(dtor_fn) = dtor_extern_name.clone() {
        quote! {
            impl Drop for #name {
                fn drop(&mut self) {
                    unsafe { #dtor_fn(self as *mut #name); }
                }
            }
        }
    } else if !has_explicit_dtor {
        let auto_dtor_ident = format_ident!("__{}__drop", class_name_str);
        let auto_dtor_link = format!("_ZN{}{}D1Ev", class_name_str.len(), class_name_str);
        extern_items.push(quote! {
            #[link_name = #auto_dtor_link]
            fn #auto_dtor_ident(this: *mut #name);
        });
        quote! {
            impl Drop for #name {
                fn drop(&mut self) {
                    unsafe { #auto_dtor_ident(self as *mut #name); }
                }
            }
        }
    } else {
        quote! {}
    };

    let expanded = quote! {
        #[repr(C #align_attr)]
        #vis struct #name {
            __opaque: [u8; #size_lit],
        }

        // Use `extern "C"` so the macro's output compiles on stable
        // rustc. For the subset this macro supports (scalars,
        // pointers, references, single-namespace user types), C's
        // calling convention matches C++'s on SysV AMD64 / AArch64
        // and the Itanium-mangled `#[link_name]` keeps the linker
        // happy. For classes with by-value records or virtuals,
        // users on the fork should use the native `#[repr(cpp)]`
        // flow, which delegates to the real Itanium ABI overlay.
        unsafe extern "C" {
            #(#extern_items)*
        }

        impl #name {
            #(#impl_items)*
        }

        #drop_impl
    };

    Ok(expanded)
}

/// Lower a method to (extern-sig-tokens, wrapper-impl-body-tokens).
fn lower_method(
    class: &Ident,
    extern_ident: &Ident,
    m: &Method,
) -> syn::Result<(TokenStream2, TokenStream2)> {
    let fn_name = &m.name;
    match &m.kind {
        MethodKind::Ctor => {
            // Ctor takes `this: *mut Class` (sret-style) plus user
            // args; returns () on the C++ side. User calls the
            // Rust wrapper which allocates MaybeUninit, runs ctor,
            // assumes_init.
            let user_args = extract_user_args(&m.sig)?;
            let extern_params: Vec<TokenStream2> = std::iter::once(quote! { this: *mut #class })
                .chain(user_args.iter().map(|(ident, ty)| quote! { #ident: #ty }))
                .collect();
            let extern_sig = quote! {
                fn #extern_ident(#(#extern_params),*)
            };

            let arg_idents: Vec<&Ident> = user_args.iter().map(|(i, _)| i).collect();
            let arg_tys: Vec<&Type> = user_args.iter().map(|(_, t)| t).collect();
            let wrapper = quote! {
                pub fn #fn_name(#(#arg_idents: #arg_tys),*) -> Self {
                    let mut slot = core::mem::MaybeUninit::<Self>::uninit();
                    unsafe {
                        #extern_ident(slot.as_mut_ptr(), #(#arg_idents),*);
                        slot.assume_init()
                    }
                }
            };
            Ok((extern_sig, wrapper))
        }
        MethodKind::Dtor => {
            let extern_sig = quote! {
                fn #extern_ident(this: *mut #class)
            };
            Ok((extern_sig, quote! {}))
        }
        MethodKind::Instance { is_const } => {
            let user_args = extract_user_args(&m.sig)?;
            let this_ty: TokenStream2 = if *is_const {
                quote! { *const #class }
            } else {
                quote! { *mut #class }
            };
            let ret = match &m.sig.output {
                ReturnType::Default => quote! {},
                ReturnType::Type(_, ty) => quote! { -> #ty },
            };
            let extern_params: Vec<TokenStream2> = std::iter::once(quote! { this: #this_ty })
                .chain(user_args.iter().map(|(ident, ty)| quote! { #ident: #ty }))
                .collect();
            let extern_sig = quote! {
                fn #extern_ident(#(#extern_params),*) #ret
            };

            let arg_idents: Vec<&Ident> = user_args.iter().map(|(i, _)| i).collect();
            let arg_tys: Vec<&Type> = user_args.iter().map(|(_, t)| t).collect();
            let self_ref = if *is_const {
                quote! { self as *const Self }
            } else {
                quote! { self as *mut Self }
            };
            let self_kw = if *is_const { quote! { &self } } else { quote! { &mut self } };
            let wrapper = quote! {
                pub fn #fn_name(#self_kw, #(#arg_idents: #arg_tys),*) #ret {
                    unsafe { #extern_ident(#self_ref, #(#arg_idents),*) }
                }
            };
            Ok((extern_sig, wrapper))
        }
        MethodKind::Static => {
            let user_args = extract_user_args(&m.sig)?;
            let ret = match &m.sig.output {
                ReturnType::Default => quote! {},
                ReturnType::Type(_, ty) => quote! { -> #ty },
            };
            let extern_params: Vec<TokenStream2> = user_args
                .iter()
                .map(|(ident, ty)| quote! { #ident: #ty })
                .collect();
            let extern_sig = quote! {
                fn #extern_ident(#(#extern_params),*) #ret
            };

            let arg_idents: Vec<&Ident> = user_args.iter().map(|(i, _)| i).collect();
            let arg_tys: Vec<&Type> = user_args.iter().map(|(_, t)| t).collect();
            let wrapper = quote! {
                pub fn #fn_name(#(#arg_idents: #arg_tys),*) #ret {
                    unsafe { #extern_ident(#(#arg_idents),*) }
                }
            };
            Ok((extern_sig, wrapper))
        }
    }
}

/// Extract non-self args from a Signature as (ident, ty) pairs.
fn extract_user_args(sig: &Signature) -> syn::Result<Vec<(Ident, Type)>> {
    let mut out = Vec::new();
    for input in &sig.inputs {
        match input {
            FnArg::Receiver(_) => {}
            FnArg::Typed(PatType { pat, ty, .. }) => {
                let ident = match &**pat {
                    Pat::Ident(pi) => pi.ident.clone(),
                    _ => {
                        return Err(syn::Error::new_spanned(
                            pat,
                            "cxx_class! requires plain-identifier parameter names",
                        ));
                    }
                };
                out.push((ident, (**ty).clone()));
            }
        }
    }
    Ok(out)
}

/// Mangle a method into its Itanium symbol. Matches what the fork's
/// `rustc_symbol_mangling::itanium` produces for Rust-defined
/// methods on `#[repr(cpp)]` types, which in turn matches Clang.
fn mangled_name(class: &str, m: &Method) -> syn::Result<String> {
    let class_len = class.len();
    let class_seg = format!("{class_len}{class}");

    match &m.kind {
        MethodKind::Ctor => {
            let args = encode_args(class, &m.sig, /*skip_self=*/ false)?;
            Ok(format!("_ZN{class_seg}C1E{args}"))
        }
        MethodKind::Dtor => Ok(format!("_ZN{class_seg}D1Ev")),
        MethodKind::Instance { is_const } => {
            let name_str = m.name.to_string();
            let name_seg = format!("{}{}", name_str.len(), name_str);
            let args = encode_args(class, &m.sig, /*skip_self=*/ true)?;
            let prefix = if *is_const { "_ZNK" } else { "_ZN" };
            Ok(format!("{prefix}{class_seg}{name_seg}E{args}"))
        }
        MethodKind::Static => {
            let name_str = m.name.to_string();
            let name_seg = format!("{}{}", name_str.len(), name_str);
            let args = encode_args(class, &m.sig, /*skip_self=*/ false)?;
            Ok(format!("_ZN{class_seg}{name_seg}E{args}"))
        }
    }
}

/// Substitution-tracking mangle context. Seeded with the
/// enclosing class at slot 0 (appears in the mangled form as
/// `S_` when referenced again in a parameter type).
struct MangleCtx {
    slots: Vec<String>,
}

impl MangleCtx {
    fn new(class: &str) -> Self {
        // Slot 0 = enclosing class. Clang's Itanium mangler
        // seeds the substitution table with the class name when
        // mangling member fns, so `MyClass` references in params
        // collapse to `S_`.
        Self { slots: vec![class.to_string()] }
    }

    /// Encode substitution index: idx 0 → `S_`, 1 → `S0_`,
    /// 10 → `S9_`, 11 → `SA_`, ..., 36 → `S10_` (base-36).
    fn sub_string(idx: usize) -> String {
        if idx == 0 {
            return "S_".into();
        }
        let n = idx - 1;
        let mut digits = Vec::new();
        let mut k = n;
        loop {
            let d = (k % 36) as u8;
            digits.push(if d < 10 { b'0' + d } else { b'A' + d - 10 });
            k /= 36;
            if k == 0 { break; }
        }
        digits.reverse();
        let mut s = String::from("S");
        s.extend(digits.iter().map(|&b| b as char));
        s.push('_');
        s
    }

    fn code(&mut self, ty: &Type) -> syn::Result<String> {
        match ty {
            Type::Path(TypePath { qself: None, path }) => {
                let segs: Vec<_> = path.segments.iter().collect();
                if segs.len() != 1 {
                    return Err(syn::Error::new_spanned(
                        ty,
                        "cxx_class! only supports single-segment type paths for args",
                    ));
                }
                let name = segs[0].ident.to_string();
                match name.as_str() {
                    "i8" => Ok("a".into()),
                    "u8" => Ok("h".into()),
                    "i16" => Ok("s".into()),
                    "u16" => Ok("t".into()),
                    "i32" => Ok("i".into()),
                    "u32" => Ok("j".into()),
                    // Rust `i64` / `u64` → Itanium `x` / `y`
                    // (`long long` / `unsigned long long`), matching
                    // what Clang emits for `int64_t` / `uint64_t`.
                    // `l` / `m` (`long`) would also be 8 bytes on
                    // SysV AMD64 / ARM64, but they're a different
                    // nominal type and mangle distinctly.
                    "i64" => Ok("x".into()),
                    "u64" => Ok("y".into()),
                    "f32" => Ok("f".into()),
                    "f64" => Ok("d".into()),
                    "bool" => Ok("b".into()),
                    "c_void" => Ok("v".into()),
                    other => {
                        // User class at global namespace. First
                        // occurrence emits the full `<len><name>`;
                        // subsequent occurrences reference the
                        // substitution slot.
                        if let Some(idx) = self.slots.iter().position(|s| s == other) {
                            Ok(Self::sub_string(idx))
                        } else {
                            self.slots.push(other.to_string());
                            Ok(format!("{}{}", other.len(), other))
                        }
                    }
                }
            }
            Type::Ptr(TypePtr { mutability, elem, .. }) => {
                let inner = self.code(elem)?;
                if mutability.is_none() {
                    Ok(format!("PK{inner}"))
                } else {
                    Ok(format!("P{inner}"))
                }
            }
            Type::Reference(TypeReference { mutability, elem, .. }) => {
                let inner = self.code(elem)?;
                if mutability.is_none() {
                    Ok(format!("RK{inner}"))
                } else {
                    Ok(format!("R{inner}"))
                }
            }
            Type::Tuple(t) if t.elems.is_empty() => Ok("v".into()),
            _ => Err(syn::Error::new_spanned(
                ty,
                "cxx_class! can't mangle this type yet — supported: primitive scalars, *const/*mut T, &T / &mut T, user class types at global namespace",
            )),
        }
    }
}

fn encode_args(class: &str, sig: &Signature, skip_self: bool) -> syn::Result<String> {
    let mut ctx = MangleCtx::new(class);
    let mut out = String::new();
    let mut count = 0usize;
    for input in &sig.inputs {
        match input {
            FnArg::Receiver(_) => {
                if !skip_self {
                    // Shouldn't happen for ctor/static per our parse.
                }
            }
            FnArg::Typed(PatType { ty, .. }) => {
                out.push_str(&ctx.code(ty)?);
                count += 1;
            }
        }
    }
    if count == 0 {
        out.push('v');
    }
    Ok(out)
}

// ============================================================
// native_cpp_class! — fork-only surface that rides on the
// compiler's unified ctor / dtor / method mangling (P09.12 for
// impls, P09.15 for dtors, P09.22 for foreign ctors). Output
// does not hand-mangle any Itanium symbol; it leans on the
// compiler to emit `_ZN<class>C1E<args>` from a plain
// `#[constructor] fn new(...) -> Self;` declaration.
//
// Differences from `cxx_class!`:
//
// - Struct is `#[repr(cpp)]`, not `#[repr(C)]`.
// - Extern block is `extern "C++"`, not `extern "C"`.
// - Ctors: no sret trampoline, no `#[link_name]`. The extern
//   fn returns `Self` by value and is tagged `#[constructor]`.
//   The compiler synthesizes the `this`-pointer sret lowering
//   and the Itanium `_ZN<class>C1E<args>` symbol.
// - Dtors / methods: still use `#[link_name]` for now — the
//   compiler path for foreign non-ctor member fns is future work.
//
// Because the output uses `#[repr(cpp)]` and `extern "C++"`,
// THIS MACRO ONLY COMPILES ON THE RUSTCC FORK. On stable rustc
// the generated code rejects `#[repr(cpp)]`. Users who need a
// stable-rustc path should use `cxx_class!`.
// ============================================================

/// Fork-only class-syntax sugar that rides on the compiler's
/// unified ctor/dtor/method mangling. See module-level docs for
/// the differences vs `cxx_class!`.
#[proc_macro]
pub fn native_cpp_class(input: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(input as ClassBlock);
    expand_class_native(parsed)
        .unwrap_or_else(|e| e.to_compile_error())
        .into()
}

fn expand_class_native(block: ClassBlock) -> syn::Result<TokenStream2> {
    let ClassBlock { size, align, vis, name, methods } = block;
    let size_lit = proc_macro2::Literal::usize_unsuffixed(size);
    let align_attr = align.map(|a| {
        let a_lit = proc_macro2::Literal::usize_unsuffixed(a);
        quote! { , align(#a_lit) }
    }).unwrap_or_default();
    let class_name_str = name.to_string();

    let mut extern_items: Vec<TokenStream2> = Vec::new();
    let mut impl_items: Vec<TokenStream2> = Vec::new();
    let mut dtor_extern_name: Option<Ident> = None;
    let has_explicit_dtor = methods.iter().any(|m| matches!(m.kind, MethodKind::Dtor));

    for m in &methods {
        let extern_ident = format_ident!("__{}__{}", class_name_str, m.name);
        let fn_name = &m.name;

        match &m.kind {
            MethodKind::Ctor => {
                // P09.22 unified path: compiler emits
                // `_ZN<class>C1E<args>` for a plain
                // `#[constructor] fn new(...) -> Self;`.
                let user_args = extract_user_args(&m.sig)?;
                let params: Vec<TokenStream2> = user_args
                    .iter()
                    .map(|(id, ty)| quote! { #id: #ty })
                    .collect();
                extern_items.push(quote! {
                    #[constructor]
                    fn #extern_ident(#(#params),*) -> #name;
                });
                let arg_idents: Vec<&Ident> = user_args.iter().map(|(i, _)| i).collect();
                let arg_tys: Vec<&Type> = user_args.iter().map(|(_, t)| t).collect();
                // `#[inline(always)]` avoids emitting an exported
                // symbol for the wrapper. Without it, P09.4
                // auto-exports the impl method as a C++ static
                // member, colliding with the C++ side's ctor
                // symbol and producing a recursive call.
                impl_items.push(quote! {
                    #[rustc_cxx_wrapper]
                    #[inline(always)]
                    pub fn #fn_name(#(#arg_idents: #arg_tys),*) -> Self {
                        unsafe { #extern_ident(#(#arg_idents),*) }
                    }
                });
            }
            MethodKind::Dtor => {
                let link = format!(
                    "_ZN{}{}D1Ev",
                    class_name_str.len(),
                    class_name_str
                );
                extern_items.push(quote! {
                    #[link_name = #link]
                    fn #extern_ident(this: *mut #name);
                });
                dtor_extern_name = Some(extern_ident.clone());
            }
            MethodKind::Instance { is_const } => {
                let link = mangled_name(&class_name_str, m)?;
                let user_args = extract_user_args(&m.sig)?;
                let this_ty: TokenStream2 = if *is_const {
                    quote! { *const #name }
                } else {
                    quote! { *mut #name }
                };
                let ret = match &m.sig.output {
                    ReturnType::Default => quote! {},
                    ReturnType::Type(_, ty) => quote! { -> #ty },
                };
                let params: Vec<TokenStream2> = std::iter::once(quote! { this: #this_ty })
                    .chain(user_args.iter().map(|(id, ty)| quote! { #id: #ty }))
                    .collect();
                extern_items.push(quote! {
                    #[link_name = #link]
                    fn #extern_ident(#(#params),*) #ret;
                });
                let arg_idents: Vec<&Ident> = user_args.iter().map(|(i, _)| i).collect();
                let arg_tys: Vec<&Type> = user_args.iter().map(|(_, t)| t).collect();
                let self_ref = if *is_const {
                    quote! { self as *const Self }
                } else {
                    quote! { self as *mut Self }
                };
                let self_kw = if *is_const { quote! { &self } } else { quote! { &mut self } };
                impl_items.push(quote! {
                    #[rustc_cxx_wrapper]
                    #[inline(always)]
                    pub fn #fn_name(#self_kw, #(#arg_idents: #arg_tys),*) #ret {
                        unsafe { #extern_ident(#self_ref, #(#arg_idents),*) }
                    }
                });
            }
            MethodKind::Static => {
                let link = mangled_name(&class_name_str, m)?;
                let user_args = extract_user_args(&m.sig)?;
                let ret = match &m.sig.output {
                    ReturnType::Default => quote! {},
                    ReturnType::Type(_, ty) => quote! { -> #ty },
                };
                let params: Vec<TokenStream2> = user_args
                    .iter()
                    .map(|(id, ty)| quote! { #id: #ty })
                    .collect();
                extern_items.push(quote! {
                    #[link_name = #link]
                    fn #extern_ident(#(#params),*) #ret;
                });
                let arg_idents: Vec<&Ident> = user_args.iter().map(|(i, _)| i).collect();
                let arg_tys: Vec<&Type> = user_args.iter().map(|(_, t)| t).collect();
                impl_items.push(quote! {
                    #[rustc_cxx_wrapper]
                    #[inline(always)]
                    pub fn #fn_name(#(#arg_idents: #arg_tys),*) #ret {
                        unsafe { #extern_ident(#(#arg_idents),*) }
                    }
                });
            }
        }
    }

    // P09.23: auto-generate `impl Drop` that forwards to the C++
    // destructor via an `extern "C++"` decl. `#[rustc_cxx_drop_wrapper]`
    // on `drop` opts the method out of P09.15's D0/D1/D2 auto-export,
    // so the C++ side's destructor (compiled by Clang) is the sole
    // definer of those symbols and there's no link-time collision.
    let drop_impl = if let Some(dtor_fn) = dtor_extern_name.clone() {
        quote! {
            impl Drop for #name {
                #[rustc_cxx_drop_wrapper]
                fn drop(&mut self) {
                    unsafe { #dtor_fn(self as *mut #name); }
                }
            }
        }
    } else if !has_explicit_dtor {
        let auto_dtor_ident = format_ident!("__{}__drop", class_name_str);
        let auto_dtor_link = format!("_ZN{}{}D1Ev", class_name_str.len(), class_name_str);
        extern_items.push(quote! {
            #[link_name = #auto_dtor_link]
            fn #auto_dtor_ident(this: *mut #name);
        });
        quote! {
            impl Drop for #name {
                #[rustc_cxx_drop_wrapper]
                fn drop(&mut self) {
                    unsafe { #auto_dtor_ident(self as *mut #name); }
                }
            }
        }
    } else {
        quote! {}
    };

    let expanded = quote! {
        #[repr(cpp #align_attr)]
        #vis struct #name {
            __opaque: [u8; #size_lit],
        }

        unsafe extern "C++" {
            #(#extern_items)*
        }

        impl #name {
            #(#impl_items)*
        }

        #drop_impl
    };

    Ok(expanded)
}

// ============================================================
// swift_value! — fork-only ergonomic wrapper for `#[repr(swift)]`
// value types. Phase 2b.2 (P09.28): takes a plain struct tagged
// `#[swift_type = "Module.Type"]` and auto-synthesizes:
//
//   - the `#[repr(swift)]` attribute on the struct
//   - an `extern "C"` metadata-accessor decl with the right
//     Itanium/Swift-mangled `#[link_name]` (`$s<mod><type>VMa`
//     for value types, `$s<mod><type>CMa` for classes)
//   - `impl Drop` forwarding to
//     `rustcc_swift_rt::drop_swift_value` (value types) or
//     `release_swift_class` (classes)
//   - `impl Clone` forwarding to
//     `rustcc_swift_rt::clone_swift_value` / `retain_swift_class`
//
// Before P09.28 users had to write the extern decl, the Drop
// impl, and the Clone impl by hand — a ~30-line chunk of
// boilerplate per type. This macro collapses it to just the
// struct declaration.
//
// Scope notes:
//
// - ASCII module and type names only; anything else would need
//   Swift's full mangling (punycode / operator-name runs). v1.
// - Non-generic types only. Generic Swift types have richer
//   metadata accessor shapes.
// - Assumes the metadata accessor takes `usize` and returns
//   `MetadataResponse` — matches every case we've seen on
//   x86_64-apple-darwin / aarch64-apple-darwin.
// ============================================================

/// Auto-synthesize `Drop` + `Clone` for a `#[repr(swift)]` struct
/// tagged with `#[swift_type = "Module.Type"]`. See the
/// block comment above for full scope.
#[proc_macro]
pub fn swift_value(input: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(input as ItemStruct);
    expand_swift_value(parsed)
        .unwrap_or_else(|e| e.to_compile_error())
        .into()
}

fn expand_swift_value(mut item: ItemStruct) -> syn::Result<TokenStream2> {
    let binding = extract_swift_binding(&item.attrs).ok_or_else(|| {
        syn::Error::new_spanned(
            &item.ident,
            "swift_value! requires a `#[swift_type = \"Module.Type\"]` \
             attribute on the struct (optionally `...:class` for a class)",
        )
    })?;

    // Drop the fork-specific attributes the user supplied —
    // we'll re-emit them ourselves with known-good shape.
    item.attrs
        .retain(|a| !attr_path_is(a, "repr") && !attr_path_is(a, "swift_type"));

    let struct_name = item.ident.clone();
    let swift_attr = binding.as_attr_string();
    let metadata_ident = format_ident!("__{}__metadata", struct_name);
    let metadata_link = binding.metadata_accessor_symbol();

    let drop_body = if binding.is_class {
        // Classes are opaque pointers; Drop releases the ref.
        quote! {
            let ptr: *mut ::core::ffi::c_void =
                unsafe { *(self as *const Self as *const *mut ::core::ffi::c_void) };
            if !ptr.is_null() {
                unsafe { ::rustcc_swift_rt::release_swift_class(ptr); }
            }
        }
    } else {
        quote! {
            unsafe {
                ::rustcc_swift_rt::drop_swift_value(
                    self as *mut Self,
                    #metadata_ident,
                );
            }
        }
    };

    let clone_body = if binding.is_class {
        // Classes: retain the ref pointer, keep all other bytes
        // as a straight copy. Copying extra fields is safe
        // because class-backed `#[repr(swift)]` bindings in v1
        // are single-field handles (the class pointer). Future
        // multi-field class wrappers would need per-field clone
        // logic.
        quote! {
            let ptr: *mut ::core::ffi::c_void =
                unsafe { *(self as *const Self as *const *mut ::core::ffi::c_void) };
            let retained = if ptr.is_null() {
                ptr
            } else {
                unsafe { ::rustcc_swift_rt::retain_swift_class(ptr) }
            };
            let mut copy: ::core::mem::MaybeUninit<Self> =
                ::core::mem::MaybeUninit::uninit();
            unsafe {
                ::core::ptr::copy_nonoverlapping(
                    self as *const Self as *const u8,
                    copy.as_mut_ptr() as *mut u8,
                    ::core::mem::size_of::<Self>(),
                );
                // Overwrite the pointer field with the retained
                // copy (it's at offset 0 by assumption).
                *(copy.as_mut_ptr() as *mut *mut ::core::ffi::c_void) = retained;
                copy.assume_init()
            }
        }
    } else {
        quote! {
            let mut copy: ::core::mem::MaybeUninit<Self> =
                ::core::mem::MaybeUninit::uninit();
            unsafe {
                ::rustcc_swift_rt::clone_swift_value(
                    copy.as_mut_ptr(),
                    self as *const Self,
                    #metadata_ident,
                );
                copy.assume_init()
            }
        }
    };

    // Classes use swift_retain/release from the C runtime — no
    // metadata accessor needed. Value types need both.
    let metadata_extern = if binding.is_class {
        quote! {}
    } else {
        quote! {
            unsafe extern "C" {
                #[link_name = #metadata_link]
                fn #metadata_ident(flags: usize)
                    -> ::rustcc_swift_rt::MetadataResponse;
            }
        }
    };

    Ok(quote! {
        #[repr(swift)]
        #[swift_type = #swift_attr]
        #item

        #metadata_extern

        impl ::core::ops::Drop for #struct_name {
            fn drop(&mut self) {
                #drop_body
            }
        }

        impl ::core::clone::Clone for #struct_name {
            fn clone(&self) -> Self {
                #clone_body
            }
        }
    })
}

/// Parsed form of `#[swift_type = "Module.Type"]` or
/// `#[swift_type = "Module.Type:class"]`.
struct SwiftBinding {
    module: String,
    name: String,
    is_class: bool,
}

impl SwiftBinding {
    fn as_attr_string(&self) -> String {
        let base = format!("{}.{}", self.module, self.name);
        if self.is_class { format!("{base}:class") } else { base }
    }

    /// Itanium-style Swift mangling for the metadata accessor:
    ///   `$s<modlen><module><typelen><type>VMa` (value type)
    ///   `$s<modlen><module><typelen><type>CMa` (class)
    fn metadata_accessor_symbol(&self) -> String {
        let tag = if self.is_class { 'C' } else { 'V' };
        format!(
            "$s{}{}{}{}{}Ma",
            self.module.len(),
            self.module,
            self.name.len(),
            self.name,
            tag,
        )
    }
}

fn extract_swift_binding(attrs: &[Attribute]) -> Option<SwiftBinding> {
    for attr in attrs {
        if !attr_path_is(attr, "swift_type") {
            continue;
        }
        let Meta::NameValue(nv) = &attr.meta else { continue };
        let Expr::Lit(ExprLit { lit: Lit::Str(s), .. }) = &nv.value else {
            continue;
        };
        return parse_binding_literal(&s);
    }
    None
}

fn parse_binding_literal(s: &LitStr) -> Option<SwiftBinding> {
    let raw = s.value();
    let (body, is_class) = match raw.rsplit_once(':') {
        Some((b, "class")) => (b.to_string(), true),
        Some((b, "struct")) => (b.to_string(), false),
        _ => (raw.clone(), false),
    };
    let (module, name) = body.rsplit_once('.')?;
    // Mangling limits — keep in sync with the doc comment above.
    if !module.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    Some(SwiftBinding {
        module: module.to_string(),
        name: name.to_string(),
        is_class,
    })
}

fn attr_path_is(attr: &Attribute, expected: &str) -> bool {
    attr.path().segments.last().map(|s| s.ident == expected).unwrap_or(false)
}

