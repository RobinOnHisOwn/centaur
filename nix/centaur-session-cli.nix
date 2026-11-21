{
  lib,
  rustPlatform,
  aws-lc,
  cacert,
  pkg-config,
  src,
}:
rustPlatform.buildRustPackage {
  pname = "centaur-session-cli";
  version = "0.1.0";

  inherit src;

  cargoHash = "sha256-aYNXYgugqeA4RY8OS1B/ty08cjwEA1GTh9Zct0dBTR4=";
  cargoBuildFlags = ["--package=centaur-session-cli"];
  cargoTestFlags = ["--package=centaur-session-cli"];

  nativeBuildInputs = [pkg-config];
  buildInputs = [aws-lc];

  preCheck = ''
    export SSL_CERT_FILE=${cacert}/etc/ssl/certs/ca-bundle.crt
  '';

  postInstall = ''
    mv "$out/bin/centaur-session-cli" "$out/bin/centaur"
  '';

  meta = {
    description = "Terminal client for Centaur sessions";
    homepage = "https://github.com/RobinOnHisOwn/centaur";
    license = lib.licenses.mit;
    mainProgram = "centaur";
    platforms = lib.platforms.unix;
  };
}
