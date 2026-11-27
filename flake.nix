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
            # tests' .snap files, moa-tmux's .tpl config template, the hook fixtures (.json)
            # and the recordings of moa's agent (.jsonl) must reach the build too.
            src = pkgs.lib.fileset.toSource {
              root = ./.;
              fileset = pkgs.lib.fileset.unions [
                (craneLib.fileset.commonCargoSources ./.)
                (pkgs.lib.fileset.fileFilter (f: f.hasExt "sql" || f.hasExt "snap" || f.hasExt "tpl" || f.hasExt "json" || f.hasExt "jsonl" || f.hasExt "proptest-regressions") ./crates)
                # moa's embedded constitution (`include_str!`)
                ./crates/moa-core/src/central/constitution.md
                # the moa-peers skill the session plugin ships (`include_str!`)
                ./crates/moa-agents/src/claude/skills/moa-peers.md
                # nextest's overrides (the hook budget test runs alone)
                ./.config/nextest.toml
                # the spike: a DDL check test reads its schema, and the forwarder tests run
                # a copy of it; removed with the spike
                ./spike
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

      # moa's agent: nixpkgs' adapter without its wrapper, which would pull in the unfree
      # `claude-code`. moa launches the user's own `claude`, never one from the store.
      adapterFor = system: (pkgsFor system).claude-agent-acp.overrideAttrs (_: { postInstall = ""; });

      # The recording of moa's agent the replay test plays (change it with the test's
      # `recording()`); its first line says which adapter version made it.
      recording = ./crates/moa-testkit/recordings/claude-agent-acp.jsonl;
    in
    {
      packages = forAllSystems (system:
        let
          b = builds.${system};
          claude-agent-acp = adapterFor system;
        in
        {
          inherit claude-agent-acp;
          default = b.craneLib.buildPackage (b.withArtifacts // {
            cargoExtraArgs = "--locked -p moa";
            # Read at compile time by `moa_agents::acp::BUILT_IN_ADAPTER`. Only this package
            # gets it: checks and the dev shell build without, so no test can reach the adapter.
            MOA_ACP_ADAPTER = "${claude-agent-acp}/bin/claude-agent-acp";
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
          # The recording was made by the adapter moa packages: a new adapter needs a new one.
          recording =
            let want = (adapterFor system).version; in
            b.pkgs.runCommand "recording" { nativeBuildInputs = [ b.pkgs.jq ]; } ''
              have=$(head -n 1 ${recording} | jq -r '.version')
              if [ "$have" != "${want}" ]; then
                echo "the recording is from claude-agent-acp $have, but moa packages ${want}: record it again" >&2
                exit 1
              fi
              touch $out
            '';
          # The Linux sandbox has no time zone database and no /etc/localtime, which every
          # real system has; jiff reads TZDIR and TZ. --no-fail-fast reports every failure of
          # a run, not just the first. The spike's forwarder tests run its scripts, which need
          # jq, sqlite3 and python3.
          nextest = b.craneLib.cargoNextest (b.withArtifacts // {
            nativeBuildInputs = [ b.pkgs.tmux b.pkgs.jq b.pkgs.sqlite b.pkgs.python3 ];
            TZDIR = "${b.pkgs.tzdata}/share/zoneinfo";
            TZ = "UTC";
            cargoNextestExtraArgs = "--no-fail-fast";
          });
        });

      devShells = forAllSystems (system:
        let b = builds.${system}; in
        {
          default = b.craneLib.devShell {
            packages = [
              b.pkgs.tmux
              # the spike's scripts, run by the forwarder tests
              b.pkgs.jq
              b.pkgs.sqlite
              b.pkgs.cargo-nextest
              b.pkgs.cargo-insta
              (adapterFor system)
            ] ++ b.pkgs.lib.optionals b.pkgs.stdenv.hostPlatform.isDarwin [ b.pkgs.libiconv ];
          };
        });
    };
}
