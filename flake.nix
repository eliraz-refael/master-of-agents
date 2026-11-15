{
  description = "moa: master of agents";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, crane, rust-overlay }:
    let
      systems = [ "aarch64-darwin" "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f system);
      pkgsFor = system: import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
      };
      craneLibFor = pkgs: (crane.mkLib pkgs).overrideToolchain
        (p: p.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml);

      # Everything a derivation needs, per system.
      buildFor = system:
        let
          pkgs = pkgsFor system;
          craneLib = craneLibFor pkgs;
          commonArgs = {
            # The root Cargo.toml is a virtual workspace, so crane cannot infer these.
            pname = "moa";
            version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
            src = craneLib.cleanCargoSource ./.;
            strictDeps = true;
            buildInputs = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isDarwin [ pkgs.libiconv ];
            nativeCheckInputs = [ pkgs.tmux ];
          };
          cargoArtifacts = craneLib.buildDepsOnly commonArgs;
          withArtifacts = commonArgs // { inherit cargoArtifacts; };
        in
        { inherit pkgs craneLib withArtifacts cargoArtifacts; };
    in
    {
      packages = forAllSystems (system:
        let b = buildFor system; in
        {
          default = b.craneLib.buildPackage (b.withArtifacts // {
            doCheck = false;
            meta.mainProgram = "moa";
          });
        });

      checks = forAllSystems (system:
        let b = buildFor system; in
        {
          moa = self.packages.${system}.default;
          clippy = b.craneLib.cargoClippy (b.withArtifacts // {
            cargoClippyExtraArgs = "--all-targets -- -D warnings";
          });
          fmt = b.craneLib.cargoFmt { inherit (b.withArtifacts) pname version src; };
          nextest = b.craneLib.cargoNextest (b.withArtifacts // {
            nativeBuildInputs = [ b.pkgs.tmux ];
          });
        });

      devShells = forAllSystems (system:
        let b = buildFor system; in
        {
          default = b.craneLib.devShell {
            packages = [
              b.pkgs.tmux
              b.pkgs.cargo-nextest
              b.pkgs.cargo-insta
            ] ++ b.pkgs.lib.optionals b.pkgs.stdenv.hostPlatform.isDarwin [ b.pkgs.libiconv ];
          };
        });
    };
}
