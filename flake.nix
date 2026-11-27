{
  description = "phoenix, a plugin framework for Discord's Rust mobile client";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      nixpkgs,
      fenix,
      ...
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            config = {
              allowUnfree = true;
              android_sdk.accept_license = true;
            };
          };
          android = import ./nix/android.nix { inherit pkgs; };
          toolchain =
            with fenix.packages.${system};
            combine [
              latest.cargo
              latest.clippy
              latest.rust-src
              latest.rustc
              latest.rustfmt
              targets.aarch64-linux-android.latest.rust-std
              targets.armv7-linux-androideabi.latest.rust-std
              targets.x86_64-linux-android.latest.rust-std
              targets.wasm32-unknown-unknown.latest.rust-std
            ];
        in
        {
          default = pkgs.mkShell {
            packages = [
              toolchain
              android.sdk
              pkgs.cargo-ndk
              pkgs.esbuild
              pkgs.jdk_headless
              pkgs.nushell
              pkgs.rust-analyzer
              pkgs.typescript
              pkgs.wasm-bindgen-cli
            ];

            ANDROID_SDK_ROOT = android.sdkRoot;
            ANDROID_NDK_ROOT = android.ndkRoot;
            ANDROID_NDK_HOME = android.ndkRoot;
            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
          };
        }
      );

      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt);
    };
}
