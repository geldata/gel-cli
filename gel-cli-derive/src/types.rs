use syn::spanned::Spanned;

use crate::attrib;

#[allow(dead_code)]
pub struct Struct {
    pub attrs: attrib::ContainerAttrs,
    pub vis: syn::Visibility,
    pub ident: syn::Ident,
    pub generics: syn::Generics,
    pub fields: Vec<Field>,
}

#[allow(dead_code)]
pub struct Enum {
    pub attrs: attrib::ContainerAttrs,
    pub vis: syn::Visibility,
    pub ident: syn::Ident,
    pub generics: syn::Generics,
    pub subcommands: Vec<Subcommand>,
}

#[allow(dead_code)]
pub struct Field {
    pub span: proc_macro2::Span,
    pub attrs: attrib::FieldAttrs,
    pub vis: syn::Visibility,
    pub ident: syn::Ident,
    pub optional: bool,
    pub multiple: bool,
    pub parse: attrib::CliParse,
    pub ty: syn::Type,
}

#[allow(dead_code)]
pub struct Subcommand {
    pub attrs: attrib::SubcommandAttrs,
    pub ident: syn::Ident,
    pub ty: Option<syn::Type>,
}

pub fn unwrap_type<'x>(ty: &'x syn::Type, name: &str) -> (bool, &'x syn::Type) {
    match ty {
        syn::Type::Path(syn::TypePath { qself: None, path }) => {
            if path.leading_colon.is_none()
                && path.segments.len() == 1
                && path.segments[0].ident == name
            {
                match &path.segments[0].arguments {
                    syn::PathArguments::AngleBracketed(ang) => {
                        if ang.args.len() == 1 {
                            match &ang.args[0] {
                                syn::GenericArgument::Type(typ) => (true, typ),
                                _ => (false, ty),
                            }
                        } else {
                            (false, ty)
                        }
                    }
                    _ => (false, ty),
                }
            } else {
                (false, ty)
            }
        }
        ty => (false, ty),
    }
}

impl Field {
    pub fn new(attrs: attrib::FieldAttrs, fld: syn::Field) -> Field {
        use attrib::CliParse;

        let (optional, ty) = unwrap_type(&fld.ty, "Option");
        let (multiple, ty) = unwrap_type(ty, "Vec");
        let parse = attrs.parse.clone().unwrap_or_else(|| {
            let kind = if matches!(ty, syn::Type::Path(syn::TypePath {path,..})
                    if path.is_ident("bool"))
            {
                attrib::ParserKind::FromFlag
            } else {
                attrib::ParserKind::TryFromStr
            };
            CliParse {
                kind,
                parser: None,
                span: fld.ty.span(),
            }
        });
        Field {
            span: fld.span(),
            attrs,
            vis: fld.vis,
            ident: fld.ident.unwrap(),
            optional,
            multiple,
            parse,
            ty: ty.clone(),
        }
    }
}
