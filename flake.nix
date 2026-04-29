{
  inputs = {
    nixpkgs.url = "nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      utils,
    }:
    utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ rust-overlay.overlays.default ];
        pkgs = import nixpkgs { inherit system overlays; };
      in
      {
        devShells.default =
          let
            toolchain = pkgs.rust-bin.stable.latest.default.override {
              extensions = [
                "rust-src"
                "rustfmt"
                "clippy"
              ];
            };
          in
          pkgs.mkShell {
            packages = with pkgs; [
              rustc
              cargo
              rustfmt
              clippy
              rust-analyzer
              pkg-config
            ];

            shellHook = ''
              mkdir -p ~/.rust-rover/toolchain

              ln -sfn ${toolchain}/lib ~/.rust-rover/toolchain
              ln -sfn ${toolchain}/bin ~/.rust-rover/toolchain

              export RUST_SRC_PATH="$HOME/.rust-rover/toolchain/lib/rustlib/src/rust/library"
            '';
          };

        formatter = pkgs.nixfmt;
      }
    );
}
