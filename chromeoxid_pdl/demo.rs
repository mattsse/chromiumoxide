use std::fs;
use std::path::Path;

use heck::CamelCase;
use proc_macro2::TokenStream;
use prost_build::{Method, Service, ServiceGenerator};
use quote::{format_ident, quote};

#[derive(Debug, Default)]
pub struct ServiceConstantGenerator {
    service_id_counter: u64,
    service_tokens: TokenStream,
    services: Vec<Service>,
}

trait HrpcIdentifier {
    fn const_name(&self) -> String;
    fn id_tag(&self) -> u32;
    fn get_unknown_fields(&self) -> &Vec<prost::UnknownField>;
    fn id(&self) -> Option<u64> {
        let tag = self.id_tag();
        let fields = self.get_unknown_fields();
        let field = fields.iter().filter(|f| f.tag == tag).nth(0)?;
        let id = varintbuf::decode(&field.value[..]);
        Some(id)
    }
}

impl HrpcIdentifier for Service {
    fn const_name(&self) -> String {
        format!("{}_SERVICE_ID", self.name.to_uppercase())
    }
    fn id_tag(&self) -> u32 {
        50000
    }
    fn get_unknown_fields(&self) -> &Vec<prost::UnknownField> {
        &self.options.protobuf_unknown_fields
    }
}

impl HrpcIdentifier for Method {
    fn const_name(&self) -> String {
        format!("{}_METHOD_ID", self.name.to_uppercase())
    }
    fn id_tag(&self) -> u32 {
        50001
    }
    fn get_unknown_fields(&self) -> &Vec<prost::UnknownField> {
        &self.options.protobuf_unknown_fields
    }
}

fn generate_service_constants(service: &Service, service_id: u64) -> TokenStream {
    let mut stream = TokenStream::new();
    let service_ident = format_ident!("{}", service.const_name());
    stream.extend(quote! {
         pub const #service_ident: u64 = #service_id;
    });

    let mut method_ids = Vec::with_capacity(service.methods.len());

    for (i, method) in service.methods.iter().enumerate() {
        let method_id = method.id().unwrap_or_else(|| i as u64 + 1);
        method_ids.push(method_id);
        let ident = format_ident!(
            "{}_SERVICE_{}",
            service.name.to_uppercase(),
            method.const_name()
        );
        stream.extend(quote! {
            pub const #ident: u64 = #method_id;

        });
    }
    let method: Vec<_> = service
        .methods
        .iter()
        .map(|m| format_ident!("{}", m.name.to_camel_case()))
        .collect();

    let name = format_ident!("{}Service", service.name);
    let service_enum = quote! {

        #[derive(Copy, Debug, Clone, Eq, PartialEq, Hash)]
        pub enum #name {
            #(#method),*
        }

        impl #name {

            pub fn service_id(&self) -> u64 {
                #service_ident
            }

            pub fn method_id(&self) -> u64 {
                match self {
                    #(#name::#method => #method_ids),*
                }
            }
        }
    };

    // impl ::std::hash::Hash for D {
    //     fn hash<H: ::std::hash::Hasher>(&self, state: &mut H) {
    //         self.service_id().hash(state)
    //         self.method_id().hash(state)
    //     }
    // }

    quote! {
        #stream
        #service_enum
    }
}

impl ServiceGenerator for ServiceConstantGenerator {
    fn generate(&mut self, service: Service, _: &mut String) {
        let service_id = if let Some(id) = service.id() {
            id
        } else {
            self.service_id_counter += 1;
            self.service_id_counter
        };
        self.service_tokens
            .extend(generate_service_constants(&service, service_id));
        self.services.push(service);
    }

    fn finalize(&mut self, buf: &mut String) {
        let encodings = quote! {
            pub type Void = ();
        };
        buf.push_str(&encodings.to_string());
        if !self.service_tokens.is_empty() {
            let mut constants = TokenStream::default();
            std::mem::swap(&mut self.service_tokens, &mut constants);
            let constants = quote! {
                #constants
            };
            buf.push_str(&constants.to_string());
        }

        let service: Vec<_> = self
            .services
            .iter()
            .map(|s| format_ident!("{}Service", s.name))
            .collect();

        let service_var: Vec<_> = self
            .services
            .iter()
            .map(|s| format_ident!("{}", s.name))
            .collect();

        let service = quote! {

             #[derive(Copy, Debug, Clone, Eq, PartialEq, Hash)]
             pub enum Service {
                #(#service_var(#service)),*
             }

             impl Service {

                pub fn service_id(&self) -> u64 {
                     match self {
                        #(Service::#service_var(s) => s.service_id()),*
                    }
                }

                pub fn method_id(&self) -> u64 {
                    match self {
                         #(Service::#service_var(s) => s.method_id()),*
                    }
                }
            }
        };
        buf.push_str(&service.to_string());
    }
}

fn compile_hrpc() {
    let mut config = prost_build::Config::new();
    config.compile_well_known_types();
    config.extern_path(".hrpc.Void", "Void");
    config.service_generator(Box::new(ServiceConstantGenerator::default()));

    let hrpc_proto = r#"syntax = "proto2";

package hrpc;

import "google/protobuf/descriptor.proto";

extend google.protobuf.ServiceOptions {
  optional uint32 service = 50000;
}
extend google.protobuf.MethodOptions {
  optional uint32 method = 50001;
}

message Void {}"#;

    let tempdir = tempfile::Builder::new()
        .prefix("hrpc-build")
        .tempdir()
        .expect("Failed to create tempdir");
    fs::write(tempdir.path().join("hrpc.proto"), hrpc_proto)
        .expect("Failed to create temporary hrpc.proto");
    config
        .compile_protos(
            &[Path::new("src/hyperspace/schema.proto")],
            &[Path::new("src/hyperspace"), tempdir.path()],
        )
        .unwrap();
    fmt(&std::env::var("OUT_DIR").unwrap());
}

pub fn fmt(out_dir: &str) {
    use std::io::{self, Write};
    use std::process::{exit, Command};
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
            .arg(format!("{}/{}", out_dir, file))
            .output();

        match result {
            Err(e) => {
                eprintln!("error running rustfmt: {:?}", e);
                exit(1)
            }
            Ok(output) => {
                eprintln!("formatted {}", out_dir);
                if !output.status.success() {
                    io::stderr().write_all(&output.stderr).unwrap();
                    exit(output.status.code().unwrap_or(1))
                }
            }
        }
    }
}

fn main() {
    prost_build::compile_protos(&["src/hypercore/schema.proto"], &["src/hypercore"]).unwrap();
    compile_hrpc();
}
