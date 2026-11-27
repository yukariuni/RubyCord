{ pkgs }:

let
  composition = pkgs.androidenv.composeAndroidPackages {
    platformVersions = [ "36" ];
    buildToolsVersions = [ "36.0.0" ];
    includeNDK = true;
  };
  sdkRoot = "${composition.androidsdk}/libexec/android-sdk";
in
{
  inherit sdkRoot;
  sdk = composition.androidsdk;
  ndkRoot = "${sdkRoot}/ndk-bundle";
}
