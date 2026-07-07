{ rustPlatform, lib }:

rustPlatform.buildRustPackage {
  pname = "crok";
  version = "0.7.0";

  src = ./.;

  cargoLock = {
    lockFile = ./Cargo.lock;
  };

  cargoBuildFlags = [
    "-p"
    "crok"
  ];
  cargoTestFlags = [
    "-p"
    "crok"
  ];

  meta = with lib; {
    description = "crok: A literate CLI testing tool";
    homepage = "https://github.com/mmastrac/crok";
    license = [
      licenses.mit
      licenses.asl20
    ];
  };
}
