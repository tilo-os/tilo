# Evaluated from a separately pinned protected-main Git input. The merge
# input is the synthetic main + PR candidate, never the source of this file.
{ git, merge, candidate, ... }:
let
  flake = builtins.getFlake (builtins.unsafeDiscardStringContext "path:${merge}");
  pkgs = (builtins.getFlake "github:NixOS/nixpkgs/7fc6f2c20af09cdcaf48b92ec3121860139ec668").legacyPackages.x86_64-linux;
  package = flake.packages.x86_64-linux.tilo-session;
in {
  package = package;
  gate = pkgs.runCommand "tilo-merge-candidate-receipt" {
    inherit candidate;
    _hydraAggregate = true;
    constituents = [ "package" ];
  } ''
    printf '%s\n' "$candidate" > "$out"
  '';
}
