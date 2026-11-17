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
            # cleanCargoSource keeps only .rs/.toml/Cargo.lock; the store's .sql and the
            # tests' .snap files and moa-tmux's .tpl config template must reach the build too.
            src = pkgs.lib.fileset.toSource {
              root = ./.;
              fileset = pkgs.lib.fileset.unions [
                (craneLib.fileset.commonCargoSources ./.)
                (pkgs.lib.fileset.fileFilter (f: f.hasExt "sql" || f.hasExt "snap" || f.hasExt "tpl" || f.hasExt "proptest-regressions") ./crates)
                # read by a DDL check test; removed with the spike
                ./spike/master-chat/schema.sql
                ./spike/moa-proto/picture.py
              ];
            };
            strictDeps = true;
            buildInputs = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isDarwin [ pkgs.libiconv ];
          };
          cargoArtifacts = craneLib.buildDepsOnly commonArgs;
          withArtifacts = commonArgs // { inherit cargoArtifacts; };
        in
        { inherit pkgs craneLib withArtifacts cargoArtifacts; };
      builds = forAllSystems buildFor;
    in
    {
      packages = forAllSystems (system:
        let b = builds.${system}; in
        {
          default = b.craneLib.buildPackage (b.withArtifacts // {
            cargoExtraArgs = "--locked -p moa";
            doCheck = false;
            # `moactl` is `moa` under another name: the binary dispatches on argv[0].
            postInstall = "ln -s moa $out/bin/moactl";
            meta.mainProgram = "moa";
          });
        });

      apps = forAllSystems (system: {
        moactl = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/moactl";
        };
      });

      checks = forAllSystems (system:
        let b = builds.${system}; in
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
        let b = builds.${system}; in
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
