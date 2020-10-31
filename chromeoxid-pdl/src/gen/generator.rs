use crate::gen::types::*;
use crate::pdl::parser::parse_pdl;
use crate::pdl::{DataType, Domain, Param, Type, Variant};
use heck::*;
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs;
use std::io::{self, Error, ErrorKind};
use std::ops::Deref;
use std::path::{Path, PathBuf};

/// Compile `.pdl` files into Rust files during a Cargo build.
///
/// The generated `.rs` files are written to the Cargo `OUT_DIR` directory,
/// suitable for use with
///
/// This function should be called in a project's `build.rs`.
///
/// # Arguments
///
/// **`pdls`** - Paths to `.pdl` files to compile.
///
/// # Errors
///
/// This function can fail for a number of reasons:
///
///   - Failure to locate `pdl` files.
///   - Failure to parse the `.pdl`s.
///
/// It's expected that this function call be `unwrap`ed in a `build.rs`; there
/// is typically no reason to gracefully recover from errors during a build.
///
/// # Example `build.rs`
///
/// ```rust,no_run
/// # use std::io::Result;
/// fn main() -> Result<()> {
///   pdl_build::compile_protos(&["src/frontend.proto", "src/backend.proto"], &["src"])?;
///   Ok(())
/// }
/// ```
pub fn compile_pdls<P: AsRef<Path>>(pdls: &[P]) -> io::Result<()> {
    Generator::default().compile_pdls(pdls)
}

/// Generates rust code for the Chrome DevTools Protocol
#[derive(Debug, Clone)]
pub struct Generator {
    serde_support: SerdeSupport,
    with_experimental: bool,
    with_deprecated: bool,
    out_dir: Option<PathBuf>,
    protocol_mods: Vec<String>,
    domains: HashMap<String, usize>,
}

impl Default for Generator {
    fn default() -> Self {
        Self {
            serde_support: Default::default(),
            with_experimental: true,
            with_deprecated: false,
            out_dir: None,
            protocol_mods: vec![],
            domains: Default::default(),
        }
    }
}

impl Generator {
    /// Configures the output directory where generated Rust files will be
    /// written.
    ///
    /// If unset, defaults to the `OUT_DIR` environment variable. `OUT_DIR` is
    /// set by Cargo when executing build scripts, so `out_dir` typically
    /// does not need to be configured.
    pub fn out_dir<P>(&mut self, path: P) -> &mut Self
    where
        P: Into<PathBuf>,
    {
        self.out_dir = Some(path.into());
        self
    }

    /// Configures the serde support that should be included for all the
    /// generated types.
    pub fn serde(&mut self, serde: SerdeSupport) -> &mut Self {
        self.serde_support = serde;
        self
    }

    /// Configures whether experimental types and fields should be included.
    ///
    /// Disabling experimental types may result in missing type definitions
    /// (E0412)
    pub fn experimental(&mut self, experimental: bool) -> &mut Self {
        self.with_experimental = experimental;
        self
    }

    /// Configures whether deprecated types and fields should be included.
    pub fn deprecated(&mut self, deprecated: bool) -> &mut Self {
        self.with_deprecated = deprecated;
        self
    }

    /// Compile `.pdls` files into Rust files during a Cargo build with
    /// additional code generator configuration options.
    ///
    /// This method is like the `chromeoxid_pdl::build::compile_pdls` function,
    /// with the added ability to specify non-default code generation
    /// options. See that function for more information about the arguments
    /// and generated outputs.
    ///
    /// # Example `build.rs`
    ///
    /// ```rust,no_run
    /// # use std::io::Result;
    /// fn main() -> Result<()> {
    ///   let mut pdl_build = chromeoxid_pdl::build::Generator::default();
    ///   pdl_build.out_dir("some/path");
    ///   pdl_build.compile_pdls(&["src/frontend.pdl", "src/backend.pdl"])?;
    ///   Ok(())
    /// }
    /// ```
    pub fn compile_pdls<P: AsRef<Path>>(&mut self, pdls: &[P]) -> io::Result<()> {
        let target: PathBuf = self.out_dir.clone().map(Ok).unwrap_or_else(|| {
            std::env::var_os("OUT_DIR")
                .ok_or_else(|| {
                    Error::new(ErrorKind::Other, "OUT_DIR environment variable is not set")
                })
                .map(Into::into)
        })?;

        let mut inputs = vec![];

        for path in pdls {
            let path = path.as_ref();
            let file_name = path.file_stem().ok_or_else(|| {
                Error::new(
                    ErrorKind::Other,
                    format!("Failed to read file name for {}", path.display()),
                )
            })?;
            let mod_name = file_name.to_string_lossy().to_string();
            self.protocol_mods.push(mod_name);

            inputs.push(fs::read_to_string(path)?);
        }

        let mut protocols = vec![];

        for (idx, input) in inputs.iter().enumerate() {
            let pdl = parse_pdl(&input).map_err(|e| Error::new(ErrorKind::Other, e.message))?;

            self.domains
                .extend(pdl.domains.iter().map(|d| (d.name.to_string(), idx)));

            protocols.push(pdl);
        }

        for (idx, pdl) in protocols.iter().enumerate() {
            let types = self.generate_types(&pdl.domains);
            let version = format!("{}.{}", pdl.version.major, pdl.version.minor);
            let module = quote! {
                #![allow(clippy::too_many_arguments)]

                /// The version of this protocol definition
                pub const VERSION : &str = #version;
                #types
            };

            let output = target.join(format!("{}.rs", &self.protocol_mods[idx]));

            fs::write(output, module.to_string()).unwrap();
        }

        fmt(target);
        Ok(())
    }

    /// Generate the types for the domains.
    ///
    /// Each domain gets it's owon module
    pub fn generate_types(&self, domains: &[Domain]) -> TokenStream {
        let mut lib = TokenStream::default();

        for domain in domains
            .iter()
            .filter(|d| self.with_deprecated || !d.deprecated)
            .filter(|d| self.with_experimental || !d.experimental)
        {
            let domain_mod = self.generate_domain(domain);
            let mod_name = format_ident!("{}", domain.name.to_snake_case());

            let mut desc = if let Some(desc) = domain.description.as_ref() {
                quote! {
                    #[doc = #desc]
                }
            } else {
                TokenStream::default()
            };

            if domain.deprecated {
                desc.extend(quote! {#[deprecated]})
            }

            lib.extend(quote! {
                #desc
                pub mod #mod_name {
                    #domain_mod
                }
            });
        }

        lib
    }

    /// Generates all types are not circular for a single domain
    pub fn generate_domain(&self, domain: &Domain) -> TokenStream {
        let mut stream = self.serde_support.generate_serde_imports();
        stream.extend(
            domain
                .into_iter()
                .filter(|dt| self.with_deprecated || !dt.is_deprecated())
                .filter(|dt| self.with_experimental || !dt.is_experimental())
                .map(|ty| self.generate_type(domain, ty)),
        );
        stream
    }

    fn generate_type(&self, domain: &Domain, dt: DomainDatatype) -> TokenStream {
        let stream = if let Some(vars) = dt.as_enum() {
            self.generate_enum(&Variant::from(&dt), vars)
        } else {
            let mut stream = self.generate_struct(
                domain,
                &dt,
                dt.ident_name(),
                dt.params()
                    .filter(|dt| self.with_deprecated || !dt.is_deprecated())
                    .filter(|dt| self.with_experimental || !dt.is_experimental()),
            );
            if let DomainDatatype::Commnad(cmd) = dt {
                let returns_name = format!("{}Returns", cmd.name().to_camel_case());
                stream.extend(
                    self.generate_struct(
                        domain,
                        &dt,
                        returns_name,
                        cmd.returns
                            .iter()
                            .filter(|p| self.with_deprecated || !p.is_deprecated())
                            .filter(|p| self.with_experimental || !p.is_experimental()),
                    ),
                );
            }
            stream
        };
        if dt.is_deprecated() {
            quote! {
                #[deprecated]
                #stream
            }
        } else {
            stream
        }
    }

    /// Generates the struct definitions including enum definitions inner
    /// parameter enums
    fn generate_struct<'a, T: 'a>(
        &self,
        domain: &Domain,
        dt: &DomainDatatype,
        struct_ident: String,
        params: T,
    ) -> TokenStream
    where
        T: Iterator<Item = &'a Param<'a>> + 'a,
    {
        // also generate enums for inner enums
        let mut enum_definitions = TokenStream::default();
        let mut field_definitions = vec![];
        let mut mandatory_names = vec![];
        let mut mandatory_types = vec![];
        let mut optional_names = vec![];

        for param in params {
            if let Type::Enum(vars) = &param.r#type {
                let enum_ident = Variant {
                    description: param.description().map(Cow::Borrowed),
                    name: Cow::Owned(subenum_name(dt.name(), param.name())),
                };
                if param.is_deprecated() {
                    enum_definitions.extend(quote! {#[deprecated]});
                }
                enum_definitions.extend(self.generate_enum(&enum_ident, vars));
            }
            let field_name = format_ident!("{}", field_name(param.name()));
            if !param.optional {
                mandatory_names.push(field_name);
                mandatory_types.push(self.generate_field_type(
                    domain,
                    dt.name(),
                    param.name(),
                    &param.r#type,
                ));
            } else {
                optional_names.push(field_name);
            }
            field_definitions.push(self.generate_field(domain, dt.name(), param));
        }

        let name = format_ident!("{}", struct_ident);

        let derives = if mandatory_types.is_empty() && !field_definitions.is_empty() {
            quote! { #[derive(Debug, Clone, PartialEq, Default)]}
        } else {
            quote! {#[derive(Debug, Clone, PartialEq)] }
        };
        let serde_derives = self.serde_support.generate_derives();

        let impl_definition = if mandatory_types.is_empty() {
            TokenStream::default()
        } else {
            quote! {
                impl #name {
                    pub fn new(
                        #(#mandatory_names: #mandatory_types),*
                    ) -> #name {
                        Self {
                            #(#mandatory_names,)*
                            #(#optional_names: Default::default()),*
                        }
                    }
                }
            }
        };

        let desc = dt.type_description_tokens(domain.name.as_ref());

        // create wrapper types if no fields present
        if field_definitions.is_empty() {
            if let DomainDatatype::Type(tydef) = dt {
                let wrapped_ty =
                    self.generate_field_type(domain, dt.name(), dt.name(), &tydef.extends);
                quote! {
                    #desc
                    #derives
                    #serde_derives
                    pub struct #name(#wrapped_ty);
                }
            } else {
                quote! {
                    #desc
                    #derives
                    #serde_derives
                    pub struct #name;
                }
            }
        } else {
            quote! {
                #desc
                #derives
                #serde_derives
                pub struct #name {
                    #(#field_definitions),*
                }
                #impl_definition
                #enum_definitions
            }
        }
    }

    /// Generate enum type with `as_str` and `FromStr` methods
    fn generate_enum(&self, ident: &Variant, variants: &[Variant]) -> TokenStream {
        let vars = variants
            .iter()
            .map(|v| self.serde_support.generate_variant(v));

        let name = format_ident!(
            "{}",
            ident
                .name
                .as_ref()
                .rsplit('.')
                .next()
                .unwrap()
                .to_camel_case()
        );

        let desc = if let Some(desc) = ident.description.as_ref() {
            quote! {
                #[doc = #desc]
            }
        } else {
            TokenStream::default()
        };

        let attr = self.serde_support.generate_derives();

        let ty_def = quote! {
            #desc
            #[derive(Debug, Clone, PartialEq, Eq, Hash)]
            #attr
            pub enum #name {
                #(#vars),*
            }
        };

        // from str to string impl
        let (vars, strs): (Vec<_>, Vec<_>) = variants
            .iter()
            .map(|s| (format_ident!("{}", s.name.to_camel_case()), s.name.as_ref()))
            .unzip();

        quote! {
            #ty_def
            impl #name {
                pub fn as_str(&self) -> &'static str {
                    match self {
                        #( #name::#vars => #strs ),*
                    }
                }
            }

            impl ::std::str::FromStr for #name {
                type Err = String;

                fn from_str(s: &str) -> Result<Self, Self::Err> {
                    match s {
                        #(#strs => Ok(#name::#vars),)*
                        _=> Err(s.to_string())
                    }
                }
            }
        }
    }

    fn generate_field_type(
        &self,
        domain: &Domain,
        parent: &str,
        param_name: &str,
        ty: &Type,
    ) -> TokenStream {
        match ty {
            Type::Integer => {
                quote! {
                    i64
                }
            }
            Type::Number => {
                quote! {
                    f64
                }
            }
            Type::Boolean => {
                quote! {
                    bool
                }
            }
            Type::String => {
                quote! {
                    String
                }
            }
            Type::Object | Type::Any => {
                quote! {serde_json::Value}
            }
            Type::Binary => {
                quote! {Vec<u8>}
            }
            Type::Enum(_) => {
                // TODO name resolution
                let ty = format_ident!("{}", subenum_name(parent, param_name));
                quote! {#ty}
            }
            Type::ArrayOf(ty) => {
                // recursive types don't need to be boxed in vec
                let ty = if let Type::Ref(name) = ty.deref() {
                    self.projected_type(domain, name)
                } else {
                    self.generate_field_type(domain, parent, param_name, &*ty)
                };
                quote! {
                    Vec<#ty>
                }
            }
            Type::Ref(name) => {
                // consider recursive types
                if name == parent {
                    let ident = format_ident!("{}", name.to_camel_case());
                    quote! {
                        Box<#ident>
                    }
                } else {
                    self.projected_type(domain, name)
                }
            }
        }
    }

    /// Resolve projections: `Runtime.ScriptId` where `Runtime` is the
    /// referenced domain where `ScriptId` is defined.
    ///
    /// In order to resolve cross pdl references a domain check is necessary.
    /// If the referenced domain is defined in another pdl than the `domain`'s
    /// pdl, we need to move up an additional level (`super::super`)
    fn projected_type(&self, domain: &Domain, name: &str) -> TokenStream {
        let mut iter = name.rsplitn(2, '.');
        let ty_name = iter.next().unwrap();
        let path = iter.collect::<String>();
        let ident = format_ident!("{}", ty_name.to_camel_case());
        if path.is_empty() {
            quote! {
                #ident
            }
        } else {
            let current_domain_idx = self.domains.get(domain.name.as_ref()).unwrap();
            let ref_domain_idx = self
                .domains
                .get(&path)
                .unwrap_or_else(|| panic!("No referenced domain found for {}", path));

            if *current_domain_idx == *ref_domain_idx {
                let super_ident = format_ident!("{}", path.to_snake_case());
                quote! {
                    super::#super_ident::#ident
                }
            } else {
                let mod_name = format_ident!("{}", self.protocol_mods[*ref_domain_idx]);
                let super_ident = format_ident!("{}", path.to_snake_case());
                quote! {
                    super::super::#mod_name::#super_ident::#ident
                }
            }
        }
    }

    fn generate_field(&self, domain: &Domain, dt_name: &str, param: &Param) -> TokenStream {
        let mut desc = if let Some(desc) = param.description() {
            quote! {
                #[doc = #desc]
            }
        } else {
            TokenStream::default()
        };

        if param.is_deprecated() {
            desc.extend(quote! {#[deprecated]});
        }

        let name = format_ident!("{}", field_name(param.name()));
        let ty = self.generate_field_type(domain, dt_name, param.name(), &param.r#type);

        if param.optional {
            let attr = self.serde_support.generate_opt_field_attr();
            quote! {
                #desc
                #attr
                pub #name : Option<#ty>
            }
        } else {
            let attr = if let Type::ArrayOf(_) = &param.r#type {
                self.serde_support.generate_vec_field_attr()
            } else {
                TokenStream::default()
            };

            quote! {
                #desc
                #attr
                pub #name : #ty
            }
        }
    }
}

fn field_name(name: &str) -> String {
    let name = name.to_snake_case();
    match name.as_str() {
        "type" => "r#type".to_string(),
        "mod" => "r#mod".to_string(),
        "override" => "r#override".to_string(),
        _ => name,
    }
}

fn type_name(name: &str) -> String {
    let name = name.to_camel_case();
    match name.as_str() {
        "type" => "r#type".to_string(),
        "mod" => "r#mod".to_string(),
        "override" => "r#override".to_string(),
        _ => name,
    }
}

fn subenum_name(parent: &str, inner: &str) -> String {
    format!("{}{}", parent.to_camel_case(), type_name(inner))
}

#[derive(Debug, Clone)]
pub enum SerdeSupport {
    None,
    Default,
    Feature(String),
}

impl SerdeSupport {
    pub fn with_feature(feature: impl Into<String>) -> Self {
        SerdeSupport::Feature(feature.into())
    }

    fn generate_enum_serde(&self) -> TokenStream {
        todo!("impl custom serialize with")
    }

    fn generate_serde_imports(&self) -> TokenStream {
        match self {
            SerdeSupport::None => TokenStream::default(),
            SerdeSupport::Default => quote! {
                 use serde::{Serialize, Deserialize};
            },
            SerdeSupport::Feature(feature) => {
                quote! {
                    #[cfg(feature = #feature)]
                    use serde::{Serialize, Deserialize};
                }
            }
        }
    }

    fn generate_derives(&self) -> TokenStream {
        match self {
            SerdeSupport::None => TokenStream::default(),
            SerdeSupport::Default => quote! {
                #[derive(Serialize, Deserialize)]
                #[serde(rename_all = "camelCase")]
            },
            SerdeSupport::Feature(feature) => {
                quote! {
                    #[cfg_attr(feature = #feature, derive(Serialize, Deserialize))]
                    #[cfg_attr(feature = #feature,  serde(rename_all = "camelCase"))]
                }
            }
        }
    }

    fn generate_opt_field_attr(&self) -> TokenStream {
        match self {
            SerdeSupport::None => TokenStream::default(),
            SerdeSupport::Default => quote! {
                 #[serde(skip_serializing_if = "Option::is_none")]
            },
            SerdeSupport::Feature(feature) => {
                quote! {
                     #[cfg_attr(feature = #feature, serde(skip_serializing_if = "Option::is_none"))]
                }
            }
        }
    }

    fn generate_vec_field_attr(&self) -> TokenStream {
        match self {
            SerdeSupport::None => TokenStream::default(),
            SerdeSupport::Default => quote! {
                 #[serde(skip_serializing_if = "Vec::is_empty")]
            },
            SerdeSupport::Feature(feature) => {
                quote! {
                     #[cfg_attr(feature = #feature, serde(skip_serializing_if = "Vec::is_empty"))]
                }
            }
        }
    }

    fn generate_variant(&self, var: &Variant) -> TokenStream {
        let v = format_ident!("{}", var.name.to_camel_case());
        if let Some(desc) = var.description.as_ref() {
            quote! {
                #[doc = #desc]
                #v
            }
        } else {
            quote! {
                #v
            }
        }
    }
}

impl Default for SerdeSupport {
    fn default() -> Self {
        SerdeSupport::Default
    }
}

pub fn fmt(out_dir: impl AsRef<Path>) {
    use std::io::Write;
    use std::process::{exit, Command};
    let out_dir = out_dir.as_ref();
    let dir = std::fs::read_dir(out_dir).unwrap();

    for entry in dir {
        let file = entry.unwrap().file_name().into_string().unwrap();
        if !file.ends_with(".rs") {
            continue;
        }
        let result = Command::new("rustfmt")
            .arg("--emit")
            .arg("files")
            .arg("--edition")
            .arg("2018")
            .arg(out_dir.join(file))
            .output();

        match result {
            Err(e) => {
                eprintln!("error running rustfmt: {:?}", e);
                exit(1)
            }
            Ok(output) => {
                eprintln!("formatted {}", out_dir.display());
                if !output.status.success() {
                    io::stderr().write_all(&output.stderr).unwrap();
                    exit(output.status.code().unwrap_or(1))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_serde_import() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        Generator::default()
            .out_dir(dir.join("src"))
            .serde(SerdeSupport::with_feature("serde0"))
            .compile_pdls(&[
                dir.join("js_protocol.pdl"),
                dir.join("browser_protocol.pdl"),
            ])
            .unwrap();
    }
}
