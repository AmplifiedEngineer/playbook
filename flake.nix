# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# The playbook binary from the GitHub release, checked against a fixed hash.
#
#   nix run github:pragmatic-engineer/playbook -- --version
#   nix profile install github:pragmatic-engineer/playbook
#
# The release workflow rewrites the releases block below after each release
# (scripts/render-flake.sh) and opens a pull request for it. Nothing else in
# this file is touched by it.
{
  description = "playbook: an engineering playbook for Claude Code";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs = { self, nixpkgs }:
    let
      # The version and the SRI hash of the release binary of each system.
      releases = {
        version = "0.40.0";
        x86_64-linux = "sha256-QfaKodYMw7pkLWbRQ7FFIahzd/LqDcbuQ8GiVu4rtYY=";
        aarch64-linux = "sha256-y+Q/liae7Qye/6zVGSTiypvbhBxZxUDp07zB+MimV7o=";
        x86_64-darwin = "sha256-goaQzNMt5Q0b8BMJA7VnHPJ6Bv6x4kFZBtp+DVxUPhI=";
        aarch64-darwin = "sha256-IXRXjZbNZbHD2DhJmAhdn8lRy6LLnTrtjA4zX0xjm/Y=";
      };

      # The release target of each Nix system. Linux is static musl.
      targets = {
        x86_64-linux = "x86_64-unknown-linux-musl";
        aarch64-linux = "aarch64-unknown-linux-musl";
        x86_64-darwin = "x86_64-apple-darwin";
        aarch64-darwin = "aarch64-apple-darwin";
      };

      systems = builtins.attrNames targets;
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f system nixpkgs.legacyPackages.${system});

      package = system: pkgs:
        pkgs.stdenvNoCC.mkDerivation {
          pname = "playbook";
          inherit (releases) version;

          src = pkgs.fetchurl {
            url = "https://github.com/pragmatic-engineer/playbook/releases/download/v${releases.version}/playbook-${releases.version}-${targets.${system}}";
            hash = releases.${system};
          };

          # A prebuilt binary: nothing to unpack or build. Stripping or
          # patching would change the Linux binary (static musl, nothing to
          # patch) and could void the ad-hoc signature of the macOS one.
          dontUnpack = true;
          dontBuild = true;
          dontStrip = true;
          dontFixup = true;

          installPhase = ''
            runHook preInstall
            install -Dm755 "$src" "$out/bin/playbook"
            runHook postInstall
          '';

          meta = {
            description = "An engineering playbook for Claude Code";
            homepage = "https://github.com/pragmatic-engineer/playbook";
            license = nixpkgs.lib.licenses.asl20;
            mainProgram = "playbook";
            platforms = systems;
            sourceProvenance = [ nixpkgs.lib.sourceTypes.binaryNativeCode ];
          };
        };
    in
    {
      packages = forAllSystems (system: pkgs: rec {
        playbook = package system pkgs;
        default = playbook;
      });

      apps = forAllSystems (system: pkgs: rec {
        playbook = {
          type = "app";
          program = "${self.packages.${system}.playbook}/bin/playbook";
        };
        default = playbook;
      });
    };
}
