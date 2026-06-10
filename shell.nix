{ pkgs ? import <nixpkgs> {} }:

let
  drv = pkgs.callPackage ./default.nix {};
in
pkgs.mkShell {
  inputsFrom = [ drv ];

  nativeBuildInputs = with pkgs; [
    cargo
    rustc
    rust-analyzer
  ];

  shellHook = ''
    export LD_LIBRARY_PATH=${pkgs.lib.makeLibraryPath drv.buildInputs}:$LD_LIBRARY_PATH
  '';
}
