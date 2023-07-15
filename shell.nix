{ pkgs ? import (fetchTarball "https://github.com/NixOS/nixpkgs/archive/fd3e33d696b81e76b30160dfad2efb7ac1f19879.tar.gz") {}
}:
pkgs.mkShell {
  buildInputs = [
    pkgs.stdenv.cc.cc.lib
    pkgs.which
    pkgs.rustup
    pkgs.libiconv
    pkgs.git
    pkgs.openssh
    pkgs.openssl.dev
    pkgs.pkg-config
    pkgs.cacert
    pkgs.zlib
  ] ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [ pkgs.darwin.apple_sdk.frameworks.SystemConfiguration ] ;
  LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
  LD_LIBRARY_PATH="${pkgs.stdenv.cc.cc.lib}/lib:${pkgs.zlib}/lib";
  RUSTC_VERSION = pkgs.lib.readFile ./rust-toolchain;
  RUST_BACKTRACE=1;
  CARGO_HOME="";
}