{ lib
, rustPlatform
, pkg-config
, wayland
, wayland-protocols
, cairo
, pango
, glib
, libxkbcommon
, fontconfig
, wayland-scanner
, gobject-introspection
}:

rustPlatform.buildRustPackage rec {
  pname = "hyprexpose";
  version = "0.1.0";

  src = ./.;

  cargoLock = {
    lockFile = ./Cargo.lock;
  };

  nativeBuildInputs = [
    pkg-config
    wayland-scanner
    gobject-introspection
  ];

  buildInputs = [
    wayland
    wayland-protocols
    cairo
    pango
    glib
    libxkbcommon
    fontconfig
  ];

  postInstall = ''
    install -Dm644 config.example.toml $out/share/hyprexpose/config.example.toml
  '';

  meta = with lib; {
    description = "Lightweight workspace overview for Hyprland and Sway";
    homepage = "https://github.com/ThiagoAVicente/hyprexpose";
    license = licenses.mit;
    platforms = platforms.linux;
    mainProgram = "hyprexpose";
  };
}
