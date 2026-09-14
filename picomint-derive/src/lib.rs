use proc_macro::TokenStream;
use proc_macro2::{Ident, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::punctuated::Punctuated;
use syn::token::Comma;
use syn::{
    Attribute, Data, DataEnum, DataStruct, DeriveInput, Fields, Index, Lit, Token, Variant,
    parse_macro_input,
};

#[proc_macro_derive(Encodable, attributes(encodable))]
pub fn derive_encodable(input: TokenStream) -> TokenStream {
    let DeriveInput {
        ident,
        data,
        generics,
        ..
    } = parse_macro_input!(input);

    let encode_body = match data {
        Data::Struct(DataStruct { fields, .. }) => derive_struct_encode(&fields),
        Data::Enum(DataEnum { variants, .. }) => derive_enum_encode(&ident, &variants),
        Data::Union(_) => error(&ident, "Encodable can't be derived for unions"),
    };
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    quote! {
        impl #impl_generics ::picomint_encoding::Encodable for #ident #ty_generics #where_clause {
            fn consensus_encode<W: ::std::io::Write>(&self, writer: &mut W) -> ::std::io::Result<()> {
                #encode_body
            }
        }
    }
    .into()
}

/// A type that travels as its `picomint`-prefixed base32 consensus
/// encoding: `Serialize`, `Deserialize`, `FromStr` and `Display` all go
/// through `picomint_base32`, and its JSON Schema is a string described by
/// the type's own doc comment, so the wire form is explained once, where
/// the type is.
#[proc_macro_derive(Base32)]
pub fn derive_base32(input: TokenStream) -> TokenStream {
    let DeriveInput { ident, attrs, .. } = parse_macro_input!(input);

    let description = format!(
        "{} Travels as a `picomint`-prefixed base32 string.",
        doc_comment(&attrs)
    );

    let name = ident.to_string();

    quote! {
        impl ::serde::Serialize for #ident {
            fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                ::serde::Serialize::serialize(&::picomint_base32::encode(self), serializer)
            }
        }

        impl<'de> ::serde::Deserialize<'de> for #ident {
            fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let encoded: String = ::serde::Deserialize::deserialize(deserializer)?;

                ::picomint_base32::decode(&encoded).map_err(::serde::de::Error::custom)
            }
        }

        impl ::std::str::FromStr for #ident {
            type Err = ::anyhow::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                ::picomint_base32::decode(s)
            }
        }

        impl ::std::fmt::Display for #ident {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(&::picomint_base32::encode(self))
            }
        }

        impl ::schemars::JsonSchema for #ident {
            fn schema_name() -> ::std::borrow::Cow<'static, str> {
                #name.into()
            }

            fn json_schema(_: &mut ::schemars::SchemaGenerator) -> ::schemars::Schema {
                ::schemars::json_schema!({
                    "type": "string",
                    "description": #description
                })
            }
        }
    }
    .into()
}

/// The item's doc comment as one paragraph: the `///` lines joined by a
/// space, each stripped of the leading space rustdoc keeps.
fn doc_comment(attrs: &[Attribute]) -> String {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("doc"))
        .filter_map(|attr| match &attr.meta {
            syn::Meta::NameValue(pair) => match &pair.value {
                syn::Expr::Lit(syn::ExprLit {
                    lit: Lit::Str(line),
                    ..
                }) => Some(line.value().trim().to_string()),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The variants of a `thiserror` enum as stable codes: `code()` is the
/// variant name in snake_case, `CODES` pairs every code with the
/// variant's `#[error("...")]` message as written.
#[proc_macro_derive(ErrorCode)]
pub fn derive_error_code(input: TokenStream) -> TokenStream {
    let DeriveInput { ident, data, .. } = parse_macro_input!(input);

    let Data::Enum(DataEnum { variants, .. }) = data else {
        return error(&ident, "ErrorCode can only be derived for enums").into();
    };

    let idents = variants.iter().map(|v| &v.ident).collect::<Vec<_>>();
    let codes = variants
        .iter()
        .map(|v| snake_case(&v.ident.to_string()))
        .collect::<Vec<_>>();
    let messages = variants
        .iter()
        .map(|v| error_message(&v.attrs))
        .collect::<Vec<_>>();

    quote! {
        impl ::picomint_core::error::ErrorCode for #ident {
            const CODES: &'static [(&'static str, &'static str)] = &[#((#codes, #messages)),*];

            fn code(&self) -> &'static str {
                match self {
                    #(Self::#idents { .. } => #codes,)*
                }
            }
        }
    }
    .into()
}

/// The string literal of a variant's `#[error("...")]` attribute.
fn error_message(attrs: &[Attribute]) -> String {
    attrs
        .iter()
        .find(|attr| attr.path().is_ident("error"))
        .and_then(|attr| attr.parse_args::<Lit>().ok())
        .and_then(|lit| match lit {
            Lit::Str(message) => Some(message.value()),
            _ => None,
        })
        .expect("every variant of an ErrorCode enum carries an #[error(\"...\")] message")
}

/// `InsufficientBalance` to `insufficient_balance`.
fn snake_case(ident: &str) -> String {
    let mut out = String::new();

    for (i, c) in ident.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push('_');
        }

        out.push(c.to_ascii_lowercase());
    }

    out
}

/// One analytics table row per struct: every named field becomes a
/// column named after it (plus the field type's unit suffix), typed and
/// rendered by its `SqlColumn` impl. Unit structs map to a table with no
/// payload columns.
#[proc_macro_derive(SqlRow)]
pub fn derive_sql_row(input: TokenStream) -> TokenStream {
    let DeriveInput {
        ident, data, attrs, ..
    } = parse_macro_input!(input);

    let fields = match data {
        Data::Struct(DataStruct {
            fields: Fields::Named(fields),
            ..
        }) => fields.named.into_iter().collect::<Vec<_>>(),
        Data::Struct(DataStruct {
            fields: Fields::Unit,
            ..
        }) => Vec::new(),
        _ => {
            return error(
                &ident,
                "SqlRow can only be derived for named-field or unit structs",
            )
            .into();
        }
    };

    let names = fields
        .iter()
        .map(|f| f.ident.clone().unwrap())
        .collect::<Vec<_>>();
    let types = fields.iter().map(|f| f.ty.clone()).collect::<Vec<_>>();
    let docs = fields
        .iter()
        .map(|f| doc_comment(&f.attrs))
        .collect::<Vec<_>>();
    let description = doc_comment(&attrs);

    quote! {
        impl ::picomint_core::sql::SqlRow for #ident {
            const DESCRIPTION: &'static str = #description;

            const COLUMNS: &'static [::picomint_core::sql::Column] = &[#(
                ::picomint_core::sql::Column {
                    name: stringify!(#names),
                    ty: <#types as ::picomint_core::sql::SqlColumn>::TYPE,
                    doc: #docs,
                }
            ),*];

            fn values(&self) -> Vec<::picomint_core::sql::SqlValue> {
                vec![#(::picomint_core::sql::SqlColumn::sql_value(&self.#names)),*]
            }
        }
    }
    .into()
}

#[proc_macro_derive(Decodable)]
pub fn derive_decodable(input: TokenStream) -> TokenStream {
    let DeriveInput {
        ident,
        data,
        generics,
        ..
    } = parse_macro_input!(input);

    let decode_body = match data {
        Data::Struct(DataStruct { fields, .. }) => derive_struct_decode(&ident, &fields),
        Data::Enum(DataEnum { variants, .. }) => derive_enum_decode(&ident, &variants),
        Data::Union(_) => error(&ident, "Decodable can't be derived for unions"),
    };
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    quote! {
        impl #impl_generics ::picomint_encoding::Decodable for #ident #ty_generics #where_clause {
            fn consensus_decode_partial<R: ::std::io::Read>(reader: &mut R) -> ::std::io::Result<Self> {
                #decode_body
            }
        }
    }
    .into()
}

// ─── Encode ─────────────────────────────────────────────────────────────

fn derive_struct_encode(fields: &Fields) -> TokenStream2 {
    if is_tuple_struct(fields) {
        let idxs = fields
            .iter()
            .enumerate()
            .map(|(i, _)| Index::from(i))
            .collect::<Vec<_>>();
        quote! {
            #(::picomint_encoding::Encodable::consensus_encode(&self.#idxs, writer)?;)*
            Ok(())
        }
    } else {
        let names = fields
            .iter()
            .map(|f| f.ident.clone().unwrap())
            .collect::<Vec<_>>();
        quote! {
            #(::picomint_encoding::Encodable::consensus_encode(&self.#names, writer)?;)*
            Ok(())
        }
    }
}

fn derive_enum_encode(ident: &Ident, variants: &Punctuated<Variant, Comma>) -> TokenStream2 {
    if variants.is_empty() {
        return quote! { match *self {} };
    }

    let arms = variant_indices(variants).into_iter().map(|(idx, variant)| {
        let vname = variant.ident.clone();
        let idx_lit = idx;

        if is_tuple_variant(&variant.fields) {
            let binds = variant
                .fields
                .iter()
                .enumerate()
                .map(|(i, _)| format_ident!("f{i}"))
                .collect::<Vec<_>>();
            quote! {
                #ident::#vname(#(#binds),*) => {
                    ::picomint_encoding::Encodable::consensus_encode(&#idx_lit, writer)?;
                    #(::picomint_encoding::Encodable::consensus_encode(#binds, writer)?;)*
                }
            }
        } else if variant.fields.is_empty() {
            quote! {
                #ident::#vname => {
                    ::picomint_encoding::Encodable::consensus_encode(&#idx_lit, writer)?;
                }
            }
        } else {
            let names = variant
                .fields
                .iter()
                .map(|f| f.ident.clone().unwrap())
                .collect::<Vec<_>>();
            quote! {
                #ident::#vname { #(#names),* } => {
                    ::picomint_encoding::Encodable::consensus_encode(&#idx_lit, writer)?;
                    #(::picomint_encoding::Encodable::consensus_encode(#names, writer)?;)*
                }
            }
        }
    });

    quote! {
        match self {
            #(#arms)*
        }
        Ok(())
    }
}

// ─── Decode ─────────────────────────────────────────────────────────────

fn derive_struct_decode(ident: &Ident, fields: &Fields) -> TokenStream2 {
    if is_tuple_struct(fields) {
        let binds = fields
            .iter()
            .enumerate()
            .map(|(i, _)| format_ident!("f{i}"))
            .collect::<Vec<_>>();
        quote! {
            #(let #binds = ::picomint_encoding::Decodable::consensus_decode_partial(reader)?;)*
            Ok(#ident(#(#binds),*))
        }
    } else if fields.is_empty() {
        quote! { Ok(#ident {}) }
    } else {
        let names = fields
            .iter()
            .map(|f| f.ident.clone().unwrap())
            .collect::<Vec<_>>();
        quote! {
            #(let #names = ::picomint_encoding::Decodable::consensus_decode_partial(reader)?;)*
            Ok(#ident { #(#names),* })
        }
    }
}

fn derive_enum_decode(ident: &Ident, variants: &Punctuated<Variant, Comma>) -> TokenStream2 {
    if variants.is_empty() {
        return quote! {
            Err(::std::io::Error::new(
                ::std::io::ErrorKind::InvalidData,
                concat!("Uninhabited enum ", stringify!(#ident), " cannot be decoded"),
            ))
        };
    }

    let arms = variant_indices(variants).into_iter().map(|(idx, variant)| {
        let vname = variant.ident.clone();
        let idx_lit = idx;

        let construct = if is_tuple_variant(&variant.fields) {
            let binds = variant
                .fields
                .iter()
                .enumerate()
                .map(|(i, _)| format_ident!("f{i}"))
                .collect::<Vec<_>>();
            quote! {
                {
                    #(let #binds = ::picomint_encoding::Decodable::consensus_decode_partial(reader)?;)*
                    Ok(#ident::#vname(#(#binds),*))
                }
            }
        } else if variant.fields.is_empty() {
            quote! { Ok(#ident::#vname) }
        } else {
            let names = variant
                .fields
                .iter()
                .map(|f| f.ident.clone().unwrap())
                .collect::<Vec<_>>();
            quote! {
                {
                    #(let #names = ::picomint_encoding::Decodable::consensus_decode_partial(reader)?;)*
                    Ok(#ident::#vname { #(#names),* })
                }
            }
        };

        quote! { #idx_lit => #construct, }
    });

    quote! {
        let variant = <u8 as ::picomint_encoding::Decodable>::consensus_decode_partial(reader)?;
        match variant {
            #(#arms)*
            other => Err(::std::io::Error::new(
                ::std::io::ErrorKind::InvalidData,
                format!("Invalid variant {} for {}", other, stringify!(#ident)),
            )),
        }
    }
}

// ─── Variant indexing ───────────────────────────────────────────────────

/// Extracts the u8 index from `#[encodable(index = N)]` if present.
fn parse_index_attribute(attributes: &[Attribute]) -> Option<u8> {
    attributes.iter().find_map(|attr| {
        if attr.path().is_ident("encodable") {
            attr.parse_args_with(|input: syn::parse::ParseStream| {
                input.parse::<syn::Ident>()?.span();
                input.parse::<Token![=]>()?;
                if let Lit::Int(lit_int) = input.parse::<Lit>()? {
                    lit_int.base10_parse()
                } else {
                    Err(input.error("Expected integer for 'index'"))
                }
            })
            .ok()
        } else {
            None
        }
    })
}

fn variant_indices(variants: &Punctuated<Variant, Comma>) -> Vec<(u8, Variant)> {
    assert!(
        variants.len() <= u8::MAX as usize + 1,
        "Encodable enums cannot have more than 256 variants (discriminant is a u8)"
    );

    let pairs = variants
        .iter()
        .cloned()
        .map(|v| (parse_index_attribute(&v.attrs), v))
        .collect::<Vec<_>>();

    let all = pairs.iter().all(|(idx, _)| idx.is_some());
    let none = pairs.iter().all(|(idx, _)| idx.is_none());
    assert!(
        all || none,
        "Either all or none of the variants should have an index annotation"
    );

    if all {
        pairs
            .into_iter()
            .map(|(idx, v)| (idx.expect("checked above"), v))
            .collect()
    } else {
        pairs
            .into_iter()
            .enumerate()
            .map(|(i, (_, v))| (u8::try_from(i).expect("checked above"), v))
            .collect()
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────

fn is_tuple_struct(fields: &Fields) -> bool {
    fields.iter().any(|f| f.ident.is_none()) && !fields.is_empty()
}

fn is_tuple_variant(fields: &Fields) -> bool {
    !fields.is_empty() && fields.iter().any(|f| f.ident.is_none())
}

fn error(_ident: &Ident, message: &str) -> TokenStream2 {
    panic!("{message}");
}
