{
  description = "Logitech TrueForce Linux driver (RS50 / G PRO / G923) - kernel module + userspace tools";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
      let
        version = (builtins.fromTOML (builtins.readFile (self + "/userspace/logi-wheel/Cargo.toml"))).workspace.package.version; 

        logitechTrueforceModuleFor = pkgs: { kernel, debug ? false }:
        let
          src = self; 
        in
        pkgs.stdenv.mkDerivation {
          pname = "logitech-trueforce-driver";
          inherit version src;
          nativeBuildInputs = kernel.moduleBuildDependencies;
          makeFlags = [
            "KDIR=${kernel.dev}/lib/modules/${kernel.modDirVersion}/build"
          ] ++ pkgs.lib.optionals debug [ "DEBUG=1" ];
          buildPhase = ''
            runHook preBuild
            (cd mainline && make $makeFlags all)
            runHook postBuild
          '';
          installPhase = ''
            moddir=$out/lib/modules/${kernel.modDirVersion}/extra
            mkdir -p "$moddir"
            cp mainline/hid-logitech-dd.ko "$moddir/"
          '';
          meta = with pkgs.lib; {
            description = "Kernel module for Logitech TrueForce wheels (RS50, G PRO, G923)";
            homepage = "https://github.com/mescon/logitech-trueforce-linux-driver";
            license = licenses.gpl2Only;
            platforms = platforms.linux;
          };
        };
    in
    flake-utils.lib.eachSystem [ "x86_64-linux" ] (system:
      let
        pkgs = import nixpkgs { inherit system; };

        src = self; 

        logitechTrueforceModule = logitechTrueforceModuleFor pkgs;
        
        logiWheel = pkgs.rustPlatform.buildRustPackage {
          pname = "logi-wheel";
          inherit version src;
          buildAndTestSubdir = "userspace/logi-wheel";
          cargoRoot = "userspace/logi-wheel";
          nativeBuildInputs = [ pkgs.pkg-config pkgs.gnumake pkgs.makeWrapper ];
          cargoLock = {
                lockFile = self + "/userspace/logi-wheel/Cargo.lock";
                };

          buildInputs = [ pkgs.fontconfig ]
            ++ pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.libxkbcommon pkgs.wayland ]
            ++ [ pkgs.libx11 pkgs.libxcursor pkgs.libxi pkgs.libxrandr ];

          postInstall = ''
                install -Dm644 desktop/logi-wheel-gui.desktop \
                        $out/share/applications/logi-wheel-gui.desktop
                install -Dm644 userspace/logi-wheel/crates/logi-wheel-gui/ui/assets/logo-mark.png \
                        $out/share/pixmaps/logi-wheel-gui.png
                '';

          postFixup = ''
            wrapProgram $out/bin/logi-wheel-gui \
              --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath [
                pkgs.wayland
                pkgs.libxkbcommon
                pkgs.libx11
                pkgs.libxcursor
                pkgs.libxi
                pkgs.libxrandr
                pkgs.libGL
                pkgs.vulkan-loader
                pkgs.libglvnd
              ]}
          '';

          meta = with pkgs.lib; {
            description = "Userspace CLI/TUI/GUI tools for the Logitech TrueForce driver";
            homepage = "https://github.com/mescon/logitech-trueforce-linux-driver";
            license = licenses.gpl2Only;
            platforms = platforms.linux;
            mainProgram = "logi-wheel";
          };
        };

        logiG923Modeswitch = pkgs.stdenv.mkDerivation {
          pname = "logi-g923-modeswitch";
          inherit version src;

          nativeBuildInputs = [ pkgs.makeWrapper ];
          dontBuild = true;

          installPhase = ''
            mkdir -p $out/bin
            cp tools/g923-xbox-modeswitch.sh $out/bin/logi-g923-modeswitch
            chmod +x $out/bin/logi-g923-modeswitch
            wrapProgram $out/bin/logi-g923-modeswitch \
              --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.usb-modeswitch ]}
          '';

          meta = with pkgs.lib; {
            description = "Switches a Logitech G923 (Xbox edition) out of console mode";
            homepage = "https://github.com/mescon/logitech-trueforce-linux-driver";
            license = licenses.gpl2Only;
            platforms = platforms.linux;
            mainProgram = "logi-g923-modeswitch";
          };
        };

        udevRules = pkgs.runCommand "logitech-trueforce-udev-rules" { } ''
          mkdir -p $out/lib/udev/rules.d
          cp ${src}/udev/70-logitech-trueforce.rules $out/lib/udev/rules.d/
          cp ${src}/udev/71-logi-ffb-uhid.rules $out/lib/udev/rules.d/
          cp ${src}/udev/72-logitech-g923-rebind.rules $out/lib/udev/rules.d/
          cp ${src}/udev/73-logitech-g923-xbox-modeswitch.rules $out/lib/udev/rules.d/

          substituteInPlace $out/lib/udev/rules.d/70-logitech-trueforce.rules --replace-quiet "/bin/sh" "${pkgs.runtimeShell}"
          substituteInPlace $out/lib/udev/rules.d/72-logitech-g923-rebind.rules --replace-quiet "/bin/sh" "${pkgs.runtimeShell}"
          substituteInPlace $out/lib/udev/rules.d/73-logitech-g923-xbox-modeswitch.rules --replace-quiet "/usr/bin/logi-g923-modeswitch" "${logiG923Modeswitch}/bin/logi-g923-modeswitch"
          substituteInPlace $out/lib/udev/rules.d/73-logitech-g923-xbox-modeswitch.rules --replace-quiet "/bin/sh" "${pkgs.runtimeShell}"
        '';

      in
      {
        packages = {
          default = logiWheel;
          logi-wheel = logiWheel;
          kernel-module = logitechTrueforceModule { kernel = pkgs.linuxPackages.kernel; };
          logi-g923-modeswitch = logiG923Modeswitch;
          udev-rules = udevRules;
        };

        checks = {
                tests = logiWheel.overrideAttrs (old: {
                doCheck = true;
                checkFlags = [ "--skip" "setup_t_arms_consent_and_only_y_plays" ];
                });
        };

        devShells.default = pkgs.mkShell {
          nativeBuildInputs = [ pkgs.pkg-config pkgs.cargo pkgs.rustc pkgs.rust-analyzer ];
          buildInputs = [
            pkgs.fontconfig
            pkgs.libxkbcommon
            pkgs.wayland
            pkgs.libx11
            pkgs.libxcursor
            pkgs.libxi
            pkgs.libxrandr
          ];
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [ pkgs.libGL pkgs.vulkan-loader ];
        };

        formatter = pkgs.nixpkgs-fmt;
      }
    ) // {
      nixosModules.default = { config, lib, pkgs, ... }:
        let 
          cfg = config.hardware.logitech-trueforce;
          logitechTrueforceModule = logitechTrueforceModuleFor pkgs;
        in {
          options.hardware.logitech-trueforce.enable = lib.mkEnableOption "Logitech TrueForce wheel driver";

          config = lib.mkIf cfg.enable {
            boot.extraModulePackages = [
                (logitechTrueforceModule {kernel = config.boot.kernelPackages.kernel; })
                ];
            boot.kernelModules = [ "hid-logitech-dd" ];
            services.udev.packages = [ self.packages.${pkgs.system}.udev-rules ];
            environment.systemPackages = [
              self.packages.${pkgs.system}.logi-wheel 
              self.packages.${pkgs.system}.logi-g923-modeswitch 
            ];
          };
        };
    };
}
